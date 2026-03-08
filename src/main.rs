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
// CROSS-COMPILE EXAMPLES
//   macOS arm64 : cargo build --release --target aarch64-apple-darwin
//   macOS x86   : cargo build --release --target x86_64-apple-darwin
//   Linux x86   : cargo build --release --target x86_64-unknown-linux-gnu
//   Windows     : cargo build --release --target x86_64-pc-windows-gnu
//
// LINUX: needs libasound2-dev (ALSA).  macOS: camera/mic prompts on first run.

use std::io::Write;
use std::net::UdpSocket;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

use clap::Parser;
use crossbeam_channel::bounded;

mod audio;
mod net;
mod video;

#[derive(Parser, Debug)]
#[command(
    name = "netface",
    about = "P2P ASCII video + audio — no servers, no accounts, just UDP"
)]
struct Args {
    /// Peer address, e.g. 192.168.1.42:4444
    #[arg(short, long)]
    peer: String,

    /// Local UDP port to listen on
    #[arg(short = 'P', long, default_value = "4444")]
    port: u16,

    /// Webcam device index (0 = default)
    #[arg(short, long, default_value = "0")]
    camera: usize,

    /// Target capture FPS
    #[arg(short, long, default_value = "15")]
    fps: u64,

    /// ANSI truecolor output (richer image, bigger packets)
    #[arg(short = 'C', long)]
    color: bool,

    /// Disable audio
    #[arg(long)]
    no_audio: bool,
}

fn main() {
    let args = Args::parse();

    // ── Terminal dimensions ───────────────────────────────────────────────
    let (term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let term_cols = term_cols as u32;
    let term_rows = term_rows as u32;

    // Each panel is half the terminal width; the separator takes one column.
    //   cols:  [local: half_w] [│] [remote: half_w]  (total = half_w*2 + 1)
    let half_w = term_cols.saturating_sub(1) / 2;
    // Reserve one row for the status bar at the bottom.
    let height = term_rows.saturating_sub(1);

    // ── Terminal setup ────────────────────────────────────────────────────
    print!("\x1b[?25l\x1b[2J\x1b[H"); // hide cursor, clear, home
    std::io::stdout().flush().unwrap();

    ctrlc::set_handler(|| {
        print!("\x1b[?25h\x1b[2J\x1b[H"); // restore cursor + clear on exit
        std::io::stdout().flush().ok();
        std::process::exit(0);
    })
    .expect("failed to set Ctrl-C handler");

    // ── Network ───────────────────────────────────────────────────────────
    let peer_addr: std::net::SocketAddr = args
        .peer
        .parse()
        .unwrap_or_else(|_| panic!("invalid peer address '{}' (expected host:port)", args.peer));

    let send_sock =
        Arc::new(UdpSocket::bind("0.0.0.0:0").expect("failed to bind send socket"));
    let recv_sock = Arc::new(
        UdpSocket::bind(format!("0.0.0.0:{}", args.port))
            .unwrap_or_else(|e| panic!("failed to listen on port {}: {e}", args.port)),
    );

    // ── Shared state ──────────────────────────────────────────────────────
    // Flipped to `true` by the receive thread the moment the first video
    // packet arrives; read by the compositor to trigger the split transition.
    let peer_connected = Arc::new(AtomicBool::new(false));

    // ── Channels ──────────────────────────────────────────────────────────
    let (display_tx, display_rx) = bounded::<video::RawFrame>(2); // capture → compositor
    let (net_raw_tx, net_raw_rx) = bounded::<video::RawFrame>(2); // capture → encoder
    let (vid_tx, vid_rx) = bounded::<Vec<u8>>(2);                 // encoder → net send
    let (remote_vid_tx, remote_vid_rx) = bounded::<Vec<u8>>(2);   // net recv → compositor
    let (local_aud_tx, local_aud_rx) = bounded::<Vec<u8>>(32);
    let (remote_aud_tx, remote_aud_rx) = bounded::<Vec<u8>>(32);

    // ── Network receive ───────────────────────────────────────────────────
    {
        let sock = recv_sock.clone();
        let pc = peer_connected.clone();
        thread::spawn(move || net::recv_loop(sock, remote_vid_tx, remote_aud_tx, pc));
    }

    // ── Network send ──────────────────────────────────────────────────────
    {
        let sock = send_sock.clone();
        thread::spawn(move || net::send_loop(sock, peer_addr, vid_rx, local_aud_rx));
    }

    // ── Camera capture ────────────────────────────────────────────────────
    {
        let (cam, w, h, fps) = (args.camera, half_w, height, args.fps);
        thread::spawn(move || video::capture_thread(cam, w, h, fps, display_tx, net_raw_tx));
    }

    // ── Network encoder (RawFrame → ASCII → lz4 → UDP) ───────────────────
    {
        let (w, h, color) = (half_w, height, args.color);
        thread::spawn(move || video::net_encode_thread(net_raw_rx, vid_tx, w, h, color));
    }

    // ── Compositor ────────────────────────────────────────────────────────
    // Solo until peer_connected flips; then snaps to 50/50 split.
    {
        let pc = peer_connected.clone();
        let (w, h, color) = (half_w, height, args.color);
        thread::spawn(move || {
            video::compositor_thread(display_rx, remote_vid_rx, pc, w, h, color)
        });
    }

    // ── Audio ─────────────────────────────────────────────────────────────
    if !args.no_audio {
        thread::spawn(move || audio::audio_loop(local_aud_tx, remote_aud_rx));
    }

    // ── Status bar ────────────────────────────────────────────────────────
    {
        let peer = args.peer.clone();
        let port = args.port;
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
