use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{Datelike, Local, Timelike};
use tracing::{info, warn};

use crate::private_fs;

/// A log entry is either waiting for its first write (Pending) or actively
/// writing to a file (Active).  Pending entries never touch the filesystem,
/// so failed connections leave no empty log files behind.
enum LogEntry {
    /// The name the log is to get, but no file created yet. The file may
    /// end up with a number after the name: see [`create_log_file`].
    Pending(PathBuf),
    /// File created on first write.
    Active(File),
}

/// How many more logs may share a name, as `_1` to `_999` after it.
const MAX_SAME_NAME: u16 = 999;

/// Create the log file for `base`: under that name, or with `_1`, `_2`, …
/// before the extension when the name is taken. Returns the file and the
/// name it got.
///
/// The name is predictable (session name and minute), and the log directory
/// may be one that others can write to. So the file is never opened if it is
/// already there: it is created, by a call that fails when anything holds the
/// name, a symlink included, and as 0600 on Unix from the start. Terminal
/// output therefore never goes into a file someone else put there, or through
/// a link to a file elsewhere, and on Unix the log is never open to others,
/// not even between creating it and restricting it. On Windows it gets its
/// access list from the log directory, so there it is as private as the
/// directory is. Two sessions with the same name that connect in the same
/// minute get a file each this way, too.
fn create_log_file(base: &Path) -> io::Result<(File, PathBuf)> {
    let stem = base.file_stem().unwrap_or_default().to_string_lossy();
    let extension = base.extension().map(|ext| ext.to_string_lossy());
    for number in 0..=MAX_SAME_NAME {
        let path = match (number, &extension) {
            (0, _) => base.to_path_buf(),
            (_, Some(ext)) => base.with_file_name(format!("{stem}_{number}.{ext}")),
            (_, None) => base.with_file_name(format!("{stem}_{number}")),
        };
        match private_fs::create_private_append_file(&path) {
            Ok(file) => return Ok((file, path)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many log files with this name",
    ))
}

/// Longest session name a log filename takes, in bytes. It keeps a long
/// session name from making a filename longer than the 255 bytes file systems
/// allow, with room for the rest of a format in plain letters and the number
/// after a taken name. A format can still be too long by itself, by holding
/// `{name}` more than twice or many letters beyond ASCII; the log then cannot
/// be created and is given up.
const MAX_LOG_NAME_BYTES: usize = 120;

/// A session name as it goes into a log filename: letters, digits and `-_.`
/// only, everything else as `_`. A dot is kept only between other characters
/// and never next to another dot, so a name cannot make the filename start
/// with a dot (a hidden file on Unix), run into the format's own dot as `..`,
/// which [`SessionLogManager::log_path`] refuses, or end in a dot. A name
/// that is empty becomes `session`, and one that is too long is cut.
fn log_name(session_name: &str) -> String {
    let mut name = String::new();
    for c in session_name.chars() {
        let c = match c {
            '.' if name.is_empty() || name.ends_with('.') => '_',
            c if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') => c,
            _ => '_',
        };
        if name.len() + c.len_utf8() > MAX_LOG_NAME_BYTES {
            break;
        }
        name.push(c);
    }
    if name.ends_with('.') {
        name.pop();
        name.push('_');
    }
    if name.is_empty() {
        name.push_str("session");
    }
    name
}

/// Per-connection state for the `ANSI` escape sequence stripper.
///
/// Stored alongside each log entry so that escape sequences split across
/// consecutive `write_log` calls are handled correctly.  Without this,
/// Windows `ConPTY` output — which frequently splits sequences across read
/// boundaries — causes the parser to lose track of in-progress sequences
/// and either drop text or emit garbage.
#[derive(Clone, Copy, Default)]
struct AnsiStripperState {
    state: StripState,
    /// Bytes consumed in a non-Normal state without seeing a terminator.
    /// Used as a safety valve: if we exceed [`Self::MAX_STUCK_BYTES`] we
    /// reset to Normal to avoid silently swallowing all subsequent output.
    stuck_bytes: usize,
}

impl AnsiStripperState {
    /// Maximum bytes we'll consume in a single non-Normal run before
    /// assuming the sequence is malformed and resetting.
    const MAX_STUCK_BYTES: usize = 4096;
}

/// Manages log file handles for active terminal sessions.
pub struct SessionLogManager {
    /// Map of `connection_id` -> (log entry, `ANSI` stripper state).
    entries: HashMap<String, (LogEntry, AnsiStripperState)>,
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
    ///
    /// [`write_log`]: Self::write_log
    pub fn open_log(&mut self, connection_id: &str, session_name: &str) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }

        // A missing log directory is created owner-only on Unix (0700 is
        // part of the create), so it is never open to others.
        private_fs::create_private_dir(&self.log_dir)
            .map_err(|e| format!("Failed to create log directory: {e}"))?;
        // One that was already there is restricted as before, as far as it
        // can be. A directory that is not the user's, or on a file system
        // without Unix modes, is still used: each log file is created
        // owner-only on its own.
        let _ = private_fs::restrict_dir_to_owner(&self.log_dir);

        // The log stays in the log directory because its name holds no path
        // separator (see `log_path`), and it is never written through a link
        // because it is created rather than opened (see `create_log_file`).
        // Neither covers the log directory itself: someone able to replace
        // it, or a directory above it, with a link decides where it leads.
        let path = self.log_path(session_name)?;

        info!(
            connection_id = %connection_id,
            path = %path.display(),
            "Session log prepared (file created on first write)"
        );
        self.entries.insert(
            connection_id.to_string(),
            (LogEntry::Pending(path), AnsiStripperState::default()),
        );
        Ok(())
    }

    /// Write terminal output to the log file, stripping `ANSI` escape sequences
    /// so the log contains only readable text.
    ///
    /// Creates the log file on the first call (lazy open).  The `ANSI` stripper
    /// state is preserved across calls so that escape sequences split across
    /// read boundaries (common with Windows `ConPTY`) are handled correctly.
    ///
    /// A log whose file cannot be created is given up: the session goes on
    /// without one, and the reason is logged once rather than on every
    /// output. Trying again each time would also mean up to a thousand
    /// failed creates per output where every name is taken, under the lock
    /// all sessions share.
    pub fn write_log(&mut self, connection_id: &str, data: &[u8]) {
        let Some((entry, stripper)) = self.entries.get_mut(connection_id) else {
            return;
        };

        // Run the stripper with per-connection persistent state.
        let clean = strip_ansi_stateful(stripper, data);
        if clean.is_empty() {
            return;
        }

        // Promote Pending → Active on first real data.
        if let LogEntry::Pending(base) = entry {
            match create_log_file(base) {
                Ok((file, path)) => {
                    info!(
                        connection_id = %connection_id,
                        path = %path.display(),
                        "Session log file created"
                    );
                    *entry = LogEntry::Active(file);
                }
                Err(e) => {
                    warn!(
                        connection_id = %connection_id,
                        path = %base.display(),
                        error = %e,
                        "Failed to create session log file; this session is not logged"
                    );
                    self.entries.remove(connection_id);
                    return;
                }
            }
        }
        let LogEntry::Active(file) = entry else {
            return;
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

    /// Whether a log is open for this connection.
    #[cfg(test)]
    pub fn has_log(&self, connection_id: &str) -> bool {
        self.entries.contains_key(connection_id)
    }

    /// Close and flush the log file for a disconnected session.
    ///
    /// If the entry was still [`LogEntry::Pending`] (no data ever written),
    /// no file exists on disk and nothing needs to be cleaned up.
    pub fn close_log(&mut self, connection_id: &str) {
        match self.entries.remove(connection_id) {
            Some((LogEntry::Active(mut file), _)) => {
                if let Err(e) = file.flush() {
                    warn!(
                        connection_id = %connection_id,
                        error = %e,
                        "Failed to flush session log"
                    );
                }
                info!(connection_id = %connection_id, "Session log closed");
            }
            Some((LogEntry::Pending(_), _)) => {
                info!(
                    connection_id = %connection_id,
                    "Session log closed (no data written, no file created)"
                );
            }
            None => {}
        }
    }

    /// The path a log for `session_name` is to get, from the filename format
    /// and the current time. Whether the name is free is settled only when
    /// the file is created: see [`create_log_file`].
    fn log_path(&self, session_name: &str) -> Result<PathBuf, String> {
        let now = Local::now();
        let safe_name = log_name(session_name);

        let mut filename = self
            .filename_format
            .replace("{name}", &safe_name)
            .replace("{mm}", &format!("{:02}", now.minute()))
            .replace("{hh}", &format!("{:02}", now.hour()))
            .replace("{dd}", &format!("{:02}", now.day()))
            .replace("{MM}", &format!("{:02}", now.month()))
            .replace("{yy}", &format!("{:02}", now.year() % 100));

        // A session named "COM1" would otherwise have its output written to
        // a serial port on Windows; the prefix makes it an ordinary file.
        if is_windows_device_name(&filename) {
            filename.insert(0, '_');
        }

        // Reject any substituted filename that could escape the log directory,
        // including through a drive (`C:x`) or name a stream (`x:y`) on Windows.
        if filename.contains(['/', '\\', ':']) || filename.contains("..") {
            return Err("Generated log filename contains path traversal characters".to_string());
        }

        Ok(self.log_dir.join(&filename))
    }
}

/// Whether Windows takes `filename` for a device (`CON`, `NUL`, `COM1`, …)
/// rather than a file. It does so whatever the extension, and ignores
/// spaces before it. Checked on every platform, so a log keeps its name
/// when the log directory moves to another system.
fn is_windows_device_name(filename: &str) -> bool {
    let stem = filename
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let Some(number) = stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
    else {
        return false;
    };
    let mut chars = number.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some('0'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}'), None)
    )
}

