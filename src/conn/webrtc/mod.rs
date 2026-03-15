//! WebRTC connection management using str0m.
//!
//! Provides DataChannel-based connectivity for video, audio, and control data.

pub mod ice;
pub mod sdp;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, Sender};
use str0m::channel::{ChannelId, Reliability};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};
use tokio::net::UdpSocket;

use crate::conn::channel::{self, DataChannel};
use crate::conn::error::ConnError;

/// WebRTC connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Initial state, not yet connecting.
    New,
    /// Gathering ICE candidates.
    Gathering,
    /// Exchanging signaling messages.
    Connecting,
    /// ICE connected, establishing DTLS.
    Connected,
    /// Fully connected and operational.
    Ready,
    /// Connection failed.
    Failed,
    /// Connection closed.
    Closed,
}

/// Configuration for a WebRTC connection.
#[derive(Debug, Clone)]
pub struct WebRtcConfig {
    /// STUN servers for ICE.
    pub stun_servers: Vec<String>,
    /// TURN server URL (optional).
    pub turn_url: Option<String>,
    /// TURN username (optional).
    pub turn_username: Option<String>,
    /// TURN credential (optional).
    pub turn_credential: Option<String>,
    /// ICE gathering timeout.
    pub ice_timeout: Duration,
    /// Connection timeout.
    pub connect_timeout: Duration,
}

impl Default for WebRtcConfig {
    fn default() -> Self {
        Self {
            stun_servers: vec!["stun:stun.l.google.com:19302".to_string()],
            turn_url: None,
            turn_username: None,
            turn_credential: None,
            ice_timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(30),
        }
    }
}

/// A WebRTC connection with data channels.
pub struct WebRtcConnection {
    /// The str0m RTC instance.
    rtc: Rtc,
    /// Connection state.
    state: ConnectionState,
    /// Local socket.
    socket: Arc<UdpSocket>,
    /// Local address.
    local_addr: SocketAddr,
    /// Remote address (once known).
    remote_addr: Option<SocketAddr>,
    /// Data channels by name.
    channels: HashMap<String, ChannelState>,
    /// Configuration.
    #[allow(dead_code)]
    config: WebRtcConfig,
    /// Pending offer (if we're the offerer).
    pending_offer: Option<str0m::change::SdpPendingOffer>,
}

/// State of a data channel.
struct ChannelState {
    /// Channel ID (None until channel opens)
    id: Option<ChannelId>,
    /// Sender to forward received data from peer to the app
    inbound_tx: Sender<Vec<u8>>,
    /// Receiver for outbound data from app to send to peer
    outbound_rx: Receiver<Vec<u8>>,
}

impl WebRtcConnection {
    /// Create a new WebRTC connection.
    pub async fn new(config: WebRtcConfig) -> Result<Self, ConnError> {
        // Bind UDP socket
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| ConnError::WebRtcConnection(e.to_string()))?;

        // Get the actual local IP by "connecting" to a public address
        // This doesn't send data, just makes the OS select the right interface
        let local_addr = get_local_addr(&socket).await?;

        // Create str0m RTC instance
        let rtc = Rtc::builder()
            .set_ice_lite(false)
            .build(Instant::now());

