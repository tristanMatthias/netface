//! Custom ratatui widgets for netface.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    widgets::Widget,
};
use unicode_width::UnicodeWidthChar;

/// A video panel that renders pre-rendered ASCII video frames.
pub struct VideoPanel<'a> {
    /// The ASCII frame data (with ANSI color codes).
    data: &'a [u8],
}

impl<'a> VideoPanel<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data }
    }
}

impl Widget for VideoPanel<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if self.data.is_empty() {
            return;
        }

        // Convert to string for proper UTF-8 handling
        let text = match std::str::from_utf8(self.data) {
            Ok(s) => s,
            Err(_) => return,
        };

        let mut x = area.x;
        let mut y = area.y;
        let mut current_fg = Color::Reset;
        let mut chars = text.chars().peekable();

        while let Some(ch) = chars.next() {
            if y >= area.y + area.height {
                break;
            }

            // Check for ANSI escape sequence
            if ch == '\x1b' {
                if chars.peek() == Some(&'[') {
                    chars.next(); // consume '['
                    let mut seq = String::new();
                    while let Some(&c) = chars.peek() {
                        if c == 'm' {
                            chars.next(); // consume 'm'
                            break;
                        }
                        seq.push(chars.next().unwrap());
                    }
                    // Parse the sequence
                    if seq == "0" {
                        current_fg = Color::Reset;
                    } else if let Some(color) = parse_ansi_color_str(&seq) {
                        current_fg = color;
                    }
                }
                continue;
            }

            if ch == '\n' {
                x = area.x;
                y += 1;
                continue;
            }

            // Regular character - render if in bounds
            if x < area.x + area.width {
                buf.set_string(x, y, ch.to_string(), Style::default().fg(current_fg));
                // Use unicode-width for accurate column width
                let char_width = ch.width().unwrap_or(1) as u16;
                x += char_width;
            }
        }
    }
}

/// Parse ANSI truecolor sequence "38;2;R;G;B" to Color (string version).
fn parse_ansi_color_str(seq: &str) -> Option<Color> {
    // Check for "38;2;R;G;B" format
    if !seq.starts_with("38;2;") {
        return None;
    }

    let parts: Vec<&str> = seq[5..].split(';').collect();
    if parts.len() != 3 {
        return None;
    }

    let r: u8 = parts[0].parse().ok()?;
    let g: u8 = parts[1].parse().ok()?;
    let b: u8 = parts[2].parse().ok()?;

    Some(Color::Rgb(r, g, b))
}


/// Status bar widget showing connection status and controls.
pub struct StatusBar {
    pub peer_addr: String,
    pub local_port: u16,
    pub connected: bool,
    pub muted: bool,
    pub video_off: bool,
    pub bg_on: bool,
}

impl StatusBar {
    pub fn new(peer_addr: String, local_port: u16) -> Self {
        Self {
            peer_addr,
            local_port,
            connected: false,
            muted: false,
            video_off: false,
            bg_on: true,
        }
    }
}

impl Widget for StatusBar {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Clear the status bar area
        for x in area.x..area.x + area.width {
            buf.set_string(x, area.y, " ", Style::default());
        }

        let mut x = area.x + 1;

        // Mute button
        let mute_style = if self.muted {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        buf.set_string(x, area.y, "[M] Mute", mute_style);
        x += 10;

        // Video button
        let video_style = if self.video_off {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        buf.set_string(x, area.y, "[V] Video", video_style);
        x += 11;

        // Background button
        let bg_style = if self.bg_on {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        buf.set_string(x, area.y, "[B] BG", bg_style);
        x += 8;

        // Separator
        buf.set_string(x, area.y, "│", Style::default().fg(Color::DarkGray));
        x += 2;

        // Connection status
        let status_text = if self.connected {
            format!("Connected to {}", self.peer_addr)
        } else {
            format!("Waiting on :{}", self.local_port)
        };
        let status_style = if self.connected {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::Yellow)
        };
        buf.set_string(x, area.y, &status_text, status_style);
        x += status_text.len() as u16 + 2;

        // Separator
        buf.set_string(x, area.y, "│", Style::default().fg(Color::DarkGray));
        x += 2;

        // Help hint
        buf.set_string(x, area.y, "? Help", Style::default().fg(Color::DarkGray));
    }
}

/// Help overlay widget.
pub struct HelpOverlay;

/// Picker modal for selecting themes or color modes.
pub struct PickerModal<'a> {
    pub title: &'a str,
    pub items: &'a [&'a str],
    pub selected: usize,
    pub current: &'a str,
}

impl Widget for HelpOverlay {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Calculate centered box
        let box_width = 40u16;
        let box_height = 14u16;
        let box_x = area.x + (area.width.saturating_sub(box_width)) / 2;
        let box_y = area.y + (area.height.saturating_sub(box_height)) / 2;

