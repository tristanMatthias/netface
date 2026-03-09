//! UI module for ratatui-based TUI.

pub mod keys;
pub mod widgets;

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use crossbeam_channel::Receiver;
use crossterm::event::{self, Event};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::Widget,
    DefaultTerminal, Frame,
};
use keys::{handle_key_event, handle_modal_key_event, KeyAction};
use widgets::{HelpOverlay, PickerModal, StatusBar, VideoPanel};
use crate::theme::{self, ThemeRenderer};

/// Which modal is currently open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalState {
    None,
    ThemePicker,
    ColorPicker,
}

/// Application state for the TUI.
pub struct App {
    // Connection state
    pub peer_connected: Arc<AtomicBool>,
    pub peer_addr: String,
    pub local_port: u16,

    // Controls (shared with other threads)
    pub audio_muted: Arc<AtomicBool>,
    pub video_disabled: Arc<AtomicBool>,
    pub bg_enabled: Arc<AtomicBool>,

    // Theme (shared with video threads)
    pub theme_renderer: Arc<RwLock<ThemeRenderer>>,
    pub current_theme: String,
    pub current_color_mode: String,

    // Display buffers (received from channels)
    pub local_ascii: Option<Vec<u8>>,
    pub remote_ascii: Option<Vec<u8>>,

    // UI state
    pub show_help: bool,
    pub should_quit: bool,
    pub modal_state: ModalState,
    pub modal_selection: usize,
}

impl App {
    pub fn new(
        peer_connected: Arc<AtomicBool>,
        peer_addr: String,
        local_port: u16,
        audio_muted: Arc<AtomicBool>,
        video_disabled: Arc<AtomicBool>,
        bg_enabled: Arc<AtomicBool>,
        theme_renderer: Arc<RwLock<ThemeRenderer>>,
        current_theme: String,
        current_color_mode: String,
    ) -> Self {
        Self {
            peer_connected,
            peer_addr,
            local_port,
            audio_muted,
            video_disabled,
            bg_enabled,
            theme_renderer,
            current_theme,
            current_color_mode,
            local_ascii: None,
            remote_ascii: None,
            show_help: false,
            should_quit: false,
            modal_state: ModalState::None,
            modal_selection: 0,
        }
    }

    /// Handle a key action.
    pub fn handle_action(&mut self, action: KeyAction) {
        match action {
            KeyAction::Quit => self.should_quit = true,
            KeyAction::ToggleMute => {
                let current = self.audio_muted.load(Ordering::Relaxed);
                self.audio_muted.store(!current, Ordering::Relaxed);
            }
            KeyAction::ToggleVideo => {
                let current = self.video_disabled.load(Ordering::Relaxed);
                self.video_disabled.store(!current, Ordering::Relaxed);
            }
            KeyAction::ToggleBackground => {
                let current = self.bg_enabled.load(Ordering::Relaxed);
                self.bg_enabled.store(!current, Ordering::Relaxed);
            }
            KeyAction::ToggleHelp => {
                self.show_help = !self.show_help;
            }
            KeyAction::OpenThemePicker => {
                self.modal_state = ModalState::ThemePicker;
                // Set selection to current theme
                let themes = theme::list_char_ramps();
                self.modal_selection = themes
                    .iter()
                    .position(|&t| t == self.current_theme)
                    .unwrap_or(0);
            }
            KeyAction::OpenColorPicker => {
                self.modal_state = ModalState::ColorPicker;
                // Set selection to current color mode
                let modes = theme::list_color_modes();
                self.modal_selection = modes
                    .iter()
                    .position(|&m| m == self.current_color_mode)
                    .unwrap_or(0);
            }
            KeyAction::ModalUp => {
                if self.modal_selection > 0 {
                    self.modal_selection -= 1;
                }
            }
            KeyAction::ModalDown => {
                let max = match self.modal_state {
                    ModalState::ThemePicker => theme::list_char_ramps().len().saturating_sub(1),
                    ModalState::ColorPicker => theme::list_color_modes().len().saturating_sub(1),
                    ModalState::None => 0,
                };
                if self.modal_selection < max {
                    self.modal_selection += 1;
                }
            }
            KeyAction::ModalSelect => {
                match self.modal_state {
                    ModalState::ThemePicker => {
                        let themes = theme::list_char_ramps();
                        if let Some(&new_theme) = themes.get(self.modal_selection) {
                            self.current_theme = new_theme.to_string();
                            self.update_theme();
                        }
                    }
                    ModalState::ColorPicker => {
                        let modes = theme::list_color_modes();
                        if let Some(&new_mode) = modes.get(self.modal_selection) {
                            self.current_color_mode = new_mode.to_string();
                            self.update_theme();
                        }
                    }
                    ModalState::None => {}
                }
                self.modal_state = ModalState::None;
            }
            KeyAction::ModalClose => {
                self.modal_state = ModalState::None;
            }
            KeyAction::None => {}
        }
    }

