//! Nostr integration for peer discovery and signaling.
//!
//! Uses NIP-78 for handle registration and NIP-04 encrypted DMs for signaling.

pub mod discovery;
pub mod signaling;

use std::sync::Arc;
use std::time::Duration;

use nostr_sdk::prelude::*;
use tokio::sync::RwLock;

use crate::conn::error::ConnError;
use crate::conn::identity::Identity;

/// Default relays for Nostr connectivity.
pub const DEFAULT_RELAYS: &[&str] = &[
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://relay.nostr.band",
];

/// Nostr client wrapper for netface.
pub struct NostrClient {
    client: Client,
    identity: Identity,
    #[allow(dead_code)]
    relays: Vec<String>,
}

impl NostrClient {
    /// Create a new Nostr client with the given identity and relays.
    pub async fn new(identity: Identity, relays: Vec<String>) -> Result<Self, ConnError> {
        let keys = Keys::new(SecretKey::from_slice(&identity.secret_bytes())
            .map_err(|e| ConnError::InvalidIdentity(e.to_string()))?);

        let client = Client::new(&keys);

        Ok(Self {
            client,
            identity,
            relays,
        })
    }

    /// Connect to configured relays.
    pub async fn connect(&self) -> Result<(), ConnError> {
        for relay in &self.relays {
            self.client
                .add_relay(relay.as_str())
                .await
                .map_err(|e| ConnError::RelayConnection(e.to_string()))?;
        }

        self.client.connect().await;

        // Very brief wait - relays connect async in background
        tokio::time::sleep(Duration::from_millis(100)).await;

        Ok(())
    }

    /// Disconnect from all relays.
    pub async fn disconnect(&self) -> Result<(), ConnError> {
        self.client.disconnect().await
            .map_err(|e| ConnError::RelayConnection(e.to_string()))
    }

    /// Get the underlying nostr-sdk client.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Get the identity.
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Get our public key as hex.
    pub fn pubkey_hex(&self) -> String {
        hex::encode(self.identity.pubkey_bytes())
    }

    /// Publish an event with timeout to avoid blocking on slow relays.
    pub async fn publish(&self, event: Event) -> Result<EventId, ConnError> {
        crate::log_debug!("Nostr: Publishing event kind={}", event.kind.as_u64());
        let event_id = event.id;

        // Wait for at least some relay confirmations, but don't block too long
        match tokio::time::timeout(
            Duration::from_secs(3),
            self.client.send_event(event)
        ).await {
            Ok(Ok(_)) => {
                crate::log_info!("Nostr: Published event {}", &event_id.to_hex()[..16]);
            }
            Ok(Err(e)) => {
                crate::log_warn!("Nostr: Publish error (continuing): {}", e);
            }
            Err(_) => {
                // Timeout is fine - event was likely sent to at least one relay
                crate::log_debug!("Nostr: Publish timeout (event likely sent)");
            }
        }

        Ok(event_id)
    }

    /// Subscribe to events matching a filter.
    pub async fn subscribe(&self, filters: Vec<Filter>) -> Result<(), ConnError> {
        self.client
            .subscribe(filters, None)
            .await;
        Ok(())
    }

    /// Get events matching a filter.
    pub async fn get_events(&self, filters: Vec<Filter>, timeout: Duration) -> Result<Vec<Event>, ConnError> {
        let events = self.client
            .get_events_of(filters, Some(timeout))
            .await
            .map_err(|e| ConnError::RelayConnection(e.to_string()))?;

        Ok(events)
    }
}

/// Shared Nostr client state.
pub type SharedNostrClient = Arc<RwLock<Option<NostrClient>>>;