        // Draw semi-transparent background
        for y in box_y..box_y + box_height {
            for x in box_x..box_x + box_width {
                if x < area.x + area.width && y < area.y + area.height {
                    buf.set_string(x, y, " ", Style::default().bg(Color::Black));
                }
            }
        }

        let border_style = Style::default().fg(Color::Cyan);
        let title_style = Style::default().fg(Color::Cyan);
        let key_style = Style::default().fg(Color::Yellow);
        let desc_style = Style::default().fg(Color::White);

        // Draw border
        let top_y = box_y;
        let bot_y = box_y + box_height - 1;
        buf.set_string(box_x, top_y, "┌", border_style);
        buf.set_string(box_x + box_width - 1, top_y, "┐", border_style);
        buf.set_string(box_x, bot_y, "└", border_style);
        buf.set_string(box_x + box_width - 1, bot_y, "┘", border_style);
        for x in box_x + 1..box_x + box_width - 1 {
            buf.set_string(x, top_y, "─", border_style);
            buf.set_string(x, bot_y, "─", border_style);
        }
        for y in box_y + 1..box_y + box_height - 1 {
            buf.set_string(box_x, y, "│", border_style);
            buf.set_string(box_x + box_width - 1, y, "│", border_style);
        }

        // Title
        let title = " Keyboard Shortcuts ";
        let title_x = box_x + (box_width - title.len() as u16) / 2;
        buf.set_string(title_x, top_y, title, title_style);

        // Shortcuts
        let shortcuts = [
            ("q / Esc", "Quit"),
            ("m", "Toggle audio mute"),
            ("v", "Toggle video"),
            ("b", "Toggle background removal"),
            ("t", "Change character theme"),
            ("c", "Change color mode"),
            ("l", "View logs"),
            ("?", "Toggle this help"),
        ];

        for (i, (key, desc)) in shortcuts.iter().enumerate() {
            let y = box_y + 2 + i as u16;
            buf.set_string(box_x + 3, y, *key, key_style);
            buf.set_string(box_x + 15, y, *desc, desc_style);
        }
    }
}

impl<'a> PickerModal<'a> {
    pub fn new(title: &'a str, items: &'a [&'a str], selected: usize, current: &'a str) -> Self {
        Self {
            title,
            items,
            selected,
            current,
        }
    }
}

/// Log viewer modal for viewing application logs.
pub struct LogViewer<'a> {
    pub lines: &'a [String],
    pub scroll: usize,
}

impl<'a> LogViewer<'a> {
    pub fn new(lines: &'a [String], scroll: usize) -> Self {
        Self { lines, scroll }
    }
}

impl Widget for LogViewer<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Use most of the screen
        let margin = 2u16;
        let box_width = area.width.saturating_sub(margin * 2);
        let box_height = area.height.saturating_sub(margin * 2);
        let box_x = area.x + margin;
        let box_y = area.y + margin;

        if box_width < 20 || box_height < 5 {
            return;
        }

        // Draw background
        for y in box_y..box_y + box_height {
            for x in box_x..box_x + box_width {
                buf.set_string(x, y, " ", Style::default().bg(Color::Black));
            }
        }

        let border_style = Style::default().fg(Color::Blue);
        let title_style = Style::default().fg(Color::Blue);
        let log_style = Style::default().fg(Color::White);
        let info_style = Style::default().fg(Color::Cyan);
        let warn_style = Style::default().fg(Color::Yellow);
        let error_style = Style::default().fg(Color::Red);
        let debug_style = Style::default().fg(Color::DarkGray);
        let hint_style = Style::default().fg(Color::DarkGray);

        // Draw border
        let top_y = box_y;
        let bot_y = box_y + box_height - 1;
        buf.set_string(box_x, top_y, "┌", border_style);
        buf.set_string(box_x + box_width - 1, top_y, "┐", border_style);
        buf.set_string(box_x, bot_y, "└", border_style);
        buf.set_string(box_x + box_width - 1, bot_y, "┘", border_style);
        for x in box_x + 1..box_x + box_width - 1 {
            buf.set_string(x, top_y, "─", border_style);
            buf.set_string(x, bot_y, "─", border_style);
        }
        for y in box_y + 1..box_y + box_height - 1 {
            buf.set_string(box_x, y, "│", border_style);
            buf.set_string(box_x + box_width - 1, y, "│", border_style);
        }

        // Title with log file path
        let log_path = crate::logging::log_path();
        let title = format!(" Logs: {} ", log_path.display());
        let title_x = box_x + 2;
        buf.set_string(title_x, top_y, &title, title_style);

        // Scroll position indicator
        let total_lines = self.lines.len();
        let pos_text = format!(" {}/{} ", self.scroll + 1, total_lines.max(1));
        let pos_x = box_x + box_width - pos_text.len() as u16 - 2;
        buf.set_string(pos_x, top_y, &pos_text, title_style);

        // Calculate visible area
        let content_height = (box_height - 3) as usize;
        let content_width = (box_width - 4) as usize;

        // Calculate which lines to show (scroll to show scroll position near bottom)
        let start_line = self.scroll.saturating_sub(content_height.saturating_sub(1));
        let end_line = (start_line + content_height).min(self.lines.len());

        // Render log lines
        for (i, line_idx) in (start_line..end_line).enumerate() {
            let y = box_y + 1 + i as u16;
            let line = &self.lines[line_idx];

            // Truncate line to fit
            let display_line: String = line.chars().take(content_width).collect();

            // Color based on log level
            let style = if line.contains("[ERROR]") {
                error_style
            } else if line.contains("[WARN]") {
                warn_style
            } else if line.contains("[INFO]") {
                info_style
            } else if line.contains("[DEBUG]") {
                debug_style
            } else {
                log_style
            };

            // Highlight the current scroll position line
            let final_style = if line_idx == self.scroll {
                style.bg(Color::Rgb(30, 30, 50))
            } else {
                style
            };

            buf.set_string(box_x + 2, y, &display_line, final_style);
        }

        // Hint bar
        let hint = "↑↓ scroll  PgUp/PgDn page  g/G start/end  q close";
        let hint_x = box_x + (box_width.saturating_sub(hint.len() as u16)) / 2;
        buf.set_string(hint_x, bot_y, hint, hint_style);
    }
}

