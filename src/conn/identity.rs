//! Identity management using secp256k1 keypairs.
//!
//! Identities are stored at `~/.config/netface/identity.key` and are compatible
//! with Nostr keypairs (npub/nsec bech32 encoding).

use std::fs;
use std::path::PathBuf;

use bech32::{Bech32m, Hrp};
use secp256k1::{Keypair, PublicKey, Secp256k1, SecretKey};

use super::error::ConnError;

/// A Nostr-compatible identity backed by a secp256k1 keypair.
#[derive(Clone)]
pub struct Identity {
    keypair: Keypair,
}

impl Identity {
    /// Generate a new random identity.
    pub fn generate() -> Result<Self, ConnError> {
        let secp = Secp256k1::new();
        let (secret_key, _) = secp.generate_keypair(&mut secp256k1::rand::thread_rng());
        let keypair = Keypair::from_secret_key(&secp, &secret_key);
        Ok(Self { keypair })
    }

    /// Create identity from an existing secret key.
    pub fn from_secret_key(secret_key: SecretKey) -> Self {
        let secp = Secp256k1::new();
        let keypair = Keypair::from_secret_key(&secp, &secret_key);
        Self { keypair }
    }

    /// Get the default identity file path.
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join("netface").join("identity.key"))
    }

    /// Load identity from the default path, or generate and save a new one.
    pub fn load_or_generate() -> Result<Self, ConnError> {
        let Some(path) = Self::default_path() else {
            return Self::generate();
        };

        if path.exists() {
            Self::load(&path)
        } else {
            let identity = Self::generate()?;
            identity.save(&path)?;
            Ok(identity)
        }
    }

    /// Load identity from a file (nsec bech32 format).
    pub fn load(path: &PathBuf) -> Result<Self, ConnError> {
        let contents = fs::read_to_string(path)
            .map_err(|e| ConnError::IdentityLoad(e.to_string()))?;

        let nsec = contents.trim();
        Self::from_nsec(nsec)
    }

    /// Save identity to a file (nsec bech32 format).
    pub fn save(&self, path: &PathBuf) -> Result<(), ConnError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| ConnError::IdentitySave(e.to_string()))?;
        }

        let nsec = self.nsec();
        fs::write(path, nsec)
            .map_err(|e| ConnError::IdentitySave(e.to_string()))?;

        // Set restrictive permissions on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            fs::set_permissions(path, perms)
                .map_err(|e| ConnError::IdentitySave(e.to_string()))?;
        }

        Ok(())
    }

    /// Get the secret key.
    pub fn secret_key(&self) -> SecretKey {
        SecretKey::from_keypair(&self.keypair)
    }

    /// Get the public key.
    pub fn public_key(&self) -> PublicKey {
        PublicKey::from_keypair(&self.keypair)
    }

    /// Get the keypair.
    pub fn keypair(&self) -> &Keypair {
        &self.keypair
    }

    /// Get the public key as a 32-byte array (x-only, for Nostr).
    pub fn pubkey_bytes(&self) -> [u8; 32] {
        let pk = self.public_key();
        let serialized = pk.serialize();
        // Skip the first byte (0x02 or 0x03 prefix) to get x-only
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&serialized[1..33]);
        bytes
    }

    /// Get the secret key as a 32-byte array.
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.secret_key().secret_bytes()
    }

    /// Encode public key as npub (bech32).
    pub fn npub(&self) -> String {
        let hrp = Hrp::parse("npub").expect("valid hrp");
        bech32::encode::<Bech32m>(hrp, &self.pubkey_bytes()).expect("valid encoding")
    }

    /// Encode secret key as nsec (bech32).
    pub fn nsec(&self) -> String {
        let hrp = Hrp::parse("nsec").expect("valid hrp");
        bech32::encode::<Bech32m>(hrp, &self.secret_bytes()).expect("valid encoding")
    }

    /// Parse identity from nsec bech32 string.
    pub fn from_nsec(nsec: &str) -> Result<Self, ConnError> {
        let (hrp, data) = bech32::decode(nsec)
            .map_err(|e| ConnError::InvalidIdentity(format!("invalid bech32: {e}")))?;

        if hrp.as_str() != "nsec" {
            return Err(ConnError::InvalidIdentity(
                "expected nsec prefix".to_string(),
            ));
        }

        if data.len() != 32 {
            return Err(ConnError::InvalidIdentity(
                "invalid secret key length".to_string(),
            ));
        }

        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&data);

        let secret_key = SecretKey::from_slice(&bytes)
            .map_err(|e| ConnError::InvalidIdentity(format!("invalid secret key: {e}")))?;

        Ok(Self::from_secret_key(secret_key))
    }

    /// Parse public key from npub bech32 string.
    pub fn pubkey_from_npub(npub: &str) -> Result<[u8; 32], ConnError> {
        let (hrp, data) = bech32::decode(npub)
            .map_err(|e| ConnError::InvalidIdentity(format!("invalid bech32: {e}")))?;

        if hrp.as_str() != "npub" {
            return Err(ConnError::InvalidIdentity(
                "expected npub prefix".to_string(),
            ));
        }

        if data.len() != 32 {
            return Err(ConnError::InvalidIdentity(
                "invalid public key length".to_string(),
            ));
        }

        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&data);
        Ok(bytes)
    }
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("npub", &self.npub())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_identity() {
        let identity = Identity::generate().unwrap();
        let npub = identity.npub();
        let nsec = identity.nsec();

        assert!(npub.starts_with("npub1"));
        assert!(nsec.starts_with("nsec1"));
    }

    #[test]
    fn test_roundtrip_nsec() {
        let identity = Identity::generate().unwrap();
        let nsec = identity.nsec();
        let recovered = Identity::from_nsec(&nsec).unwrap();

        assert_eq!(identity.npub(), recovered.npub());
    }
}
