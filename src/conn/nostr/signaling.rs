//! WebRTC signaling over Nostr.
//!
//! Uses custom kind 21337 for netface signaling messages.
//! Supports call offers, answers, ICE candidates, and call control messages.

use std::sync::Arc;
use std::time::Duration;

use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use super::NostrClient;
use crate::conn::error::ConnError;

/// Custom ephemeral kind for netface signaling (20000-29999 = ephemeral, not stored per NIP-01).
const KIND_NETFACE_SIGNAL: u64 = 21337;

/// Wait for an incoming call offer from any peer.
/// Returns the caller's pubkey, call_id, and SDP offer.
pub async fn wait_for_offer(
    client: &NostrClient,
    timeout: Duration,
) -> Result<(PublicKey, String, String), ConnError> {
    let our_pubkey = PublicKey::from_slice(&client.identity().pubkey_bytes())
        .map_err(|e| ConnError::InvalidIdentity(e.to_string()))?;

    crate::log_info!("Signal: Listening for offers to {}", &our_pubkey.to_hex()[..16]);

    let start = std::time::Instant::now();

    // Poll for offers - more reliable than notifications
    while start.elapsed() < timeout {
        // Fresh filter each time - only look at very recent offers
        let filter = Filter::new()
            .kind(Kind::Custom(KIND_NETFACE_SIGNAL))
            .pubkey(our_pubkey)
            .since(Timestamp::now() - Duration::from_secs(15));

        if let Ok(events) = client.get_events(vec![filter], Duration::from_millis(500)).await {
            // Get most recent offer
            for event in events {
                if let Ok(SignalMessage::CallOffer { call_id, sdp }) = SignalMessage::from_json(&event.content) {
                    crate::log_info!("Signal: Received offer from {}", &event.author().to_hex()[..16]);
                    return Ok((event.author(), call_id, sdp));
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    Err(ConnError::SignalingTimeout)
}

/// Wait for a call answer from a specific peer.
pub async fn wait_for_answer(
    client: &NostrClient,
    peer_pubkey: &PublicKey,
    call_id: &str,
    timeout: Duration,
) -> Result<String, ConnError> {
    let our_pubkey = PublicKey::from_slice(&client.identity().pubkey_bytes())
        .map_err(|e| ConnError::InvalidIdentity(e.to_string()))?;

    crate::log_info!("Signal: Waiting for answer from {}", &peer_pubkey.to_hex()[..16]);

    let start = std::time::Instant::now();

    // Poll for the answer - more reliable than notifications
    while start.elapsed() < timeout {
        // Fresh filter each time to get recent events
        let filter = Filter::new()
            .kind(Kind::Custom(KIND_NETFACE_SIGNAL))
            .author(*peer_pubkey)
            .pubkey(our_pubkey)
            .since(Timestamp::now() - Duration::from_secs(30));

        if let Ok(events) = client.get_events(vec![filter], Duration::from_millis(500)).await {
            for event in events {
                if let Ok(SignalMessage::CallAnswer { call_id: cid, sdp }) = SignalMessage::from_json(&event.content) {
                    if cid == call_id {
                        crate::log_info!("Signal: Received answer from {}", &peer_pubkey.to_hex()[..16]);
                        return Ok(sdp);
                    }
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    Err(ConnError::SignalingTimeout)
}

/// Signaling message types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SignalMessage {
    /// Call offer with SDP.
    CallOffer {
        call_id: String,
        sdp: String,
    },
    /// Call answer with SDP.
    CallAnswer {
        call_id: String,
        sdp: String,
    },
    /// ICE candidate.
    IceCandidate {
        call_id: String,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
    },
    /// Call rejected.
    CallReject {
        call_id: String,
        reason: Option<String>,
    },
    /// Call ended.
    CallHangup {
        call_id: String,
    },
    /// Ringing (call received, waiting for user action).
    CallRinging {
        call_id: String,
    },
}

impl SignalMessage {
    /// Get the call ID.
    pub fn call_id(&self) -> &str {
        match self {
            Self::CallOffer { call_id, .. } => call_id,
            Self::CallAnswer { call_id, .. } => call_id,
            Self::IceCandidate { call_id, .. } => call_id,
            Self::CallReject { call_id, .. } => call_id,
            Self::CallHangup { call_id } => call_id,
            Self::CallRinging { call_id } => call_id,
        }
    }

    /// Serialize to JSON.
    pub fn to_json(&self) -> Result<String, ConnError> {
        serde_json::to_string(self)
            .map_err(|e| ConnError::SignalingError(e.to_string()))
    }

    /// Deserialize from JSON.
    pub fn from_json(json: &str) -> Result<Self, ConnError> {
        serde_json::from_str(json)
            .map_err(|e| ConnError::SignalingError(e.to_string()))
    }
}

/// Signaling channel for WebRTC negotiation.
pub struct SignalingChannel {
    client: Arc<NostrClient>,
    peer_pubkey: PublicKey,
    call_id: String,
    #[allow(dead_code)]
    msg_tx: mpsc::UnboundedSender<SignalMessage>,
    msg_rx: mpsc::UnboundedReceiver<SignalMessage>,
}

impl SignalingChannel {
    /// Create a new signaling channel for a call.
    pub fn new(client: Arc<NostrClient>, peer_pubkey: PublicKey, call_id: String) -> Self {
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();

        Self {
            client,
            peer_pubkey,
            call_id,
            msg_tx,
            msg_rx,
        }
    }

    /// Generate a new call ID.
    pub fn generate_call_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    /// Send a signaling message to the peer.
    pub async fn send(&self, message: SignalMessage) -> Result<(), ConnError> {
        let json = message.to_json()?;

        // Build netface signaling event with p-tag for recipient
        let p_tag = Tag::public_key(self.peer_pubkey);
        let app_tag = Tag::custom(
            TagKind::Custom("app".to_string()),
            vec!["netface".to_string()],
        );
        let tags = vec![p_tag, app_tag];
        let builder = EventBuilder::new(Kind::Custom(KIND_NETFACE_SIGNAL), &json, tags);

        let event = self.client.client()
            .sign_event_builder(builder)
            .await
            .map_err(|e| ConnError::SignalingError(e.to_string()))?;

        crate::log_info!("Signal: Sending to {}", &self.peer_pubkey.to_hex()[..16]);
        self.client.publish(event).await?;
        crate::log_debug!("Signal: Message sent successfully");
        Ok(())
    }

    /// Send a call offer.
    pub async fn send_offer(&self, sdp: String) -> Result<(), ConnError> {
        self.send(SignalMessage::CallOffer {
            call_id: self.call_id.clone(),
            sdp,
        }).await
    }

    /// Send a call answer.
    pub async fn send_answer(&self, sdp: String) -> Result<(), ConnError> {
        self.send(SignalMessage::CallAnswer {
            call_id: self.call_id.clone(),
            sdp,
        }).await
    }

    /// Send an ICE candidate.
    pub async fn send_ice_candidate(
        &self,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
    ) -> Result<(), ConnError> {
        self.send(SignalMessage::IceCandidate {
            call_id: self.call_id.clone(),
            candidate,
            sdp_mid,
            sdp_mline_index,
        }).await
    }

    /// Send a call rejection.
    pub async fn send_reject(&self, reason: Option<String>) -> Result<(), ConnError> {
        self.send(SignalMessage::CallReject {
            call_id: self.call_id.clone(),
            reason,
        }).await
    }

    /// Send a hangup.
    pub async fn send_hangup(&self) -> Result<(), ConnError> {
        self.send(SignalMessage::CallHangup {
            call_id: self.call_id.clone(),
        }).await
    }

    /// Send ringing notification.
    pub async fn send_ringing(&self) -> Result<(), ConnError> {
        self.send(SignalMessage::CallRinging {
            call_id: self.call_id.clone(),
        }).await
    }

    /// Receive the next signaling message.
    pub async fn recv(&mut self) -> Option<SignalMessage> {
        self.msg_rx.recv().await
    }

    /// Receive with timeout.
    pub async fn recv_timeout(&mut self, timeout: Duration) -> Result<SignalMessage, ConnError> {
        tokio::time::timeout(timeout, self.msg_rx.recv())
            .await
            .map_err(|_| ConnError::SignalingTimeout)?
            .ok_or(ConnError::SignalingError("channel closed".to_string()))
    }

    /// Get the call ID.
    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    /// Get the peer's public key.
    pub fn peer_pubkey(&self) -> &PublicKey {
        &self.peer_pubkey
    }
}

/// Listen for incoming signaling messages.
pub struct SignalingListener {
    client: Arc<NostrClient>,
    incoming_tx: mpsc::UnboundedSender<(PublicKey, SignalMessage)>,
}

impl SignalingListener {
    /// Create a new signaling listener.
    pub fn new(client: Arc<NostrClient>) -> (Self, mpsc::UnboundedReceiver<(PublicKey, SignalMessage)>) {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        (Self { client, incoming_tx }, incoming_rx)
    }

    /// Start listening for incoming messages.
    pub async fn start(&self) -> Result<(), ConnError> {
        // Subscribe to encrypted direct messages to our pubkey
        let our_pubkey = PublicKey::from_slice(&self.client.identity().pubkey_bytes())
            .map_err(|e| ConnError::InvalidIdentity(e.to_string()))?;

        let filter = Filter::new()
            .kind(Kind::EncryptedDirectMessage)
            .pubkey(our_pubkey)
            .since(Timestamp::now());

        self.client.subscribe(vec![filter]).await?;

        Ok(())
    }

    /// Process an incoming event.
    pub fn process_event(&self, event: &Event) -> Result<(), ConnError> {
        if event.kind != Kind::EncryptedDirectMessage {
            return Ok(());
        }

        // Try to parse as a signaling message
        // Note: In a real implementation, we'd need to decrypt first
        let message = match SignalMessage::from_json(&event.content) {
            Ok(msg) => msg,
            Err(_) => return Ok(()), // Not a netface message
        };

        // Send to listener
        let _ = self.incoming_tx.send((event.author(), message));

        Ok(())
    }
}
