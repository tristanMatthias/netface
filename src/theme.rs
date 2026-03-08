//! Theming system for ASCII rendering.
//!
//! Supports character ramps (ASCII, Unicode blocks, emoji) and
//! various color modes (original, monochrome, gradients, tints).

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Color Mode
// ---------------------------------------------------------------------------

/// How colors are applied to the ASCII output.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ColorMode {
    /// Use the pixel's actual RGB color.
    Original,
    /// Single color for all pixels.
    Monochrome { color: [u8; 3] },
    /// Interpolate between two colors based on luminance.
    Gradient { dark: [u8; 3], light: [u8; 3] },
    /// Map luminance to a palette of N colors.
    Palette { colors: Vec<[u8; 3]> },
    /// Apply HSV transform to original colors.
    Tint { hue_shift: f32, saturation: f32 },
}

impl Default for ColorMode {
    fn default() -> Self {
        ColorMode::Original
    }
}

// ---------------------------------------------------------------------------
// Theme Definition
// ---------------------------------------------------------------------------

/// A complete theme with character ramp and color mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    /// Characters from dark to light (can be ASCII or emoji).
    pub chars: String,
    pub color_mode: ColorMode,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            name: "classic".to_string(),
            chars: " .:-=+*#%@".to_string(),
            color_mode: ColorMode::Original,
        }
    }
}

// ---------------------------------------------------------------------------
// Built-in Character Ramps
// ---------------------------------------------------------------------------

/// Get built-in character ramp by name.
pub fn get_char_ramp(name: &str) -> Option<&'static str> {
    match name {
        "classic" => Some(" .:-=+*#%@"),
        "detailed" => Some(" `.-':_,^=;><+!rc*/z?sLTv)J7(|Fi{C}fI31tlu[neoZ5Yxjya]2ESwqkP6h9d4VpOGbUAKXHm8RD#$Bg0MNWQ%&@"),
        "blocks" => Some(" ░▒▓█"),
        "dots" => Some("⠀⠄⠆⠖⠶⡶⣶⣿"),
        "emoji-moon" => Some("🌑🌒🌓🌔🌕"),
        "emoji-hearts" => Some("🖤💜💙💚💛🧡❤️"),
        "emoji-fire" => Some("⬛🟫🟠🟡⬜"),
        _ => None,
    }
}

/// List all built-in character ramp names.
pub fn list_char_ramps() -> &'static [&'static str] {
    &[
        "classic",
        "detailed",
        "blocks",
        "dots",
        "emoji-moon",
        "emoji-hearts",
        "emoji-fire",
    ]
}

// ---------------------------------------------------------------------------
// Built-in Color Modes
// ---------------------------------------------------------------------------

/// Get built-in color mode by name.
pub fn get_color_mode(name: &str) -> Option<ColorMode> {
    match name {
        "original" => Some(ColorMode::Original),
        "mono-white" => Some(ColorMode::Monochrome { color: [255, 255, 255] }),
        "mono-green" => Some(ColorMode::Monochrome { color: [0, 255, 0] }),
        "mono-amber" => Some(ColorMode::Monochrome { color: [255, 176, 0] }),
        "matrix" => Some(ColorMode::Gradient {
            dark: [0, 20, 0],
            light: [0, 255, 0],
        }),
        "sunset" => Some(ColorMode::Gradient {
            dark: [75, 0, 130],
            light: [255, 200, 0],
        }),
        "cyberpunk" => Some(ColorMode::Gradient {
            dark: [255, 0, 128],
            light: [0, 255, 255],
        }),
        "ice" => Some(ColorMode::Gradient {
            dark: [0, 0, 64],
            light: [200, 240, 255],
        }),
        "sepia" => Some(ColorMode::Tint {
            hue_shift: 30.0,
            saturation: 0.4,
        }),
        "cool" => Some(ColorMode::Tint {
            hue_shift: 200.0,
            saturation: 0.6,
        }),
        _ => None,
    }
}

/// List all built-in color mode names.
pub fn list_color_modes() -> &'static [&'static str] {
    &[
        "original",
        "mono-white",
        "mono-green",
        "mono-amber",
        "matrix",
        "sunset",
        "cyberpunk",
        "ice",
        "sepia",
        "cool",
    ]
}

