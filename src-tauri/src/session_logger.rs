use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{Datelike, Local, Timelike};
use tracing::{info, warn};

/// Manages log file handles for active terminal sessions.
pub struct SessionLogManager {
    /// Map of connection_id -> open log file handle.
    handles: HashMap<String, File>,
    pub enabled: bool,
    pub log_dir: PathBuf,
    pub filename_format: String,
}

impl SessionLogManager {
    pub fn new(enabled: bool, log_dir: PathBuf, filename_format: String) -> Self {
        Self {
            handles: HashMap::new(),
            enabled,
            log_dir,
            filename_format,
        }
    }

    /// Open a log file for a new session connection.
    pub fn open_log(&mut self, connection_id: &str, session_name: &str) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }

        fs::create_dir_all(&self.log_dir)
            .map_err(|e| format!("Failed to create log directory: {e}"))?;

        // Secure the log directory on Unix (owner-only access).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.log_dir, fs::Permissions::from_mode(0o700));
        }

        let path = self.generate_unique_path(session_name)?;

        // Verify the resolved path stays within log_dir (prevent traversal).
        let canonical_dir = self
            .log_dir
            .canonicalize()
            .map_err(|e| format!("Failed to canonicalize log directory: {e}"))?;
        // The file doesn't exist yet, so canonicalize the parent directory.
        let parent = path
            .parent()
            .ok_or_else(|| "Log file path has no parent directory".to_string())?;
        let canonical_parent = parent
            .canonicalize()
            .map_err(|e| format!("Failed to canonicalize log file parent: {e}"))?;
        if !canonical_parent.starts_with(&canonical_dir) {
            return Err("Log file path escapes the configured log directory".to_string());
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("Failed to open log file {}: {e}", path.display()))?;

        // Set restrictive permissions on Unix (owner read/write only).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|e| {
                // Remove the file if we can't secure it.
                let _ = fs::remove_file(&path);
                format!("Failed to set log file permissions: {e}")
            })?;
        }

        info!(
            connection_id = %connection_id,
            path = %path.display(),
            "Session log opened"
        );
        self.handles.insert(connection_id.to_string(), file);
        Ok(())
    }

    /// Write terminal output to the log file, stripping ANSI escape sequences
    /// so the log contains only readable text.
    pub fn write_log(&mut self, connection_id: &str, data: &[u8]) {
        if let Some(file) = self.handles.get_mut(connection_id) {
            let clean = strip_ansi(data);
            if clean.is_empty() {
                return;
            }
            if let Err(e) = file.write_all(&clean) {
                warn!(
                    connection_id = %connection_id,
                    error = %e,
                    "Failed to write session log"
                );
            }
        }
    }

    /// Close and flush the log file for a disconnected session.
    pub fn close_log(&mut self, connection_id: &str) {
        if let Some(mut file) = self.handles.remove(connection_id) {
            if let Err(e) = file.flush() {
                warn!(connection_id = %connection_id, error = %e, "Failed to flush session log");
            }
            info!(connection_id = %connection_id, "Session log closed");
        }
    }

    /// Generate a unique file path, appending _1, _2, etc. if the name exists.
    fn generate_unique_path(&self, session_name: &str) -> Result<PathBuf, String> {
        let now = Local::now();
        let safe_name: String = session_name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect();

        let filename = self
            .filename_format
            .replace("{name}", &safe_name)
            .replace("{mm}", &format!("{:02}", now.minute()))
            .replace("{hh}", &format!("{:02}", now.hour()))
            .replace("{dd}", &format!("{:02}", now.day()))
            .replace("{MM}", &format!("{:02}", now.month()))
            .replace("{yy}", &format!("{:02}", now.year() % 100));

        // Reject any substituted filename that could escape the log directory.
        if filename.contains('/') || filename.contains('\\') || filename.contains("..") {
            return Err("Generated log filename contains path traversal characters".to_string());
        }

        let base_path = self.log_dir.join(&filename);

        if !base_path.exists() {
            return Ok(base_path);
        }

        let stem = base_path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let ext = base_path
            .extension()
            .map(|e| e.to_string_lossy().to_string());

        for i in 1..=999u16 {
            let deduped = match &ext {
                Some(e) => format!("{stem}_{i}.{e}"),
                None => format!("{stem}_{i}"),
            };
            let path = self.log_dir.join(deduped);
            if !path.exists() {
                return Ok(path);
            }
        }

        Err("Too many log files with the same name in this minute".to_string())
    }
}

/// Thread-safe wrapper registered as Tauri managed state.
pub struct SessionLogState(pub Arc<Mutex<SessionLogManager>>);

// ── ANSI escape sequence stripping ─────────────────────────────────

