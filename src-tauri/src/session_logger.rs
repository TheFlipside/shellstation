use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{Datelike, Local, Timelike};
use tracing::{info, warn};

/// A log entry is either waiting for its first write (Pending) or actively
/// writing to a file (Active).  Pending entries never touch the filesystem,
/// so failed connections leave no empty log files behind.
enum LogEntry {
    /// Path validated and generated, but no file created yet.
    Pending(PathBuf),
    /// File opened on first write.
    Active(File),
}

/// Manages log file handles for active terminal sessions.
pub struct SessionLogManager {
    /// Map of connection_id -> log entry (pending or active file handle).
    entries: HashMap<String, LogEntry>,
    pub enabled: bool,
    pub log_dir: PathBuf,
    pub filename_format: String,
}

impl SessionLogManager {
    pub fn new(enabled: bool, log_dir: PathBuf, filename_format: String) -> Self {
        Self {
            entries: HashMap::new(),
            enabled,
            log_dir,
            filename_format,
        }
    }

    /// Prepare a log for a new session connection.
    ///
    /// Validates the path and stores it as [`LogEntry::Pending`].  The actual
    /// file is created lazily on the first [`write_log`] call, so failed
    /// connections never leave empty log files on disk.
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

        info!(
            connection_id = %connection_id,
            path = %path.display(),
            "Session log prepared (file created on first write)"
        );
        self.entries
            .insert(connection_id.to_string(), LogEntry::Pending(path));
        Ok(())
    }

    /// Promote a [`LogEntry::Pending`] to [`LogEntry::Active`] by creating the
    /// file on disk.  Returns a mutable reference to the opened file.
    fn activate_log(&mut self, connection_id: &str) -> Result<&mut File, String> {
        // Take the entry out so we can replace it.
        let entry = self
            .entries
            .remove(connection_id)
            .ok_or_else(|| format!("No log entry for {connection_id}"))?;

        let path = match entry {
            LogEntry::Pending(p) => p,
            LogEntry::Active(file) => {
                // Already active — put it back and return.
                self.entries
                    .insert(connection_id.to_string(), LogEntry::Active(file));
                if let Some(LogEntry::Active(ref mut f)) = self.entries.get_mut(connection_id) {
                    return Ok(f);
                }
                unreachable!();
            }
        };

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| {
                // Re-insert as Pending so close_log can still clean up.
                self.entries
                    .insert(connection_id.to_string(), LogEntry::Pending(path.clone()));
                format!("Failed to open log file {}: {e}", path.display())
            })?;

        // Set restrictive permissions on Unix (owner read/write only).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|e| {
                let _ = fs::remove_file(&path);
                format!("Failed to set log file permissions: {e}")
            })?;
        }

        info!(
            connection_id = %connection_id,
            path = %path.display(),
            "Session log file created"
        );
        self.entries
            .insert(connection_id.to_string(), LogEntry::Active(file));

        match self.entries.get_mut(connection_id) {
            Some(LogEntry::Active(ref mut f)) => Ok(f),
            _ => unreachable!(),
        }
    }

    /// Write terminal output to the log file, stripping ANSI escape sequences
    /// so the log contains only readable text.
    ///
    /// Creates the log file on the first call (lazy open).
    pub fn write_log(&mut self, connection_id: &str, data: &[u8]) {
        if !self.entries.contains_key(connection_id) {
            return;
        }

        let clean = strip_ansi(data);
        if clean.is_empty() {
            return;
        }

        // Promote Pending → Active on first real data.
        let file = match self.activate_log(connection_id) {
            Ok(f) => f,
            Err(e) => {
                warn!(
                    connection_id = %connection_id,
                    error = %e,
                    "Failed to activate session log"
                );
                return;
            }
        };

        if let Err(e) = file.write_all(&clean) {
            warn!(
                connection_id = %connection_id,
                error = %e,
                "Failed to write session log"
            );
        }
        // Flush immediately so data is visible on disk (important on Windows).
        if let Err(e) = file.flush() {
            warn!(
                connection_id = %connection_id,
                error = %e,
                "Failed to flush session log"
            );
        }
    }

    /// Close and flush the log file for a disconnected session.
    ///
    /// If the entry was still [`LogEntry::Pending`] (no data ever written),
    /// no file exists on disk and nothing needs to be cleaned up.
    pub fn close_log(&mut self, connection_id: &str) {
        match self.entries.remove(connection_id) {
            Some(LogEntry::Active(mut file)) => {
                if let Err(e) = file.flush() {
                    warn!(
                        connection_id = %connection_id,
                        error = %e,
                        "Failed to flush session log"
                    );
                }
                info!(connection_id = %connection_id, "Session log closed");
            }
            Some(LogEntry::Pending(_)) => {
                info!(
                    connection_id = %connection_id,
                    "Session log closed (no data written, no file created)"
                );
            }
            None => {}
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
                if byte == 0x1B {
                    // ESC interrupts the current CSI sequence (ECMA-48 §5.4).
                    state = StripState::Escape;
                } else if byte == b'\n' || byte == b'\r' || byte == b'\t' {
                    // C0 controls are "executed" even mid-sequence (ECMA-48).
                    out.push(byte);
                } else if (0x40..=0x7E).contains(&byte) {
                    // Final byte ends the CSI sequence.
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
                } else if byte == b'\n' {
                    // Newline in an unterminated OSC — reset parser.
                    state = StripState::Normal;
                    out.push(byte);
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
