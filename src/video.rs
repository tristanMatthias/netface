use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use image::imageops;
use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{CameraIndex, RequestedFormat, RequestedFormatType};
use nokhwa::Camera;

const CHARS: &[u8] = b" `.-':_,^=;><+!rc*/z?sLTv)J7(|Fi{C}fI31tlu[neoZ5Yxjya]2ESwqkP6h9d4VpOGbUAKXHm8RD#$Bg0MNWQ%&@";

/// Terminal characters are roughly 2x taller than wide.
const CHAR_ASPECT: f32 = 2.0;

/// Scale image to cover target dimensions (like CSS background-size: cover),
/// accounting for terminal character aspect ratio, then crop center.
fn crop_center(
    img: &image::ImageBuffer<image::Rgb<u8>, Vec<u8>>,
    target_w: u32,
    target_h: u32,
) -> image::ImageBuffer<image::Rgb<u8>, Vec<u8>> {
    let src_w = img.width() as f32;
    let src_h = img.height() as f32;

    // Target in visual pixel space (accounting for char aspect)
    let visual_w = target_w as f32;
    let visual_h = target_h as f32 * CHAR_ASPECT;

    // Scale factor to cover visual target
    let scale = (visual_w / src_w).max(visual_h / src_h);

    let scaled_w = (src_w * scale).round() as u32;
    let scaled_h = (src_h * scale).round() as u32;

    // Scale the image
    let scaled = imageops::resize(img, scaled_w, scaled_h, imageops::FilterType::Nearest);

    // Crop center - but we need target_h rows worth of pixels (which is target_h * CHAR_ASPECT)
    let crop_h_pixels = (target_h as f32 * CHAR_ASPECT).round() as u32;
    let crop_x = (scaled_w.saturating_sub(target_w)) / 2;
    let crop_y = (scaled_h.saturating_sub(crop_h_pixels)) / 2;

    let cropped = imageops::crop_imm(&scaled, crop_x, crop_y, target_w, crop_h_pixels).to_image();

    // Squash vertically to terminal rows
    imageops::resize(&cropped, target_w, target_h, imageops::FilterType::Nearest)
}

/// Raw frame from camera (native resolution).
pub type RawFrame = image::ImageBuffer<image::Rgb<u8>, Vec<u8>>;

// ---------------------------------------------------------------------------
// Capture thread
// ---------------------------------------------------------------------------

/// Grabs webcam frames at native resolution and broadcasts to
/// the compositor and the network encoder.
pub fn capture_thread(
    camera_idx: usize,
    fps: u64,
    display_tx: Sender<RawFrame>, // → compositor
    net_tx: Sender<RawFrame>,     // → network encoder
) {
    let index = CameraIndex::Index(camera_idx as u32);
    let format =
        RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate);

    let mut camera = Camera::new(index, format).expect("failed to open camera");
    camera.open_stream().expect("failed to start camera stream");

    let frame_dur = Duration::from_micros(1_000_000 / fps.max(1));
    let mut last = Instant::now();

    loop {
        let elapsed = last.elapsed();
        if elapsed < frame_dur {
            thread::sleep(frame_dur - elapsed);
        }
        last = Instant::now();

        let raw = match camera.frame() {
            Ok(f) => f,
            Err(e) => {
                eprintln!("camera error: {e}");
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };

        let mut rgb = match raw.decode_image::<RgbFormat>() {
            Ok(img) => img,
            Err(e) => {
                eprintln!("decode error: {e}");
                continue;
            }
        };

        // Flip horizontally (mirror effect).
        imageops::flip_horizontal_in_place(&mut rgb);
        let _ = display_tx.try_send(rgb.clone());
        let _ = net_tx.try_send(rgb);
    }
}

// ---------------------------------------------------------------------------
// Network encoder thread
// ---------------------------------------------------------------------------

/// Converts raw frames → crop center → ASCII → lz4 for transmission.
/// Uses peer's requested dimensions for cropping.
pub fn net_encode_thread(
    frame_rx: Receiver<RawFrame>,
    vid_tx: Sender<Vec<u8>>,
    peer_w: Arc<AtomicU32>,
    peer_h: Arc<AtomicU32>,
    color: bool,
) {
    let mut ascii = String::with_capacity(64 * 1024);

    loop {
        if let Ok(frame) = frame_rx.recv() {
            let w = peer_w.load(Ordering::Relaxed);
            let h = peer_h.load(Ordering::Relaxed);

            // Crop center of camera frame to peer's requested dimensions.
            let cropped = crop_center(&frame, w, h);

            ascii.clear();
            let actual_w = cropped.width();
            let actual_h = cropped.height();
            if color {
                frame_to_color_ascii(&cropped, actual_w, actual_h, &mut ascii);
            } else {
                frame_to_ascii(&cropped, actual_w, actual_h, &mut ascii);
            }
            let compressed = lz4_flex::compress_prepend_size(ascii.as_bytes());
            let _ = vid_tx.try_send(compressed);
        }
    }
}

// ---------------------------------------------------------------------------
// Compositor thread
// ---------------------------------------------------------------------------

