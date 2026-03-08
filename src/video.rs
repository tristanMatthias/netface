use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use image::imageops::{self, FilterType};
use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{CameraIndex, RequestedFormat, RequestedFormatType};
use nokhwa::Camera;
use ort::session::Session;

use crate::config::Config;
use crate::theme::ThemeRenderer;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// MediaPipe model input size.
const MODEL_SIZE: u32 = 256;

/// Embedded MediaPipe Selfie Segmentation model (~450KB).
const SELFIE_SEG_MODEL: &[u8] = include_bytes!("../models/selfie_seg.onnx");

// ---------------------------------------------------------------------------
// Video processing configuration
// ---------------------------------------------------------------------------

/// Runtime configuration for video processing.
#[derive(Debug, Clone, Copy)]
pub struct VideoConfig {
    /// Terminal character aspect ratio.
    pub char_aspect: f32,
    /// Contrast enhancement factor.
    pub contrast: f32,
    /// Contrast midpoint.
    pub contrast_midpoint: f32,
    /// Morphology kernel radius.
    pub morph_radius: i32,
    /// Blur radius for mask edges.
    pub blur_radius: usize,
    /// Mask threshold (0.0-1.0).
    pub mask_threshold: f32,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            char_aspect: 2.0,
            contrast: 1.15,
            contrast_midpoint: 128.0,
            morph_radius: 2,
            blur_radius: 3,
            mask_threshold: 0.5,
        }
    }
}

impl From<&Config> for VideoConfig {
    fn from(cfg: &Config) -> Self {
        Self {
            char_aspect: cfg.char_aspect,
            contrast: cfg.contrast,
            contrast_midpoint: cfg.contrast_midpoint,
            morph_radius: cfg.morph_radius,
            blur_radius: cfg.blur_radius,
            mask_threshold: cfg.mask_threshold,
        }
    }
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Raw frame from camera (native resolution).
pub type RawFrame = image::ImageBuffer<image::Rgb<u8>, Vec<u8>>;

/// Reusable processing buffers to avoid per-frame allocations.
pub struct ProcessingBuffers {
    /// Input tensor for ONNX model [1, 3, 256, 256].
    tensor_data: Vec<f32>,
    /// Mask at model resolution (256x256).
    mask_256: Vec<u8>,
    /// Mask resized to target dimensions.
    mask_resized: Vec<u8>,
    /// Mask after erosion.
    mask_eroded: Vec<u8>,
    /// Mask after dilation.
    mask_dilated: Vec<u8>,
    /// Final blurred mask.
    mask_final: Vec<u8>,
    /// Resized input for model.
    input_resized: Vec<u8>,
    /// Output image buffer.
    output: Vec<u8>,
    /// Last known dimensions for resized buffers.
    last_w: u32,
    last_h: u32,
}

impl ProcessingBuffers {
    pub fn new() -> Self {
        Self {
            tensor_data: vec![0.0f32; 3 * (MODEL_SIZE as usize) * (MODEL_SIZE as usize)],
            mask_256: vec![0u8; (MODEL_SIZE as usize) * (MODEL_SIZE as usize)],
            input_resized: vec![0u8; 3 * (MODEL_SIZE as usize) * (MODEL_SIZE as usize)],
            mask_resized: Vec::new(),
            mask_eroded: Vec::new(),
            mask_dilated: Vec::new(),
            mask_final: Vec::new(),
            output: Vec::new(),
            last_w: 0,
            last_h: 0,
        }
    }

