//! Error types for the connectivity module.

use thiserror::Error;

/// All errors that can occur in the connectivity module.
#[derive(Debug, Error)]
pub enum ConnError {
    // ─── Identity Errors ────────────────────────────────────────────────────
    #[error("failed to generate keypair: {0}")]
    KeyGeneration(String),

    #[error("failed to load identity: {0}")]
    IdentityLoad(String),

    #[error("failed to save identity: {0}")]
    IdentitySave(String),

    #[error("invalid identity format: {0}")]
    InvalidIdentity(String),

    // ─── Nostr Errors ───────────────────────────────────────────────────────
    #[error("failed to connect to relay: {0}")]
    RelayConnection(String),

    #[error("handle not found: {0}")]
    HandleNotFound(String),

    #[error("handle already registered: {0}")]
    HandleAlreadyRegistered(String),

    #[error("failed to publish event: {0}")]
    PublishFailed(String),

    #[error("signaling timeout")]
    SignalingTimeout,

    #[error("signaling error: {0}")]
    SignalingError(String),

    #[error("call rejected by peer")]
    CallRejected,

    // ─── WebRTC Errors ──────────────────────────────────────────────────────
    #[error("ICE gathering failed: {0}")]
    IceGathering(String),

    #[error("ICE connection failed")]
    IceConnectionFailed,

    #[error("SDP parsing error: {0}")]
    SdpParse(String),

    #[error("SDP generation error: {0}")]
    SdpGeneration(String),

    #[error("WebRTC connection failed: {0}")]
    WebRtcConnection(String),

    #[error("data channel not found: {0}")]
    ChannelNotFound(String),

    #[error("data channel closed")]
    ChannelClosed,

    #[error("data channel send error: {0}")]
    ChannelSend(String),

    // ─── Bridge Errors ──────────────────────────────────────────────────────
    #[error("async runtime error: {0}")]
    RuntimeError(String),

    #[error("channel send error")]
    BridgeSend,

    #[error("channel receive error")]
    BridgeRecv,

    // ─── Connection State Errors ────────────────────────────────────────────
    #[error("not connected")]
    NotConnected,

    #[error("already connected")]
    AlreadyConnected,

    #[error("connection closed")]
    ConnectionClosed,

    // ─── Configuration Errors ───────────────────────────────────────────────
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("no relays configured")]
    NoRelays,

    #[error("no STUN servers configured")]
    NoStunServers,

    // ─── Generic Errors ─────────────────────────────────────────────────────
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("timeout")]
    Timeout,

    #[error("{0}")]
    Other(String),
}

impl From<crossbeam_channel::RecvError> for ConnError {
    fn from(_: crossbeam_channel::RecvError) -> Self {
        ConnError::BridgeRecv
    }
}

impl<T> From<crossbeam_channel::SendError<T>> for ConnError {
    fn from(_: crossbeam_channel::SendError<T>) -> Self {
        ConnError::BridgeSend
    }
}
