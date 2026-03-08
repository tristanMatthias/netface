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

fn encode(buf: &mut Vec<u8>, pkt_type: u8, seq: u32, payload: &[u8]) {
    buf.clear();
    buf.push(pkt_type);
    buf.extend_from_slice(&seq.to_le_bytes());
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
}

pub fn send_loop(
    sock: Arc<UdpSocket>,
    peer: SocketAddr,
    vid_rx: Receiver<Vec<u8>>,
    aud_rx: Receiver<Vec<u8>>,
    local_w: u32,
    local_h: u32,
) {
    let mut seq: u32 = 0;
    let mut buf = Vec::with_capacity(MAX_UDP);
    let mut config_counter: u32 = 0;

    loop {
        let mut active = false;

        // Send config every ~60 iterations (~1/sec at 60fps)
        config_counter += 1;
        if config_counter >= 60 {
            config_counter = 0;
            let mut cfg = Vec::with_capacity(8);
            cfg.extend_from_slice(&local_w.to_le_bytes());
            cfg.extend_from_slice(&local_h.to_le_bytes());
            encode(&mut buf, PKT_CONFIG, seq, &cfg);
            let _ = sock.send_to(&buf, peer);
            seq = seq.wrapping_add(1);
        }

        // One video frame per loop iteration
        if let Ok(payload) = vid_rx.try_recv() {
            if payload.len() + HDR <= MAX_UDP {
                encode(&mut buf, PKT_VIDEO, seq, &payload);
                let _ = sock.send_to(&buf, peer);
                seq = seq.wrapping_add(1);
                active = true;
            }
        }

        // Up to 8 audio chunks per loop
        for _ in 0..8 {
            match aud_rx.try_recv() {
                Ok(payload) if payload.len() + HDR <= MAX_UDP => {
                    encode(&mut buf, PKT_AUDIO, seq, &payload);
                    let _ = sock.send_to(&buf, peer);
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
                let payload_len =
                    u32::from_le_bytes([buf[5], buf[6], buf[7], buf[8]]) as usize;
                let end = HDR + payload_len.min(len - HDR);
                let payload = buf[HDR..end].to_vec();

                match pkt_type {
                    PKT_VIDEO => {
                        // Signal the compositor to switch to split mode.
                        peer_connected.store(true, Ordering::Relaxed);
                        let _ = vid_tx.try_send(payload);
                    }
                    PKT_AUDIO => {
                        let _ = aud_tx.try_send(payload);
                    }
                    PKT_CONFIG => {
                        if payload.len() >= 8 {
                            let w = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                            let h = u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]);
                            peer_w.store(w, Ordering::Relaxed);
                            peer_h.store(h, Ordering::Relaxed);
                            peer_connected.store(true, Ordering::Relaxed);
                        }
                    }
                    _ => {}
                }
            }
            Err(e) => {
                eprintln!("recv error: {e}");
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
