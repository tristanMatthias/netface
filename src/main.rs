// netface — P2P ASCII webcam + audio
//
// USAGE
//   WebRTC mode (via Nostr):
//     netface --handle @alice     # Connect to @alice
//     netface --listen            # Listen for incoming calls
//
//   Legacy UDP mode:
//     Alice:  netface --peer bob.ip:4444
//     Bob:    netface --peer alice.ip:4444
//
// CONFIG
//   Settings are loaded from ~/.config/netface/config.toml
//   Run with --init-config to create a default config file.

use std::net::UdpSocket;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Arc, RwLock,
};
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

use clap::Parser;
use crossbeam_channel::{bounded, Receiver, Sender};

mod audio;
mod logging;
mod config;
mod conn;
mod net;
mod theme;
mod ui;
mod video;

use config::Config;
use conn::ConnError;

/// Connection mode for the session.
#[derive(Debug, Clone)]
enum ConnectionMode {
    /// Connect via Nostr handle (WebRTC).
    Handle(String),
    /// Legacy UDP mode with direct peer address.
    Udp(String),
    /// Listen for incoming calls (WebRTC).
    Listen,
}

/// Network transport abstraction.
/// Both UDP and WebRTC modes provide the same interface.
struct NetworkTransport {
    /// Send video packets.
    vid_tx: Sender<Vec<u8>>,
    /// Receive video packets.
    vid_rx: Receiver<Vec<u8>>,
    /// Send audio packets.
    aud_tx: Sender<Vec<u8>>,
    /// Receive audio packets.
    aud_rx: Receiver<Vec<u8>>,
    /// Peer connected flag.
    peer_connected: Arc<AtomicBool>,
    /// Peer's requested width.
    peer_w: Arc<AtomicU32>,
    /// Peer's requested height.
    peer_h: Arc<AtomicU32>,
}

#[derive(Parser, Debug)]
#[command(
    name = "netface",
    about = "P2P ASCII video + audio — WebRTC + Nostr, or legacy UDP"
)]
struct Args {
    /// Connect to peer by handle (e.g., @alice or npub1...)
    #[arg(short = 'H', long)]
    handle: Option<String>,

    /// Peer address for legacy UDP mode, e.g. 192.168.1.42:4444
    #[arg(short, long)]
    peer: Option<String>,

    /// Local UDP port to listen on (for legacy UDP mode)
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

    /// Show your identity (npub) for sharing with peers
    #[arg(long)]
    show_identity: bool,

    /// Register a handle for your identity
    #[arg(long)]
    register_handle: Option<String>,

    /// Listen for incoming calls (WebRTC mode)
    #[arg(long)]
    listen: bool,

    /// Look up a handle without connecting (for testing)
    #[arg(long)]
    lookup: Option<String>,

    /// Enable verbose debug output
    #[arg(long, short = 'v')]
    verbose: bool,

    /// Update netface to the latest version
    #[arg(long)]
    update: bool,

    /// Send a test ping to a handle (for debugging signaling)
    #[arg(long)]
    ping: Option<String>,

    /// Wait for a test ping (for debugging signaling)
    #[arg(long)]
    wait_ping: bool,

    /// Test STUN binding (for debugging ICE)
    #[arg(long)]
    test_stun: bool,
}

