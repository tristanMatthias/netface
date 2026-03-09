// UDP packet format:
//   [0]    type    : u8  ('V' = video, 'A' = audio, 'C' = config, 'F' = fragment)
//   [1..5] seq     : u32 LE
//   [5..9] len     : u32 LE (payload length)
//   [9..]  payload : [u8]
//
// Config payload:
//   [0..4] width   : u32 LE (desired display width)
//   [4..8] height  : u32 LE (desired display height)
//
// Fragment payload:
//   [0..2] frame_id   : u16 LE (identifies the frame)
//   [2]    frag_idx   : u8 (0-based fragment index)
//   [3]    frag_total : u8 (total number of fragments)
//   [4..]  data       : [u8] (fragment data)

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};

const HDR: usize = 9;
const FRAG_HDR: usize = 4;
const MAX_UDP: usize = 65507;
// Use 1400 bytes per fragment to stay under typical MTU and avoid IP fragmentation
const MAX_FRAG_DATA: usize = 1400;

pub const PKT_VIDEO: u8 = b'V';
pub const PKT_AUDIO: u8 = b'A';
pub const PKT_CONFIG: u8 = b'C';
pub const PKT_FRAGMENT: u8 = b'F';

/// Encode packet header into buffer, returns total length.
#[inline]
fn encode(buf: &mut [u8], pkt_type: u8, seq: u32, payload: &[u8]) -> usize {
    buf[0] = pkt_type;
    buf[1..5].copy_from_slice(&seq.to_le_bytes());
    buf[5..9].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    buf[HDR..HDR + payload.len()].copy_from_slice(payload);
    HDR + payload.len()
}

pub fn send_loop(
    sock: Arc<UdpSocket>,
    peer: SocketAddr,
    vid_rx: Receiver<Vec<u8>>,
    aud_rx: Receiver<Vec<u8>>,
    local_w: u32,
    local_h: u32,
    config_interval: u32,
) {
    let mut seq: u32 = 0;
    let mut buf = vec![0u8; MAX_UDP];
    let mut config_counter: u32 = 0;
    let mut frame_id: u16 = 0;

    // Pre-encode config payload
    let mut cfg = [0u8; 8];
    cfg[0..4].copy_from_slice(&local_w.to_le_bytes());
    cfg[4..8].copy_from_slice(&local_h.to_le_bytes());

    loop {
        let mut active = false;

        // Send config periodically
        config_counter += 1;
        if config_counter >= config_interval {
            config_counter = 0;
            let len = encode(&mut buf, PKT_CONFIG, seq, &cfg);
            let _ = sock.send_to(&buf[..len], peer);
            seq = seq.wrapping_add(1);
        }

        // One video frame per loop iteration
        if let Ok(payload) = vid_rx.try_recv() {
            // Fragment anything larger than MAX_FRAG_DATA to avoid IP fragmentation
            if payload.len() <= MAX_FRAG_DATA {
                let len = encode(&mut buf, PKT_VIDEO, seq, &payload);
                let _ = sock.send_to(&buf[..len], peer);
                seq = seq.wrapping_add(1);
            } else {
                let total_frags = (payload.len() + MAX_FRAG_DATA - 1) / MAX_FRAG_DATA;
                if total_frags <= 255 {
                    for (i, chunk) in payload.chunks(MAX_FRAG_DATA).enumerate() {
                        // Build fragment payload: frame_id + frag_idx + frag_total + data
                        let frag_payload_len = FRAG_HDR + chunk.len();
                        buf[HDR..HDR + 2].copy_from_slice(&frame_id.to_le_bytes());
                        buf[HDR + 2] = i as u8;
                        buf[HDR + 3] = total_frags as u8;
                        buf[HDR + FRAG_HDR..HDR + frag_payload_len].copy_from_slice(chunk);

                        // Encode and send
                        buf[0] = PKT_FRAGMENT;
                        buf[1..5].copy_from_slice(&seq.to_le_bytes());
                        buf[5..9].copy_from_slice(&(frag_payload_len as u32).to_le_bytes());
                        let _ = sock.send_to(&buf[..HDR + frag_payload_len], peer);
                        seq = seq.wrapping_add(1);
                    }
                    frame_id = frame_id.wrapping_add(1);
                }
            }
            active = true;
        }

        // Up to 8 audio chunks per loop
        for _ in 0..8 {
            match aud_rx.try_recv() {
                Ok(payload) if payload.len() + HDR <= MAX_UDP => {
                    let len = encode(&mut buf, PKT_AUDIO, seq, &payload);
                    let _ = sock.send_to(&buf[..len], peer);
                    seq = seq.wrapping_add(1);
                    active = true;
                }
                _ => break,
            }
        }

        if !active {
            thread::sleep(Duration::from_millis(1));
        }
    }
}

