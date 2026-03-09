//! ICE (Interactive Connectivity Establishment) helpers.
//!
//! Handles STUN binding and ICE candidate gathering.

use std::net::{SocketAddr, ToSocketAddrs};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::timeout;

use crate::conn::error::ConnError;

/// STUN message type for binding request.
const STUN_BINDING_REQUEST: u16 = 0x0001;
/// STUN message type for binding response.
const STUN_BINDING_RESPONSE: u16 = 0x0101;
/// STUN magic cookie.
const STUN_MAGIC_COOKIE: u32 = 0x2112A442;
/// XOR-MAPPED-ADDRESS attribute type.
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
/// MAPPED-ADDRESS attribute type.
const ATTR_MAPPED_ADDRESS: u16 = 0x0001;

/// Perform STUN binding request to discover public address.
pub async fn stun_binding(
    socket: &UdpSocket,
    stun_server: &str,
    request_timeout: Duration,
) -> Result<SocketAddr, ConnError> {
    // Parse STUN server address
    crate::log_debug!("STUN: Parsing server URL: {}", stun_server);
    let stun_addr = parse_stun_url(stun_server)?;
    crate::log_debug!("STUN: Resolved to: {}", stun_addr);

    // Build STUN binding request
    let transaction_id: [u8; 12] = rand::random();
    let request = build_stun_request(&transaction_id);

    // Send request
    crate::log_debug!("STUN: Sending {} byte request to {}", request.len(), stun_addr);
    socket
        .send_to(&request, stun_addr)
        .await
        .map_err(|e| {
            crate::log_error!("STUN: send_to failed: {}", e);
            ConnError::IceGathering(format!("send failed: {}", e))
        })?;
    crate::log_debug!("STUN: Request sent, waiting for response...");

    // Wait for response
    let mut buf = [0u8; 512];
    let (len, from) = timeout(request_timeout, socket.recv_from(&mut buf))
        .await
        .map_err(|_| {
            crate::log_error!("STUN: recv timeout");
            ConnError::IceGathering("STUN timeout".to_string())
        })?
        .map_err(|e| {
            crate::log_error!("STUN: recv_from failed: {}", e);
            ConnError::IceGathering(format!("recv failed: {}", e))
        })?;
    crate::log_debug!("STUN: Received {} bytes from {}", len, from);

    // Parse response
    parse_stun_response(&buf[..len], &transaction_id)
}

/// Parse a STUN URL (stun:host:port or host:port).
fn parse_stun_url(url: &str) -> Result<SocketAddr, ConnError> {
    let host_port = url
        .strip_prefix("stun:")
        .or_else(|| url.strip_prefix("stun://"))
        .unwrap_or(url);

    // Prefer IPv4 addresses to avoid IPv4/IPv6 mismatch issues
    let addrs: Vec<SocketAddr> = host_port
        .to_socket_addrs()
        .map_err(|e| ConnError::IceGathering(format!("invalid STUN server: {e}")))?
        .collect();

    // Try to find an IPv4 address first
    addrs.iter()
        .find(|a| a.is_ipv4())
        .or_else(|| addrs.first())
        .copied()
        .ok_or_else(|| ConnError::IceGathering("could not resolve STUN server".to_string()))
}

/// Build a STUN binding request.
fn build_stun_request(transaction_id: &[u8; 12]) -> Vec<u8> {
    let mut request = Vec::with_capacity(20);

    // Message Type: Binding Request
    request.extend_from_slice(&STUN_BINDING_REQUEST.to_be_bytes());
    // Message Length: 0 (no attributes)
    request.extend_from_slice(&0u16.to_be_bytes());
    // Magic Cookie
    request.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    // Transaction ID
    request.extend_from_slice(transaction_id);

    request
}

/// Parse a STUN binding response.
fn parse_stun_response(data: &[u8], expected_txn_id: &[u8; 12]) -> Result<SocketAddr, ConnError> {
    if data.len() < 20 {
        return Err(ConnError::IceGathering("STUN response too short".to_string()));
    }

    // Check message type
    let msg_type = u16::from_be_bytes([data[0], data[1]]);
    if msg_type != STUN_BINDING_RESPONSE {
        return Err(ConnError::IceGathering(format!(
            "unexpected STUN message type: 0x{:04x}",
            msg_type
        )));
    }

    // Check magic cookie
    let magic = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    if magic != STUN_MAGIC_COOKIE {
        return Err(ConnError::IceGathering("invalid STUN magic cookie".to_string()));
    }

    // Check transaction ID
    if &data[8..20] != expected_txn_id {
        return Err(ConnError::IceGathering("transaction ID mismatch".to_string()));
    }

    // Parse attributes
    let msg_len = u16::from_be_bytes([data[2], data[3]]) as usize;
    let attrs_end = 20 + msg_len.min(data.len() - 20);
    let mut pos = 20;

    while pos + 4 <= attrs_end {
        let attr_type = u16::from_be_bytes([data[pos], data[pos + 1]]);
        let attr_len = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize;
        pos += 4;

        if pos + attr_len > attrs_end {
            break;
        }

        match attr_type {
            ATTR_XOR_MAPPED_ADDRESS => {
                return parse_xor_mapped_address(&data[pos..pos + attr_len]);
            }
            ATTR_MAPPED_ADDRESS => {
                return parse_mapped_address(&data[pos..pos + attr_len]);
            }
            _ => {}
        }

        // Move to next attribute (4-byte aligned)
        pos += (attr_len + 3) & !3;
    }

    Err(ConnError::IceGathering("no mapped address in STUN response".to_string()))
}