/// Renders the terminal display.
///
/// • Solo mode  (peer not yet connected): local cam cropped to full width.
/// • Split mode (peer connected):         local on left half, remote on right.
///
/// Transitions between modes automatically when `peer_connected` flips.
pub fn compositor_thread(
    local_rx: Receiver<RawFrame>,
    remote_rx: Receiver<Vec<u8>>, // lz4-compressed ASCII from peer
    peer_connected: Arc<AtomicBool>,
    half_w: u32,
    height: u32,
    color: bool,
) {
    let full_w = half_w * 2;
    let sep_col = half_w as u16 + 1; // 1-based column of the │ separator
    let remote_col = half_w as u16 + 2; // where the right panel starts

    let mut stdout = std::io::BufWriter::with_capacity(512 * 1024, std::io::stdout());

    let mut local_frame: Option<RawFrame> = None;
    let mut remote_bytes: Option<Vec<u8>> = None; // decompressed ASCII
    let mut prev_connected = false;

    let solo_cap = (full_w as usize + 1) * height as usize * if color { 22 } else { 1 };
    let half_cap = (half_w as usize + 1) * height as usize * if color { 22 } else { 1 };
    let mut ascii_buf = String::with_capacity(solo_cap);

    loop {
        // Block until a new local frame arrives (natural FPS pacing).
        match local_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(f) => local_frame = Some(f),
            Err(_) => {}
        }
        // Drain any extra buffered local frames — keep only the newest.
        while let Ok(f) = local_rx.try_recv() {
            local_frame = Some(f);
        }

        let connected = peer_connected.load(Ordering::Relaxed);

        if connected {
            // Drain remote frames, keep newest.
            while let Ok(p) = remote_rx.try_recv() {
                if let Ok(d) = lz4_flex::decompress_size_prepended(&p) {
                    remote_bytes = Some(d);
                }
            }
        }

        // On state transition: clear screen, redraw chrome.
        if connected != prev_connected {
            write!(stdout, "\x1b[2J").ok();
            if connected {
                draw_separator(&mut stdout, height, sep_col);
            }
            ascii_buf.reserve(solo_cap.max(half_cap));
            prev_connected = connected;
        }

        let Some(ref frame) = local_frame else {
            stdout.flush().ok();
            continue;
        };

        if connected {
            // ── Split mode ──────────────────────────────────────────────
            // Crop local frame to display size.
            let local_cropped = crop_center(frame, half_w, height);
            ascii_buf.clear();
            let w = local_cropped.width();
            let h = local_cropped.height();
            if color {
                frame_to_color_ascii(&local_cropped, w, h, &mut ascii_buf);
            } else {
                frame_to_ascii(&local_cropped, w, h, &mut ascii_buf);
            }
            render_panel(&mut stdout, ascii_buf.as_bytes(), 1, half_w, height);

            if let Some(ref rb) = remote_bytes {
                // Remote video is already sized for us, just render it.
                render_panel(&mut stdout, rb, remote_col, half_w, height);
            }
        } else {
            // ── Solo mode ───────────────────────────────────────────────
            // Crop to fill the whole terminal.
            let full = crop_center(frame, full_w, height);
            ascii_buf.clear();
            let w = full.width();
            let h = full.height();
            if color {
                frame_to_color_ascii(&full, w, h, &mut ascii_buf);
            } else {
                frame_to_ascii(&full, w, h, &mut ascii_buf);
            }
            render_panel(&mut stdout, ascii_buf.as_bytes(), 1, full_w, height);
        }

        stdout.flush().ok();
    }
}

// ---------------------------------------------------------------------------
// Rendering helpers
// ---------------------------------------------------------------------------

/// Write ASCII `bytes` into the terminal panel whose left edge is `start_col`
/// (1-based). Lines are placed row by row starting from row 1.
fn render_panel(out: &mut impl Write, bytes: &[u8], start_col: u16, _width: u32, height: u32) {
    let mut row = 1u16;
    for line in bytes.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        write!(out, "\x1b[{row};{start_col}H").ok();
        out.write_all(line).ok();
        row += 1;
        if row > height as u16 {
            break;
        }
    }
}

/// Draw a dim vertical bar at the given column to separate the two panels.
fn draw_separator(out: &mut impl Write, height: u32, col: u16) {
    write!(out, "\x1b[90m").ok(); // dim gray
    for row in 1..=height as u16 {
        write!(out, "\x1b[{row};{col}H│").ok();
    }
    write!(out, "\x1b[0m").ok();
    out.flush().ok();
}

// ---------------------------------------------------------------------------
// ASCII conversion
// ---------------------------------------------------------------------------

fn frame_to_ascii(
    img: &image::ImageBuffer<image::Rgb<u8>, Vec<u8>>,
    width: u32,
    height: u32,
    out: &mut String,
) {
    let n = CHARS.len() as f32 - 1.0;
    for y in 0..height {
        for x in 0..width {
            let p = img.get_pixel(x, y);
            let lum =
                (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32) / 255.0;
            out.push(CHARS[(lum * n) as usize] as char);
        }
        out.push('\n');
    }
}

fn frame_to_color_ascii(
    img: &image::ImageBuffer<image::Rgb<u8>, Vec<u8>>,
    width: u32,
    height: u32,
    out: &mut String,
) {
    let n = CHARS.len() as f32 - 1.0;
    for y in 0..height {
        for x in 0..width {
            let p = img.get_pixel(x, y);
            let [r, g, b] = p.0;
            let lum =
                (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32) / 255.0;
            out.push_str("\x1b[38;2;");
            push_dec(out, r);
            out.push(';');
            push_dec(out, g);
            out.push(';');
            push_dec(out, b);
            out.push('m');
            out.push(CHARS[(lum * n) as usize] as char);
        }
        out.push_str("\x1b[0m\n");
    }
}

/// Zero-allocation decimal serialisation of a u8.
#[inline]
fn push_dec(s: &mut String, v: u8) {
    if v >= 100 {
        s.push((b'0' + v / 100) as char);
    }
    if v >= 10 {
        s.push((b'0' + (v / 10) % 10) as char);
    }
    s.push((b'0' + v % 10) as char);
}