/// Reassembly buffer for fragmented frames.
struct FragmentBuffer {
    fragments: HashMap<u16, FrameFragments>,
}

struct FrameFragments {
    total: u8,
    received: Vec<Option<Vec<u8>>>,
    received_count: u8,
    timestamp: Instant,
}

impl FragmentBuffer {
    fn new() -> Self {
        Self {
            fragments: HashMap::new(),
        }
    }

    /// Add a fragment, returns completed frame if all fragments received.
    fn add_fragment(&mut self, frame_id: u16, frag_idx: u8, frag_total: u8, data: &[u8]) -> Option<Vec<u8>> {
        // Clean up old incomplete frames (older than 1 second)
        let now = Instant::now();
        self.fragments.retain(|_, f| now.duration_since(f.timestamp) < Duration::from_secs(1));

        let entry = self.fragments.entry(frame_id).or_insert_with(|| FrameFragments {
            total: frag_total,
            received: vec![None; frag_total as usize],
            received_count: 0,
            timestamp: now,
        });

        // Validate
        if frag_total != entry.total || frag_idx >= frag_total {
            return None;
        }

        // Store fragment if not already received
        let idx = frag_idx as usize;
        if entry.received[idx].is_none() {
            entry.received[idx] = Some(data.to_vec());
            entry.received_count += 1;
        }

        // Check if complete
        if entry.received_count == entry.total {
            // Reassemble
            let mut result = Vec::new();
            for frag in &entry.received {
                if let Some(data) = frag {
                    result.extend_from_slice(data);
                }
            }
            self.fragments.remove(&frame_id);
            Some(result)
        } else {
            None
        }
    }
}

pub fn recv_loop(
    sock: Arc<UdpSocket>,
    vid_tx: Sender<Vec<u8>>,
    aud_tx: Sender<Vec<u8>>,
    peer_connected: Arc<AtomicBool>,
    peer_w: Arc<AtomicU32>,
    peer_h: Arc<AtomicU32>,
) {
    let mut buf = vec![0u8; MAX_UDP];
    let mut frag_buf = FragmentBuffer::new();

    loop {
        match sock.recv_from(&mut buf) {
            Ok((len, _addr)) => {
                if len < HDR {
                    continue;
                }

                let pkt_type = buf[0];
                let payload_len = u32::from_le_bytes([buf[5], buf[6], buf[7], buf[8]]) as usize;
                let end = HDR + payload_len.min(len - HDR);

                match pkt_type {
                    PKT_VIDEO => {
                        peer_connected.store(true, Ordering::Relaxed);
                        let _ = vid_tx.try_send(buf[HDR..end].to_vec());
                    }
                    PKT_FRAGMENT => {
                        peer_connected.store(true, Ordering::Relaxed);
                        if payload_len >= FRAG_HDR {
                            let frame_id = u16::from_le_bytes([buf[HDR], buf[HDR + 1]]);
                            let frag_idx = buf[HDR + 2];
                            let frag_total = buf[HDR + 3];
                            let data = &buf[HDR + FRAG_HDR..end];

                            if let Some(complete) = frag_buf.add_fragment(frame_id, frag_idx, frag_total, data) {
                                let _ = vid_tx.try_send(complete);
                            }
                        }
                    }
                    PKT_AUDIO => {
                        let _ = aud_tx.try_send(buf[HDR..end].to_vec());
                    }
                    PKT_CONFIG => {
                        if end - HDR >= 8 {
                            let w = u32::from_le_bytes([buf[HDR], buf[HDR + 1], buf[HDR + 2], buf[HDR + 3]]);
                            let h = u32::from_le_bytes([buf[HDR + 4], buf[HDR + 5], buf[HDR + 6], buf[HDR + 7]]);
                            peer_w.store(w, Ordering::Relaxed);
                            peer_h.store(h, Ordering::Relaxed);
                            peer_connected.store(true, Ordering::Relaxed);
                        }
                    }
                    _ => {}
                }
            }
            Err(_) => {
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