/// Parse XOR-MAPPED-ADDRESS attribute.
fn parse_xor_mapped_address(data: &[u8]) -> Result<SocketAddr, ConnError> {
    if data.len() < 8 {
        return Err(ConnError::IceGathering("XOR-MAPPED-ADDRESS too short".to_string()));
    }

    let family = data[1];
    let port = u16::from_be_bytes([data[2], data[3]]) ^ (STUN_MAGIC_COOKIE >> 16) as u16;

    match family {
        0x01 => {
            // IPv4
            let ip_bytes: [u8; 4] = [
                data[4] ^ ((STUN_MAGIC_COOKIE >> 24) as u8),
                data[5] ^ ((STUN_MAGIC_COOKIE >> 16) as u8),
                data[6] ^ ((STUN_MAGIC_COOKIE >> 8) as u8),
                data[7] ^ (STUN_MAGIC_COOKIE as u8),
            ];
            Ok(SocketAddr::new(ip_bytes.into(), port))
        }
        0x02 => {
            // IPv6
            if data.len() < 20 {
                return Err(ConnError::IceGathering("XOR-MAPPED-ADDRESS IPv6 too short".to_string()));
            }
            // IPv6 XOR is more complex, skip for now
            Err(ConnError::IceGathering("IPv6 not yet supported".to_string()))
        }
        _ => Err(ConnError::IceGathering(format!("unknown address family: {}", family))),
    }
}

/// Parse MAPPED-ADDRESS attribute (legacy, non-XOR).
fn parse_mapped_address(data: &[u8]) -> Result<SocketAddr, ConnError> {
    if data.len() < 8 {
        return Err(ConnError::IceGathering("MAPPED-ADDRESS too short".to_string()));
    }

    let family = data[1];
    let port = u16::from_be_bytes([data[2], data[3]]);

    match family {
        0x01 => {
            // IPv4
            let ip_bytes: [u8; 4] = [data[4], data[5], data[6], data[7]];
            Ok(SocketAddr::new(ip_bytes.into(), port))
        }
        _ => Err(ConnError::IceGathering(format!("unknown address family: {}", family))),
    }
}

/// ICE candidate type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateType {
    Host,
    ServerReflexive,
    PeerReflexive,
    Relay,
}

/// An ICE candidate.
#[derive(Debug, Clone)]
pub struct IceCandidate {
    pub foundation: String,
    pub component: u8,
    pub protocol: String,
    pub priority: u32,
    pub address: SocketAddr,
    pub typ: CandidateType,
    pub related_address: Option<SocketAddr>,
}

impl IceCandidate {
    /// Create a host candidate.
    pub fn host(address: SocketAddr) -> Self {
        Self {
            foundation: format!("host-{}", address),
            component: 1,
            protocol: "udp".to_string(),
            priority: 2130706431, // Host candidate priority
            address,
            typ: CandidateType::Host,
            related_address: None,
        }
    }

    /// Create a server-reflexive candidate.
    pub fn srflx(address: SocketAddr, base: SocketAddr) -> Self {
        Self {
            foundation: format!("srflx-{}", address),
            component: 1,
            protocol: "udp".to_string(),
            priority: 1694498815, // SRFLX candidate priority
            address,
            typ: CandidateType::ServerReflexive,
            related_address: Some(base),
        }
    }

    /// Convert to SDP attribute format.
    pub fn to_sdp(&self) -> String {
        let typ_str = match self.typ {
            CandidateType::Host => "host",
            CandidateType::ServerReflexive => "srflx",
            CandidateType::PeerReflexive => "prflx",
            CandidateType::Relay => "relay",
        };

        let mut sdp = format!(
            "candidate:{} {} {} {} {} {} typ {}",
            self.foundation,
            self.component,
            self.protocol,
            self.priority,
            self.address.ip(),
            self.address.port(),
            typ_str
        );

        if let Some(raddr) = self.related_address {
            sdp.push_str(&format!(" raddr {} rport {}", raddr.ip(), raddr.port()));
        }

        sdp
    }
}

/// Gather ICE candidates.
pub async fn gather_candidates(
    socket: &UdpSocket,
    stun_servers: &[String],
    timeout_duration: Duration,
) -> Vec<IceCandidate> {
    let mut candidates = Vec::new();

    // Add host candidate
    if let Ok(local_addr) = socket.local_addr() {
        candidates.push(IceCandidate::host(local_addr));

        // Gather server-reflexive candidates via STUN
        for stun_server in stun_servers {
            if let Ok(srflx_addr) = stun_binding(socket, stun_server, timeout_duration).await {
                if srflx_addr != local_addr {
                    candidates.push(IceCandidate::srflx(srflx_addr, local_addr));
                }
            }
        }
    }

    candidates
}

// Random number generation for transaction IDs
mod rand {
    use std::time::{SystemTime, UNIX_EPOCH};

    pub fn random<T: Default + AsMut<[u8]>>() -> T {
        let mut result = T::default();
        let bytes = result.as_mut();

        // Simple PRNG seeded with time + counter
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        let mut state = seed;
        for byte in bytes.iter_mut() {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            *byte = (state >> 33) as u8;
        }

        result
    }
}