impl Widget for PickerModal<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let item_count = self.items.len();
        let box_width = 35u16;
        let box_height = (item_count as u16 + 4).min(area.height - 2);
        let box_x = area.x + (area.width.saturating_sub(box_width)) / 2;
        let box_y = area.y + (area.height.saturating_sub(box_height)) / 2;

        // Draw background
        for y in box_y..box_y + box_height {
            for x in box_x..box_x + box_width {
                if x < area.x + area.width && y < area.y + area.height {
                    buf.set_string(x, y, " ", Style::default().bg(Color::Black));
                }
            }
        }

        let border_style = Style::default().fg(Color::Magenta);
        let title_style = Style::default().fg(Color::Magenta);
        let normal_style = Style::default().fg(Color::White);
        let selected_style = Style::default().fg(Color::Black).bg(Color::Cyan);
        let current_style = Style::default().fg(Color::Green);
        let hint_style = Style::default().fg(Color::DarkGray);

        // Draw border
        let top_y = box_y;
        let bot_y = box_y + box_height - 1;
        buf.set_string(box_x, top_y, "┌", border_style);
        buf.set_string(box_x + box_width - 1, top_y, "┐", border_style);
        buf.set_string(box_x, bot_y, "└", border_style);
        buf.set_string(box_x + box_width - 1, bot_y, "┘", border_style);
        for x in box_x + 1..box_x + box_width - 1 {
            buf.set_string(x, top_y, "─", border_style);
            buf.set_string(x, bot_y, "─", border_style);
        }
        for y in box_y + 1..box_y + box_height - 1 {
            buf.set_string(box_x, y, "│", border_style);
            buf.set_string(box_x + box_width - 1, y, "│", border_style);
        }

        // Title
        let title = format!(" {} ", self.title);
        let title_x = box_x + (box_width - title.len() as u16) / 2;
        buf.set_string(title_x, top_y, &title, title_style);

        // Items
        let visible_items = (box_height - 4) as usize;
        let scroll_offset = if self.selected >= visible_items {
            self.selected - visible_items + 1
        } else {
            0
        };

        for (i, item) in self.items.iter().enumerate().skip(scroll_offset).take(visible_items) {
            let y = box_y + 2 + (i - scroll_offset) as u16;
            let is_selected = i == self.selected;
            let is_current = *item == self.current;

            // Clear line
            for x in box_x + 1..box_x + box_width - 1 {
                buf.set_string(x, y, " ", if is_selected { selected_style } else { Style::default().bg(Color::Black) });
            }

            let prefix = if is_current { "● " } else { "  " };
            let style = if is_selected {
                selected_style
            } else if is_current {
                current_style
            } else {
                normal_style
            };

            buf.set_string(box_x + 2, y, prefix, style);
            buf.set_string(box_x + 4, y, *item, style);
        }

        // Hint
        let hint = "↑↓ navigate  Enter select  Esc close";
        let hint_x = box_x + (box_width.saturating_sub(hint.len() as u16)) / 2;
        buf.set_string(hint_x, bot_y, hint, hint_style);
    }
}