// ---------------------------------------------------------------------------
// Theme Renderer
// ---------------------------------------------------------------------------

/// Pre-computed renderer for a theme.
pub struct ThemeRenderer {
    /// Characters as UTF-8 byte sequences (supports multi-byte emoji).
    chars: Vec<Vec<u8>>,
    /// Luminance (0-255) to character index lookup.
    lum_to_idx: [u8; 256],
    /// Color mode for rendering.
    color_mode: ColorMode,
    /// Pre-computed colors for gradient/palette modes [256 entries].
    lum_to_color: [[u8; 3]; 256],
    /// Pre-computed ANSI color sequences for luminance-based modes [256 entries].
    /// Empty for Original/Tint modes (computed per-pixel).
    lum_to_ansi: Vec<Vec<u8>>,
    /// Character width in terminal columns (1 for ASCII, 2 for emoji/wide chars).
    char_width: usize,
    /// Whether we can use pre-computed ANSI sequences.
    use_lum_ansi: bool,
}

impl ThemeRenderer {
    /// Create a new renderer from a theme.
    pub fn new(theme: &Theme) -> Self {
        // Parse characters - each grapheme cluster becomes one "character"
        let mut chars: Vec<Vec<u8>> = theme
            .chars
            .chars()
            .map(|c| c.to_string().into_bytes())
            .collect();

        // Ensure we have at least one character
        if chars.is_empty() {
            chars.push(vec![b' ']);
        }

        // Detect if theme uses wide characters (emoji, CJK, etc.)
        // Wide chars are typically multi-byte UTF-8 sequences > 1 byte
        // and render as 2 terminal columns
        let char_width = if theme.chars.chars().any(|c| is_wide_char(c)) {
            2
        } else {
            1
        };

        let char_count = chars.len();

        // Build luminance to character index lookup
        let mut lum_to_idx = [0u8; 256];
        let n = (char_count - 1) as f64;
        for i in 0..256 {
            lum_to_idx[i] = ((i as f64 / 255.0) * n) as u8;
        }

        // Pre-compute colors for luminance values
        let lum_to_color = Self::build_color_table(&theme.color_mode);

        // Pre-compute ANSI sequences for luminance-based color modes
        let use_lum_ansi = matches!(
            &theme.color_mode,
            ColorMode::Monochrome { .. } | ColorMode::Gradient { .. } | ColorMode::Palette { .. }
        );

        let lum_to_ansi = if use_lum_ansi {
            (0..256)
                .map(|i| {
                    let mut seq = Vec::with_capacity(19);
                    seq.extend_from_slice(b"\x1b[38;2;");
                    push_dec_fast(&mut seq, lum_to_color[i][0]);
                    seq.push(b';');
                    push_dec_fast(&mut seq, lum_to_color[i][1]);
                    seq.push(b';');
                    push_dec_fast(&mut seq, lum_to_color[i][2]);
                    seq.push(b'm');
                    seq
                })
                .collect()
        } else {
            Vec::new()
        };

        Self {
            chars,
            lum_to_idx,
            color_mode: theme.color_mode.clone(),
            lum_to_color,
            lum_to_ansi,
            char_width,
            use_lum_ansi,
        }
    }

    /// Get the character width (1 for ASCII, 2 for emoji).
    #[allow(dead_code)]
    pub fn char_width(&self) -> usize {
        self.char_width
    }

    /// Build a 256-entry color lookup table for the color mode.
    fn build_color_table(mode: &ColorMode) -> [[u8; 3]; 256] {
        let mut table = [[0u8; 3]; 256];

        match mode {
            ColorMode::Original => {
                // Not used - we use original pixel colors
            }
            ColorMode::Monochrome { color } => {
                for entry in &mut table {
                    *entry = *color;
                }
            }
            ColorMode::Gradient { dark, light } => {
                for (i, entry) in table.iter_mut().enumerate() {
                    let t = i as f32 / 255.0;
                    *entry = lerp_color(*dark, *light, t);
                }
            }
            ColorMode::Palette { colors } => {
                if colors.is_empty() {
                    // Fallback to white
                    for entry in &mut table {
                        *entry = [255, 255, 255];
                    }
                } else {
                    let n = colors.len();
                    for (i, entry) in table.iter_mut().enumerate() {
                        let idx = (i * (n - 1)) / 255;
                        let idx = idx.min(n - 1);
                        *entry = colors[idx];
                    }
                }
            }
            ColorMode::Tint { .. } => {
                // Not pre-computed - applied per pixel
            }
        }

        table
    }

