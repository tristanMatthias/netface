//! Handle-based peer discovery using NIP-78 app-specific data.
//!
//! Handles are registered as kind 30078 events with a `d` tag containing
//! the handle name. This allows users to find each other by human-readable
//! names instead of cryptographic public keys.

use std::time::Duration;

use nostr_sdk::prelude::*;

use super::NostrClient;
use crate::conn::error::ConnError;

/// Kind for NIP-78 app-specific data.
const KIND_APP_SPECIFIC: u64 = 30078;

/// App identifier for netface handles.
const APP_ID: &str = "netface";

/// Handle registration and lookup.
pub struct HandleRegistry<'a> {
    client: &'a NostrClient,
}

impl<'a> HandleRegistry<'a> {
    /// Create a new handle registry.
    pub fn new(client: &'a NostrClient) -> Self {
        Self { client }
    }

    /// Register a handle for this identity.
    ///
    /// The handle will be published as a NIP-78 replaceable event.
    pub async fn register(&self, handle: &str) -> Result<EventId, ConnError> {
        let handle = normalize_handle(handle);

        // Check if handle is already taken by someone else
        if let Some(pubkey) = self.lookup(&handle).await? {
            let our_pubkey = self.client.pubkey_hex();
            if pubkey != our_pubkey {
                return Err(ConnError::HandleAlreadyRegistered(handle));
            }
        }

        // Build NIP-78 event with tags
        let d_tag = Tag::custom(
            TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::D)),
            vec![format!("{}:{}", APP_ID, handle)],
        );
        let app_tag = Tag::custom(
            TagKind::Custom("app".to_string()),
            vec![APP_ID.to_string()],
        );
        let handle_tag = Tag::custom(
            TagKind::Custom("handle".to_string()),
            vec![handle.clone()],
        );

        let tags = vec![d_tag, app_tag, handle_tag];
        let builder = EventBuilder::new(Kind::Custom(KIND_APP_SPECIFIC), format!("netface handle: {}", handle), tags);

        let event = self.client.client()
            .sign_event_builder(builder)
            .await
            .map_err(|e| ConnError::PublishFailed(e.to_string()))?;

        self.client.publish(event).await
    }

    /// Look up a handle and return the associated public key (hex).
    pub async fn lookup(&self, handle: &str) -> Result<Option<String>, ConnError> {
        let handle = normalize_handle(handle);

        let d_value = format!("{}:{}", APP_ID, handle);
        let filter = Filter::new()
            .kind(Kind::Custom(KIND_APP_SPECIFIC))
            .custom_tag(SingleLetterTag::lowercase(Alphabet::D), vec![d_value])
            .limit(1);

        let events = self.client
            .get_events(vec![filter], Duration::from_secs(5))
            .await?;

        // Get the most recent event
        let event = events.into_iter().max_by_key(|e| e.created_at);

        Ok(event.map(|e| e.author().to_hex()))
    }

    /// Look up a handle and return the public key bytes.
    pub async fn lookup_bytes(&self, handle: &str) -> Result<Option<[u8; 32]>, ConnError> {
        match self.lookup(handle).await? {
            Some(hex) => {
                let bytes = hex::decode(&hex)
                    .map_err(|e| ConnError::InvalidIdentity(e.to_string()))?;
                if bytes.len() != 32 {
                    return Err(ConnError::InvalidIdentity("invalid pubkey length".to_string()));
                }
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                Ok(Some(arr))
            }
            None => Ok(None),
        }
    }

    /// Resolve a handle or npub to a PublicKey.
    ///
    /// If the input starts with "npub1", it's treated as a bech32-encoded public key.
    /// Otherwise, it's looked up as a handle in the registry.
    pub async fn resolve(&self, handle: &str) -> Result<PublicKey, ConnError> {
        let input = handle.trim();

        // Check if it's an npub (bech32-encoded public key)
        if input.starts_with("npub1") {
            crate::log_info!("Discovery: Parsing npub directly");
            return PublicKey::from_bech32(input)
                .map_err(|e| ConnError::InvalidIdentity(format!("invalid npub: {}", e)));
        }

        // Otherwise, look up as a handle
        let handle = normalize_handle(input);
        crate::log_debug!("Discovery: Looking up handle '{}'", handle);

        match self.lookup_bytes(&handle).await? {
            Some(bytes) => {
                crate::log_info!("Discovery: Found handle '{}' -> {}", handle, hex::encode(&bytes[..8]));
                PublicKey::from_slice(&bytes)
                    .map_err(|e| ConnError::InvalidIdentity(e.to_string()))
            }
            None => {
                crate::log_warn!("Discovery: Handle '{}' not found", handle);
                Err(ConnError::HandleNotFound(handle))
            }
        }
    }

    /// Unregister (delete) a handle.
    ///
    /// Publishes an empty replacement event.
    pub async fn unregister(&self, handle: &str) -> Result<EventId, ConnError> {
        let handle = normalize_handle(handle);

        let d_tag = Tag::custom(
            TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::D)),
            vec![format!("{}:{}", APP_ID, handle)],
        );
        let app_tag = Tag::custom(
            TagKind::Custom("app".to_string()),
            vec![APP_ID.to_string()],
        );
        let deleted_tag = Tag::custom(
            TagKind::Custom("deleted".to_string()),
            vec!["true".to_string()],
        );

        let tags = vec![d_tag, app_tag, deleted_tag];
        let builder = EventBuilder::new(Kind::Custom(KIND_APP_SPECIFIC), "", tags);

        let event = self.client.client()
            .sign_event_builder(builder)
            .await
            .map_err(|e| ConnError::PublishFailed(e.to_string()))?;

        self.client.publish(event).await
    }
}

/// Normalize a handle by lowercasing and stripping the @ prefix.
fn normalize_handle(handle: &str) -> String {
    handle.trim().to_lowercase().trim_start_matches('@').to_string()
}
