//! SDP (Session Description Protocol) helpers.
//!
//! Utilities for working with SDP for WebRTC data channel connections.

use std::net::SocketAddr;

use super::ice::IceCandidate;
use crate::conn::error::ConnError;

/// Generate a minimal SDP offer for data channels.
pub fn generate_offer(
    session_id: u64,
    ice_ufrag: &str,
    ice_pwd: &str,
    fingerprint: &str,
    candidates: &[IceCandidate],
) -> String {
    let mut sdp = String::new();

    // Session description
    sdp.push_str("v=0\r\n");
    sdp.push_str(&format!("o=- {} 2 IN IP4 127.0.0.1\r\n", session_id));
    sdp.push_str("s=-\r\n");
    sdp.push_str("t=0 0\r\n");

    // Bundle and RTC-mux
    sdp.push_str("a=group:BUNDLE 0\r\n");
    sdp.push_str("a=extmap-allow-mixed\r\n");
    sdp.push_str("a=msid-semantic: WMS\r\n");

    // Media section for data channels
    sdp.push_str("m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n");
    sdp.push_str("c=IN IP4 0.0.0.0\r\n");

    // ICE credentials
    sdp.push_str(&format!("a=ice-ufrag:{}\r\n", ice_ufrag));
    sdp.push_str(&format!("a=ice-pwd:{}\r\n", ice_pwd));
    sdp.push_str("a=ice-options:trickle\r\n");

    // DTLS fingerprint
    sdp.push_str(&format!("a=fingerprint:sha-256 {}\r\n", fingerprint));
    sdp.push_str("a=setup:actpass\r\n");

    // Mid
    sdp.push_str("a=mid:0\r\n");

    // SCTP
    sdp.push_str("a=sctp-port:5000\r\n");
    sdp.push_str("a=max-message-size:262144\r\n");

    // ICE candidates
    for candidate in candidates {
        sdp.push_str(&format!("a={}\r\n", candidate.to_sdp()));
    }

    sdp
}

/// Generate a minimal SDP answer for data channels.
pub fn generate_answer(
    session_id: u64,
    ice_ufrag: &str,
    ice_pwd: &str,
    fingerprint: &str,
    candidates: &[IceCandidate],
) -> String {
    let mut sdp = String::new();

    // Session description
    sdp.push_str("v=0\r\n");
    sdp.push_str(&format!("o=- {} 2 IN IP4 127.0.0.1\r\n", session_id));
    sdp.push_str("s=-\r\n");
    sdp.push_str("t=0 0\r\n");

    // Bundle and RTC-mux
    sdp.push_str("a=group:BUNDLE 0\r\n");
    sdp.push_str("a=extmap-allow-mixed\r\n");
    sdp.push_str("a=msid-semantic: WMS\r\n");

    // Media section for data channels
    sdp.push_str("m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n");
    sdp.push_str("c=IN IP4 0.0.0.0\r\n");

    // ICE credentials
    sdp.push_str(&format!("a=ice-ufrag:{}\r\n", ice_ufrag));
    sdp.push_str(&format!("a=ice-pwd:{}\r\n", ice_pwd));
    sdp.push_str("a=ice-options:trickle\r\n");

    // DTLS fingerprint (passive role for answer)
    sdp.push_str(&format!("a=fingerprint:sha-256 {}\r\n", fingerprint));
    sdp.push_str("a=setup:active\r\n");

    // Mid
    sdp.push_str("a=mid:0\r\n");

    // SCTP
    sdp.push_str("a=sctp-port:5000\r\n");
    sdp.push_str("a=max-message-size:262144\r\n");

    // ICE candidates
    for candidate in candidates {
        sdp.push_str(&format!("a={}\r\n", candidate.to_sdp()));
    }

    sdp
}

/// Parse ICE credentials from SDP.
pub fn parse_ice_credentials(sdp: &str) -> Result<(String, String), ConnError> {
    let mut ufrag = None;
    let mut pwd = None;

    for line in sdp.lines() {
        if let Some(value) = line.strip_prefix("a=ice-ufrag:") {
            ufrag = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("a=ice-pwd:") {
            pwd = Some(value.trim().to_string());
        }
    }

    match (ufrag, pwd) {
        (Some(u), Some(p)) => Ok((u, p)),
        _ => Err(ConnError::SdpParse("missing ICE credentials".to_string())),
    }
}

/// Parse fingerprint from SDP.
pub fn parse_fingerprint(sdp: &str) -> Result<String, ConnError> {
    for line in sdp.lines() {
        if let Some(value) = line.strip_prefix("a=fingerprint:sha-256 ") {
            return Ok(value.trim().to_string());
        }
    }
    Err(ConnError::SdpParse("missing fingerprint".to_string()))
}

/// Parse ICE candidates from SDP.
pub fn parse_candidates(sdp: &str) -> Vec<String> {
    sdp.lines()
        .filter_map(|line| {
            if line.starts_with("a=candidate:") {
                Some(line.strip_prefix("a=").unwrap().to_string())
            } else {
                None
            }
        })
        .collect()
}

/// Generate random ICE credentials.
pub fn generate_ice_credentials() -> (String, String) {
    use std::time::{SystemTime, UNIX_EPOCH};

    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    let ufrag = format!("{:x}", (seed & 0xFFFFFFFF) as u32);
    let pwd = format!("{:x}{:x}", (seed >> 32) as u64, (seed & 0xFFFFFFFF) as u64);

    (ufrag, pwd)
}

/// Generate session ID.
pub fn generate_session_id() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