    /// Update the theme renderer with current settings.
    fn update_theme(&self) {
        let new_theme = theme::build_theme(&self.current_theme, &self.current_color_mode);
        let new_renderer = ThemeRenderer::new(&new_theme);
        if let Ok(mut renderer) = self.theme_renderer.write() {
            *renderer = new_renderer;
        }
    }
}

/// Run the main UI event loop.
pub fn run(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    local_ascii_rx: Receiver<Vec<u8>>,
    remote_ascii_rx: Receiver<Vec<u8>>,
) -> io::Result<()> {
    loop {
        // Poll for keyboard events (non-blocking)
        if event::poll(Duration::from_millis(16))? {
            if let Event::Key(key) = event::read()? {
                // Use different key handler when modal is open
                let action = if app.modal_state != ModalState::None {
                    handle_modal_key_event(key)
                } else {
                    handle_key_event(key)
                };
                app.handle_action(action);

                if app.should_quit {
                    return Ok(());
                }
            }
        }

        // Poll video frame channels (non-blocking)
        while let Ok(frame) = local_ascii_rx.try_recv() {
            app.local_ascii = Some(frame);
        }

        let connected = app.peer_connected.load(Ordering::Relaxed);
        if connected {
            while let Ok(frame) = remote_ascii_rx.try_recv() {
                app.remote_ascii = Some(frame);
            }
        }

        // Render
        terminal.draw(|frame| render(frame, app))?;
    }
}

/// Render the UI.
fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let connected = app.peer_connected.load(Ordering::Relaxed);

    // Layout: video area + status bar
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let video_area = chunks[0];
    let status_area = chunks[1];

    // Render video panel(s)
    if connected {
        // Split view: local (left) | remote (right)
        let video_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(video_area);

        // Draw separator
        let sep_x = video_chunks[0].x + video_chunks[0].width;
        for y in video_area.y..video_area.y + video_area.height {
            frame.buffer_mut().set_string(
                sep_x.saturating_sub(1),
                y,
                "│",
                Style::default().fg(Color::DarkGray),
            );
        }

        // Local video panel (left)
        if let Some(ref data) = app.local_ascii {
            let panel = VideoPanel::new(data);
            panel.render(video_chunks[0], frame.buffer_mut());
        }

        // Remote video panel (right)
        if let Some(ref data) = app.remote_ascii {
            let panel = VideoPanel::new(data);
            panel.render(video_chunks[1], frame.buffer_mut());
        }
    } else {
        // Full-screen local video
        if let Some(ref data) = app.local_ascii {
            let panel = VideoPanel::new(data);
            panel.render(video_area, frame.buffer_mut());
        }
    }

    // Render status bar
    let mut status = StatusBar::new(app.peer_addr.clone(), app.local_port);
    status.connected = connected;
    status.muted = app.audio_muted.load(Ordering::Relaxed);
    status.video_off = app.video_disabled.load(Ordering::Relaxed);
    status.bg_on = app.bg_enabled.load(Ordering::Relaxed);
    status.render(status_area, frame.buffer_mut());

    // Render help overlay if active
    if app.show_help {
        HelpOverlay.render(area, frame.buffer_mut());
    }

    // Render picker modals
    match app.modal_state {
        ModalState::ThemePicker => {
            let themes = theme::list_char_ramps();
            let modal = PickerModal::new(
                "Character Theme",
                themes,
                app.modal_selection,
                &app.current_theme,
            );
            modal.render(area, frame.buffer_mut());
        }
        ModalState::ColorPicker => {
            let modes = theme::list_color_modes();
            let modal = PickerModal::new(
                "Color Mode",
                modes,
                app.modal_selection,
                &app.current_color_mode,
            );
            modal.render(area, frame.buffer_mut());
        }
        ModalState::None => {}
    }
}