/// Parser states for stripping ANSI/VT escape sequences from terminal output.
#[derive(Clone, Copy)]
enum StripState {
    /// Normal text — pass through.
    Normal,
    /// Saw ESC (0x1B), waiting for sequence type.
    Escape,
    /// CSI sequence (ESC [ ...): consume parameter bytes (0x30-0x3F),
    /// intermediate bytes (0x20-0x2F), then final byte (0x40-0x7E).
    Csi,
    /// OSC sequence (ESC ] ...): consume until ST (ESC \ or BEL).
    Osc,
    /// Inside OSC, saw ESC — next byte should be '\' to end the sequence.
    OscEscape,
}

/// Strip ANSI escape sequences, leaving only printable text + whitespace.
///
/// Handles:
/// - CSI sequences: `ESC [` ... final byte
/// - OSC sequences: `ESC ]` ... `BEL` or `ESC \`
/// - Two-character escapes: `ESC` + single byte (0x40-0x7E)
/// - Common C0 controls that xterm renders: CR, LF, TAB, BS
fn strip_ansi(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut state = StripState::Normal;

    for &byte in data {
        match state {
            StripState::Normal => {
                if byte == 0x1B {
                    state = StripState::Escape;
                } else if byte == b'\n' || byte == b'\r' || byte == b'\t' {
                    out.push(byte);
                } else if byte == 0x08 {
                    // Backspace — remove last character if present.
                    out.pop();
                } else if byte >= 0x20 {
                    // Printable ASCII and UTF-8 continuation/start bytes.
                    out.push(byte);
                }
                // Drop other C0 controls (BEL, etc.) silently.
            }
            StripState::Escape => match byte {
                b'[' => state = StripState::Csi,
                b']' => state = StripState::Osc,
                // Two-character escape: ESC + final byte — discard both.
                0x40..=0x7E => state = StripState::Normal,
                // Unexpected byte after ESC — drop the ESC, re-process byte.
                _ => {
                    state = StripState::Normal;
                    if byte >= 0x20 {
                        out.push(byte);
                    }
                }
            },
            StripState::Csi => {
                // Final byte ends the CSI sequence.
                if (0x40..=0x7E).contains(&byte) {
                    state = StripState::Normal;
                }
                // Parameter bytes (0x30-0x3F) and intermediate bytes (0x20-0x2F)
                // are consumed silently.
            }
            StripState::Osc => {
                if byte == 0x07 {
                    // BEL terminates OSC.
                    state = StripState::Normal;
                } else if byte == 0x1B {
                    state = StripState::OscEscape;
                }
                // All other bytes inside OSC are consumed.
            }
            StripState::OscEscape => {
                // Expect '\' for ST (String Terminator).
                state = StripState::Normal;
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_plain_text() {
        assert_eq!(strip_ansi(b"hello world"), b"hello world");
    }

    #[test]
    fn strip_csi_color() {
        // ESC[1;34m "blue" ESC[0m
        let input = b"\x1b[1;34mblue\x1b[0m";
        assert_eq!(strip_ansi(input), b"blue");
    }

    #[test]
    fn strip_cursor_control() {
        // ESC[6n (cursor position request) — should be stripped entirely
        let input = b"prompt$ \x1b[6ncommand";
        assert_eq!(strip_ansi(input), b"prompt$ command");
    }

    #[test]
    fn strip_bracketed_paste() {
        // ESC[?2004h and ESC[?2004l
        let input = b"\x1b[?2004htext\x1b[?2004l";
        assert_eq!(strip_ansi(input), b"text");
    }

    #[test]
    fn strip_osc_title() {
        // OSC 0;title BEL
        let input = b"\x1b]0;my title\x07visible";
        assert_eq!(strip_ansi(input), b"visible");
    }

    #[test]
    fn strip_osc_st() {
        // OSC terminated by ST (ESC \)
        let input = b"\x1b]0;title\x1b\\visible";
        assert_eq!(strip_ansi(input), b"visible");
    }

    #[test]
    fn preserve_newlines_and_tabs() {
        let input = b"line1\r\nline2\ttab";
        assert_eq!(strip_ansi(input), b"line1\r\nline2\ttab");
    }

    #[test]
    fn strip_backspace() {
        // "ab" then BS then "c" → "ac"
        let input = b"ab\x08c";
        assert_eq!(strip_ansi(input), b"ac");
    }

    #[test]
    fn strip_erase_line() {
        // ESC[K (erase to end of line), ESC[J (erase display)
        let input = b"prompt\x1b[K\x1b[Jtext";
        assert_eq!(strip_ansi(input), b"prompttext");
    }
}