    /// Render a single pixel to the output buffer.
    #[inline(always)]
    pub fn render_pixel(&self, out: &mut Vec<u8>, r: u8, g: u8, b: u8) {
        let lum = fast_luminance(r, g, b);
        let char_idx = self.lum_to_idx[lum as usize] as usize;
        // Safety: char_idx is bounded by lum_to_idx construction
        let char_bytes = unsafe { self.chars.get_unchecked(char_idx) };

        // Use pre-computed ANSI sequence if available
        if self.use_lum_ansi {
            out.extend_from_slice(unsafe { self.lum_to_ansi.get_unchecked(lum as usize) });
        } else {
            let color = self.get_color(r, g, b, lum);
            write_ansi_color(out, color);
        }
        out.extend_from_slice(char_bytes);
    }

    /// Get the color to use for a pixel.
    #[inline]
    fn get_color(&self, r: u8, g: u8, b: u8, lum: u8) -> [u8; 3] {
        match &self.color_mode {
            ColorMode::Original => [r, g, b],
            ColorMode::Monochrome { .. }
            | ColorMode::Gradient { .. }
            | ColorMode::Palette { .. } => self.lum_to_color[lum as usize],
            ColorMode::Tint {
                hue_shift,
                saturation,
            } => apply_tint(r, g, b, *hue_shift, *saturation),
        }
    }

    /// Render a full frame to ASCII with theme applied.
    pub fn render_frame(&self, img: &[u8], width: usize, height: usize, out: &mut Vec<u8>) {
        debug_assert!(width > 0 && height > 0, "Invalid dimensions");
        debug_assert!(img.len() >= width * height * 3, "Image buffer too small");
        debug_assert!(!self.chars.is_empty(), "No characters in theme");

        // For wide characters, we step by char_width pixels horizontally
        let step = self.char_width;
        let effective_width = width / step;

        // Estimate output size: ~20 bytes per pixel for color escape + char
        out.reserve(effective_width * height * 20 + height * 5);

        for y in 0..height {
            let row = y * width * 3;
            for x in (0..width).step_by(step) {
                let i = row + x * 3;
                self.render_pixel(out, img[i], img[i + 1], img[i + 2]);
            }
            out.extend_from_slice(b"\x1b[0m\n");
        }
    }

    /// Render a full frame to monochrome ASCII (no colors).
    pub fn render_frame_mono(&self, img: &[u8], width: usize, height: usize, out: &mut Vec<u8>) {
        debug_assert!(width > 0 && height > 0, "Invalid dimensions");
        debug_assert!(img.len() >= width * height * 3, "Image buffer too small");

        // For wide characters, we step by char_width pixels horizontally
        let step = self.char_width;

        out.reserve((width / step) * height + height);

        for y in 0..height {
            let row = y * width * 3;
            for x in (0..width).step_by(step) {
                let i = row + x * 3;
                let lum = fast_luminance(img[i], img[i + 1], img[i + 2]);
                let char_idx = self.lum_to_idx[lum as usize] as usize;
                let char_idx = if char_idx < self.chars.len() { char_idx } else { self.chars.len() - 1 };
                let char_bytes = &self.chars[char_idx];
                out.extend_from_slice(char_bytes);
            }
            out.push(b'\n');
        }
    }
}

// ---------------------------------------------------------------------------
// Character Width Detection
// ---------------------------------------------------------------------------