/// Thread-safe wrapper registered as Tauri managed state.
pub struct SessionLogState(pub Arc<Mutex<SessionLogManager>>);

// ── ANSI escape sequence stripping ─────────────────────────────────

/// Parser states for stripping `ANSI`/VT escape sequences from terminal output.
#[derive(Clone, Copy, Debug, Default)]
enum StripState {
    /// Normal text — pass through.
    #[default]
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

/// Strip `ANSI` escape sequences using persistent parser state.
///
/// The caller-owned [`AnsiStripperState`] preserves the parser position
/// across calls so that escape sequences split across read boundaries
/// (common with Windows `ConPTY`) are handled correctly.
///
/// Handles:
/// - CSI sequences: `ESC [` ... final byte
/// - OSC sequences: `ESC ]` ... `BEL` or `ESC \`
/// - Two-character escapes: `ESC` + single byte (0x40-0x7E)
/// - Common C0 controls that xterm renders: CR, LF, TAB, BS
fn strip_ansi_stateful(ctx: &mut AnsiStripperState, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());

    for &byte in data {
        // Safety valve: if we have consumed too many bytes inside an escape
        // sequence without finding a terminator, assume the sequence is
        // malformed and reset.  This prevents a single broken sequence from
        // silently swallowing all subsequent output.  The check runs before
        // incrementing so the triggering byte is re-processed in Normal state.
        if !matches!(ctx.state, StripState::Normal) {
            if ctx.stuck_bytes >= AnsiStripperState::MAX_STUCK_BYTES {
                warn!(
                    "ANSI stripper: reset after {} bytes in {:?} state",
                    ctx.stuck_bytes, ctx.state
                );
                ctx.state = StripState::Normal;
                ctx.stuck_bytes = 0;
                // Fall through to the Normal arm below for this byte.
            } else {
                ctx.stuck_bytes += 1;
            }
        }

        match ctx.state {
            StripState::Normal => {
                ctx.stuck_bytes = 0;
                if byte == 0x1B {
                    ctx.state = StripState::Escape;
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
                b'[' => ctx.state = StripState::Csi,
                b']' => ctx.state = StripState::Osc,
                // Two-character escape: ESC + final byte — discard both.
                0x40..=0x7E => {
                    ctx.state = StripState::Normal;
                    ctx.stuck_bytes = 0;
                }
                // Unexpected byte after ESC — drop the ESC, re-process byte.
                _ => {
                    ctx.state = StripState::Normal;
                    ctx.stuck_bytes = 0;
                    if byte >= 0x20 {
                        out.push(byte);
                    }
                }
            },
            StripState::Csi => {
                if byte == 0x1B {
                    // ESC interrupts the current CSI sequence (ECMA-48 §5.4).
                    ctx.state = StripState::Escape;
                } else if byte == b'\n' || byte == b'\r' || byte == b'\t' {
                    // C0 controls are "executed" even mid-sequence (ECMA-48).
                    out.push(byte);
                } else if (0x40..=0x7E).contains(&byte) {
                    // Final byte ends the CSI sequence.
                    ctx.state = StripState::Normal;
                    ctx.stuck_bytes = 0;
                }
                // Parameter bytes (0x30-0x3F) and intermediate bytes (0x20-0x2F)
                // are consumed silently.
            }
            StripState::Osc => {
                if byte == 0x07 {
                    // BEL terminates OSC.
                    ctx.state = StripState::Normal;
                    ctx.stuck_bytes = 0;
                } else if byte == 0x1B {
                    ctx.state = StripState::OscEscape;
                } else if byte == b'\n' {
                    // Newline in an unterminated OSC — reset parser.
                    ctx.state = StripState::Normal;
                    ctx.stuck_bytes = 0;
                    out.push(byte);
                }
                // All other bytes inside OSC are consumed.
            }
            StripState::OscEscape => {
                // Expect '\' for ST (String Terminator).
                ctx.state = StripState::Normal;
                ctx.stuck_bytes = 0;
            }
        }
    }

    out
}

/// Convenience wrapper that strips `ANSI` sequences in a single pass (no
/// persistent state).  Used only by unit tests.
#[cfg(test)]
fn strip_ansi(data: &[u8]) -> Vec<u8> {
    strip_ansi_stateful(&mut AnsiStripperState::default(), data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_device_names_are_recognised() {
        for name in [
            "CON.log",
            "con.txt",
            "NUL",
            "aux .log",
            "COM1.log",
            "lpt9.log",
            "COM\u{b9}.log",
        ] {
            assert!(is_windows_device_name(name), "{name}");
        }
        for name in [
            "CONSOLE.log",
            "COM10.log",
            "lpt9_1.log",
            "COM.log",
            "my_CON.log",
            "router_COM1.log",
            "",
        ] {
            assert!(!is_windows_device_name(name), "{name}");
        }
    }

    #[test]
    fn a_session_named_like_a_device_gets_an_ordinary_log_file() {
        let dir = std::env::temp_dir().join(format!("ss-devlog-{}", uuid::Uuid::new_v4()));
        let mgr = SessionLogManager::new(true, dir.clone(), "{name}.log".to_string());
        assert_eq!(mgr.log_path("COM1").unwrap(), dir.join("_COM1.log"));
        assert_eq!(mgr.log_path("router").unwrap(), dir.join("router.log"));
    }

    /// A manager logging into a new directory under the temp directory,
    /// which the caller removes.
    fn manager() -> (SessionLogManager, PathBuf) {
        let dir = std::env::temp_dir().join(format!("ss-sessionlog-{}", uuid::Uuid::new_v4()));
        let mgr = SessionLogManager::new(true, dir.clone(), "{name}.log".to_string());
        (mgr, dir)
    }

    #[test]
    fn no_file_is_created_before_the_first_output() {
        let (mut mgr, dir) = manager();
        mgr.open_log("a", "router").unwrap();
        assert!(dir.is_dir());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        // Escape sequences alone are no output.
        mgr.write_log("a", b"\x1b[0m");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        mgr.write_log("a", b"hello\n");
        mgr.write_log("a", b"again\n");
        mgr.close_log("a");
        assert_eq!(
            std::fs::read(dir.join("router.log")).unwrap(),
            b"hello\nagain\n"
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sessions_with_the_same_name_get_a_file_each() {
        let (mut mgr, dir) = manager();
        // Both are prepared before either has a file.
        mgr.open_log("a", "router").unwrap();
        mgr.open_log("b", "router").unwrap();
        mgr.write_log("b", b"second session\n");
        mgr.write_log("a", b"first session\n");
        mgr.write_log("b", b"more\n");
        mgr.close_log("a");
        mgr.close_log("b");
        assert_eq!(
            std::fs::read(dir.join("router.log")).unwrap(),
            b"second session\nmore\n"
        );
        assert_eq!(
            std::fs::read(dir.join("router_1.log")).unwrap(),
            b"first session\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_already_there_is_left_alone() {
        let (mut mgr, dir) = manager();
        mgr.open_log("a", "router").unwrap();
        // Put there after the log was prepared, as someone else who can
        // write to the directory might; a directory holds a name as well.
        std::fs::write(dir.join("router.log"), b"not ours").unwrap();
        std::fs::create_dir(dir.join("router_1.log")).unwrap();
        mgr.write_log("a", b"output\n");
        mgr.close_log("a");
        assert_eq!(std::fs::read(dir.join("router.log")).unwrap(), b"not ours");
        assert_eq!(
            std::fs::read(dir.join("router_2.log")).unwrap(),
            b"output\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_symlink_at_the_log_path_is_not_written_through() {
        let (mut mgr, dir) = manager();
        mgr.open_log("a", "router").unwrap();
        let elsewhere = std::env::temp_dir().join(format!("ss-elsewhere-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&elsewhere).unwrap();
        let existing = elsewhere.join("existing");
        std::fs::write(&existing, b"untouched").unwrap();
        let missing = elsewhere.join("missing");
        // One link to a file that is there and one to a file that would be
        // created, at the first two names the log would take.
        for (name, target) in [("router.log", &existing), ("router_1.log", &missing)] {
            let link = dir.join(name);
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &link).unwrap();
            #[cfg(windows)]
            if std::os::windows::fs::symlink_file(target, &link).is_err() {
                // Creating a symlink takes Developer Mode or an elevated process.
                eprintln!("skipped: this process may not create symbolic links");
                let _ = std::fs::remove_dir_all(&dir);
                let _ = std::fs::remove_dir_all(&elsewhere);
                return;
            }
        }

        mgr.write_log("a", b"output\n");
        mgr.close_log("a");

        assert_eq!(std::fs::read(&existing).unwrap(), b"untouched");
        assert!(!missing.exists());
        assert_eq!(
            std::fs::read(dir.join("router_2.log")).unwrap(),
            b"output\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&elsewhere).unwrap();
    }

    #[test]
    fn a_log_is_given_up_when_every_name_is_taken() {
        let (mut mgr, dir) = manager();
        mgr.open_log("a", "router").unwrap();
        std::fs::write(dir.join("router.log"), b"").unwrap();
        for number in 1..=MAX_SAME_NAME {
            std::fs::write(dir.join(format!("router_{number}.log")), b"").unwrap();
        }
        let taken = usize::from(MAX_SAME_NAME) + 1;
        mgr.write_log("a", b"output\n");
        assert!(!mgr.has_log("a"));
        // The names are not gone through again on the next output: freeing
        // one now changes nothing.
        std::fs::remove_file(dir.join("router_7.log")).unwrap();
        mgr.write_log("a", b"more\n");
        let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert_eq!(entries.len(), taken - 1);
        for entry in entries {
            assert_eq!(entry.unwrap().metadata().unwrap().len(), 0);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_session_name_cannot_misshape_the_filename() {
        for (session, expected) in [
            ("admin@core-sw1.example.com", "admin_core-sw1.example.com"),
            ("a..b", "a._b"),
            ("..", "__"),
            (".ssh", "_ssh"),
            ("trailing.", "trailing_"),
            ("a/b\\c:d", "a_b_c_d"),
            ("", "session"),
            ("größe", "größe"),
        ] {
            assert_eq!(log_name(session), expected, "{session:?}");
        }
        // Cut at a character boundary, and not left ending in a dot.
        let long = log_name(&"ä.".repeat(200));
        assert!(long.len() <= MAX_LOG_NAME_BYTES, "{}", long.len());
        assert!(long.len() > MAX_LOG_NAME_BYTES - 4, "{}", long.len());
        assert!(!long.ends_with('.'));

        // Each of these names got no log at all, or a hidden one.
        let (mgr, dir) = manager();
        for (session, expected) in [
            ("a..b", "a._b.log"),
            ("..", "__.log"),
            ("trailing.", "trailing_.log"),
            (".ssh", "_ssh.log"),
            ("", "session.log"),
        ] {
            assert_eq!(mgr.log_path(session).unwrap(), dir.join(expected));
        }
    }

    #[test]
    fn a_log_that_cannot_be_created_is_given_up() {
        let (mut mgr, dir) = manager();
        mgr.open_log("a", "router").unwrap();
        std::fs::remove_dir(&dir).unwrap();
        mgr.write_log("a", b"output\n");
        assert!(!mgr.has_log("a"));
        // Not tried again, even once the directory is back.
        std::fs::create_dir(&dir).unwrap();
        mgr.write_log("a", b"more\n");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        mgr.close_log("a");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_new_log_and_its_directory_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let (mut mgr, dir) = manager();
        mgr.open_log("a", "router").unwrap();
        mgr.write_log("a", b"output\n");
        mgr.close_log("a");
        for path in [dir.clone(), dir.join("router.log")] {
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o077,
                0,
                "{}: group/other bits set: {mode:o}",
                path.display()
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_format_naming_a_drive_or_stream_is_refused() {
        let dir = std::env::temp_dir().join(format!("ss-colon-{}", uuid::Uuid::new_v4()));
        for format in ["C:{name}.log", "{name}.log:x"] {
            let mgr = SessionLogManager::new(true, dir.clone(), format.to_string());
            assert!(mgr.log_path("router").is_err(), "{format}");
        }
    }

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

    #[test]
    fn stateful_csi_split_across_calls() {
        // Simulate a CSI sequence split across two read chunks.
        let mut ctx = AnsiStripperState::default();
        let out1 = strip_ansi_stateful(&mut ctx, b"hello\x1b[1;34");
        let out2 = strip_ansi_stateful(&mut ctx, b"mblue\x1b[0m");
        assert_eq!(out1, b"hello");
        assert_eq!(out2, b"blue");
    }

    #[test]
    fn stateful_osc_split_across_calls() {
        // OSC title change split: first chunk has ESC], second has the rest.
        let mut ctx = AnsiStripperState::default();
        let out1 = strip_ansi_stateful(&mut ctx, b"before\x1b]0;my ti");
        let out2 = strip_ansi_stateful(&mut ctx, b"tle\x07after");
        assert_eq!(out1, b"before");
        assert_eq!(out2, b"after");
    }

    #[test]
    fn stateful_esc_at_chunk_boundary() {
        // ESC is the very last byte of a chunk.
        let mut ctx = AnsiStripperState::default();
        let out1 = strip_ansi_stateful(&mut ctx, b"text\x1b");
        let out2 = strip_ansi_stateful(&mut ctx, b"[0mmore");
        assert_eq!(out1, b"text");
        assert_eq!(out2, b"more");
    }
}
