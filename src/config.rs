//! Configuration management for netface.
//!
//! Settings are loaded from `~/.config/netface/config.toml` if it exists,
//! with CLI arguments taking precedence.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// All configurable parameters for netface.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    // ─── Video Settings ─────────────────────────────────────────────────────
    /// Target capture FPS.
    pub fps: u64,

    /// Enable ANSI truecolor output.
    pub color: bool,

    /// Terminal character aspect ratio (chars are typically 2x taller than wide).
    pub char_aspect: f32,

    // ─── Background Removal ─────────────────────────────────────────────────
    /// Enable background removal.
    pub bg_removal: bool,

    /// Background color (hex RGB, e.g., "000000" for black).
    pub bg_color: String,

    /// Contrast enhancement factor (1.0 = no change, 1.15 = 15% boost).
    pub contrast: f32,

    /// Contrast midpoint (typically 128 for 8-bit images).
    pub contrast_midpoint: f32,

    /// Morphology kernel radius for erosion/dilation (removes small islands).
    pub morph_radius: i32,

    /// Blur radius for softening mask edges.
    pub blur_radius: usize,

    /// Mask threshold (0.0-1.0, pixels below this are considered background).
    pub mask_threshold: f32,

    // ─── Network Settings ───────────────────────────────────────────────────
    /// Local UDP port to listen on.
    pub port: u16,

    /// Config packet send interval (frames between config packets).
    pub config_interval: u32,

    // ─── Audio Settings ─────────────────────────────────────────────────────
    /// Disable audio.
    pub no_audio: bool,

    /// Audio buffer size in samples.
    pub audio_buffer_size: usize,

    // ─── Camera Settings ────────────────────────────────────────────────────
    /// Webcam device index (0 = default).
    pub camera: usize,

    // ─── Model Settings ─────────────────────────────────────────────────────
    /// Number of threads for ONNX inference.
    pub model_threads: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // Video
            fps: 15,
            color: false,
            char_aspect: 2.0,

            // Background removal
            bg_removal: false,
            bg_color: "000000".to_string(),
            contrast: 1.15,
            contrast_midpoint: 128.0,
            morph_radius: 2,
            blur_radius: 3,
            mask_threshold: 0.5,

            // Network
            port: 4444,
            config_interval: 60,

            // Audio
            no_audio: false,
            audio_buffer_size: 24000,

            // Camera
            camera: 0,

            // Model
            model_threads: 2,
        }
    }
}

impl Config {
    /// Get the config file path.
    pub fn config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join("netface").join("config.toml"))
    }

    /// Load config from file, falling back to defaults.
    pub fn load() -> Self {
        let Some(path) = Self::config_path() else {
            return Self::default();
        };

        if !path.exists() {
            return Self::default();
        }

        match fs::read_to_string(&path) {
            Ok(contents) => match toml::from_str(&contents) {
                Ok(config) => config,
                Err(e) => {
                    eprintln!("Warning: Failed to parse config file: {e}");
                    Self::default()
                }
            },
            Err(e) => {
                eprintln!("Warning: Failed to read config file: {e}");
                Self::default()
            }
        }
    }

    /// Save config to file.
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let Some(path) = Self::config_path() else {
            return Err("Could not determine config directory".into());
        };

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let contents = toml::to_string_pretty(self)?;
        fs::write(&path, contents)?;
        Ok(())
    }

    /// Save default config to file (useful for creating initial config).
    pub fn save_default() -> Result<PathBuf, Box<dyn std::error::Error>> {
        let config = Self::default();
        config.save()?;
        Ok(Self::config_path().unwrap())
    }

    /// Parse hex color string to RGB array.
    pub fn bg_color_rgb(&self) -> [u8; 3] {
        let hex = self.bg_color.trim_start_matches('#');
        if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
            let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
            let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
            [r, g, b]
        } else {
            [0, 0, 0]
        }
    }
}

/// Generate example config file content.
pub fn example_config() -> &'static str {
    r#"# netface configuration
# Place this file at ~/.config/netface/config.toml

# ─── Video Settings ─────────────────────────────────────────────────────────

# Target capture FPS
fps = 15

# Enable ANSI truecolor output (richer colors, larger packets)
color = false

# Terminal character aspect ratio (chars are typically 2x taller than wide)
char_aspect = 2.0

# ─── Background Removal ─────────────────────────────────────────────────────

# Enable background removal (requires more CPU)
bg_removal = false

# Background replacement color (hex RGB)
bg_color = "000000"

# Contrast enhancement factor (1.0 = no change, 1.15 = 15% boost)
contrast = 1.15

# Contrast midpoint (typically 128 for 8-bit images)
contrast_midpoint = 128.0

# Morphology kernel radius for erosion/dilation (removes small islands)
# Higher = more aggressive island removal, but may erode edges
morph_radius = 2

# Blur radius for softening mask edges (higher = softer edges)
blur_radius = 3

# Mask threshold (0.0-1.0, pixels below this confidence are background)
mask_threshold = 0.5

# ─── Network Settings ───────────────────────────────────────────────────────

# Local UDP port to listen on
port = 4444

# Config packet send interval (frames between sending resolution info)
config_interval = 60

# ─── Audio Settings ─────────────────────────────────────────────────────────

# Disable audio
no_audio = false

# Audio buffer size in samples (~0.5s at 48kHz)
audio_buffer_size = 24000

# ─── Camera Settings ────────────────────────────────────────────────────────

# Webcam device index (0 = default camera)
camera = 0

# ─── Model Settings ─────────────────────────────────────────────────────────

# Number of threads for ONNX model inference
model_threads = 2
"#
}
