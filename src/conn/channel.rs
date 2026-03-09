//! DataChannel abstraction over WebRTC data channels.
//!
//! Provides a sync API for sending/receiving data over WebRTC data channels,
//! bridged from the async str0m implementation.

use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, TryRecvError};

use super::error::ConnError;

/// Configuration for a data channel.
#[derive(Debug, Clone)]
pub struct ChannelConfig {
    /// Channel name/label.
    pub name: String,
    /// Whether messages are delivered in order.
    pub ordered: bool,
    /// Whether messages are reliably delivered (retransmitted if lost).
    pub reliable: bool,
    /// Maximum retransmits (only if reliable is false).
    pub max_retransmits: Option<u16>,
    /// Maximum packet lifetime in ms (only if reliable is false).
    pub max_packet_lifetime: Option<u16>,
}

impl ChannelConfig {
    /// Create a new channel config.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            ordered: true,
            reliable: true,
            max_retransmits: None,
            max_packet_lifetime: None,
        }
    }

    /// Set ordered delivery.
    pub fn ordered(mut self, ordered: bool) -> Self {
        self.ordered = ordered;
        self
    }

    /// Set reliable delivery.
    pub fn reliable(mut self, reliable: bool) -> Self {
        self.reliable = reliable;
        self
    }

    /// Set max retransmits (implies unreliable).
    pub fn max_retransmits(mut self, max: u16) -> Self {
        self.reliable = false;
        self.max_retransmits = Some(max);
        self
    }

    /// Set max packet lifetime in ms (implies unreliable).
    pub fn max_packet_lifetime(mut self, ms: u16) -> Self {
        self.reliable = false;
        self.max_packet_lifetime = Some(ms);
        self
    }
}

/// Default channel configurations for netface.
pub mod defaults {
    use super::ChannelConfig;

    /// Video channel: unordered, unreliable (frame drops OK).
    pub fn video() -> ChannelConfig {
        ChannelConfig::new("video")
            .ordered(false)
            .max_packet_lifetime(100) // 100ms max lifetime
    }

    /// Audio channel: unordered, unreliable (latency-sensitive).
    pub fn audio() -> ChannelConfig {
        ChannelConfig::new("audio")
            .ordered(false)
            .max_packet_lifetime(50) // 50ms max lifetime
    }

    /// Chat channel: ordered, reliable (text messages).
    pub fn chat() -> ChannelConfig {
        ChannelConfig::new("chat")
            .ordered(true)
            .reliable(true)
    }

    /// Control channel: ordered, reliable (hangup, keepalive).
    pub fn control() -> ChannelConfig {
        ChannelConfig::new("control")
            .ordered(true)
            .reliable(true)
    }

    /// Get all default channel configs.
    pub fn all() -> Vec<ChannelConfig> {
        vec![video(), audio(), chat(), control()]
    }
}

/// A data channel for sending and receiving bytes.
///
/// This provides a synchronous API that bridges to the async WebRTC implementation.
#[derive(Clone)]
pub struct DataChannel {
    name: String,
    tx: Sender<Vec<u8>>,
    rx: Receiver<Vec<u8>>,
}

impl DataChannel {
    /// Create a new DataChannel with the given name and channel pair.
    pub fn new(name: String, tx: Sender<Vec<u8>>, rx: Receiver<Vec<u8>>) -> Self {
        Self { name, tx, rx }
    }

    /// Get the channel name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Send data on this channel.
    ///
    /// This is non-blocking; data is queued for async sending.
    pub fn send(&self, data: &[u8]) -> Result<(), ConnError> {
        self.tx
            .send(data.to_vec())
            .map_err(|_| ConnError::ChannelClosed)
    }

    /// Try to send data without blocking.
    ///
    /// Returns `Ok(true)` if sent, `Ok(false)` if channel is full.
    pub fn try_send(&self, data: &[u8]) -> Result<bool, ConnError> {
        match self.tx.try_send(data.to_vec()) {
            Ok(()) => Ok(true),
            Err(crossbeam_channel::TrySendError::Full(_)) => Ok(false),
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => Err(ConnError::ChannelClosed),
        }
    }

    /// Receive data from this channel, blocking until data is available.
    pub fn recv(&self) -> Result<Vec<u8>, ConnError> {
        self.rx.recv().map_err(|_| ConnError::ChannelClosed)
    }

    /// Receive data with a timeout.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Vec<u8>, ConnError> {
        self.rx
            .recv_timeout(timeout)
            .map_err(|e| match e {
                crossbeam_channel::RecvTimeoutError::Timeout => ConnError::Timeout,
                crossbeam_channel::RecvTimeoutError::Disconnected => ConnError::ChannelClosed,
            })
    }

    /// Try to receive data without blocking.
    pub fn try_recv(&self) -> Option<Vec<u8>> {
        match self.rx.try_recv() {
            Ok(data) => Some(data),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => None,
        }
    }

    /// Check if the channel has data available.
    pub fn has_data(&self) -> bool {
        !self.rx.is_empty()
    }

    /// Get the number of pending messages in the receive queue.
    pub fn pending(&self) -> usize {
        self.rx.len()
    }
}

impl std::fmt::Debug for DataChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataChannel")
            .field("name", &self.name)
            .field("pending_recv", &self.rx.len())
            .finish()
    }
}
