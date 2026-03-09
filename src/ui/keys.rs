//! Keyboard shortcuts and action handlers.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Actions that can be triggered by keyboard input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    /// Quit the application.
    Quit,
    /// Toggle audio mute.
    ToggleMute,
    /// Toggle local video on/off.
    ToggleVideo,
    /// Toggle background removal.
    ToggleBackground,
    /// Show/hide help overlay.
    ToggleHelp,
    /// Open theme picker modal.
    OpenThemePicker,
    /// Open color mode picker modal.
    OpenColorPicker,
    /// Navigate up in modal.
    ModalUp,
    /// Navigate down in modal.
    ModalDown,
    /// Select current item in modal.
    ModalSelect,
    /// Close modal.
    ModalClose,
    /// No action (unbound key).
    None,
}

/// Map a key event to an action when no modal is open.
pub fn handle_key_event(key: KeyEvent) -> KeyAction {
    match key.code {
        // Quit
        KeyCode::Char('q') | KeyCode::Esc => KeyAction::Quit,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => KeyAction::Quit,

        // Toggle mute
        KeyCode::Char('m') => KeyAction::ToggleMute,

        // Toggle video
        KeyCode::Char('v') => KeyAction::ToggleVideo,

        // Toggle background removal
        KeyCode::Char('b') => KeyAction::ToggleBackground,

        // Show help
        KeyCode::Char('?') => KeyAction::ToggleHelp,

        // Open theme picker
        KeyCode::Char('t') => KeyAction::OpenThemePicker,

        // Open color picker
        KeyCode::Char('c') => KeyAction::OpenColorPicker,

        _ => KeyAction::None,
    }
}

/// Map a key event to an action when a modal is open.
pub fn handle_modal_key_event(key: KeyEvent) -> KeyAction {
    match key.code {
        // Close modal
        KeyCode::Esc | KeyCode::Char('q') => KeyAction::ModalClose,

        // Navigate
        KeyCode::Up | KeyCode::Char('k') => KeyAction::ModalUp,
        KeyCode::Down | KeyCode::Char('j') => KeyAction::ModalDown,

        // Select
        KeyCode::Enter | KeyCode::Char(' ') => KeyAction::ModalSelect,

        _ => KeyAction::None,
    }
}