        Ok(Self {
            rtc,
            state: ConnectionState::New,
            socket: Arc::new(socket),
            local_addr,
            remote_addr: None,
            channels: HashMap::new(),
            config,
            pending_offer: None,
        })
    }

    /// Gather ICE candidates including STUN reflexive candidates.
    pub async fn gather_candidates(&mut self) -> Result<(), ConnError> {
        // Skip if already gathered
        if self.state != ConnectionState::New {
            crate::log_debug!("ICE: Already gathered, skipping");
            return Ok(());
        }
        crate::log_debug!("ICE: Adding host candidate: {}", self.local_addr);
        let host_candidate = Candidate::host(self.local_addr, Protocol::Udp)
            .map_err(|e| ConnError::IceGathering(e.to_string()))?;
        crate::log_debug!("ICE: Host candidate created, adding to RTC...");
        self.rtc.add_local_candidate(host_candidate);
        crate::log_debug!("ICE: Host candidate added");

        // Do STUN discovery with a SEPARATE socket to avoid conflicts with str0m
        crate::log_debug!("ICE: Starting STUN gathering, {} servers configured", self.config.stun_servers.len());
        for stun_url in &self.config.stun_servers {
            crate::log_info!("ICE: Trying STUN server: {}", stun_url);

            // Create a fresh socket just for STUN
            let stun_socket = match UdpSocket::bind("0.0.0.0:0").await {
                Ok(s) => s,
                Err(e) => {
                    crate::log_error!("ICE: Failed to create STUN socket: {}", e);
                    continue;
                }
            };

            match tokio::time::timeout(
                Duration::from_millis(1500),
                ice::stun_binding(&stun_socket, stun_url, Duration::from_millis(1500))
            ).await {
                Ok(Ok(srflx_addr)) => {
                    crate::log_info!("ICE: Got STUN response: {}", srflx_addr);
                    // Create srflx candidate with our actual local addr as base
                    if let Ok(srflx_candidate) = Candidate::server_reflexive(
                        srflx_addr,
                        self.local_addr,
                        Protocol::Udp,
                    ) {
                        self.rtc.add_local_candidate(srflx_candidate);
                        crate::log_info!("ICE: Added server-reflexive candidate: {}", srflx_addr);
                    }
                    break; // Got one, that's enough
                }
                Ok(Err(e)) => {
                    crate::log_error!("ICE: STUN failed: {}", e);
                }
                Err(_) => {
                    crate::log_warn!("ICE: STUN timeout for {}", stun_url);
                }
            }
        }

        self.state = ConnectionState::Gathering;
        crate::log_info!("ICE: Gathering complete");
        Ok(())
    }

    /// Prepare a data channel (will be opened after SDP negotiation).
    /// The channel name must match what's added to the SDP offer.
    pub fn prepare_channel(&mut self, name: &str) -> DataChannel {
        // Channel pairs:
        // - outbound: app sends → outbound_rx → WebRTC → peer
        // - inbound: peer → WebRTC → inbound_tx → app receives
        let (outbound_tx, outbound_rx) = bounded::<Vec<u8>>(64);
        let (inbound_tx, inbound_rx) = bounded::<Vec<u8>>(64);

        // Store channel state (id will be set when channel opens)
        self.channels.insert(
            name.to_string(),
            ChannelState {
                id: None,
                inbound_tx,   // WebRTC writes here when data arrives from peer
                outbound_rx,  // WebRTC reads from here to send to peer
            },
        );

        // DataChannel: app uses outbound_tx to send, inbound_rx to receive
        DataChannel::new(name.to_string(), outbound_tx, inbound_rx)
    }

    /// Legacy method for compatibility - just calls prepare_channel
    #[allow(dead_code)]
    pub fn create_channel(&mut self, cfg: &channel::ChannelConfig) -> Result<DataChannel, ConnError> {
        Ok(self.prepare_channel(&cfg.name))
    }

    /// Create an SDP offer (async to allow STUN gathering).
    pub async fn create_offer(&mut self) -> Result<String, ConnError> {
        // Gather ICE candidates (host + STUN reflexive)
        self.gather_candidates().await?;

        // Generate offer - add all data channels we need via SDP API
        crate::log_debug!("SDP: Creating offer...");
        let mut change = self.rtc.sdp_api();

        // Add data channels for negotiation (these will open after DTLS)
        crate::log_debug!("SDP: Adding data channels to offer...");
        change.add_channel("video".to_string());
        change.add_channel("audio".to_string());
        change.add_channel("control".to_string());

        crate::log_debug!("SDP: Applying changes...");
        let (offer, pending) = change.apply()
            .ok_or_else(|| {
                crate::log_error!("SDP: Failed to create offer - no changes to apply");
                ConnError::SdpGeneration("failed to create offer".to_string())
            })?;

        let sdp_str = offer.to_sdp_string();
        crate::log_info!("SDP: Offer created, {} bytes", sdp_str.len());
        // Log key SDP sections to verify channels are included
        for line in sdp_str.lines() {
            if line.starts_with("m=") || line.starts_with("a=sctpmap") || line.starts_with("a=max-message-size") {
                crate::log_debug!("SDP: {}", line);
            }
        }
        self.pending_offer = Some(pending);

        Ok(sdp_str)
    }

    /// Create an SDP answer from a remote offer (async to allow STUN gathering).
    pub async fn create_answer(&mut self, remote_sdp: &str) -> Result<String, ConnError> {
        // Gather ICE candidates (host + STUN reflexive)
        self.gather_candidates().await?;

        // Parse remote offer
        let offer = str0m::change::SdpOffer::from_sdp_string(remote_sdp)
            .map_err(|e| ConnError::SdpParse(e.to_string()))?;

        // Accept the offer and generate answer
        let answer = self.rtc.sdp_api()
            .accept_offer(offer)
            .map_err(|e| ConnError::SdpGeneration(e.to_string()))?;

        self.state = ConnectionState::Connecting;

        Ok(answer.to_sdp_string())
    }

    /// Set the remote SDP answer.
    pub fn set_remote_answer(&mut self, remote_sdp: &str) -> Result<(), ConnError> {
        let answer = str0m::change::SdpAnswer::from_sdp_string(remote_sdp)
            .map_err(|e| ConnError::SdpParse(e.to_string()))?;

        let pending = self.pending_offer.take()
            .ok_or_else(|| ConnError::SdpParse("no pending offer".to_string()))?;

        self.rtc.sdp_api()
            .accept_answer(pending, answer)
            .map_err(|e| ConnError::SdpParse(e.to_string()))?;

        self.state = ConnectionState::Connecting;

        Ok(())
    }

    /// Add a remote ICE candidate.
    pub fn add_ice_candidate(&mut self, candidate: &str) -> Result<(), ConnError> {
        let cand = Candidate::from_sdp_string(candidate)
            .map_err(|e| ConnError::IceGathering(e.to_string()))?;

        self.rtc.add_remote_candidate(cand);
        Ok(())
    }

    /// Get the connection state.
    pub fn state(&self) -> ConnectionState {
        self.state
    }

    /// Check if connected.
    pub fn is_connected(&self) -> bool {
        matches!(self.state, ConnectionState::Connected | ConnectionState::Ready)
    }

    /// Poll the connection and process events.
    /// Returns the next timeout instant when we should poll again.
    pub async fn poll(&mut self) -> Result<Option<Instant>, ConnError> {
        // First, drive the state machine with current time
        let now = Instant::now();
        if let Err(e) = self.rtc.handle_input(Input::Timeout(now)) {
            crate::log_error!("WebRTC: Timeout input error: {}", e);
        }

        // Handle outgoing packets and events until we get a timeout
        let next_timeout = loop {
            match self.rtc.poll_output() {
                Ok(Output::Transmit(transmit)) => {
                    // IMPORTANT: Always use str0m's destination, not our remote_addr
                    // str0m knows where to send ICE connectivity checks
                    crate::log_debug!(
                        "WebRTC: TX {} bytes to {}",
                        transmit.contents.len(),
                        transmit.destination
                    );
                    if let Err(e) = self.socket.send_to(&transmit.contents, transmit.destination).await {
                        crate::log_error!("WebRTC: Send error: {}", e);
                    }
                }
                Ok(Output::Timeout(t)) => {
                    // str0m wants us to call handle_input(Input::Timeout) at time t
                    break Some(t);
                }
                Ok(Output::Event(event)) => {
                    self.handle_event(event)?;
                }
                Err(e) => {
                    self.state = ConnectionState::Failed;
                    return Err(ConnError::WebRtcConnection(e.to_string()));
                }
            }
        };

        // Process outbound data from channels
        for (name, channel_state) in &self.channels {
            while let Ok(data) = channel_state.outbound_rx.try_recv() {
                if let Some(id) = channel_state.id {
                    if let Some(mut channel) = self.rtc.channel(id) {
                        crate::log_debug!("WebRTC: Sending {} bytes on channel '{}'", data.len(), name);
                        let _ = channel.write(true, &data);
                    } else {
                        crate::log_warn!("WebRTC: Channel '{}' (id={:?}) exists but not in RTC", name, id);
                    }
                } else {
                    crate::log_warn!("WebRTC: Channel '{}' not open yet, dropping {} bytes", name, data.len());
                }
            }
        }

        Ok(next_timeout)
    }

    /// Handle a str0m event.
    fn handle_event(&mut self, event: Event) -> Result<(), ConnError> {
        // Log ALL events for debugging
        crate::log_debug!("WebRTC: Event received: {:?}", event);

        match event {
            Event::IceConnectionStateChange(state) => {
                crate::log_info!("WebRTC: ICE state changed to {:?}", state);
                match state {
                    IceConnectionState::New => {
                        self.state = ConnectionState::New;
                    }
                    IceConnectionState::Checking => {
                        self.state = ConnectionState::Connecting;
                    }
                    IceConnectionState::Connected | IceConnectionState::Completed => {
                        crate::log_info!("WebRTC: ICE CONNECTED!");
                        self.state = ConnectionState::Connected;
                    }
                    IceConnectionState::Disconnected => {
                        crate::log_warn!("WebRTC: ICE disconnected");
                        self.state = ConnectionState::Closed;
                    }
                }
            }
            Event::ChannelOpen(id, name) => {
                crate::log_info!("WebRTC: Channel opened: {} ({:?})", name, id);
                // Update the channel state with the actual channel ID
                if let Some(state) = self.channels.get_mut(&name) {
                    state.id = Some(id);
                    crate::log_info!("WebRTC: Channel '{}' is now active", name);
                } else {
                    crate::log_warn!("WebRTC: Channel '{}' opened but not prepared", name);
                }
                self.state = ConnectionState::Ready;
            }
            Event::ChannelData(data) => {
                crate::log_debug!("WebRTC: Channel data: {} bytes on {:?}", data.data.len(), data.id);
                // Find channel and forward data to app
                if let Some((name, state)) = self.channels.iter().find(|(_, s)| s.id == Some(data.id)) {
                    crate::log_debug!("WebRTC: Forwarding {} bytes to '{}'", data.data.len(), name);
                    let _ = state.inbound_tx.try_send(data.data.to_vec());
                } else {
                    crate::log_warn!("WebRTC: Received data for unknown channel {:?}", data.id);
                }
            }
            Event::ChannelClose(id) => {
                crate::log_info!("WebRTC: Channel closed: {:?}", id);
            }
            Event::Connected => {
                crate::log_info!("WebRTC: DTLS connected!");
            }
            _ => {
                crate::log_debug!("WebRTC: Event: {:?}", event);
            }
        }

        Ok(())
    }

    /// Receive incoming UDP packets.
    pub async fn receive(&mut self) -> Result<bool, ConnError> {
        let mut buf = vec![0u8; 65535];

        match self.socket.try_recv_from(&mut buf) {
            Ok((len, addr)) => {
                crate::log_debug!("WebRTC: RX {} bytes from {}", len, addr);
                self.remote_addr = Some(addr);

                let receive = Receive {
                    proto: Protocol::Udp,
                    source: addr,
                    destination: self.local_addr,
                    contents: (&buf[..len]).try_into()
                        .map_err(|_| ConnError::WebRtcConnection("packet too large".to_string()))?,
                };

                let input = Input::Receive(Instant::now(), receive);

                self.rtc.handle_input(input)
                    .map_err(|e| {
                        crate::log_error!("WebRTC: Handle input error: {}", e);
                        ConnError::WebRtcConnection(e.to_string())
                    })?;

                Ok(true) // Received data
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                Ok(false) // No data available
            }
            Err(e) => {
                Err(ConnError::Io(e))
            }
        }
    }

    /// Close the connection.
    pub fn close(&mut self) {
        self.state = ConnectionState::Closed;
        // str0m will clean up on drop
    }

    /// Get the local address.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }
}

/// Get the actual local IP address for a socket.
///
/// When bound to 0.0.0.0, we need to discover which local IP to use.
/// This "connects" to a public IP (without sending data) to let the OS
/// select the appropriate interface, then reads back the local address.
async fn get_local_addr(socket: &UdpSocket) -> Result<SocketAddr, ConnError> {
    // First get the port we're bound to
    let bound_addr = socket.local_addr()
        .map_err(|e| ConnError::WebRtcConnection(e.to_string()))?;
    let port = bound_addr.port();

    // Create a temporary socket to discover local IP
    let temp_socket = std::net::UdpSocket::bind("0.0.0.0:0")
        .map_err(|e| ConnError::WebRtcConnection(e.to_string()))?;

    // "Connect" to a public IP - this doesn't send data, just selects interface
    temp_socket.connect("8.8.8.8:80")
        .map_err(|e| ConnError::WebRtcConnection(e.to_string()))?;

    // Get the local address the OS chose
    let local_ip = temp_socket.local_addr()
        .map_err(|e| ConnError::WebRtcConnection(e.to_string()))?
        .ip();

    Ok(SocketAddr::new(local_ip, port))
}