/// Check if a character is a "wide" character (takes 2 terminal columns).
/// This includes emoji, CJK characters, and other full-width characters.
fn is_wide_char(c: char) -> bool {
    // Quick check: ASCII is never wide
    if c.is_ascii() {
        return false;
    }

    // Check for common wide character ranges
    let cp = c as u32;

    // Emoji ranges (simplified)
    if (0x1F300..=0x1F9FF).contains(&cp) {  // Misc symbols, emoticons, etc.
        return true;
    }
    if (0x2600..=0x27BF).contains(&cp) {  // Misc symbols
        return true;
    }
    if (0x1F600..=0x1F64F).contains(&cp) {  // Emoticons
        return true;
    }

    // Block elements and braille (these are typically single-width)
    if (0x2580..=0x259F).contains(&cp) {  // Block elements
        return false;
    }
    if (0x2800..=0x28FF).contains(&cp) {  // Braille
        return false;
    }

    // CJK characters
    if (0x4E00..=0x9FFF).contains(&cp) {  // CJK Unified Ideographs
        return true;
    }

    // Fullwidth forms
    if (0xFF00..=0xFFEF).contains(&cp) {
        return true;
    }

    // Regional indicators (flags)
    if (0x1F1E0..=0x1F1FF).contains(&cp) {
        return true;
    }

    // Default: assume multi-byte non-ASCII might be wide
    // This is a heuristic - proper detection would use unicode-width crate
    cp > 0x2000
}

// ---------------------------------------------------------------------------
// Color Utilities
// ---------------------------------------------------------------------------

/// Fast luminance calculation using integer approximation.
/// Approximates 0.299R + 0.587G + 0.114B using (R*77 + G*150 + B*29) >> 8
#[inline(always)]
pub fn fast_luminance(r: u8, g: u8, b: u8) -> u8 {
    ((r as u32 * 77 + g as u32 * 150 + b as u32 * 29) >> 8) as u8
}

/// Linear interpolation between two colors.
#[inline]
fn lerp_color(dark: [u8; 3], light: [u8; 3], t: f32) -> [u8; 3] {
    [
        (dark[0] as f32 + (light[0] as f32 - dark[0] as f32) * t) as u8,
        (dark[1] as f32 + (light[1] as f32 - dark[1] as f32) * t) as u8,
        (dark[2] as f32 + (light[2] as f32 - dark[2] as f32) * t) as u8,
    ]
}

/// Apply hue shift and saturation adjustment via HSV transform.
#[inline]
fn apply_tint(r: u8, g: u8, b: u8, hue_shift: f32, sat_mult: f32) -> [u8; 3] {
    let (h, s, v) = rgb_to_hsv(r, g, b);
    let new_h = (h + hue_shift) % 360.0;
    let new_s = (s * sat_mult).clamp(0.0, 1.0);
    hsv_to_rgb(new_h, new_s, v)
}

/// Convert RGB to HSV.
fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let r = r as f32 / 255.0;
    let g = g as f32 / 255.0;
    let b = b as f32 / 255.0;

    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;

    let v = max;

    let s = if max == 0.0 { 0.0 } else { delta / max };

    let h = if delta == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta) % 6.0)
    } else if max == g {
        60.0 * (((b - r) / delta) + 2.0)
    } else {
        60.0 * (((r - g) / delta) + 4.0)
    };

    let h = if h < 0.0 { h + 360.0 } else { h };

    (h, s, v)
}

/// Convert HSV to RGB.
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [u8; 3] {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;

    let (r1, g1, b1) = if h < 60.0 {
        (c, x, 0.0)
    } else if h < 120.0 {
        (x, c, 0.0)
    } else if h < 180.0 {
        (0.0, c, x)
    } else if h < 240.0 {
        (0.0, x, c)
    } else if h < 300.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };

    [
        ((r1 + m) * 255.0) as u8,
        ((g1 + m) * 255.0) as u8,
        ((b1 + m) * 255.0) as u8,
    ]
}

/// Write ANSI truecolor escape sequence to output.
#[inline]
fn write_ansi_color(out: &mut Vec<u8>, color: [u8; 3]) {
    out.extend_from_slice(b"\x1b[38;2;");
    push_dec_fast(out, color[0]);
    out.push(b';');
    push_dec_fast(out, color[1]);
    out.push(b';');
    push_dec_fast(out, color[2]);
    out.push(b'm');
}

