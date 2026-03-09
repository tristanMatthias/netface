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

use std::net::UdpSocket;
use std::sync::{
    atomic::{AtomicBool, AtomicU32},
    Arc, RwLock,
};
use std::thread;

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

use clap::Parser;
use crossbeam_channel::bounded;

mod audio;
mod config;
mod net;
mod theme;
mod ui;
mod video;

use config::Config;

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
    #[arg(long)]
    camera: Option<usize>,

    /// Target capture FPS
    #[arg(short, long)]
    fps: Option<u64>,

    /// Character theme (classic, detailed, blocks, dots, emoji-moon, etc.)
    #[arg(short = 't', long)]
    theme: Option<String>,

    /// Color mode (original, mono-green, matrix, cyberpunk, sunset, etc.)
    #[arg(short = 'c', long)]
    color_mode: Option<String>,

    /// List available themes and color modes
    #[arg(long)]
    list_themes: bool,

    /// Disable audio
    #[arg(long)]
    no_audio: bool,

    /// Disable background removal
    #[arg(long)]
    no_bg_removal: bool,

    /// Background color when removal is enabled (hex RGB, e.g., "000000" for black)
    #[arg(long)]
    bg_color: Option<String>,

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

    if args.list_themes {
        println!("Character themes:");
        for name in theme::list_char_ramps() {
            let chars = theme::get_char_ramp(name).unwrap_or("");
            println!("  {:<15} {}", name, chars);
        }
        println!("\nColor modes:");
        for name in theme::list_color_modes() {
            println!("  {}", name);
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
    if args.no_audio {
        cfg.no_audio = true;
    }
    if args.no_bg_removal {
        cfg.bg_removal = false;
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

    // Auto-enable color output when color_mode is anything other than "original"
    // (matrix, cyberpunk, mono-green, etc. all need ANSI colors to display)
    if cfg.color_mode != "original" {
        cfg.color = true;
    }

    // Parse derived values
    let bg_color = cfg.bg_color_rgb();

    // Build theme renderer (wrapped in RwLock for runtime updates)
    let theme_obj = theme::build_theme(&cfg.theme, &cfg.color_mode);
    let theme_renderer = Arc::new(RwLock::new(theme::ThemeRenderer::new(&theme_obj)));
    let current_theme = cfg.theme.clone();
    let current_color_mode = cfg.color_mode.clone();

    // ── Terminal dimensions ───────────────────────────────────────────────
    let (term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let term_cols = term_cols as u32;
    let term_rows = term_rows as u32;

    // Reserve one row for status bar
    let half_w = term_cols.saturating_sub(1) / 2;
    let height = term_rows.saturating_sub(1);

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
        video::load_bg_session(cfg.model_threads)
            .ok()
            .map(|s| Arc::new(std::sync::Mutex::new(s)))
    } else {
        None
    };

    // ── Shared state ──────────────────────────────────────────────────────
    let peer_connected = Arc::new(AtomicBool::new(false));
    let audio_muted = Arc::new(AtomicBool::new(false));
    let video_disabled = Arc::new(AtomicBool::new(false));
    // Background removal toggle (only effective if bg_session is Some)
    let bg_enabled = Arc::new(AtomicBool::new(bg_session.is_some()));
    // Initialize to 0 - we'll wait for peer config before encoding video
    let peer_w = Arc::new(AtomicU32::new(0));
    let peer_h = Arc::new(AtomicU32::new(0));

    // ── Channels ──────────────────────────────────────────────────────────
    let (display_tx, display_rx) = bounded::<video::RawFrame>(2);
    let (net_raw_tx, net_raw_rx) = bounded::<video::RawFrame>(2);
    let (vid_tx, vid_rx) = bounded::<Vec<u8>>(2);
    let (remote_vid_tx, remote_vid_rx) = bounded::<Vec<u8>>(2);
    let (local_aud_tx, local_aud_rx) = bounded::<Vec<u8>>(32);
    let (remote_aud_tx, remote_aud_rx) = bounded::<Vec<u8>>(32);

    // UI channels for ASCII frames
    let (local_ascii_tx, local_ascii_rx) = bounded::<Vec<u8>>(2);
    let (remote_ascii_tx, remote_ascii_rx) = bounded::<Vec<u8>>(2);

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
        let renderer = theme_renderer.clone();
        let vid_disabled = video_disabled.clone();
        let bg_on = bg_enabled.clone();
        thread::spawn(move || {
            video::net_encode_thread(
                net_raw_rx, vid_tx, pw, ph, vid_disabled, bg_on, color, bgs, bg_color, video_cfg, renderer,
            )
        });
    }

    // ── Local render (for UI) ─────────────────────────────────────────────
    {
        let pc = peer_connected.clone();
        let vid_disabled = video_disabled.clone();
        let bg_on = bg_enabled.clone();
        let color = cfg.color;
        let bgs = bg_session.clone();
        let video_cfg = video::VideoConfig::from(&cfg);
        let renderer = theme_renderer.clone();
        thread::spawn(move || {
            video::local_render_thread(
                display_rx,
                local_ascii_tx,
                pc,
                vid_disabled,
                bg_on,
                half_w,
                height,
                color,
                bgs,
                bg_color,
                video_cfg,
                renderer,
            )
        });
    }

    // ── Remote decode (for UI) ────────────────────────────────────────────
    {
        let pc = peer_connected.clone();
        thread::spawn(move || {
            video::remote_decode_thread(remote_vid_rx, remote_ascii_tx, pc)
        });
    }

    // ── Audio ─────────────────────────────────────────────────────────────
    if !cfg.no_audio {
        let audio_buffer = cfg.audio_buffer_size;
        let muted = audio_muted.clone();
        thread::spawn(move || audio::audio_loop(local_aud_tx, remote_aud_rx, audio_buffer, muted));
    }

    // ── Redirect stderr to suppress library warnings ───────────────────────
    // AVFoundation and other libraries print warnings to stderr which corrupt the TUI
    #[cfg(unix)]
    let _stderr_guard = {
        use std::fs::File;
        let dev_null = File::open("/dev/null").ok();
        if let Some(ref null) = dev_null {
            unsafe {
                libc::dup2(null.as_raw_fd(), libc::STDERR_FILENO);
            }
        }
        dev_null
    };

    // ── Initialize ratatui terminal ───────────────────────────────────────
    let mut terminal = ratatui::init();

    // ── Create App and run UI event loop ──────────────────────────────────
    let mut app = ui::App::new(
        peer_connected.clone(),
        peer_str.clone(),
        cfg.port,
        audio_muted.clone(),
        video_disabled.clone(),
        bg_enabled.clone(),
        theme_renderer.clone(),
        current_theme,
        current_color_mode,
    );

    let result = ui::run(&mut terminal, &mut app, local_ascii_rx, remote_ascii_rx);

    // ── Cleanup terminal ──────────────────────────────────────────────────
    ratatui::restore();

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