fn main() {
    let args = Args::parse();

    // Initialize logging
    if let Err(e) = logging::init() {
        eprintln!("Warning: Could not initialize logging: {}", e);
    }
    log_info!("netface starting");
    log_info!("Log file: {:?}", logging::log_path());

    // Self-update
    if args.update {
        println!("Checking for updates (current: v{})...", env!("CARGO_PKG_VERSION"));
        match self_update::backends::github::Update::configure()
            .repo_owner("tristanMatthias")
            .repo_name("netface")
            .bin_name("netface")
            .target(self_update::get_target())
            .show_download_progress(true)
            .current_version(env!("CARGO_PKG_VERSION"))
            .build()
            .and_then(|u| u.update())
        {
            Ok(status) => {
                if status.updated() {
                    println!("Updated to v{}!", status.version());
                } else {
                    println!("Already up to date (v{}).", status.version());
                }
            }
            Err(e) => {
                eprintln!("Update failed: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

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

    // Handle identity commands
    if args.show_identity {
        match conn::Identity::load_or_generate() {
            Ok(identity) => {
                println!("Your netface identity:");
                println!("  npub: {}", identity.npub());
                if let Some(path) = conn::Identity::default_path() {
                    println!("  stored at: {}", path.display());
                }
            }
            Err(e) => {
                eprintln!("Error loading identity: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    if let Some(handle) = args.register_handle {
        match conn::Identity::load_or_generate() {
            Ok(identity) => {
                let cfg = Config::load();
                println!("Registering handle @{} for {}...", handle, identity.npub());
                println!("Using relays: {:?}", cfg.nostr.relays);

                // Register the handle via Nostr
                let result = conn::bridge::block_on(async {
                    let nostr = conn::nostr::NostrClient::new(identity.clone(), cfg.nostr.relays.clone()).await?;
                    println!("Connecting to relays...");
                    nostr.connect().await?;
                    println!("Connected. Publishing handle registration...");

                    let registry = conn::nostr::discovery::HandleRegistry::new(&nostr);
                    let event_id = registry.register(&handle).await?;
                    println!("Published event: {}", event_id.to_hex());

                    // Wait a moment for relays to propagate
                    println!("Waiting for relay propagation...");
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

                    // Verify we can look it up
                    println!("Verifying registration...");
                    match registry.lookup(&handle).await? {
                        Some(pubkey) => println!("Verified: handle @{} -> {}", handle, &pubkey[..16]),
                        None => println!("Warning: handle not found after registration (relay may be slow)"),
                    }

                    nostr.disconnect().await?;
                    Ok::<_, ConnError>(())
                });

                match result {
                    Ok(()) => {
                        println!("Successfully registered @{}", handle);
                        println!("Others can now call you with: netface --handle @{}", handle);
                    }
                    Err(e) => {
                        eprintln!("Error registering handle: {e}");
                        std::process::exit(1);
                    }
                }
            }
            Err(e) => {
                eprintln!("Error loading identity: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    // Test STUN binding
    if args.test_stun {
        let cfg = Config::load();
        println!("Testing STUN binding...");

        let result = conn::bridge::block_on(async {
            let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await
                .map_err(|e| ConnError::WebRtcConnection(e.to_string()))?;
            let local_addr = socket.local_addr()
                .map_err(|e| ConnError::WebRtcConnection(e.to_string()))?;
            println!("Local socket: {}", local_addr);

            for stun_server in &cfg.ice.stun_servers {
                println!("Trying STUN server: {}", stun_server);
                match tokio::time::timeout(
                    Duration::from_secs(3),
                    conn::webrtc::ice::stun_binding(&socket, stun_server, Duration::from_secs(3))
                ).await {
                    Ok(Ok(addr)) => {
                        println!("  Public address: {}", addr);
                    }
                    Ok(Err(e)) => {
                        println!("  Error: {}", e);
                    }
                    Err(_) => {
                        println!("  Timeout");
                    }
                }
            }
            Ok::<_, ConnError>(())
        });

        if let Err(e) = result {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Handle ping test (send)
    if let Some(handle) = args.ping {
        let cfg = Config::load();
        println!("Sending test ping to @{}...", handle);

        let result = conn::bridge::block_on(async {
            let identity = conn::Identity::load_or_generate()?;
            let nostr = std::sync::Arc::new(conn::nostr::NostrClient::new(identity, cfg.nostr.relays.clone()).await?);
            println!("Connecting to relays...");
            nostr.connect().await?;

            let registry = conn::nostr::discovery::HandleRegistry::new(&nostr);
            let peer_pubkey = registry.resolve(&handle).await?;
            println!("Found peer: {}", &peer_pubkey.to_hex()[..16]);

            let call_id = "test-ping".to_string();
            let signaling = conn::nostr::signaling::SignalingChannel::new(
                nostr.clone(),
                peer_pubkey,
                call_id,
            );

            signaling.send_offer("TEST_PING_SDP".to_string()).await?;
            println!("Ping sent! If peer is running --wait-ping, they should see it.");

            nostr.disconnect().await?;
            Ok::<_, ConnError>(())
        });

        if let Err(e) = result {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Handle ping test (receive)
    if args.wait_ping {
        let cfg = Config::load();
        let identity = conn::Identity::load_or_generate().unwrap();
        println!("Waiting for test ping...");
        println!("Your identity: {}", identity.npub());
        println!("Tell the other person to run: netface --ping <your-handle>");

        let result = conn::bridge::block_on(async {
            let nostr = conn::nostr::NostrClient::new(identity, cfg.nostr.relays.clone()).await?;
            println!("Connecting to relays...");
            nostr.connect().await?;
            println!("Connected. Waiting for ping (up to 60 seconds)...");

            match conn::nostr::signaling::wait_for_offer(&nostr, Duration::from_secs(60)).await {
                Ok((sender, call_id, sdp)) => {
                    println!("✓ Received ping from: {}", sender.to_hex());
                    println!("  Call ID: {}", call_id);
                    println!("  Content: {}", &sdp[..std::cmp::min(50, sdp.len())]);
                    println!("\nSignaling is working!");
                }
                Err(e) => {
                    println!("✗ No ping received: {e}");
                }
            }

            nostr.disconnect().await?;
            Ok::<_, ConnError>(())
        });

        if let Err(e) = result {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Handle lookup test
    if let Some(handle) = args.lookup {
        let cfg = Config::load();
        println!("Looking up handle: @{}...", handle);

        let result = conn::bridge::block_on(async {
            let identity = conn::Identity::load_or_generate()?;
            let nostr = conn::nostr::NostrClient::new(identity, cfg.nostr.relays.clone()).await?;
            println!("Connecting to relays...");
            nostr.connect().await?;
            println!("Connected.");

            let registry = conn::nostr::discovery::HandleRegistry::new(&nostr);
            match registry.lookup(&handle).await? {
                Some(pubkey) => {
                    println!("Found: {}", pubkey);
                    println!("npub: npub1{}", &pubkey[..58]);
                }
                None => {
                    println!("Handle @{} not found", handle);
                }
            }

            nostr.disconnect().await?;
            Ok::<_, ConnError>(())
        });

        if let Err(e) = result {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Determine connection mode
    let connection_mode = if let Some(handle) = args.handle {
        ConnectionMode::Handle(handle)
    } else if let Some(peer) = args.peer {
        ConnectionMode::Udp(peer)
    } else if args.listen {
        ConnectionMode::Listen
    } else {
        eprintln!("Error: --handle, --peer, or --listen is required");
        eprintln!("Usage:");
        eprintln!("  netface --handle @alice           # Connect via Nostr handle");
        eprintln!("  netface --peer 192.168.1.42:4444  # Legacy UDP mode");
        eprintln!("  netface --listen                  # Listen for incoming calls");
        eprintln!("Run with --help for more options");
        std::process::exit(1);
    };

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
    if let Some(ref theme) = args.theme {
        cfg.theme = theme.clone();
    }
    if let Some(ref color_mode) = args.color_mode {
        cfg.color_mode = color_mode.clone();
    }

    // Auto-enable color output when color_mode is anything other than "original"
    if cfg.color_mode != "original" {
        cfg.color = true;
    }

    // ── Terminal dimensions ───────────────────────────────────────────────
    let (term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let term_cols = term_cols as u32;
    let term_rows = term_rows as u32;

    // Reserve one row for status bar
    let half_w = term_cols.saturating_sub(1) / 2;
    let height = term_rows.saturating_sub(1);

    // Set up network transport based on mode
    let (transport, peer_str) = match connection_mode {
        ConnectionMode::Udp(ref peer_addr) => {
            let transport = setup_udp_transport(&cfg, peer_addr, half_w, height);
            (transport, peer_addr.clone())
        }
        ConnectionMode::Handle(ref handle) => {
            println!("Connecting to {} via Nostr...", handle);
            log_info!("Connection mode: Handle({})", handle);
            match setup_webrtc_transport(&cfg, handle, false, half_w, height) {
                Ok(transport) => (transport, handle.clone()),
                Err(e) => {
                    log_error!("Failed to connect: {}", e);
                    eprintln!("Failed to connect: {e}");
                    std::process::exit(1);
                }
            }
        }
        ConnectionMode::Listen => {
            let identity = conn::Identity::load_or_generate().unwrap_or_else(|e| {
                log_error!("Error loading identity: {}", e);
                eprintln!("Error loading identity: {e}");
                std::process::exit(1);
            });
            println!("Listening for incoming calls...");
            println!("Your identity: {}", identity.npub());
            log_info!("Connection mode: Listen, identity={}", identity.npub());
            match setup_webrtc_transport(&cfg, "", true, half_w, height) {
                Ok(transport) => (transport, "incoming".to_string()),
                Err(e) => {
                    log_error!("Failed to listen: {}", e);
                    eprintln!("Failed to listen: {e}");
                    std::process::exit(1);
                }
            }
        }
    };

    // ── Continue with common setup ────────────────────────────────────────
    run_session(cfg, transport, peer_str, half_w, height);
}

/// Set up UDP transport (legacy mode).
fn setup_udp_transport(cfg: &Config, peer_addr: &str, half_w: u32, height: u32) -> NetworkTransport {
    let peer_addr: std::net::SocketAddr = peer_addr
        .parse()
        .unwrap_or_else(|_| panic!("invalid peer address '{}' (expected host:port)", peer_addr));

    let send_sock = Arc::new(UdpSocket::bind("0.0.0.0:0").expect("failed to bind send socket"));
    let recv_sock = Arc::new(
        UdpSocket::bind(format!("0.0.0.0:{}", cfg.port))
            .unwrap_or_else(|e| panic!("failed to listen on port {}: {e}", cfg.port)),
    );

    let peer_connected = Arc::new(AtomicBool::new(false));
    let peer_w = Arc::new(AtomicU32::new(0));
    let peer_h = Arc::new(AtomicU32::new(0));

    // Channels for video/audio
    let (vid_tx, vid_rx_internal) = bounded::<Vec<u8>>(2);
    let (remote_vid_tx, vid_rx) = bounded::<Vec<u8>>(2);
    let (aud_tx, aud_rx_internal) = bounded::<Vec<u8>>(32);
    let (remote_aud_tx, aud_rx) = bounded::<Vec<u8>>(32);

    // Network receive thread
    {
        let sock = recv_sock.clone();
        let pc = peer_connected.clone();
        let pw = peer_w.clone();
        let ph = peer_h.clone();
        thread::spawn(move || net::recv_loop(sock, remote_vid_tx, remote_aud_tx, pc, pw, ph));
    }

    // Network send thread
    {
        let sock = send_sock.clone();
        let config_interval = cfg.config_interval;
        thread::spawn(move || {
            net::send_loop(sock, peer_addr, vid_rx_internal, aud_rx_internal, half_w, height, config_interval)
        });
    }

    NetworkTransport {
        vid_tx,
        vid_rx,
        aud_tx,
        aud_rx,
        peer_connected,
        peer_w,
        peer_h,
    }
}

/// Set up WebRTC transport via Nostr signaling.
fn setup_webrtc_transport(cfg: &Config, handle: &str, listen_mode: bool, local_w: u32, local_h: u32) -> Result<NetworkTransport, ConnError> {
    use std::sync::Arc;

    let identity = conn::Identity::load_or_generate()?;
    let peer_connected = Arc::new(AtomicBool::new(false));
    let peer_w = Arc::new(AtomicU32::new(0));
    let peer_h = Arc::new(AtomicU32::new(0));

    // Create channels for video/audio data
    let (vid_tx, vid_rx_bridge) = bounded::<Vec<u8>>(2);
    let (vid_tx_bridge, vid_rx) = bounded::<Vec<u8>>(2);
    let (aud_tx, aud_rx_bridge) = bounded::<Vec<u8>>(32);
    let (aud_tx_bridge, aud_rx) = bounded::<Vec<u8>>(32);

    // Clone for the async task
    let relays = cfg.nostr.relays.clone();
    let stun_servers = cfg.ice.stun_servers.clone();
    let turn_config = cfg.turn.clone();
    let handle_owned = handle.to_string();
    let peer_connected_clone = peer_connected.clone();
    let peer_w_clone = peer_w.clone();
    let peer_h_clone = peer_h.clone();

    // Spawn WebRTC connection thread
    thread::spawn(move || {
        crate::log_info!("WebRTC: Starting connection thread");
        let result = conn::bridge::block_on(async {
            // Create WebRTC config
            let webrtc_config = conn::webrtc::WebRtcConfig {
                stun_servers,
                turn_url: turn_config.as_ref().map(|t| t.url.clone()),
                turn_username: turn_config.as_ref().map(|t| t.username.clone()),
                turn_credential: turn_config.as_ref().map(|t| t.credential.clone()),
                ice_timeout: Duration::from_secs(10),
                connect_timeout: Duration::from_secs(30),
            };

            // Run Nostr connect and WebRTC setup in parallel
            crate::log_info!("WebRTC: Starting parallel setup (Nostr + WebRTC)...");
            let nostr_fut = async {
                let nostr = conn::nostr::NostrClient::new(identity.clone(), relays).await?;
                nostr.connect().await?;
                crate::log_info!("WebRTC: Nostr connected");
                Ok::<_, ConnError>(Arc::new(nostr))
            };
            let webrtc_fut = async {
                let webrtc = conn::webrtc::WebRtcConnection::new(webrtc_config).await?;
                crate::log_info!("WebRTC: Connection created");
                Ok::<_, ConnError>(webrtc)
            };

            let (nostr_result, webrtc_result) = tokio::join!(nostr_fut, webrtc_fut);
            let nostr = nostr_result?;
            let mut webrtc = webrtc_result?;
            crate::log_info!("WebRTC: Parallel setup complete");

            // Start ICE gathering early (STUN)
            crate::log_info!("WebRTC: Gathering ICE candidates...");
            webrtc.gather_candidates().await?;

            // Create data channels
            crate::log_debug!("WebRTC: Creating data channels");
            let video_cfg = conn::channel::ChannelConfig::new("video")
                .ordered(false)
                .max_packet_lifetime(100);
            let audio_cfg = conn::channel::ChannelConfig::new("audio")
                .ordered(false)
                .max_packet_lifetime(50);
            let control_cfg = conn::channel::ChannelConfig::new("control")
                .ordered(true);

            let video_channel = webrtc.create_channel(&video_cfg)?;
            let audio_channel = webrtc.create_channel(&audio_cfg)?;
            let control_channel = webrtc.create_channel(&control_cfg)?;
            crate::log_info!("WebRTC: Data channels created");

            if listen_mode {
                // Listen for incoming calls
                crate::log_info!("WebRTC: Waiting for incoming call...");

                // Wait for an offer from any peer
                let (caller_pubkey, call_id, offer_sdp) = conn::nostr::signaling::wait_for_offer(
                    &nostr,
                    Duration::from_secs(300), // 5 minute timeout
                ).await?;

                crate::log_info!("WebRTC: Incoming call from {}", &caller_pubkey.to_hex()[..16]);

                // Create answer from the offer (gathers ICE candidates via STUN)
                crate::log_info!("WebRTC: Gathering ICE candidates...");
                let answer_sdp = webrtc.create_answer(&offer_sdp).await?;
                crate::log_info!("WebRTC: ICE gathering complete, answer ready");

                // Create signaling channel to respond
                let signaling = conn::nostr::signaling::SignalingChannel::new(
                    nostr.clone(),
                    caller_pubkey,
                    call_id,
                );

                // Send the answer
                crate::log_info!("WebRTC: Sending answer...");
                signaling.send_answer(answer_sdp).await?;
                crate::log_info!("WebRTC: Answer sent, establishing connection...");

            } else {
                // Connect to peer by handle
                crate::log_info!("WebRTC: Looking up handle: {}", handle_owned);
                let registry = conn::nostr::discovery::HandleRegistry::new(&nostr);
                let peer_pubkey = registry.resolve(&handle_owned).await?;
                crate::log_info!("WebRTC: Found peer: {}", &peer_pubkey.to_hex()[..16]);

                // Create signaling channel
                let call_id = conn::nostr::signaling::SignalingChannel::generate_call_id();
                let signaling = conn::nostr::signaling::SignalingChannel::new(
                    nostr.clone(),
                    peer_pubkey,
                    call_id.clone(),
                );

                // Generate offer (gathers ICE candidates via STUN)
                crate::log_info!("WebRTC: Gathering ICE candidates...");
                let offer = webrtc.create_offer().await?;
                crate::log_info!("WebRTC: ICE gathering complete, sending offer...");
                signaling.send_offer(offer).await?;
                crate::log_info!("WebRTC: Offer sent, waiting for answer...");

                // Wait for the answer
                crate::log_debug!("WebRTC: Waiting for answer (60s timeout)...");
                let answer_sdp = conn::nostr::signaling::wait_for_answer(
                    &nostr,
                    &peer_pubkey,
                    &call_id,
                    Duration::from_secs(60),
                ).await?;

                // Set the remote answer
                crate::log_info!("WebRTC: Setting remote answer ({} bytes)", answer_sdp.len());
                webrtc.set_remote_answer(&answer_sdp)?;
                crate::log_info!("WebRTC: Remote answer set, establishing connection...");
            }

            // Wait for WebRTC connection with timeout
            crate::log_info!("WebRTC: Starting connection loop (30s timeout)");
            let start = std::time::Instant::now();
            let timeout = Duration::from_secs(30);
            let mut last_log = std::time::Instant::now();

            while start.elapsed() < timeout {
                // Receive any incoming packets
                let received = webrtc.receive().await?;

                // Poll str0m - this drives ICE and returns next timeout
                let next_timeout = webrtc.poll().await?;

                if webrtc.is_connected() {
                    peer_connected_clone.store(true, Ordering::Relaxed);
                    crate::log_info!("WebRTC: Connected!");
                    break;
                }

                // Log progress every 5 seconds
                if last_log.elapsed() > Duration::from_secs(5) {
                    crate::log_debug!("WebRTC: Still connecting... ({:.1}s elapsed, state={:?})",
                        start.elapsed().as_secs_f32(), webrtc.state());
                    last_log = std::time::Instant::now();
                }

                // Calculate sleep duration based on str0m's next timeout
                let sleep_duration = if received {
                    // If we received data, check again immediately
                    Duration::ZERO
                } else if let Some(next) = next_timeout {
                    // Sleep until str0m needs us, but cap at 20ms for responsiveness
                    let until_timeout = next.saturating_duration_since(std::time::Instant::now());
                    until_timeout.min(Duration::from_millis(20))
                } else {
                    Duration::from_millis(5)
                };

                tokio::time::sleep(sleep_duration).await;
            }

            if !webrtc.is_connected() {
                crate::log_error!("WebRTC: Connection timeout after 30s");
                return Err(ConnError::WebRtcConnection("connection timeout".to_string()));
            }

            // Main data loop - bridge WebRTC channels to crossbeam channels
            crate::log_info!("WebRTC: Entering main data loop");

            // Prepare config message: [width: u32 LE, height: u32 LE]
            let mut config_msg = [0u8; 8];
            config_msg[0..4].copy_from_slice(&local_w.to_le_bytes());
            config_msg[4..8].copy_from_slice(&local_h.to_le_bytes());

            let mut config_counter = 0u32;
            let config_interval = 30; // Send config every 30 iterations

            // Send initial config immediately
            let _ = control_channel.send(&config_msg);
            crate::log_info!("WebRTC: Sent config {}x{}", local_w, local_h);

            loop {
                // Send config periodically
                config_counter += 1;
                if config_counter >= config_interval {
                    config_counter = 0;
                    let _ = control_channel.send(&config_msg);
                }

                // Send outgoing video
                if let Ok(data) = vid_rx_bridge.try_recv() {
                    let _ = video_channel.send(&data);
                }

                // Send outgoing audio
                if let Ok(data) = aud_rx_bridge.try_recv() {
                    let _ = audio_channel.send(&data);
                }

                // Receive incoming video
                if let Some(data) = video_channel.try_recv() {
                    let _ = vid_tx_bridge.try_send(data);
                }

                // Receive incoming audio
                if let Some(data) = audio_channel.try_recv() {
                    let _ = aud_tx_bridge.try_send(data);
                }

                // Receive incoming control messages (config)
                if let Some(data) = control_channel.try_recv() {
                    if data.len() >= 8 {
                        let w = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                        let h = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
                        if w > 0 && h > 0 {
                            peer_w_clone.store(w, Ordering::Relaxed);
                            peer_h_clone.store(h, Ordering::Relaxed);
                            crate::log_info!("WebRTC: Received peer config {}x{}", w, h);
                        }
                    }
                }

                // Receive incoming packets
                let received = webrtc.receive().await?;

                // Poll WebRTC - drives ICE and DTLS
                let next_timeout = webrtc.poll().await?;

                // Check if still connected
                if !webrtc.is_connected() {
                    crate::log_warn!("WebRTC: Disconnected, exiting data loop");
                    peer_connected_clone.store(false, Ordering::Relaxed);
                    break;
                }

                // Calculate sleep duration
                let sleep_duration = if received {
                    Duration::from_millis(1)
                } else if let Some(next) = next_timeout {
                    let until_timeout = next.saturating_duration_since(std::time::Instant::now());
                    until_timeout.min(Duration::from_millis(10))
                } else {
                    Duration::from_millis(1)
                };

                tokio::time::sleep(sleep_duration).await;
            }

            Ok::<(), ConnError>(())
        });

        if let Err(e) = result {
            eprintln!("WebRTC connection error: {e}");
        }
    });

    // Give connection a moment to start
    thread::sleep(Duration::from_millis(100));

    Ok(NetworkTransport {
        vid_tx,
        vid_rx,
        aud_tx,
        aud_rx,
        peer_connected,
        peer_w,
        peer_h,
    })
}

/// Run the main session with the given transport.
fn run_session(cfg: Config, transport: NetworkTransport, peer_str: String, half_w: u32, height: u32) {
    let bg_color = cfg.bg_color_rgb();

    // Build theme renderer (wrapped in RwLock for runtime updates)
    let theme_obj = theme::build_theme(&cfg.theme, &cfg.color_mode);
    let theme_renderer = Arc::new(RwLock::new(theme::ThemeRenderer::new(&theme_obj)));
    let current_theme = cfg.theme.clone();
    let current_color_mode = cfg.color_mode.clone();

    // ── Background removal ─────────────────────────────────────────────────
    let bg_session: Option<Arc<std::sync::Mutex<ort::session::Session>>> = if cfg.bg_removal {
        video::load_bg_session(cfg.model_threads)
            .ok()
            .map(|s| Arc::new(std::sync::Mutex::new(s)))
    } else {
        None
    };

    // ── Shared state ──────────────────────────────────────────────────────
    let audio_muted = Arc::new(AtomicBool::new(false));
    let video_disabled = Arc::new(AtomicBool::new(false));
    let bg_enabled = Arc::new(AtomicBool::new(bg_session.is_some()));

    // ── Channels ──────────────────────────────────────────────────────────
    let (display_tx, display_rx) = bounded::<video::RawFrame>(2);
    let (net_raw_tx, net_raw_rx) = bounded::<video::RawFrame>(2);

    // UI channels for ASCII frames
    let (local_ascii_tx, local_ascii_rx) = bounded::<Vec<u8>>(2);
    let (remote_ascii_tx, remote_ascii_rx) = bounded::<Vec<u8>>(2);

    // ── Camera capture ────────────────────────────────────────────────────
    {
        let camera_idx = cfg.camera;
        let fps = cfg.fps;
        thread::spawn(move || video::capture_thread(camera_idx, fps, display_tx, net_raw_tx));
    }

    // ── Network encoder ───────────────────────────────────────────────────
    {
        let color = cfg.color;
        let pw = transport.peer_w.clone();
        let ph = transport.peer_h.clone();
        let bgs = bg_session.clone();
        let video_cfg = video::VideoConfig::from(&cfg);
        let renderer = theme_renderer.clone();
        let vid_disabled = video_disabled.clone();
        let bg_on = bg_enabled.clone();
        let vid_tx = transport.vid_tx.clone();
        thread::spawn(move || {
            video::net_encode_thread(
                net_raw_rx, vid_tx, pw, ph, vid_disabled, bg_on, color, bgs, bg_color, video_cfg, renderer,
            )
        });
    }

    // ── Local render (for UI) ─────────────────────────────────────────────
    {
        let pc = transport.peer_connected.clone();
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
        let pc = transport.peer_connected.clone();
        let vid_rx = transport.vid_rx.clone();
        thread::spawn(move || {
            video::remote_decode_thread(vid_rx, remote_ascii_tx, pc)
        });
    }

    // ── Audio ─────────────────────────────────────────────────────────────
    if !cfg.no_audio {
        let audio_buffer = cfg.audio_buffer_size;
        let muted = audio_muted.clone();
        let aud_tx = transport.aud_tx;
        let aud_rx = transport.aud_rx;
        thread::spawn(move || audio::audio_loop(aud_tx, aud_rx, audio_buffer, muted));
    }

    // ── Redirect stderr to suppress library warnings ───────────────────────
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
        transport.peer_connected.clone(),
        peer_str,
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
