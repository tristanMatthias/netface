// UDP packet format:
//   [0]    type    : u8  ('V' = video, 'A' = audio, 'C' = config)
//   [1..5] seq     : u32 LE
//   [5..9] len     : u32 LE (payload length)
//   [9..]  payload : [u8]
//
// Config payload:
//   [0..4] width   : u32 LE (desired display width)
//   [4..8] height  : u32 LE (desired display height)

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};

const HDR: usize = 9;
const MAX_UDP: usize = 65507;

pub const PKT_VIDEO: u8 = b'V';
pub const PKT_AUDIO: u8 = b'A';
pub const PKT_CONFIG: u8 = b'C';

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
            if payload.len() + HDR <= MAX_UDP {
                let len = encode(&mut buf, PKT_VIDEO, seq, &payload);
                let _ = sock.send_to(&buf[..len], peer);
                seq = seq.wrapping_add(1);
                active = true;
            }
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

pub fn recv_loop(
    sock: Arc<UdpSocket>,
    vid_tx: Sender<Vec<u8>>,
    aud_tx: Sender<Vec<u8>>,
    peer_connected: Arc<AtomicBool>,
    peer_w: Arc<AtomicU32>,
    peer_h: Arc<AtomicU32>,
) {
    let mut buf = vec![0u8; MAX_UDP];

    loop {
        match sock.recv_from(&mut buf) {
            Ok((len, _)) => {
                if len < HDR {
                    continue;
                }

                let pkt_type = buf[0];
                let payload_len = u32::from_le_bytes([buf[5], buf[6], buf[7], buf[8]]) as usize;
                let end = HDR + payload_len.min(len - HDR);

                match pkt_type {
                    PKT_VIDEO => {
                        peer_connected.store(true, Ordering::Relaxed);
                        // Allocate only for video payloads (unavoidable for channel)
                        let _ = vid_tx.try_send(buf[HDR..end].to_vec());
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