/// Pre-computed decimal strings for 0-255.
static DECIMAL_STRINGS: [[u8; 3]; 256] = {
    let mut table = [[0u8; 3]; 256];
    let mut i = 0usize;
    while i < 256 {
        if i >= 100 {
            table[i][0] = b'0' + (i / 100) as u8;
            table[i][1] = b'0' + ((i / 10) % 10) as u8;
            table[i][2] = b'0' + (i % 10) as u8;
        } else if i >= 10 {
            table[i][0] = b'0' + (i / 10) as u8;
            table[i][1] = b'0' + (i % 10) as u8;
            table[i][2] = 0;
        } else {
            table[i][0] = b'0' + i as u8;
            table[i][1] = 0;
            table[i][2] = 0;
        }
        i += 1;
    }
    table
};

/// Fast decimal push for u8 using lookup table.
#[inline(always)]
fn push_dec_fast(out: &mut Vec<u8>, v: u8) {
    let digits = &DECIMAL_STRINGS[v as usize];
    out.push(digits[0]);
    if digits[1] != 0 {
        out.push(digits[1]);
        if digits[2] != 0 {
            out.push(digits[2]);
        }
    }
}

// ---------------------------------------------------------------------------
// Theme Builder
// ---------------------------------------------------------------------------

/// Build a theme from name strings.
pub fn build_theme(char_theme: &str, color_mode_name: &str) -> Theme {
    let chars = get_char_ramp(char_theme)
        .unwrap_or(" .:-=+*#%@")
        .to_string();

    let color_mode = get_color_mode(color_mode_name).unwrap_or(ColorMode::Original);

    Theme {
        name: format!("{}+{}", char_theme, color_mode_name),
        chars,
        color_mode,
    }
}

// ---------------------------------------------------------------------------
// Config integration
// ---------------------------------------------------------------------------

/// Custom theme configuration from TOML.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CustomThemeConfig {
    /// Custom character string.
    pub chars: Option<String>,
    /// Color mode type: "original", "monochrome", "gradient", "palette", "tint".
    pub color_mode: Option<String>,
    /// Hex color for monochrome mode.
    pub color: Option<String>,
    /// Dark color for gradient mode (hex).
    pub color_dark: Option<String>,
    /// Light color for gradient mode (hex).
    pub color_light: Option<String>,
    /// Palette colors for palette mode (list of hex).
    pub palette: Option<Vec<String>>,
    /// Hue shift for tint mode (0-360).
    pub hue_shift: Option<f32>,
    /// Saturation multiplier for tint mode (0-1).
    pub saturation: Option<f32>,
}

impl CustomThemeConfig {
    /// Parse hex color string to RGB.
    fn parse_hex(hex: &str) -> [u8; 3] {
        let hex = hex.trim_start_matches('#');
        if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(255);
            let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(255);
            let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(255);
            [r, g, b]
        } else {
            [255, 255, 255]
        }
    }

    /// Convert to a Theme.
    pub fn to_theme(&self) -> Option<Theme> {
        let chars = self.chars.clone()?;

        let color_mode = match self.color_mode.as_deref() {
            Some("monochrome") => {
                let color = self.color.as_deref().map(Self::parse_hex).unwrap_or([255, 255, 255]);
                ColorMode::Monochrome { color }
            }
            Some("gradient") => {
                let dark = self.color_dark.as_deref().map(Self::parse_hex).unwrap_or([0, 0, 0]);
                let light = self.color_light.as_deref().map(Self::parse_hex).unwrap_or([255, 255, 255]);
                ColorMode::Gradient { dark, light }
            }
            Some("palette") => {
                let colors = self
                    .palette
                    .as_ref()
                    .map(|p| p.iter().map(|h| Self::parse_hex(h)).collect())
                    .unwrap_or_else(|| vec![[255, 255, 255]]);
                ColorMode::Palette { colors }
            }
            Some("tint") => ColorMode::Tint {
                hue_shift: self.hue_shift.unwrap_or(0.0),
                saturation: self.saturation.unwrap_or(1.0),
            },
            _ => ColorMode::Original,
        };

        Some(Theme {
            name: "custom".to_string(),
            chars,
            color_mode,
        })
    }
}
