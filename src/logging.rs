//! Persistent file logging for netface.
//!
//! Logs are written to platform-appropriate locations:
//! - macOS: ~/Library/Logs/netface/netface.log
//! - Linux: ~/.local/share/netface/netface.log
//! - Windows: %APPDATA%/netface/logs/netface.log

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

static LOG_FILE: OnceLock<Mutex<Option<File>>> = OnceLock::new();
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Get the platform-appropriate log directory.
pub fn log_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        dirs::home_dir()
            .map(|h| h.join("Library/Logs/netface"))
            .unwrap_or_else(|| PathBuf::from("/tmp/netface"))
    }

    #[cfg(target_os = "linux")]
    {
        dirs::data_local_dir()
            .map(|d| d.join("netface"))
            .unwrap_or_else(|| PathBuf::from("/tmp/netface"))
    }

    #[cfg(target_os = "windows")]
    {
        dirs::data_local_dir()
            .map(|d| d.join("netface/logs"))
            .unwrap_or_else(|| PathBuf::from("C:/temp/netface"))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        PathBuf::from("/tmp/netface")
    }
}

/// Get the log file path.
pub fn log_path() -> PathBuf {
    LOG_PATH.get_or_init(|| log_dir().join("netface.log")).clone()
}

/// Initialize the logging system.
pub fn init() -> std::io::Result<()> {
    let dir = log_dir();
    fs::create_dir_all(&dir)?;

    let path = dir.join("netface.log");

    // Rotate log if it's too large (> 1MB)
    if let Ok(meta) = fs::metadata(&path) {
        if meta.len() > 1_000_000 {
            let backup = dir.join("netface.log.old");
            let _ = fs::rename(&path, backup);
        }
    }

    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;

    LOG_PATH.get_or_init(|| path);
    LOG_FILE.get_or_init(|| Mutex::new(Some(file)));

    // Write session start marker
    log_raw("========================================");
    log_raw(&format!("Session started at {:?}", SystemTime::now()));
    log_raw("========================================");

    Ok(())
}

/// Write a raw line to the log file.
fn log_raw(msg: &str) {
    if let Some(mutex) = LOG_FILE.get() {
        if let Ok(mut guard) = mutex.lock() {
            if let Some(ref mut file) = *guard {
                let _ = writeln!(file, "{}", msg);
                let _ = file.flush();
            }
        }
    }
}

/// Log a message with timestamp and level.
pub fn log(level: &str, msg: &str) {
    let timestamp = chrono::Local::now().format("%H:%M:%S%.3f");
    let formatted = format!("[{}] [{}] {}", timestamp, level, msg);
    log_raw(&formatted);
}

/// Log an info message.
pub fn info(msg: &str) {
    log("INFO", msg);
}

/// Log a debug message.
pub fn debug(msg: &str) {
    log("DEBUG", msg);
}

/// Log a warning message.
pub fn warn(msg: &str) {
    log("WARN", msg);
}

/// Log an error message.
pub fn error(msg: &str) {
    log("ERROR", msg);
}

/// Read the last N lines from the log file.
pub fn read_last_lines(n: usize) -> Vec<String> {
    let path = log_path();

    let file = match File::open(&path) {
        Ok(f) => f,
        Err(_) => return vec![format!("Could not open log file: {:?}", path)],
    };

    let reader = BufReader::new(file);
    let all_lines: Vec<String> = reader.lines().filter_map(|l| l.ok()).collect();

    let start = all_lines.len().saturating_sub(n);
    all_lines[start..].to_vec()
}

/// Read all lines from the log file.
pub fn read_all_lines() -> Vec<String> {
    let path = log_path();

    let file = match File::open(&path) {
        Ok(f) => f,
        Err(_) => return vec![format!("Could not open log file: {:?}", path)],
    };

    let reader = BufReader::new(file);
    reader.lines().filter_map(|l| l.ok()).collect()
}

/// Convenience macros for logging.
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        $crate::logging::info(&format!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => {
        $crate::logging::debug(&format!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {
        $crate::logging::warn(&format!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {
        $crate::logging::error(&format!($($arg)*))
    };
}
