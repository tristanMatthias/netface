use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream};

pub fn audio_loop(
    send_tx: Sender<Vec<u8>>,
    recv_rx: Receiver<Vec<u8>>,
    buffer_size: usize,
    audio_muted: Arc<AtomicBool>,
) {
    let host = cpal::default_host();

    // Ring buffer fed by the network receive thread, drained by the output callback.
    let ring: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::with_capacity(buffer_size)));

    // Thread: network bytes → ring buffer
    {
        let ring = ring.clone();
        thread::spawn(move || loop {
            if let Ok(bytes) = recv_rx.recv() {
                let mut buf = ring.lock().unwrap();
                if buf.len() < buffer_size {
                    for chunk in bytes.chunks_exact(4) {
                        buf.push_back(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                    }
                }
                // If buffer is full we silently drop — better than unbounded latency growth
            }
        });
    }

    // --- Input (microphone) ---
    let input_stream: Option<Stream> = host.default_input_device().and_then(|dev| {
        let cfg = dev.default_input_config().ok()?;
        let stream = build_input_stream(&dev, &cfg, send_tx, audio_muted);
        stream.play().ok()?;
        Some(stream)
    });

    // --- Output (speakers) ---
    let output_stream: Option<Stream> = host.default_output_device().and_then(|dev| {
        let cfg = dev.default_output_config().ok()?;
        let stream = build_output_stream(&dev, &cfg, ring);
        stream.play().ok()?;
        Some(stream)
    });

    // Keep streams alive forever (they run via internal callbacks).
    std::mem::forget(input_stream);
    std::mem::forget(output_stream);

    loop {
        thread::sleep(Duration::from_secs(60));
    }
}

// ---------------------------------------------------------------------------
// Stream builders — generic over sample type via runtime dispatch
// ---------------------------------------------------------------------------

fn build_input_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    tx: Sender<Vec<u8>>,
    audio_muted: Arc<AtomicBool>,
) -> Stream {
    let channels = config.channels() as usize;

    match config.sample_format() {
        SampleFormat::F32 => {
            let muted = audio_muted.clone();
            device
                .build_input_stream(
                    &config.config(),
                    move |data: &[f32], _| {
                        if muted.load(Ordering::Relaxed) {
                            return;
                        }
                        let bytes = mono_f32_bytes(data, channels);
                        let _ = tx.try_send(bytes);
                    },
                    |_| {},
                    None,
                )
                .expect("failed to build f32 input stream")
        }

        SampleFormat::I16 => {
            let muted = audio_muted.clone();
            device
                .build_input_stream(
                    &config.config(),
                    move |data: &[i16], _| {
                        if muted.load(Ordering::Relaxed) {
                            return;
                        }
                        let ch = channels.max(1);
                        let bytes: Vec<u8> = data
                            .chunks(ch)
                            .flat_map(|frame| {
                                let s = frame.iter().map(|&v| v as f32 / 32768.0).sum::<f32>()
                                    / ch as f32;
                                s.to_le_bytes()
                            })
                            .collect();
                        let _ = tx.try_send(bytes);
                    },
                    |_| {},
                    None,
                )
                .expect("failed to build i16 input stream")
        }

        SampleFormat::U16 => {
            let muted = audio_muted.clone();
            device
                .build_input_stream(
                    &config.config(),
                    move |data: &[u16], _| {
                        if muted.load(Ordering::Relaxed) {
                            return;
                        }
                        let ch = channels.max(1);
                        let bytes: Vec<u8> = data
                            .chunks(ch)
                            .flat_map(|frame| {
                                let s = frame
                                    .iter()
                                    .map(|&v| v as f32 / u16::MAX as f32 * 2.0 - 1.0)
                                    .sum::<f32>()
                                    / ch as f32;
                                s.to_le_bytes()
                            })
                            .collect();
                        let _ = tx.try_send(bytes);
                    },
                    |_| {},
                    None,
                )
                .expect("failed to build u16 input stream")
        }

        f => panic!("unsupported input sample format: {f:?}"),
    }
}

fn build_output_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    ring: Arc<Mutex<VecDeque<f32>>>,
) -> Stream {
    let channels = config.channels() as usize;

    match config.sample_format() {
        SampleFormat::F32 => device
            .build_output_stream(
                &config.config(),
                move |out: &mut [f32], _| {
                    let mut buf = ring.lock().unwrap();
                    let ch = channels.max(1);
                    for frame in out.chunks_mut(ch) {
                        let s = buf.pop_front().unwrap_or(0.0);
                        for c in frame.iter_mut() {
                            *c = s;
                        }
                    }
                },
                |_| {},
                None,
            )
            .expect("failed to build f32 output stream"),

        SampleFormat::I16 => device
            .build_output_stream(
                &config.config(),
                move |out: &mut [i16], _| {
                    let mut buf = ring.lock().unwrap();
                    let ch = channels.max(1);
                    for frame in out.chunks_mut(ch) {
                        let s = buf.pop_front().unwrap_or(0.0);
                        let s16 = (s * 32767.0).clamp(-32768.0, 32767.0) as i16;
                        for c in frame.iter_mut() {
                            *c = s16;
                        }
                    }
                },
                |_| {},
                None,
            )
            .expect("failed to build i16 output stream"),

        SampleFormat::U16 => device
            .build_output_stream(
                &config.config(),
                move |out: &mut [u16], _| {
                    let mut buf = ring.lock().unwrap();
                    let ch = channels.max(1);
                    for frame in out.chunks_mut(ch) {
                        let s = buf.pop_front().unwrap_or(0.0);
                        let u = ((s + 1.0) / 2.0 * u16::MAX as f32)
                            .clamp(0.0, u16::MAX as f32) as u16;
                        for c in frame.iter_mut() {
                            *c = u;
                        }
                    }
                },
                |_| {},
                None,
            )
            .expect("failed to build u16 output stream"),

        f => panic!("unsupported output sample format: {f:?}"),
    }
}

/// Mix multichannel f32 interleaved data to mono and serialise to bytes.
fn mono_f32_bytes(data: &[f32], channels: usize) -> Vec<u8> {
    let ch = channels.max(1);
    data.chunks(ch)
        .flat_map(|frame| {
            let s = frame.iter().sum::<f32>() / ch as f32;
            s.to_le_bytes()
        })
        .collect()
}
