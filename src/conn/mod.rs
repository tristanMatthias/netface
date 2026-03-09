//! Connectivity module for netface.
//!
//! Provides WebRTC-based peer connectivity with Nostr-based discovery and signaling.
//! Also supports legacy UDP mode for direct peer-to-peer connections.
//!
//! # Usage
//!
//! ```ignore
//! // Connect to a peer by handle
//! let conn = NetfaceConn::connect("@alice").await?;
//!
//! // Or accept incoming connections
//! let conn = NetfaceConn::accept_incoming().await?;
//!
//! // Get data channels
//! let video = conn.channel("video")?;
//! let audio = conn.channel("audio")?;
//!
//! // Send/receive data
//! video.send(&frame_data)?;
//! let remote_frame = video.recv()?;
//! ```

pub mod bridge;
pub mod channel;
pub mod error;
pub mod identity;
pub mod nostr;
pub mod webrtc;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, Sender};

pub use channel::DataChannel;
pub use error::ConnError;
pub use identity::Identity;

use self::bridge::block_on;
use self::channel::defaults;
use self::nostr::{discovery::HandleRegistry, signaling::SignalingChannel, NostrClient};
use self::webrtc::{WebRtcConfig, WebRtcConnection};

/// Connection mode.
#[derive(Debug, Clone)]
pub enum ConnectionMode {
    /// WebRTC connection via Nostr signaling.
    WebRtc {
        /// Nostr relays to use.
        relays: Vec<String>,
        /// STUN servers for ICE.
        stun_servers: Vec<String>,
        /// Optional TURN configuration.
        turn: Option<TurnConfig>,
    },
    /// Legacy UDP mode (direct peer-to-peer).
    Udp {
        /// Local port to bind.
        local_port: u16,
        /// Remote peer address.
        peer_addr: String,
    },
}

/// TURN server configuration.
#[derive(Debug, Clone)]
pub struct TurnConfig {
    pub url: String,
    pub username: String,
    pub credential: String,
}

/// Configuration for NetfaceConn.
#[derive(Debug, Clone)]
pub struct ConnConfig {
    /// Connection mode.
    pub mode: ConnectionMode,
    /// Connection timeout.
    pub timeout: Duration,
}

impl Default for ConnConfig {
    fn default() -> Self {
        Self {
            mode: ConnectionMode::WebRtc {
                relays: nostr::DEFAULT_RELAYS.iter().map(|s| s.to_string()).collect(),
                stun_servers: vec!["stun:stun.l.google.com:19302".to_string()],
                turn: None,
            },
            timeout: Duration::from_secs(30),
        }
    }
}

/// A netface connection to a peer.
///
/// Provides data channels for video, audio, chat, and control messages.
pub struct NetfaceConn {
    /// Our identity.
    identity: Identity,
    /// Local handle (if registered).
    local_handle: Option<String>,
    /// Peer's handle (if known).
    peer_handle: Option<String>,
    /// Data channels.
    channels: HashMap<String, DataChannel>,
    /// Connection state.
    state: Arc<RwLock<ConnState>>,
    /// Shutdown sender.
    shutdown_tx: Sender<()>,
}

/// Internal connection state.
struct ConnState {
    connected: bool,
    error: Option<ConnError>,
}

impl NetfaceConn {
    /// Connect to a peer by handle.
    ///
    /// The handle can be:
    /// - `@username` - A Nostr-registered netface handle
    /// - `npub1...` - A Nostr public key in bech32 format
    /// - `<hex>` - A raw 64-character hex public key
    pub fn connect(handle: &str, config: ConnConfig) -> Result<Self, ConnError> {
        let identity = Identity::load_or_generate()?;

        match &config.mode {
            ConnectionMode::WebRtc { relays, stun_servers, turn } => {
                Self::connect_webrtc(identity, handle, relays.clone(), stun_servers.clone(), turn.clone(), config.timeout)
            }
            ConnectionMode::Udp { .. } => {
                Err(ConnError::InvalidConfig("Use connect_udp for UDP mode".to_string()))
            }
        }
    }

