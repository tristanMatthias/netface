// netface — P2P ASCII webcam + audio over raw UDP
//
// USAGE
//   Both parties run the same binary:
//     Alice:  netface --peer bob.ip:4444
//     Bob:    netface --peer alice.ip:4444
//
//   Before the peer connects you see your own camera full-screen.
//   The moment their first packet arrives the view snaps to a 50/50
//   left (you) / right (them) split automatically.
//
// CONFIG
//   Settings are loaded from ~/.config/netface/config.toml
//   Run with --init-config to create a default config file.

use std::io::Write;
use std::net::UdpSocket;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

use clap::Parser;
use crossbeam_channel::bounded;

mod audio;
mod config;
mod net;
mod theme;
mod video;

use config::Config;
use theme::{build_theme, ThemeRenderer};

#[derive(Parser, Debug)]
#[command(
    name = "netface",
    about = "P2P ASCII video + audio — no servers, no accounts, just UDP"
)]
struct Args {
    /// Peer address, e.g. 192.168.1.42:4444
    #[arg(short, long)]
    peer: Option<String>,

    /// Local UDP port to listen on
    #[arg(short = 'P', long)]
    port: Option<u16>,

    /// Webcam device index (0 = default)
    #[arg(short, long)]
    camera: Option<usize>,

    /// Target capture FPS
    #[arg(short, long)]
    fps: Option<u64>,

    /// ANSI truecolor output (richer image, bigger packets)
    #[arg(short = 'C', long)]
    color: bool,

    /// Disable audio
    #[arg(long)]
    no_audio: bool,

    /// Enable background removal (requires more CPU)
    #[arg(short = 'b', long)]
    bg_removal: bool,

    /// Background color when removal is enabled (hex RGB, e.g., "000000" for black)
    #[arg(long)]
    bg_color: Option<String>,

    /// Character theme (classic, blocks, dots, emoji-faces, emoji-moon, etc.)
    #[arg(short = 't', long)]
    theme: Option<String>,

    /// Color mode (original, mono-green, matrix, cyberpunk, sunset, etc.)
    #[arg(short = 'm', long)]
    color_mode: Option<String>,

    /// Create default config file at ~/.config/netface/config.toml
    #[arg(long)]
    init_config: bool,

    /// Print example config to stdout
    #[arg(long)]
    example_config: bool,
}