    /// Ensure buffers are sized for target dimensions.
    #[inline]
    fn ensure_size(&mut self, w: u32, h: u32) {
        if self.last_w != w || self.last_h != h {
            let size = (w as usize) * (h as usize);
            self.mask_resized.resize(size, 0);
            self.mask_eroded.resize(size, 0);
            self.mask_dilated.resize(size, 0);
            self.mask_final.resize(size, 0);
            self.output.resize(size * 3, 0);
            self.last_w = w;
            self.last_h = h;
        }
    }
}

// ---------------------------------------------------------------------------
// Background removal
// ---------------------------------------------------------------------------

/// Load the embedded MediaPipe Selfie Segmentation model.
pub fn load_bg_session(threads: usize) -> ort::Result<Session> {
    Session::builder()?
        .with_intra_threads(threads)?
        .commit_from_memory(SELFIE_SEG_MODEL)
}

/// Remove background from image using MediaPipe Selfie Segmentation.
/// Uses pre-allocated buffers for performance.
#[inline(never)]
pub fn remove_background(
    img: &RawFrame,
    session: &Mutex<Session>,
    bg_color: [u8; 3],
    buffers: &mut ProcessingBuffers,
    cfg: &VideoConfig,
) -> RawFrame {
    let (w, h) = (img.width(), img.height());
    buffers.ensure_size(w, h);

    let src = img.as_raw();

    // 1. Resize to 256x256 using fast nearest-neighbor
    resize_rgb_nearest(src, w, h, &mut buffers.input_resized, MODEL_SIZE, MODEL_SIZE);

    // 2. Convert to normalized NCHW tensor
    rgb_to_nchw(&buffers.input_resized, MODEL_SIZE, &mut buffers.tensor_data);

    // 3. Run inference
    let input_tensor = match ort::value::Tensor::from_array((
        [1usize, 3, MODEL_SIZE as usize, MODEL_SIZE as usize],
        buffers.tensor_data.clone(), // TODO: avoid this clone with better ort API
    )) {
        Ok(t) => t,
        Err(_) => return img.clone(),
    };

    let mut guard = match session.lock() {
        Ok(g) => g,
        Err(_) => return img.clone(),
    };

    let outputs = match guard.run(ort::inputs![input_tensor]) {
        Ok(o) => o,
        Err(_) => return img.clone(),
    };

    // 4. Extract mask
    let (_, mask_data) = match outputs[0].try_extract_tensor::<f32>() {
        Ok(m) => m,
        Err(_) => return img.clone(),
    };

    // 5. Convert mask to u8 with threshold (256x256)
    let threshold = cfg.mask_threshold;
    for (i, &val) in mask_data.iter().take(buffers.mask_256.len()).enumerate() {
        // Apply threshold: values above threshold become foreground (255), below become background (0)
        // Then scale by the confidence for soft edges
        let clamped = val.clamp(0.0, 1.0);
        let thresholded = if clamped >= threshold {
            // Remap [threshold, 1.0] to [0.0, 1.0] for soft edges above threshold
            ((clamped - threshold) / (1.0 - threshold)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        buffers.mask_256[i] = (thresholded * 255.0) as u8;
    }

    // 6. Resize mask to target dimensions
    resize_gray_nearest(&buffers.mask_256, MODEL_SIZE, MODEL_SIZE, &mut buffers.mask_resized, w, h);

    // 7. Morphological opening: erode then dilate (removes islands)
    erode(&buffers.mask_resized, &mut buffers.mask_eroded, w, h, cfg.morph_radius);
    dilate(&buffers.mask_eroded, &mut buffers.mask_dilated, w, h, cfg.morph_radius);

    // 8. Box blur for soft edges
    box_blur(&buffers.mask_dilated, &mut buffers.mask_final, w, h, cfg.blur_radius);

    // 9. Composite with contrast enhancement
    composite_with_contrast(
        src,
        &buffers.mask_final,
        &mut buffers.output,
        w,
        h,
        bg_color,
        cfg.contrast,
        cfg.contrast_midpoint,
    );

    // Create output image from buffer
    RawFrame::from_raw(w, h, buffers.output.clone()).unwrap_or_else(|| img.clone())
}

// ---------------------------------------------------------------------------
// Fast image processing primitives
// ---------------------------------------------------------------------------

/// Fast nearest-neighbor resize for RGB images.
#[inline]
fn resize_rgb_nearest(src: &[u8], sw: u32, sh: u32, dst: &mut [u8], dw: u32, dh: u32) {
    let sw = sw as usize;
    let sh = sh as usize;
    let dw = dw as usize;
    let dh = dh as usize;

    for dy in 0..dh {
        let sy = (dy * sh) / dh;
        let src_row = sy * sw * 3;
        let dst_row = dy * dw * 3;

        for dx in 0..dw {
            let sx = (dx * sw) / dw;
            let si = src_row + sx * 3;
            let di = dst_row + dx * 3;
            dst[di] = src[si];
            dst[di + 1] = src[si + 1];
            dst[di + 2] = src[si + 2];
        }
    }
}

/// Fast nearest-neighbor resize for grayscale images.
#[inline]
fn resize_gray_nearest(src: &[u8], sw: u32, sh: u32, dst: &mut [u8], dw: u32, dh: u32) {
    let sw = sw as usize;
    let sh = sh as usize;
    let dw = dw as usize;
    let dh = dh as usize;

    for dy in 0..dh {
        let sy = (dy * sh) / dh;
        let dst_row = dy * dw;
        let src_row = sy * sw;

        for dx in 0..dw {
            let sx = (dx * sw) / dw;
            dst[dst_row + dx] = src[src_row + sx];
        }
    }
}

/// Convert RGB bytes to normalized NCHW tensor.
#[inline]
fn rgb_to_nchw(rgb: &[u8], size: u32, out: &mut [f32]) {
    let size = size as usize;
    let plane = size * size;
    let inv = 1.0 / 255.0;

    for y in 0..size {
        for x in 0..size {
            let i = (y * size + x) * 3;
            let pi = y * size + x;
            out[pi] = rgb[i] as f32 * inv;
            out[plane + pi] = rgb[i + 1] as f32 * inv;
            out[2 * plane + pi] = rgb[i + 2] as f32 * inv;
        }
    }
}

/// Fast morphological erosion (min filter).
#[inline]
fn erode(src: &[u8], dst: &mut [u8], w: u32, h: u32, r: i32) {
    let w = w as usize;
    let h = h as usize;
    let r = r as usize;

    // Copy edges unchanged
    dst.copy_from_slice(src);

    for y in r..(h - r) {
        for x in r..(w - r) {
            let mut min = 255u8;
            for ky in 0..=(r * 2) {
                let row = (y + ky - r) * w;
                for kx in 0..=(r * 2) {
                    min = min.min(src[row + x + kx - r]);
                }
            }
            dst[y * w + x] = min;
        }
    }
}

/// Fast morphological dilation (max filter).
#[inline]
fn dilate(src: &[u8], dst: &mut [u8], w: u32, h: u32, r: i32) {
    let w = w as usize;
    let h = h as usize;
    let r = r as usize;

    dst.copy_from_slice(src);

    for y in r..(h - r) {
        for x in r..(w - r) {
            let mut max = 0u8;
            for ky in 0..=(r * 2) {
                let row = (y + ky - r) * w;
                for kx in 0..=(r * 2) {
                    max = max.max(src[row + x + kx - r]);
                }
            }
            dst[y * w + x] = max;
        }
    }
}

/// Fast box blur (separable).
#[inline]
fn box_blur(src: &[u8], dst: &mut [u8], w: u32, h: u32, r: usize) {
    let w = w as usize;
    let h = h as usize;

    // Simple box blur (not separable for simplicity, but still fast)
    for y in 0..h {
        for x in 0..w {
            let mut sum = 0u32;
            let mut count = 0u32;

            let y_start = y.saturating_sub(r);
            let y_end = (y + r + 1).min(h);
            let x_start = x.saturating_sub(r);
            let x_end = (x + r + 1).min(w);

            for ky in y_start..y_end {
                let row = ky * w;
                for kx in x_start..x_end {
                    sum += src[row + kx] as u32;
                    count += 1;
                }
            }

            dst[y * w + x] = (sum / count) as u8;
        }
    }
}

/// Composite foreground with background using mask and contrast enhancement.
#[inline]
fn composite_with_contrast(
    src: &[u8],
    mask: &[u8],
    dst: &mut [u8],
    w: u32,
    h: u32,
    bg: [u8; 3],
    contrast: f32,
    mid: f32,
) {
    let w = w as usize;
    let h = h as usize;
    let bg_f = [bg[0] as f32, bg[1] as f32, bg[2] as f32];

    for y in 0..h {
        let row = y * w;
        for x in 0..w {
            let i = row + x;
            let si = i * 3;
            let alpha = mask[i] as f32 * (1.0 / 255.0);
            let inv_alpha = 1.0 - alpha;

            // Contrast-enhanced foreground
            let r = ((src[si] as f32 - mid) * contrast + mid).clamp(0.0, 255.0);
            let g = ((src[si + 1] as f32 - mid) * contrast + mid).clamp(0.0, 255.0);
            let b = ((src[si + 2] as f32 - mid) * contrast + mid).clamp(0.0, 255.0);

            // Alpha blend
            dst[si] = (r * alpha + bg_f[0] * inv_alpha) as u8;
            dst[si + 1] = (g * alpha + bg_f[1] * inv_alpha) as u8;
            dst[si + 2] = (b * alpha + bg_f[2] * inv_alpha) as u8;
        }
    }
}

// ---------------------------------------------------------------------------
// Image scaling
// ---------------------------------------------------------------------------

/// Scale image to cover target dimensions (like CSS background-size: cover),
/// accounting for terminal character aspect ratio, then crop center.
pub fn crop_center(img: &RawFrame, target_w: u32, target_h: u32, char_aspect: f32) -> RawFrame {
    let src_w = img.width() as f32;
    let src_h = img.height() as f32;

    let visual_h = target_h as f32 * char_aspect;
    let scale = (target_w as f32 / src_w).max(visual_h / src_h);

    let scaled_w = (src_w * scale).round() as u32;
    let scaled_h = (src_h * scale).round() as u32;

    // Scale the image
    let scaled = imageops::resize(img, scaled_w, scaled_h, FilterType::Nearest);

    // Crop center
    let crop_h_pixels = (target_h as f32 * char_aspect).round() as u32;
    let crop_x = scaled_w.saturating_sub(target_w) / 2;
    let crop_y = scaled_h.saturating_sub(crop_h_pixels) / 2;

    let cropped = imageops::crop_imm(&scaled, crop_x, crop_y, target_w, crop_h_pixels).to_image();

    // Squash vertically to terminal rows
    imageops::resize(&cropped, target_w, target_h, FilterType::Nearest)
}

// ---------------------------------------------------------------------------
// Capture thread
// ---------------------------------------------------------------------------

/// Grabs webcam frames at native resolution and broadcasts to
/// the compositor and the network encoder.
pub fn capture_thread(
    camera_idx: usize,
    fps: u64,
    display_tx: Sender<RawFrame>,
    net_tx: Sender<RawFrame>,
) {
    let index = CameraIndex::Index(camera_idx as u32);
    let format = RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate);

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
            Err(_) => {
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };

        let mut rgb = match raw.decode_image::<RgbFormat>() {
            Ok(img) => img,
            Err(_) => continue,
        };

        imageops::flip_horizontal_in_place(&mut rgb);
        let _ = display_tx.try_send(rgb.clone());
        let _ = net_tx.try_send(rgb);
    }
}

// ---------------------------------------------------------------------------
// Network encoder thread
// ---------------------------------------------------------------------------

/// Converts raw frames → crop → bg removal → ASCII → lz4.
pub fn net_encode_thread(
    frame_rx: Receiver<RawFrame>,
    vid_tx: Sender<Vec<u8>>,
    peer_w: Arc<AtomicU32>,
    peer_h: Arc<AtomicU32>,
    color: bool,
    bg_session: Option<Arc<Mutex<Session>>>,
    bg_color: [u8; 3],
    cfg: VideoConfig,
    renderer: Arc<ThemeRenderer>,
) {
    let mut ascii = Vec::with_capacity(128 * 1024);
    let mut buffers = ProcessingBuffers::new();

    loop {
        let frame = match frame_rx.recv() {
            Ok(f) => f,
            Err(_) => continue,
        };

        let w = peer_w.load(Ordering::Relaxed);
        let h = peer_h.load(Ordering::Relaxed);

        let cropped = crop_center(&frame, w, h, cfg.char_aspect);

        let processed = match bg_session {
            Some(ref session) => remove_background(&cropped, session, bg_color, &mut buffers, &cfg),
            None => cropped,
        };

        ascii.clear();
        if color {
            renderer.render_frame(processed.as_raw(), processed.width() as usize, processed.height() as usize, &mut ascii);
        } else {
            renderer.render_frame_mono(processed.as_raw(), processed.width() as usize, processed.height() as usize, &mut ascii);
        }

        let compressed = lz4_flex::compress_prepend_size(&ascii);
        let _ = vid_tx.try_send(compressed);
    }
}

// ---------------------------------------------------------------------------
// Compositor thread
// ---------------------------------------------------------------------------

/// Renders the terminal display.
pub fn compositor_thread(
    local_rx: Receiver<RawFrame>,
    remote_rx: Receiver<Vec<u8>>,
    peer_connected: Arc<AtomicBool>,
    half_w: u32,
    height: u32,
    color: bool,
    bg_session: Option<Arc<Mutex<Session>>>,
    bg_color: [u8; 3],
    cfg: VideoConfig,
    renderer: Arc<ThemeRenderer>,
) {
    use std::io::Write;

    let full_w = half_w * 2;
    let sep_col = half_w as u16 + 1;
    let remote_col = half_w as u16 + 2;

    let mut stdout = std::io::BufWriter::with_capacity(512 * 1024, std::io::stdout());
    let mut local_frame: Option<RawFrame> = None;
    let mut remote_bytes: Option<Vec<u8>> = None;
    let mut prev_connected = false;
    let mut ascii_buf = Vec::with_capacity(256 * 1024);
    let mut buffers = ProcessingBuffers::new();

    loop {
        match local_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(f) => local_frame = Some(f),
            Err(_) => {}
        }
        while let Ok(f) = local_rx.try_recv() {
            local_frame = Some(f);
        }

        let connected = peer_connected.load(Ordering::Relaxed);

        if connected {
            while let Ok(p) = remote_rx.try_recv() {
                if let Ok(d) = lz4_flex::decompress_size_prepended(&p) {
                    remote_bytes = Some(d);
                }
            }
        }

        if connected != prev_connected {
            write!(stdout, "\x1b[2J").ok();
            if connected {
                draw_separator(&mut stdout, height, sep_col);
            }
            prev_connected = connected;
        }

        let Some(ref frame) = local_frame else {
            stdout.flush().ok();
            continue;
        };

        if connected {
            let cropped = crop_center(frame, half_w, height, cfg.char_aspect);
            let processed = match bg_session {
                Some(ref session) => remove_background(&cropped, session, bg_color, &mut buffers, &cfg),
                None => cropped,
            };

            ascii_buf.clear();
            if color {
                renderer.render_frame(processed.as_raw(), processed.width() as usize, processed.height() as usize, &mut ascii_buf);
            } else {
                renderer.render_frame_mono(processed.as_raw(), processed.width() as usize, processed.height() as usize, &mut ascii_buf);
            }
            render_panel(&mut stdout, &ascii_buf, 1, height);

            if let Some(ref rb) = remote_bytes {
                render_panel(&mut stdout, rb, remote_col, height);
            }
        } else {
            let full = crop_center(frame, full_w, height, cfg.char_aspect);
            let processed = match bg_session {
                Some(ref session) => remove_background(&full, session, bg_color, &mut buffers, &cfg),
                None => full,
            };

            ascii_buf.clear();
            if color {
                renderer.render_frame(processed.as_raw(), processed.width() as usize, processed.height() as usize, &mut ascii_buf);
            } else {
                renderer.render_frame_mono(processed.as_raw(), processed.width() as usize, processed.height() as usize, &mut ascii_buf);
            }
            render_panel(&mut stdout, &ascii_buf, 1, height);
        }

        stdout.flush().ok();
    }
}

// ---------------------------------------------------------------------------
// Rendering helpers
// ---------------------------------------------------------------------------

/// Write ASCII bytes into terminal panel.
#[inline]
fn render_panel(out: &mut impl std::io::Write, bytes: &[u8], start_col: u16, height: u32) {
    let mut row = 1u16;
    for line in bytes.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let _ = write!(out, "\x1b[{row};{start_col}H");
        let _ = out.write_all(line);
        row += 1;
        if row > height as u16 {
            break;
        }
    }
}

/// Draw separator line.
fn draw_separator(out: &mut impl std::io::Write, height: u32, col: u16) {
    let _ = write!(out, "\x1b[90m");
    for row in 1..=height as u16 {
        let _ = write!(out, "\x1b[{row};{col}H│");
    }
    let _ = write!(out, "\x1b[0m");
    let _ = out.flush();
}