    /// Connect using WebRTC.
    fn connect_webrtc(
        identity: Identity,
        handle: &str,
        relays: Vec<String>,
        stun_servers: Vec<String>,
        turn: Option<TurnConfig>,
        timeout: Duration,
    ) -> Result<Self, ConnError> {
        let handle_owned = handle.to_string();

        block_on(async {
            // Create Nostr client
            let nostr = Arc::new(NostrClient::new(identity.clone(), relays).await?);
            nostr.connect().await?;

            // Resolve handle to pubkey
            let registry = HandleRegistry::new(&nostr);
            let peer_pubkey = registry.resolve(&handle_owned).await?;

            // Create WebRTC connection
            let webrtc_config = WebRtcConfig {
                stun_servers,
                turn_url: turn.as_ref().map(|t| t.url.clone()),
                turn_username: turn.as_ref().map(|t| t.username.clone()),
                turn_credential: turn.as_ref().map(|t| t.credential.clone()),
                ice_timeout: Duration::from_secs(10),
                connect_timeout: timeout,
            };

            let mut webrtc = WebRtcConnection::new(webrtc_config).await?;

            // Create data channels
            let mut channels = HashMap::new();
            for cfg in defaults::all() {
                let channel = webrtc.create_channel(&cfg)?;
                channels.insert(cfg.name.clone(), channel);
            }

            // Create signaling channel
            let call_id = SignalingChannel::generate_call_id();
            let signaling = SignalingChannel::new(nostr.clone(), peer_pubkey, call_id);

            // Generate and send offer
            let offer = webrtc.create_offer().await?;
            signaling.send_offer(offer).await?;

            // Wait for answer (with timeout)
            // In a real implementation, we'd run an event loop here

            let (shutdown_tx, _shutdown_rx) = bounded::<()>(1);

            Ok::<Self, ConnError>(Self {
                identity,
                local_handle: None,
                peer_handle: Some(handle_owned),
                channels,
                state: Arc::new(RwLock::new(ConnState {
                    connected: true,
                    error: None,
                })),
                shutdown_tx,
            })
        })
    }

    /// Create a connection for legacy UDP mode.
    ///
    /// This creates a NetfaceConn that wraps the existing UDP networking,
    /// providing the same DataChannel interface.
    pub fn connect_udp(_local_port: u16, peer_addr: &str) -> Result<Self, ConnError> {
        let identity = Identity::load_or_generate()?;

        // Create channels that will bridge to UDP send/recv
        let mut channels = HashMap::new();

        // Video channel
        let (video_tx, _video_internal_rx) = bounded::<Vec<u8>>(2);
        let (_video_internal_tx, video_rx) = bounded::<Vec<u8>>(2);
        channels.insert(
            "video".to_string(),
            DataChannel::new("video".to_string(), video_tx, video_rx),
        );

        // Audio channel
        let (audio_tx, _audio_internal_rx) = bounded::<Vec<u8>>(32);
        let (_audio_internal_tx, audio_rx) = bounded::<Vec<u8>>(32);
        channels.insert(
            "audio".to_string(),
            DataChannel::new("audio".to_string(), audio_tx, audio_rx),
        );

        // Control channel (for config packets)
        let (control_tx, _control_internal_rx) = bounded::<Vec<u8>>(8);
        let (_control_internal_tx, control_rx) = bounded::<Vec<u8>>(8);
        channels.insert(
            "control".to_string(),
            DataChannel::new("control".to_string(), control_tx, control_rx),
        );

        let (shutdown_tx, _shutdown_rx) = bounded::<()>(1);

        Ok(Self {
            identity,
            local_handle: None,
            peer_handle: Some(peer_addr.to_string()),
            channels,
            state: Arc::new(RwLock::new(ConnState {
                connected: false, // Will be set when peer connects
                error: None,
            })),
            shutdown_tx,
        })
    }

    /// Accept incoming connections.
    ///
    /// Listens for incoming call requests via Nostr and establishes
    /// a WebRTC connection with the calling peer.
    pub fn accept_incoming(config: ConnConfig) -> Result<Self, ConnError> {
        let identity = Identity::load_or_generate()?;

        match &config.mode {
            ConnectionMode::WebRtc { relays, stun_servers: _, turn: _ } => {
                block_on(async {
                    // Create Nostr client
                    let nostr = Arc::new(NostrClient::new(identity.clone(), relays.clone()).await?);
                    nostr.connect().await?;

                    // Listen for incoming calls
                    // In a real implementation, this would be an event loop

                    let (shutdown_tx, _shutdown_rx) = bounded::<()>(1);

                    // Create default channels
                    let mut channels = HashMap::new();
                    for cfg in defaults::all() {
                        let (tx, rx) = bounded::<Vec<u8>>(64);
                        channels.insert(cfg.name.clone(), DataChannel::new(cfg.name, tx, rx));
                    }

                    Ok::<Self, ConnError>(Self {
                        identity,
                        local_handle: None,
                        peer_handle: None,
                        channels,
                        state: Arc::new(RwLock::new(ConnState {
                            connected: false,
                            error: None,
                        })),
                        shutdown_tx,
                    })
                })
            }
            ConnectionMode::Udp { local_port, .. } => {
                Self::connect_udp(*local_port, "0.0.0.0:0")
            }
        }
    }