fn main() {
    let args = Args::parse();

    // Handle config commands
    if args.example_config {
        print!("{}", config::example_config());
        return;
    }

    if args.init_config {
        match Config::save_default() {
            Ok(path) => {
                println!("Created config file at: {}", path.display());
                println!("Edit this file to customize settings.");
            }
            Err(e) => {
                eprintln!("Failed to create config file: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    // Require peer address for normal operation
    let peer_str = args.peer.unwrap_or_else(|| {
        eprintln!("Error: --peer is required");
        eprintln!("Usage: netface --peer <host:port>");
        eprintln!("Run with --help for more options");
        std::process::exit(1);
    });

    // Load config from file, then override with CLI args
    let mut cfg = Config::load();

    // Override config with CLI args where provided
    if let Some(port) = args.port {
        cfg.port = port;
    }
    if let Some(camera) = args.camera {
        cfg.camera = camera;
    }
    if let Some(fps) = args.fps {
        cfg.fps = fps;
    }
    if args.color {
        cfg.color = true;
    }
    if args.no_audio {
        cfg.no_audio = true;
    }
    if args.bg_removal {
        cfg.bg_removal = true;
    }
    if let Some(bg_color) = args.bg_color {
        cfg.bg_color = bg_color;
    }
    if let Some(theme) = args.theme {
        cfg.theme = theme;
    }
    if let Some(color_mode) = args.color_mode {
        cfg.color_mode = color_mode;
    }

    // Auto-enable color output when using a non-original color mode
    if cfg.color_mode != "original" {
        cfg.color = true;
    }

    // Parse derived values
    let bg_color = cfg.bg_color_rgb();

    // Build theme and renderer
    let theme = if let Some(ref custom) = cfg.custom_theme {
        custom.to_theme().unwrap_or_else(|| build_theme(&cfg.theme, &cfg.color_mode))
    } else {
        build_theme(&cfg.theme, &cfg.color_mode)
    };
    let renderer = Arc::new(ThemeRenderer::new(&theme));

    // ── Terminal dimensions ───────────────────────────────────────────────
    let (term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let term_cols = term_cols as u32;
    let term_rows = term_rows as u32;

    let half_w = term_cols.saturating_sub(1) / 2;
    let height = term_rows.saturating_sub(1);

    // ── Terminal setup ────────────────────────────────────────────────────
    print!("\x1b[?25l\x1b[2J\x1b[H");
    std::io::stdout().flush().unwrap();

    ctrlc::set_handler(|| {
        print!("\x1b[?25h\x1b[2J\x1b[H");
        std::io::stdout().flush().ok();
        std::process::exit(0);
    })
    .expect("failed to set Ctrl-C handler");

    // ── Network ───────────────────────────────────────────────────────────
    let peer_addr: std::net::SocketAddr = peer_str
        .parse()
        .unwrap_or_else(|_| panic!("invalid peer address '{}' (expected host:port)", peer_str));

    let send_sock = Arc::new(UdpSocket::bind("0.0.0.0:0").expect("failed to bind send socket"));
    let recv_sock = Arc::new(
        UdpSocket::bind(format!("0.0.0.0:{}", cfg.port))
            .unwrap_or_else(|e| panic!("failed to listen on port {}: {e}", cfg.port)),
    );

    // ── Background removal ─────────────────────────────────────────────────
    let bg_session: Option<Arc<std::sync::Mutex<ort::session::Session>>> = if cfg.bg_removal {
        match video::load_bg_session(cfg.model_threads) {
            Ok(s) => {
                eprintln!("Background removal enabled");
                Some(Arc::new(std::sync::Mutex::new(s)))
            }
            Err(e) => {
                eprintln!("Failed to load background model: {e}");
                None
            }
        }
    } else {
        None
    };

    // ── Shared state ──────────────────────────────────────────────────────
    let peer_connected = Arc::new(AtomicBool::new(false));
    let peer_w = Arc::new(AtomicU32::new(half_w));
    let peer_h = Arc::new(AtomicU32::new(height));

    // ── Channels ──────────────────────────────────────────────────────────
    let (display_tx, display_rx) = bounded::<video::RawFrame>(2);
    let (net_raw_tx, net_raw_rx) = bounded::<video::RawFrame>(2);
    let (vid_tx, vid_rx) = bounded::<Vec<u8>>(2);
    let (remote_vid_tx, remote_vid_rx) = bounded::<Vec<u8>>(2);
    let (local_aud_tx, local_aud_rx) = bounded::<Vec<u8>>(32);
    let (remote_aud_tx, remote_aud_rx) = bounded::<Vec<u8>>(32);

    // ── Network receive ───────────────────────────────────────────────────
    {
        let sock = recv_sock.clone();
        let pc = peer_connected.clone();
        let pw = peer_w.clone();
        let ph = peer_h.clone();
        thread::spawn(move || net::recv_loop(sock, remote_vid_tx, remote_aud_tx, pc, pw, ph));
    }

    // ── Network send ──────────────────────────────────────────────────────
    {
        let sock = send_sock.clone();
        let config_interval = cfg.config_interval;
        thread::spawn(move || {
            net::send_loop(sock, peer_addr, vid_rx, local_aud_rx, half_w, height, config_interval)
        });
    }

    // ── Camera capture ────────────────────────────────────────────────────
    {
        let camera_idx = cfg.camera;
        let fps = cfg.fps;
        thread::spawn(move || video::capture_thread(camera_idx, fps, display_tx, net_raw_tx));
    }

    // ── Network encoder ───────────────────────────────────────────────────
    {
        let color = cfg.color;
        let pw = peer_w.clone();
        let ph = peer_h.clone();
        let bgs = bg_session.clone();
        let video_cfg = video::VideoConfig::from(&cfg);
        let rend = renderer.clone();
        thread::spawn(move || {
            video::net_encode_thread(net_raw_rx, vid_tx, pw, ph, color, bgs, bg_color, video_cfg, rend)
        });
    }

    // ── Compositor ────────────────────────────────────────────────────────
    {
        let pc = peer_connected.clone();
        let color = cfg.color;
        let bgs = bg_session.clone();
        let video_cfg = video::VideoConfig::from(&cfg);
        let rend = renderer.clone();
        thread::spawn(move || {
            video::compositor_thread(
                display_rx,
                remote_vid_rx,
                pc,
                half_w,
                height,
                color,
                bgs,
                bg_color,
                video_cfg,
                rend,
            )
        });
    }

    // ── Audio ─────────────────────────────────────────────────────────────
    if !cfg.no_audio {
        let audio_buffer = cfg.audio_buffer_size;
        thread::spawn(move || audio::audio_loop(local_aud_tx, remote_aud_rx, audio_buffer));
    }

    // ── Status bar ────────────────────────────────────────────────────────
    {
        let peer = peer_str.clone();
        let port = cfg.port;
        let pc = peer_connected.clone();
        let status_row = term_rows as u16;

        thread::spawn(move || loop {
            let msg = if pc.load(Ordering::Relaxed) {
                format!(
                    "\x1b[90mConnected  \u{2502}  peer={peer}  \u{2502}  Ctrl-C to quit\x1b[0m"
                )
            } else {
                format!(
                    "\x1b[90mWaiting for peer on :{port}  \u{2502}  Ctrl-C to quit\x1b[0m"
                )
            };
            print!("\x1b[{status_row};1H\x1b[2K{msg}");
            std::io::stdout().flush().ok();
            thread::sleep(Duration::from_millis(500));
        });
    }

    loop {
        thread::sleep(Duration::from_secs(60));
    }
}