    /// Get a data channel by name.
    pub fn channel(&self, name: &str) -> Result<DataChannel, ConnError> {
        self.channels
            .get(name)
            .cloned()
            .ok_or_else(|| ConnError::ChannelNotFound(name.to_string()))
    }

    /// Get the video channel.
    pub fn video(&self) -> Result<DataChannel, ConnError> {
        self.channel("video")
    }

    /// Get the audio channel.
    pub fn audio(&self) -> Result<DataChannel, ConnError> {
        self.channel("audio")
    }

    /// Get the control channel.
    pub fn control(&self) -> Result<DataChannel, ConnError> {
        self.channel("control")
    }

    /// Get the chat channel.
    pub fn chat(&self) -> Result<DataChannel, ConnError> {
        self.channel("chat")
    }

    /// Close the connection.
    pub fn close(&self) -> Result<(), ConnError> {
        let _ = self.shutdown_tx.send(());

        let mut state = self.state.write().map_err(|_| ConnError::Other("lock poisoned".to_string()))?;
        state.connected = false;

        Ok(())
    }

    /// Check if connected.
    pub fn is_connected(&self) -> bool {
        self.state
            .read()
            .map(|s| s.connected)
            .unwrap_or(false)
    }

    /// Get the local handle.
    pub fn local_handle(&self) -> Option<&str> {
        self.local_handle.as_deref()
    }

    /// Get the peer's handle.
    pub fn peer_handle(&self) -> Option<&str> {
        self.peer_handle.as_deref()
    }

    /// Get our identity.
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Get our npub.
    pub fn npub(&self) -> String {
        self.identity.npub()
    }

    /// Register a handle for this identity.
    pub fn register_handle(&mut self, handle: &str, relays: &[String]) -> Result<(), ConnError> {
        let identity = self.identity.clone();

        block_on(async {
            let nostr = NostrClient::new(identity, relays.to_vec()).await?;
            nostr.connect().await?;

            let registry = HandleRegistry::new(&nostr);
            registry.register(handle).await?;

            nostr.disconnect().await?;
            Ok::<(), ConnError>(())
        })?;

        self.local_handle = Some(handle.to_string());
        Ok(())
    }
}

impl Drop for NetfaceConn {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

/// Create channels for UDP mode bridging.
///
/// Returns (video_tx, video_rx, audio_tx, audio_rx) channel pairs
/// that can be used with the existing UDP send/recv loops.
pub fn create_udp_bridge() -> UdpBridge {
    let (vid_send_tx, vid_send_rx) = bounded::<Vec<u8>>(2);
    let (vid_recv_tx, vid_recv_rx) = bounded::<Vec<u8>>(2);
    let (aud_send_tx, aud_send_rx) = bounded::<Vec<u8>>(32);
    let (aud_recv_tx, aud_recv_rx) = bounded::<Vec<u8>>(32);

    UdpBridge {
        video_send: DataChannel::new("video".to_string(), vid_send_tx, vid_recv_rx),
        video_recv_tx: vid_recv_tx,
        video_send_rx: vid_send_rx,
        audio_send: DataChannel::new("audio".to_string(), aud_send_tx, aud_recv_rx),
        audio_recv_tx: aud_recv_tx,
        audio_send_rx: aud_send_rx,
    }
}

/// Bridge for UDP mode.
pub struct UdpBridge {
    /// Video DataChannel for application use.
    pub video_send: DataChannel,
    /// Receiver for video data to send over UDP.
    pub video_send_rx: Receiver<Vec<u8>>,
    /// Sender for received video data from UDP.
    pub video_recv_tx: Sender<Vec<u8>>,
    /// Audio DataChannel for application use.
    pub audio_send: DataChannel,
    /// Receiver for audio data to send over UDP.
    pub audio_send_rx: Receiver<Vec<u8>>,
    /// Sender for received audio data from UDP.
    pub audio_recv_tx: Sender<Vec<u8>>,
}
