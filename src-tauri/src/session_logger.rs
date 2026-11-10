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
    Active {
        file: File,
        /// Whether the last write to the file failed: see [`write_text`].
        after_failed_write: bool,
    },
    /// The log could not even be prepared when the session connected. Kept
    /// so that the session's first output says so in its tab, as it does
    /// for a file that cannot be created: the readers hold output back
    /// until the tab has said that it is listening.
    Unavailable,
}

/// Called with the connection ID of a session whose log is given up.
type GivenUpNotifier = Box<dyn Fn(&str) + Send>;

/// The event that tells a session's tab that the session is not logged,
/// although session logging is on. It carries nothing: the reason may name
/// paths and stays in the application log.
pub fn not_logged_event(connection_id: &str) -> String {
    format!("terminal-log-failed-{connection_id}")
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
/// name, a symlink included, and for its owner alone from the start: as 0600
/// on Unix, and on Windows with an access list of its own, for the current
/// user, rather than the log directory's. Terminal output therefore never
/// goes into a file someone else put there, or through a link to a file
/// elsewhere, and the log is never open to others, not even between creating
/// it and restricting it. The exception is a log directory on a network
/// share on Windows, where a log gets the directory's access list and is as
/// private as the share makes it. Two sessions with the same name that
/// connect in the same minute get a file each this way, too.
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
// Four facts about a session's output that do not depend on one another.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Default)]
struct AnsiStripperState {
    state: StripState,
    /// Characters consumed in a non-Normal state without seeing a
    /// terminator. Used as a safety valve: if we exceed
    /// [`Self::MAX_STUCK_CHARS`] we reset to Normal to avoid silently
    /// swallowing all subsequent output.
    ///
    /// This is for a sequence that was cut short or is broken. It is no
    /// limit on what a host can keep out of a log: what a host puts inside
    /// escape sequences, each with its end or one after the other without,
    /// is not in the log, as it is not on the screen.
    stuck_chars: usize,
    /// The start of a UTF-8 sequence that the last chunk ended in the
    /// middle of: the first `pending_len` bytes. The next chunk is read
    /// after them, so a character split across two reads is still one
    /// character.
    pending: [u8; Self::MAX_PENDING],
    pending_len: usize,
    /// Whether the application log has been told that a sequence without an
    /// end was given up. It is told once for a session: a host can send as
    /// many such sequences as it likes.
    valve_reported: bool,
    /// A CR that has not been written yet: what follows it decides what it
    /// becomes. See [`Self::emit`].
    pending_cr: bool,
    /// Whether the line the log is on holds anything yet. A CR at the start
    /// of a line has nothing to go back over.
    line_has_text: bool,
    /// What `line_has_text` was when the call under way began: what holds
    /// again when a backspace has removed all that the call added.
    line_had_text: bool,
}

impl AnsiStripperState {
    /// Maximum characters we'll consume in a single non-Normal run before
    /// assuming the sequence is malformed and resetting.
    const MAX_STUCK_CHARS: usize = 4096;

    /// The most bytes a UTF-8 sequence can have and still lack one.
    const MAX_PENDING: usize = 3;
}

/// Manages log file handles for active terminal sessions.
pub struct SessionLogManager {
    /// Map of `connection_id` -> (log entry, `ANSI` stripper state).
    entries: HashMap<String, (LogEntry, AnsiStripperState)>,
    pub enabled: bool,
    pub log_dir: PathBuf,
    pub filename_format: String,
    /// Tells the window that a session goes on without a log: see
    /// [`Self::notify_when_given_up`].
    on_given_up: Option<GivenUpNotifier>,
}

impl SessionLogManager {
    pub fn new(enabled: bool, log_dir: PathBuf, filename_format: String) -> Self {
        Self {
            entries: HashMap::new(),
            enabled,
            log_dir,
            filename_format,
            on_given_up: None,
        }
    }

    /// Have `notify` called with the connection ID of each session whose log
    /// is given up, so that the window can say that the session is not
    /// logged; the application log alone is not where a user looks. It is
    /// called once per session, on the session's first output, which is when
    /// the log would have been created. That goes for a log that could not
    /// be prepared at connect as well. By then the tab is listening, as the
    /// readers hold output back until it says so. They give up waiting after
    /// a few seconds, though: a tab that is later than that can miss the
    /// notice, as it misses the output.
    ///
    /// `notify` runs inside [`write_log`], and so under the lock all sessions
    /// share: it must not wait for anything, nor use the logger. Whatever
    /// takes time, such as sending an event, it has to hand on.
    ///
    /// [`write_log`]: Self::write_log
    pub fn notify_when_given_up(&mut self, notify: impl Fn(&str) + Send + 'static) {
        self.on_given_up = Some(Box::new(notify));
    }

    /// Prepare a log for a new session connection.
    ///
    /// Validates the path and stores it as [`LogEntry::Pending`].  The actual
    /// file is created lazily on the first [`write_log`] call, so failed
    /// connections never leave empty log files on disk.
    ///
    /// A log that cannot be prepared is an error, and the session goes on
    /// without one. It is stored as [`LogEntry::Unavailable`], for the
    /// first output to report: see [`Self::notify_when_given_up`].
    ///
    /// [`write_log`]: Self::write_log
    pub fn open_log(&mut self, connection_id: &str, session_name: &str) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }

        let (entry, result) = match self.prepare_log(connection_id, session_name) {
            Ok(path) => (LogEntry::Pending(path), Ok(())),
            Err(e) => (LogEntry::Unavailable, Err(e)),
        };
        self.entries.insert(
            connection_id.to_string(),
            (entry, AnsiStripperState::default()),
        );
        result
    }

    /// Make sure the log directory is there and work out the name the log
    /// of this session is to get.
    fn prepare_log(&self, connection_id: &str, session_name: &str) -> Result<PathBuf, String> {
        // A missing log directory is created for its owner alone (0700 on
        // Unix, an access list of its own on Windows, as part of the create),
        // so it is never open to others.
        private_fs::create_private_dir(&self.log_dir)
            .map_err(|e| format!("Failed to create log directory: {e}"))?;
        // One that was already there is restricted as before on Unix, as
        // far as it can be, and left as it is on Windows. A directory that
        // is not the user's, or on a file system without Unix modes, is
        // still used: each log file is created owner-only on its own. A
        // directory with a link at its name is still used as well, but the
        // link is not followed to restrict what it leads to, since the
        // directory may be where others can put one. That goes for the
        // default directory too, as it does for the application log. Said
        // in the application log for each session, as the line for a
        // network share is, since nothing else tells the user.
        if let Err(e) = private_fs::restrict_dir_to_owner_nofollow(&self.log_dir) {
            info!(
                connection_id = %connection_id,
                error = %e,
                "The session log directory could not be restricted to its owner: \
                 others may see the names of the logs in it, but cannot open them"
            );
        }

        // The log stays in the log directory because its name holds no path
        // separator (see `log_path`), and it is never written through a link
        // because it is created rather than opened (see `create_log_file`).
        // Neither covers the log directory itself: someone able to replace
        // it, or a directory above it, with a link decides where it leads.
        let path = self.log_path(session_name)?;

        // Said in the application log, as nothing else tells the user.
        if private_fs::on_network_share(&path) {
            info!(
                connection_id = %connection_id,
                "The session log directory is on a network share: the log gets \
                 the access list of the directory there, not one of its own"
            );
        }

        info!(
            connection_id = %connection_id,
            path = %path.display(),
            "Session log prepared (file created on first write)"
        );
        Ok(path)
    }

    /// Write terminal output to the log file, stripping `ANSI` escape sequences
    /// and control characters so the log contains only readable text: see
    /// [`strip_ansi_stateful`] for what is left.
    ///
    /// Creates the log file on the first call (lazy open).  The `ANSI` stripper
    /// state is preserved across calls so that escape sequences split across
    /// read boundaries (common with Windows `ConPTY`) are handled correctly.
    ///
    /// A log whose file cannot be created is given up: the session goes on
    /// without one, and the reason is logged once rather than on every
    /// output. Trying again each time would also mean up to a thousand
    /// failed creates per output where every name is taken, under the lock
    /// all sessions share. The window is told as well, once: see
    /// [`Self::notify_when_given_up`].
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
        let given_up = match entry {
            LogEntry::Active { .. } => false,
            LogEntry::Pending(base) => match create_log_file(base) {
                Ok((file, path)) => {
                    info!(
                        connection_id = %connection_id,
                        path = %path.display(),
                        "Session log file created"
                    );
                    *entry = LogEntry::Active {
                        file,
                        after_failed_write: false,
                    };
                    false
                }
                Err(e) => {
                    warn!(
                        connection_id = %connection_id,
                        path = %base.display(),
                        error = %e,
                        "Failed to create session log file; this session is not logged"
                    );
                    true
                }
            },
            // The reason was logged when the session connected.
            LogEntry::Unavailable => true,
        };
        if given_up {
            self.entries.remove(connection_id);
            if let Some(notify) = &self.on_given_up {
                notify(connection_id);
            }
            return;
        }
        let LogEntry::Active {
            file,
            after_failed_write,
        } = entry
        else {
            return;
        };

        if let Err(e) = write_text(file, after_failed_write, &clean) {
            warn!(
                connection_id = %connection_id,
                error = %e,
                "Failed to write session log"
            );
        }
    }

    /// Whether a log is open, or prepared, for this connection.
    #[cfg(test)]
    pub fn has_log(&self, connection_id: &str) -> bool {
        matches!(
            self.entries.get(connection_id),
            Some((LogEntry::Pending(_) | LogEntry::Active { .. }, _))
        )
    }

    /// Close and flush the log file for a disconnected session.
    ///
    /// If the entry was still [`LogEntry::Pending`] (no data ever written),
    /// no file exists on disk and nothing needs to be cleaned up.
    pub fn close_log(&mut self, connection_id: &str) {
        match self.entries.remove(connection_id) {
            Some((LogEntry::Active { mut file, .. }, _)) => {
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
            Some((LogEntry::Unavailable, _)) | None => {}
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

/// Write `text`, which the stripper returned, to a log, and flush it so that
/// it is visible on disk at once (important on Windows).
///
/// A write that fails, as on a full disk, may have put part of its text into
/// the file all the same: up to the CR of a CR LF, or up to the middle of a
/// character. What the next write adds would then follow a CR without its
/// LF, which a terminal writes over the line, or the first bytes of a
/// character. So the write after a failed one starts with a line feed.
/// `after_failed_write` says whether the last one failed, and is set here.
fn write_text<W: Write>(log: &mut W, after_failed_write: &mut bool, text: &str) -> io::Result<()> {
    let result = (|| {
        if *after_failed_write {
            log.write_all(b"\n")?;
        }
        log.write_all(text.as_bytes())?;
        log.flush()
    })();
    *after_failed_write = result.is_err();
    result
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
    /// Saw ESC and one or more intermediate bytes (0x20-0x2F), as in
    /// `ESC ( B`: consume more of them, then the final byte (0x30-0x7E).
    EscapeIntermediate,
    /// A control string: OSC (ESC ] ...), DCS (ESC P), SOS (ESC X), PM
    /// (ESC ^) or APC (ESC _). Consume until ST (ESC \) or BEL. The ESC of
    /// an ST leaves the string for [`StripState::Escape`], where the `\`
    /// is the end of a two-character escape; any other escape that ends a
    /// string is read there as well.
    ControlString,
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
/// - The other control strings, DCS, SOS, PM and APC (`ESC P`, `ESC X`,
///   `ESC ^`, `ESC _`), read as OSC is
/// - Other escapes: `ESC`, any intermediate bytes (0x20-0x2F) and a final
///   byte (0x30-0x7E), such as `ESC ( B` and `ESC 7`
/// - The C1 control characters, which a terminal reads as `ESC` and a
///   letter: CSI (U+009B), OSC (U+009D) and the other control strings open
///   their sequences, and ST (U+009C) ends a string
/// - CAN and SUB, which abort a sequence
/// - Common C0 controls that xterm renders: CR, LF, TAB, BS
/// - What the terminal moves to the next line for, as a line feed: VT, FF,
///   and IND and NEL as `ESC D`, `ESC E`, U+0084 or U+0085
/// - A CR that no LF follows, as a line feed: see [`AnsiStripperState::emit`]
/// - Characters that reorder or hide text: see [`is_hidden`]
///
/// The output is read as UTF-8 first, as the terminal reads it, and only
/// then stripped. The C1 control characters (U+0080–U+009F) are two bytes
/// each in UTF-8, and their second byte is one that ordinary characters end
/// in as well (`é` is C3 A9, U+009B is C2 9B), so they cannot be told apart
/// byte by byte. Bytes that are not UTF-8 become U+FFFD, as they do in the
/// terminal.
///
/// What is returned holds no control character but CR, LF and TAB: not the
/// rest of C0, not DEL and no C1 character, whatever the input and wherever
/// it is split. That it is a `String` keeps a stray byte from being one of
/// them, too. A CR is in it only as part of CR LF, so nothing in a log goes
/// back over what the log holds before it, and it holds none of the
/// characters that [`is_hidden`] lists. A log shown in a terminal that reads
/// UTF-8 therefore prints as the text it is.
fn strip_ansi_stateful(ctx: &mut AnsiStripperState, data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len());
    ctx.line_had_text = ctx.line_has_text;

    // Read the chunk after what the last one left unfinished.
    let joined;
    let mut rest = if ctx.pending_len == 0 {
        data
    } else {
        let pending = ctx.pending.get(..ctx.pending_len).unwrap_or_default();
        joined = [pending, data].concat();
        ctx.pending_len = 0;
        joined.as_slice()
    };

    loop {
        let error = match std::str::from_utf8(rest) {
            Ok(text) => {
                ctx.strip_str(text, &mut out);
                break;
            }
            Err(error) => error,
        };
        let (valid, after) = rest.split_at(error.valid_up_to());
        // `from_utf8` has just said that `valid` is UTF-8.
        ctx.strip_str(std::str::from_utf8(valid).unwrap_or_default(), &mut out);
        let Some(invalid_len) = error.error_len() else {
            // The chunk ends inside a character: the next one may finish it.
            // A session that ends there loses those bytes, which were no
            // character yet.
            if let Some(pending) = ctx.pending.get_mut(..after.len()) {
                pending.copy_from_slice(after);
                ctx.pending_len = after.len();
            }
            break;
        };
        ctx.strip_char(char::REPLACEMENT_CHARACTER, &mut out);
        rest = after.get(invalid_len..).unwrap_or_default();
    }

    out
}

const ESC: char = '\x1b';
/// CAN and SUB: either aborts an escape sequence that is under way.
const CAN: char = '\x18';
const SUB: char = '\x1a';
/// The C1 control characters that are read here. Each is what `ESC` and the
/// letter named with it are in seven bits.
const C1_IND: char = '\u{84}'; // ESC D
const C1_NEL: char = '\u{85}'; // ESC E
const C1_DCS: char = '\u{90}'; // ESC P
const C1_SOS: char = '\u{98}'; // ESC X
const C1_CSI: char = '\u{9b}'; // ESC [
const C1_OSC: char = '\u{9d}'; // ESC ]
const C1_PM: char = '\u{9e}'; // ESC ^
const C1_APC: char = '\u{9f}'; // ESC _

/// Whether `c` is a C1 control character. A terminal acts on one wherever it
/// stands, inside an escape sequence too.
fn is_c1(c: char) -> bool {
    matches!(c, '\u{80}'..='\u{9f}')
}

/// Whether `c` is a character that is no control character and yet can make
/// a log show as something it does not hold: what the import drops from a
/// name for that reason (the bidi embedding, override and isolate controls,
/// the line and paragraph separators, zero-width space, BOM, soft hyphen and
/// other invisible characters), and the tag characters (U+E0000–U+E007F),
/// which can carry text that is not shown. Directional marks and joiners are
/// kept, as they are in a name: right-to-left and Indic text and emoji
/// sequences need them.
fn is_hidden(c: char) -> bool {
    crate::import::is_display_spoofing_char(c) || matches!(c, '\u{E0000}'..='\u{E007F}')
}

impl AnsiStripperState {
    fn strip_str(&mut self, text: &str, out: &mut String) {
        for c in text.chars() {
            self.strip_char(c, out);
        }
    }

    /// Take the next character of the output: add it to `out` if it is text,
    /// and move through an escape sequence if it is part of one.
    ///
    /// Nothing is added to `out` but through [`Self::emit`], and nothing is
    /// handed to that but CR, LF, TAB and, in [`Self::ground`], characters
    /// that are no control characters.
    fn strip_char(&mut self, c: char, out: &mut String) {
        // Safety valve: if we have consumed too many characters inside an
        // escape sequence without finding a terminator, assume the sequence
        // is malformed and reset.  This prevents a single broken sequence
        // from silently swallowing all subsequent output.  The check runs
        // before incrementing so the triggering character is re-processed in
        // Normal state.
        if !matches!(self.state, StripState::Normal) {
            if self.stuck_chars >= Self::MAX_STUCK_CHARS {
                if !self.valve_reported {
                    self.valve_reported = true;
                    warn!(
                        "ANSI stripper: reset after {} characters in {:?} state \
                         (said once for a session)",
                        self.stuck_chars, self.state
                    );
                }
                // Fall through to the Normal arm below for this character.
                self.state = StripState::Normal;
            } else {
                self.stuck_chars += 1;
            }
        }

        match self.state {
            StripState::Normal => {
                // Here and nowhere else: a sequence that goes on in another,
                // as a CSI does that a C1 CSI ends, counts on.
                self.stuck_chars = 0;
                self.ground(c, out);
            }
            StripState::Escape => match c {
                '[' => self.state = StripState::Csi,
                // OSC, DCS, SOS, PM and APC.
                ']' | 'P' | 'X' | '^' | '_' => self.state = StripState::ControlString,
                // IND and NEL: the terminal moves to the next line.
                'D' | 'E' => {
                    self.state = StripState::Normal;
                    self.emit('\n', out);
                }
                '\x20'..='\x2f' => self.state = StripState::EscapeIntermediate,
                // Two-character escape: ESC + final byte — discard both.
                '\x30'..='\x7e' => self.state = StripState::Normal,
                // DEL is ignored, and the escape goes on.
                '\x7f' => {}
                // Unexpected character after ESC — drop the ESC and take the
                // character as one outside a sequence.
                _ => self.ground(c, out),
            },
            StripState::EscapeIntermediate => match c {
                '\x20'..='\x2f' | '\x7f' => {}
                // Final byte ends the escape.
                '\x30'..='\x7e' => self.state = StripState::Normal,
                // Anything else ends it too, and is taken as anywhere.
                _ => self.ground(c, out),
            },
            StripState::Csi => match c {
                // ESC interrupts the current CSI sequence (ECMA-48 §5.4).
                ESC => self.state = StripState::Escape,
                // C0 controls are "executed" even mid-sequence (ECMA-48):
                // those that are text in a log, and VT and FF as the line
                // feed they are anywhere. A backspace takes nothing back
                // from here.
                '\n' | '\r' | '\t' => self.emit(c, out),
                '\x0b' | '\x0c' => self.emit('\n', out),
                // Final byte ends the CSI sequence, and CAN and SUB abort
                // it.
                CAN | SUB | '\x40'..='\x7e' => self.state = StripState::Normal,
                // A C1 control ends the sequence and is taken as anywhere.
                c if is_c1(c) => self.ground(c, out),
                // Parameter bytes (0x30-0x3F), intermediate bytes (0x20-0x2F)
                // and everything else, DEL included, are consumed silently.
                _ => {}
            },
            StripState::ControlString => match c {
                // BEL terminates OSC. It is taken to end the other strings
                // as well, where a terminal reads it as part of the string:
                // what follows is then kept rather than lost.
                '\x07' | CAN | SUB => self.state = StripState::Normal,
                // ST is `ESC \`, and any other escape ends the string too.
                ESC => self.state = StripState::Escape,
                // Newline in an unterminated string — reset parser.
                '\n' => {
                    self.state = StripState::Normal;
                    self.emit(c, out);
                }
                // So does ST as a single character (U+009C), and any other
                // C1 control, which is taken as anywhere.
                c if is_c1(c) => self.ground(c, out),
                // All other characters inside a string are consumed.
                _ => {}
            },
        }
    }

    /// Take a character that is not part of an escape sequence.
    fn ground(&mut self, c: char, out: &mut String) {
        self.state = StripState::Normal;
        match c {
            ESC => self.state = StripState::Escape,
            C1_CSI => self.state = StripState::Csi,
            C1_OSC | C1_DCS | C1_SOS | C1_PM | C1_APC => self.state = StripState::ControlString,
            '\n' | '\r' | '\t' => self.emit(c, out),
            // VT, FF, IND and NEL: the terminal moves to the next line.
            '\x0b' | '\x0c' | C1_IND | C1_NEL => self.emit('\n', out),
            // Backspace — remove the last character if present, whole. Not a
            // line break, and nothing after a CR: the terminal's cursor
            // stops at the start of a line. Only what this call has added so
            // far can be removed; what an earlier one added is in the file
            // already.
            '\x08' => {
                if !self.pending_cr && !out.ends_with('\n') && out.pop().is_some() {
                    // With nothing left of what this call added, the line
                    // holds what it held before the call.
                    self.line_has_text = if out.is_empty() {
                        self.line_had_text
                    } else {
                        !out.ends_with('\n')
                    };
                }
            }
            // Every other control character is dropped: the rest of C0 (BEL,
            // …), DEL, and the rest of C1. So are the characters that are
            // none and reorder or hide text.
            c if c.is_control() || is_hidden(c) => {}
            c => self.emit(c, out),
        }
    }

    /// Add `c` to the log: CR, LF, TAB, or a character that is no control
    /// character.
    ///
    /// A CR is not written as it comes. In a terminal it puts the cursor
    /// back at the start of the line, and what follows is written over what
    /// the line held: a host could have a log show something other than
    /// what it holds, when the log is shown with `cat`. So a CR waits for
    /// what follows it, across calls too. Before an LF it is the line break
    /// CR LF and is written as it is. Before anything else it becomes a line
    /// feed, so that what would have been written over the line follows it
    /// on a line of its own; at the start of a line, where there is nothing
    /// to write over, it is dropped. Several in a row count as one, and one
    /// that nothing follows before the session ends is not written.
    fn emit(&mut self, c: char, out: &mut String) {
        if c == '\r' {
            self.pending_cr = true;
            return;
        }
        if self.pending_cr {
            self.pending_cr = false;
            if c == '\n' {
                out.push('\r');
            } else if self.line_has_text {
                out.push('\n');
            }
        }
        out.push(c);
        self.line_has_text = c != '\n';
    }
}

/// Convenience wrapper that strips `ANSI` sequences in a single pass (no
/// persistent state).  Used only by unit tests.
#[cfg(test)]
fn strip_ansi(data: &[u8]) -> String {
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

    /// Have `mgr` record the sessions it reports as not logged.
    fn record_given_up(mgr: &mut SessionLogManager) -> Arc<Mutex<Vec<String>>> {
        let reported = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&reported);
        mgr.notify_when_given_up(move |id| sink.lock().unwrap().push(id.to_string()));
        reported
    }

    /// A manager whose filename format `log_path` refuses, as a hand-edited
    /// config could hold before startup replaces it, so that no log can be
    /// prepared.
    fn manager_that_cannot_prepare() -> (SessionLogManager, PathBuf) {
        let dir = std::env::temp_dir().join(format!("ss-sessionlog-{}", uuid::Uuid::new_v4()));
        let mgr = SessionLogManager::new(true, dir.clone(), "{name}.log:x".to_string());
        (mgr, dir)
    }

    #[test]
    fn a_session_that_is_logged_is_not_reported() {
        let (mut mgr, dir) = manager();
        let reported = record_given_up(&mut mgr);
        mgr.open_log("a", "router").unwrap();
        mgr.write_log("a", b"hello\n");
        mgr.write_log("a", b"again\n");
        mgr.close_log("a");
        // Nor is one that has no log because logging is off.
        mgr.enabled = false;
        mgr.open_log("b", "router").unwrap();
        mgr.write_log("b", b"hello\n");
        mgr.close_log("b");
        assert!(reported.lock().unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_log_that_cannot_be_prepared_is_reported_on_the_first_output() {
        let (mut mgr, dir) = manager_that_cannot_prepare();
        let reported = record_given_up(&mut mgr);
        assert!(mgr.open_log("a", "router").is_err());
        assert!(!mgr.has_log("a"));
        // Not before the tab can be listening, and not for escape sequences
        // alone, which are no output.
        assert!(reported.lock().unwrap().is_empty());
        mgr.write_log("a", b"\x1b[0m");
        assert!(reported.lock().unwrap().is_empty());
        mgr.write_log("a", b"output\n");
        mgr.write_log("a", b"more\n");
        mgr.close_log("a");
        assert_eq!(*reported.lock().unwrap(), ["a"]);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_session_closed_before_any_output_is_not_reported() {
        let (mut mgr, dir) = manager_that_cannot_prepare();
        let reported = record_given_up(&mut mgr);
        assert!(mgr.open_log("a", "router").is_err());
        mgr.close_log("a");
        // Output that arrives after the close finds nothing to report.
        mgr.write_log("a", b"output\n");
        assert!(reported.lock().unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A manager that was given no notifier gives a log up all the same,
    /// without a panic: tests elsewhere build one that way.
    #[test]
    fn a_log_is_given_up_without_a_notifier() {
        let (mut mgr, dir) = manager_that_cannot_prepare();
        assert!(mgr.open_log("a", "router").is_err());
        assert!(mgr.entries.contains_key("a"));
        mgr.write_log("a", b"output\n");
        assert!(!mgr.entries.contains_key("a"));
        mgr.write_log("a", b"more\n");
        mgr.close_log("a");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// What is kept for a log that could not be prepared goes with the
    /// report, so that nothing is left to report a second time.
    #[test]
    fn a_log_that_is_reported_is_forgotten() {
        let (mut mgr, dir) = manager_that_cannot_prepare();
        let reported = record_given_up(&mut mgr);
        assert!(mgr.open_log("a", "router").is_err());
        assert!(mgr.entries.contains_key("a"));
        mgr.write_log("a", b"output\n");
        assert_eq!(*reported.lock().unwrap(), ["a"]);
        assert!(!mgr.entries.contains_key("a"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_event_is_named_after_the_session() {
        assert_eq!(not_logged_event("abc-1"), "terminal-log-failed-abc-1");
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
        let reported = record_given_up(&mut mgr);
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
        // Said once, however much output follows.
        assert_eq!(*reported.lock().unwrap(), ["a"]);
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
        let reported = record_given_up(&mut mgr);
        mgr.open_log("a", "router").unwrap();
        mgr.open_log("b", "switch").unwrap();
        std::fs::remove_dir(&dir).unwrap();
        mgr.write_log("a", b"output\n");
        assert!(!mgr.has_log("a"));
        assert_eq!(*reported.lock().unwrap(), ["a"]);
        // Not tried again, even once the directory is back.
        std::fs::create_dir(&dir).unwrap();
        mgr.write_log("a", b"more\n");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        mgr.close_log("a");
        // The other session's log can be created by now, and is: only the
        // session without a log was reported.
        mgr.write_log("b", b"output\n");
        mgr.close_log("b");
        assert_eq!(*reported.lock().unwrap(), ["a"]);
        assert_eq!(std::fs::read(dir.join("switch.log")).unwrap(), b"output\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_new_log_and_its_directory_are_owner_only() {
        let (mut mgr, dir) = manager();
        mgr.open_log("a", "router").unwrap();
        mgr.write_log("a", b"output\n");
        mgr.close_log("a");
        for path in [dir.clone(), dir.join("router.log")] {
            private_fs::assert_private(&path);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A log directory that is already there is left as it is on Windows,
    /// with the access list it got from the directory above, which may let
    /// others in. A log in it is still its owner's alone. On Unix the
    /// directory is restricted as well, and the log's own mode is what is
    /// checked.
    #[test]
    fn a_log_in_a_directory_already_there_is_owner_only() {
        let (mut mgr, dir) = manager();
        std::fs::create_dir(&dir).unwrap();
        mgr.open_log("a", "router").unwrap();
        mgr.write_log("a", b"output\n");
        mgr.close_log("a");
        let log = dir.join("router.log");
        private_fs::assert_private(&log);
        assert_eq!(std::fs::read(&log).unwrap(), b"output\n");
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
        assert_eq!(strip_ansi(b"hello world"), "hello world");
    }

    #[test]
    fn strip_csi_color() {
        // ESC[1;34m "blue" ESC[0m
        let input = b"\x1b[1;34mblue\x1b[0m";
        assert_eq!(strip_ansi(input), "blue");
    }

    #[test]
    fn strip_cursor_control() {
        // ESC[6n (cursor position request) — should be stripped entirely
        let input = b"prompt$ \x1b[6ncommand";
        assert_eq!(strip_ansi(input), "prompt$ command");
    }

    #[test]
    fn strip_bracketed_paste() {
        // ESC[?2004h and ESC[?2004l
        let input = b"\x1b[?2004htext\x1b[?2004l";
        assert_eq!(strip_ansi(input), "text");
    }

    #[test]
    fn strip_osc_title() {
        // OSC 0;title BEL
        let input = b"\x1b]0;my title\x07visible";
        assert_eq!(strip_ansi(input), "visible");
    }

    #[test]
    fn strip_osc_st() {
        // OSC terminated by ST (ESC \)
        let input = b"\x1b]0;title\x1b\\visible";
        assert_eq!(strip_ansi(input), "visible");
    }

    #[test]
    fn preserve_newlines_and_tabs() {
        let input = b"line1\r\nline2\ttab";
        assert_eq!(strip_ansi(input), "line1\r\nline2\ttab");
    }

    #[test]
    fn strip_backspace() {
        // "ab" then BS then "c" → "ac"
        let input = b"ab\x08c";
        assert_eq!(strip_ansi(input), "ac");
    }

    #[test]
    fn strip_erase_line() {
        // ESC[K (erase to end of line), ESC[J (erase display)
        let input = b"prompt\x1b[K\x1b[Jtext";
        assert_eq!(strip_ansi(input), "prompttext");
    }

    #[test]
    fn stateful_csi_split_across_calls() {
        // Simulate a CSI sequence split across two read chunks.
        let mut ctx = AnsiStripperState::default();
        let out1 = strip_ansi_stateful(&mut ctx, b"hello\x1b[1;34");
        let out2 = strip_ansi_stateful(&mut ctx, b"mblue\x1b[0m");
        assert_eq!(out1, "hello");
        assert_eq!(out2, "blue");
    }

    #[test]
    fn stateful_osc_split_across_calls() {
        // OSC title change split: first chunk has ESC], second has the rest.
        let mut ctx = AnsiStripperState::default();
        let out1 = strip_ansi_stateful(&mut ctx, b"before\x1b]0;my ti");
        let out2 = strip_ansi_stateful(&mut ctx, b"tle\x07after");
        assert_eq!(out1, "before");
        assert_eq!(out2, "after");
    }

    #[test]
    fn stateful_esc_at_chunk_boundary() {
        // ESC is the very last byte of a chunk.
        let mut ctx = AnsiStripperState::default();
        let out1 = strip_ansi_stateful(&mut ctx, b"text\x1b");
        let out2 = strip_ansi_stateful(&mut ctx, b"[0mmore");
        assert_eq!(out1, "text");
        assert_eq!(out2, "more");
    }

    #[test]
    fn del_is_dropped() {
        assert_eq!(strip_ansi(b"ab\x7fc"), "abc");
        // After ESC as well, where it used to be kept as text, and inside
        // the sequences, which go on past it.
        assert_eq!(strip_ansi(b"a\x1b\x7f[1\x7fmb"), "ab");
        assert_eq!(strip_ansi(b"a\x1b\x7f(\x7fBb"), "ab");
        assert_eq!(strip_ansi(b"\x1b[3\x7f1mred"), "red");
        assert_eq!(strip_ansi(b"\x1b]0;ti\x7ftle\x07visible"), "visible");
    }

    #[test]
    fn c1_controls_are_dropped() {
        for code in 0x80..=0x9f_u32 {
            let c1 = char::from_u32(code).unwrap();
            let expected = match c1 {
                // CSI and the control strings take what follows them along.
                C1_CSI | C1_OSC | C1_DCS | C1_SOS | C1_PM | C1_APC => "a",
                // The terminal moves to the next line.
                C1_IND | C1_NEL => "a\nb",
                _ => "ab",
            };
            let out = strip_ansi(format!("a{c1}b").as_bytes());
            assert_eq!(out, expected, "U+{code:04X}");
        }
        // Between words they leave the words.
        assert_eq!(
            strip_ansi("one\u{8d}two\u{86}three".as_bytes()),
            "onetwothree"
        );
    }

    #[test]
    fn a_c1_csi_or_osc_takes_its_sequence_along() {
        // U+009B is CSI: what moves the cursor or clears the screen when a
        // log is shown in a terminal that acts on C1 controls.
        assert_eq!(strip_ansi("\u{9b}31mred\u{9b}0m".as_bytes()), "red");
        assert_eq!(
            strip_ansi("shown\u{9b}2J\u{9b}Hfake".as_bytes()),
            "shownfake"
        );
        // U+009D is OSC, ended by BEL, by ESC \ or by U+009C (ST).
        assert_eq!(strip_ansi("\u{9d}0;title\x07visible".as_bytes()), "visible");
        assert_eq!(
            strip_ansi("\u{9d}0;title\x1b\\visible".as_bytes()),
            "visible"
        );
        assert_eq!(
            strip_ansi("\u{9d}0;title\u{9c}visible".as_bytes()),
            "visible"
        );
        assert_eq!(
            strip_ansi("\x1b]0;title\u{9c}visible".as_bytes()),
            "visible"
        );
        // A C1 control ends a sequence that is under way and counts itself.
        assert_eq!(strip_ansi("\x1b[3\u{9b}1mred".as_bytes()), "red");
        assert_eq!(strip_ansi("\x1b[3\u{8d}1mred".as_bytes()), "1mred");
        assert_eq!(strip_ansi("\x1b[3\u{85}1mred".as_bytes()), "\n1mred");
        assert_eq!(strip_ansi("\x1b]0;ti\u{9b}1mred".as_bytes()), "red");
        assert_eq!(strip_ansi("a\x1b\u{9b}1mred".as_bytes()), "ared");
    }

    #[test]
    fn text_beyond_ascii_is_kept() {
        // `é` ends in A9 and `ě` in 9B, the byte U+009B ends in.
        let text = "café ěščř €100 日本語 🙂";
        assert_eq!(strip_ansi(text.as_bytes()), text);
        assert_eq!(strip_ansi("\x1b[1mě\x1b[0m\u{9b}1mě".as_bytes()), "ěě");
    }

    #[test]
    fn a_character_split_across_calls_is_one_character() {
        // Split at every byte, text and C1 controls alike.
        let input = "aé€🙂\u{9b}31mred\u{85}\u{9d}0;t\u{9c}é".as_bytes();
        let mut ctx = AnsiStripperState::default();
        let mut out = String::new();
        for byte in input {
            out.push_str(&strip_ansi_stateful(&mut ctx, &[*byte]));
        }
        assert_eq!(out, "aé€🙂red\né");

        // The first byte of a C1 control at the end of one read.
        let mut ctx = AnsiStripperState::default();
        let out1 = strip_ansi_stateful(&mut ctx, b"text\xc2");
        let out2 = strip_ansi_stateful(&mut ctx, b"\x9b2Jmore");
        assert_eq!(out1, "text");
        assert_eq!(out2, "more");

        // A character that is never finished is no character.
        let mut ctx = AnsiStripperState::default();
        let out1 = strip_ansi_stateful(&mut ctx, b"a\xe2\x82");
        let out2 = strip_ansi_stateful(&mut ctx, b"b");
        assert_eq!(out1, "a");
        assert_eq!(out2, "\u{fffd}b");
        // Nor are empty reads the end of one.
        let mut ctx = AnsiStripperState::default();
        let out1 = strip_ansi_stateful(&mut ctx, b"\xe2");
        let out2 = strip_ansi_stateful(&mut ctx, b"");
        let out3 = strip_ansi_stateful(&mut ctx, b"\x82\xac");
        assert_eq!(
            (out1, out2, out3),
            (String::new(), String::new(), "€".to_string())
        );
    }

    #[test]
    fn bytes_that_are_not_utf8_become_the_replacement_character() {
        assert_eq!(strip_ansi(b"a\xffb"), "a\u{fffd}b");
        // A C1 control as a single byte, as an 8-bit terminal reads it, is
        // no character in UTF-8, and neither is one written too long.
        assert_eq!(strip_ansi(b"a\x9b31mb"), "a\u{fffd}31mb");
        assert_eq!(strip_ansi(b"a\x85b"), "a\u{fffd}b");
        assert_eq!(strip_ansi(b"a\xc0\x9bb"), "a\u{fffd}\u{fffd}b");
        assert_eq!(strip_ansi(b"a\xe0\x82\x9bb"), "a\u{fffd}\u{fffd}\u{fffd}b");
        // Latin-1 text from a device that does not send UTF-8.
        assert_eq!(strip_ansi(b"Gr\xf6\xdfe"), "Gr\u{fffd}\u{fffd}e");
        // Inside a sequence it is consumed with the rest.
        assert_eq!(strip_ansi(b"\x1b[3\xff1mred"), "red");
    }

    #[test]
    fn backspace_removes_a_whole_character() {
        assert_eq!(strip_ansi("aä\x08c".as_bytes()), "ac");
        assert_eq!(strip_ansi("a🙂\x08\x08c".as_bytes()), "c");
        assert_eq!(strip_ansi(b"\x08c"), "c");
        // Not a line break, which the terminal's cursor does not go back
        // over.
        assert_eq!(strip_ansi(b"ab\n\x08c"), "ab\nc");
        assert_eq!(strip_ansi(b"ab\r\n\x08\x08c"), "ab\r\nc");
        // A line that backspaces have emptied is the start of a line again
        // for a CR, unless an earlier output put text on it.
        assert_eq!(strip_ansi(b"ab\x08\x08\rX"), "X");
        assert_eq!(strip_ansi(b"a\r\nbc\x08\x08\rX"), "a\r\nX");
        assert_eq!(strip_ansi(b"ab\x08\rX"), "a\nX");
        let mut ctx = AnsiStripperState::default();
        let out1 = strip_ansi_stateful(&mut ctx, b"a");
        let out2 = strip_ansi_stateful(&mut ctx, b"b\x08\rX");
        assert_eq!((out1.as_str(), out2.as_str()), ("a", "\nX"));
        let mut ctx = AnsiStripperState::default();
        let out1 = strip_ansi_stateful(&mut ctx, b"a\r\n");
        let out2 = strip_ansi_stateful(&mut ctx, b"b\x08\rX");
        assert_eq!((out1.as_str(), out2.as_str()), ("a\r\n", "X"));
        // Nor does it do anything after a CR, at the start of the line.
        assert_eq!(strip_ansi(b"ab\r\x08c"), "ab\nc");
        assert_eq!(strip_ansi(b"ab\r\x08\nc"), "ab\r\nc");
        // Nor what an earlier output of the session put into the file.
        let mut ctx = AnsiStripperState::default();
        let out1 = strip_ansi_stateful(&mut ctx, b"ab");
        let out2 = strip_ansi_stateful(&mut ctx, b"\x08c");
        assert_eq!(out1, "ab");
        assert_eq!(out2, "c");
    }

    #[test]
    fn other_escapes_leave_nothing_behind() {
        // With a character between ESC and the last one: the character
        // sets, as most programs reset them, and DECALN.
        assert_eq!(strip_ansi(b"a\x1b(Bb"), "ab");
        assert_eq!(strip_ansi(b"\x1b(0lqk\x1b(B"), "lqk");
        assert_eq!(strip_ansi(b"a\x1b#8b"), "ab");
        assert_eq!(strip_ansi(b"a\x1b Fb"), "ab");
        // Ending in a digit or a sign: saving and restoring the cursor, and
        // the keypad modes.
        assert_eq!(strip_ansi(b"a\x1b7b\x1b8c"), "abc");
        assert_eq!(strip_ansi(b"a\x1b=b\x1b>c"), "abc");
        // Ending in a letter, as before.
        assert_eq!(strip_ansi(b"a\x1bMb\x1bcc"), "abc");
        // Split across calls.
        let mut ctx = AnsiStripperState::default();
        let out1 = strip_ansi_stateful(&mut ctx, b"a\x1b(");
        let out2 = strip_ansi_stateful(&mut ctx, b"Bb");
        assert_eq!(out1, "a");
        assert_eq!(out2, "b");
    }

    #[test]
    fn a_control_string_is_consumed_whole() {
        // DCS, SOS, PM and APC, as two characters and as one, ended by
        // ESC \, by U+009C or by BEL.
        for opener in [
            "\x1bP", "\x1bX", "\x1b^", "\x1b_", "\u{90}", "\u{98}", "\u{9e}", "\u{9f}",
        ] {
            for end in ["\x1b\\", "\u{9c}", "\x07"] {
                let input = format!("a{opener}1$r0m payload{end}b");
                assert_eq!(strip_ansi(input.as_bytes()), "ab", "{input:?}");
            }
        }
        // A line break ends one that has no end, as it does for OSC.
        assert_eq!(strip_ansi(b"a\x1bPpayload\nb"), "a\nb");
        // An escape other than ESC \ ends a string too, and is read.
        assert_eq!(strip_ansi(b"\x1b]0;t\x1b[31mred"), "red");
        assert_eq!(strip_ansi("\x1b]0;t\x1b\u{9b}31mred".as_bytes()), "red");
        assert_eq!(strip_ansi(b"\x1bPq\x1b(Bshown"), "shown");
        assert_eq!(strip_ansi(b"\x1b]0;t\x1b]1;u\x07shown"), "shown");
    }

    #[test]
    fn can_and_sub_abort_a_sequence() {
        // What follows is text again, as it is in the terminal.
        assert_eq!(strip_ansi(b"\x1b[3\x18 1mred"), " 1mred");
        assert_eq!(strip_ansi(b"\x1b[3\x1a1mred"), "1mred");
        assert_eq!(strip_ansi(b"\x1b]0;ti\x18tle"), "tle");
        assert_eq!(strip_ansi(b"\x1bPpay\x1aload"), "load");
        assert_eq!(strip_ansi(b"\x1b\x18[1m"), "[1m");
        assert_eq!(strip_ansi(b"a\x1b(\x18Bb"), "aBb");
    }

    #[test]
    fn what_the_terminal_moves_to_the_next_line_for_is_a_line_feed() {
        // VT and FF inside a CSI sequence, which goes on after them.
        assert_eq!(strip_ansi(b"a\x1b[3\x0b1mb\x1b[\x0cKc"), "a\nb\nc");
        // VT and FF, and IND and NEL as one character and as two.
        assert_eq!(strip_ansi(b"a\x0bb\x0cc"), "a\nb\nc");
        assert_eq!(strip_ansi("a\u{84}b\u{85}c".as_bytes()), "a\nb\nc");
        assert_eq!(strip_ansi(b"a\x1bDb\x1bEc"), "a\nb\nc");
    }

    /// Pieces of output to put together: what opens, continues and ends each
    /// kind of sequence, text, and parts of characters. No backspace, which
    /// only reaches back within one call.
    const PIECES: [&[u8]; 42] = [
        b"E",
        b"X",
        b"^",
        b"_",
        b"#",
        b"\x0b",
        b"\x0c",
        b"\x1a",
        b"\x84",
        b"\x98",
        b"\x9e",
        b"\x9f",
        b"\t",
        b"\xe2\x80\xae",
        b"\x1b",
        b"[",
        b"]",
        b"\\",
        b"(",
        b"P",
        b"D",
        b"1;3",
        b"m",
        b"a",
        b" ",
        b"\n",
        b"\r",
        b"\x07",
        b"\x18",
        b"\x7f",
        b"\xc2",
        b"\x9b",
        b"\x9d",
        b"\x9c",
        b"\x90",
        b"\x85",
        b"\xc3\xa9",
        b"\xe2\x82",
        b"\xac",
        b"\xf0\x9f",
        b"\x99\x82",
        b"\xff",
    ];

    #[test]
    fn a_log_is_the_same_wherever_the_output_is_split() {
        // Split in two at every byte.
        let inputs: [&[u8]; 7] = [
            "a\rb\r\nc\r\r\nd\r\x1b[Ke\n\rf\u{202e}g\r".as_bytes(),
            "aé€🙂\u{9b}31mred\u{85}\u{9d}0;t\u{9c}é".as_bytes(),
            b"a\x1b[1;34mblue\x1b[0m\x1b(B\x1b7b\x1b8\x1b]0;title\x1b\\c",
            b"a\x1bPpay\xc3\xa9load\x1b\\b\x1b_apc\x07c\x1bDd",
            b"a\xff\xc0\x9b\xe2\x82b\xf0\x9f\x99\x1b[3\xff1mc\xe2",
            b"a\x1b[3\x18b\x1b]0;t\x1ac\x7f\x0bd\x1b\x7f[1me",
            "\x1b]0;t\x1b\u{9b}31mred\u{90}q\u{9c}\u{98}s\u{9c}x".as_bytes(),
        ];
        for input in inputs {
            let whole = strip_ansi(input);
            assert!(is_log_text(&whole), "{input:?}: {whole:?}");
            for at in 0..=input.len() {
                let (first, second) = input.split_at(at);
                let mut ctx = AnsiStripperState::default();
                let mut out = strip_ansi_stateful(&mut ctx, first);
                out.push_str(&strip_ansi_stateful(&mut ctx, second));
                assert_eq!(out, whole, "{input:?} split at {at}");
            }
        }

        // Pieces put together and split by chance, the same each run.
        let mut seed: u32 = 0x9e37_79b9;
        let mut next = |below: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as usize % below
        };
        for _ in 0..2000 {
            let mut input = Vec::new();
            for _ in 0..40 {
                input.extend_from_slice(PIECES[next(PIECES.len())]);
            }
            let whole = strip_ansi(&input);
            assert!(is_log_text(&whole), "{input:?}: {whole:?}");

            let mut ctx = AnsiStripperState::default();
            let mut out = String::new();
            let mut rest = input.as_slice();
            while !rest.is_empty() {
                let (chunk, after) = rest.split_at(next(5).min(rest.len()));
                out.push_str(&strip_ansi_stateful(&mut ctx, chunk));
                rest = after;
            }
            assert_eq!(out, whole, "{input:?}");
        }
    }

    #[test]
    fn a_sequence_without_an_end_gives_way() {
        // Counted in characters, of which `ESC ]` has one more than U+009D:
        // the text comes back once the limit is reached, and not a
        // character earlier.
        let limit = AnsiStripperState::MAX_STUCK_CHARS;
        for (opener, consumed) in [("\x1b]", limit - 1), ("\u{9d}", limit)] {
            let mut input = String::from(opener);
            input.push_str(&"é".repeat(consumed));
            assert_eq!(strip_ansi(input.as_bytes()), "", "{opener:?}");
            input.push_str("éback");
            assert_eq!(strip_ansi(input.as_bytes()), "éback", "{opener:?}");
        }

        // The limit is the same wherever the output is split.
        let whole = format!("\u{9d}{}", "é".repeat(limit + 2));
        let mut ctx = AnsiStripperState::default();
        let mut out = String::new();
        for byte in whole.as_bytes() {
            out.push_str(&strip_ansi_stateful(&mut ctx, &[*byte]));
        }
        assert_eq!(out, "éé");

        // A sequence that ends in the start of the next counts on, so that
        // one after the other cannot keep all that follows out of the log:
        // the first takes its 4000 characters, and the second what is left
        // of the 4096, less the characters that open the sequences.
        for (opener, text, kept) in [
            ("\u{9b}", "1", 3905),
            ("\x1b[", "1", 3907),
            ("\u{9d}", "a", 3905),
            ("\x1bP", "a", 3907),
        ] {
            let run = format!("{opener}{}", text.repeat(4000));
            let out = strip_ansi(run.repeat(2).as_bytes());
            assert_eq!(out.len(), kept, "{opener:?}");
            assert!(out.chars().all(|c| text.starts_with(c)), "{opener:?}");
        }

        // The application log is told the first time, and that time only.
        let mut ctx = AnsiStripperState::default();
        let given_up = format!("\u{9d}{}", "é".repeat(limit + 1));
        assert!(!ctx.valve_reported);
        assert_eq!(strip_ansi_stateful(&mut ctx, given_up.as_bytes()), "é");
        assert!(ctx.valve_reported);
        assert_eq!(strip_ansi_stateful(&mut ctx, given_up.as_bytes()), "é");
        assert!(ctx.valve_reported);
    }

    /// Whether `out` is what a log may hold: no control character other than
    /// CR, LF and TAB, a CR only before an LF, and no character that
    /// reorders or hides text.
    fn is_log_text(out: &str) -> bool {
        let refused =
            |c: char| (c.is_control() && !matches!(c, '\n' | '\r' | '\t')) || is_hidden(c);
        !out.chars().any(refused) && !out.replace("\r\n", "").contains('\r')
    }

    #[test]
    fn a_carriage_return_stays_only_before_a_line_feed() {
        // What follows a CR would be written over the line in a terminal.
        // It goes on a line of its own.
        assert_eq!(strip_ansi(b"real\rFAKE"), "real\nFAKE");
        assert_eq!(strip_ansi(b"10%\r20%\r30%\r\n"), "10%\n20%\n30%\r\n");
        assert_eq!(strip_ansi(b"a\r\tb"), "a\n\tb");
        // CR LF is a line break and stays, with more than one CR too, and
        // with an escape sequence in between.
        assert_eq!(strip_ansi(b"a\r\nb\r\n"), "a\r\nb\r\n");
        assert_eq!(strip_ansi(b"a\r\r\nb"), "a\r\nb");
        assert_eq!(strip_ansi(b"a\r\x1b[K\nb"), "a\r\nb");
        assert_eq!(strip_ansi(b"a\r\x1b[Kb"), "a\nb");
        assert_eq!(strip_ansi(b"a\r\x0bb"), "a\r\nb");
        // Inside a sequence a CR counts as well.
        assert_eq!(strip_ansi(b"a\x1b[3\r1mb"), "a\nb");
        // At the start of a line there is nothing to write over.
        assert_eq!(strip_ansi(b"\rprompt"), "prompt");
        assert_eq!(strip_ansi(b"a\n\rb"), "a\nb");
        assert_eq!(strip_ansi(b"a\r\n\rb\r\n\r\nc"), "a\r\nb\r\n\r\nc");
        // One that nothing follows is not written.
        assert_eq!(strip_ansi(b"a\r"), "a");

        // Across calls: the CR waits for the next output.
        let mut ctx = AnsiStripperState::default();
        let out1 = strip_ansi_stateful(&mut ctx, b"a\r");
        let out2 = strip_ansi_stateful(&mut ctx, b"\nb\r");
        let out3 = strip_ansi_stateful(&mut ctx, b"c");
        assert_eq!(
            (out1.as_str(), out2.as_str(), out3.as_str()),
            ("a", "\r\nb", "\nc")
        );
        // The start of a line is known across calls as well.
        let mut ctx = AnsiStripperState::default();
        let out1 = strip_ansi_stateful(&mut ctx, b"a\r\n");
        let out2 = strip_ansi_stateful(&mut ctx, b"\rb");
        assert_eq!((out1.as_str(), out2.as_str()), ("a\r\n", "b"));
    }

    #[test]
    fn characters_that_reorder_or_hide_text_are_dropped() {
        // A right-to-left override, as in a file name made to read as
        // another, and the embeddings and isolates.
        assert_eq!(strip_ansi("safe\u{202e}gnp.exe".as_bytes()), "safegnp.exe");
        assert_eq!(
            strip_ansi("a\u{202a}b\u{202c}c\u{2066}d\u{2069}e".as_bytes()),
            "abcde"
        );
        // Line and paragraph separators, zero-width space, BOM, soft hyphen
        // and word joiner.
        assert_eq!(
            strip_ansi("\u{feff}a\u{2028}b\u{2029}c\u{200b}d\u{ad}e\u{2060}f".as_bytes()),
            "abcdef"
        );
        // Interlinear annotation marks and the standalone Hangul fillers.
        assert_eq!(
            strip_ansi("a\u{fff9}b\u{fffa}c\u{fffb}d\u{3164}e\u{ffa0}f".as_bytes()),
            "abcdef"
        );
        // Inside a sequence they go with it, and after ESC they are dropped
        // as anywhere.
        assert_eq!(strip_ansi("\x1b[3\u{202e}1mred".as_bytes()), "red");
        assert_eq!(strip_ansi("\x1b]0;t\u{202e}x\x07shown".as_bytes()), "shown");
        assert_eq!(strip_ansi("a\x1b\u{202e}b".as_bytes()), "ab");
        // Tag characters, which can spell out text that is not shown.
        assert_eq!(
            strip_ansi("a\u{e0001}\u{e0068}\u{e0069}\u{e007f}b".as_bytes()),
            "ab"
        );
        // Directional marks and joiners stay, and so do the emoji that need
        // a joiner or a variation selector.
        let kept = "a\u{200e}b\u{200f}c\u{061c}d\u{200c}e\u{200d}f 👩\u{200d}💻 ❤\u{fe0f} שלום";
        assert_eq!(strip_ansi(kept.as_bytes()), kept);
    }

    /// Whatever comes, in whatever state: every pair of bytes and every pair
    /// of control characters, after a start that leaves the stripper in each
    /// of its states, or in the middle of a character.
    #[test]
    fn no_control_character_but_cr_lf_and_tab_reaches_a_log() {
        let prefixes: [&[u8]; 16] = [
            b"",
            b"text",
            b"\x1b",
            b"\x1b(",
            b"\x1b[",
            b"\x1b[1;3",
            b"\x1b]",
            b"\x1b]0;t",
            b"\x1b]0;t\x1b",
            b"\x1bP",
            b"\x1bPq\x1b",
            b"\xc2\x9b",
            b"\xc2\x9d",
            b"\xc2\x90",
            b"\xc2",
            b"\xf0\x9f\x99",
        ];
        let controls: Vec<char> = (0..=0x9f_u32)
            .filter_map(char::from_u32)
            .filter(|c| c.is_control())
            .collect();
        assert_eq!(controls.len(), 65);

        for prefix in prefixes {
            let mut after_prefix = AnsiStripperState::default();
            let _ = strip_ansi_stateful(&mut after_prefix, prefix);

            for first in 0..=u8::MAX {
                for second in 0..=u8::MAX {
                    let mut ctx = after_prefix;
                    let mut out = strip_ansi_stateful(&mut ctx, &[first, second]);
                    out.push_str(&strip_ansi_stateful(&mut ctx, b"tail"));
                    assert!(
                        is_log_text(&out),
                        "{prefix:?} {first:#04x} {second:#04x}: {out:?}"
                    );
                }
            }

            for first in &controls {
                for second in &controls {
                    let mut ctx = after_prefix;
                    let input = format!("{first}x{second}y");
                    let mut out = strip_ansi_stateful(&mut ctx, input.as_bytes());
                    out.push_str(&strip_ansi_stateful(&mut ctx, b"tail"));
                    assert!(is_log_text(&out), "{prefix:?} {input:?}: {out:?}");
                }
            }
        }
    }

    /// A backspace reaches back only within one output, so what a log
    /// holds can depend on where the output is split around one. What it may
    /// hold does not.
    #[test]
    fn a_backspace_anywhere_leaves_what_a_log_may_hold() {
        let mut seed: u32 = 0x85eb_ca6b;
        let mut next = |below: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as usize % below
        };
        for _ in 0..2000 {
            let mut ctx = AnsiStripperState::default();
            let mut out = String::new();
            for _ in 0..20 {
                // A read of up to four pieces, one in four a backspace.
                let mut chunk = Vec::new();
                for _ in 0..next(5) {
                    if next(4) == 0 {
                        chunk.push(0x08);
                    } else {
                        chunk.extend_from_slice(PIECES[next(PIECES.len())]);
                    }
                }
                out.push_str(&strip_ansi_stateful(&mut ctx, &chunk));
            }
            assert!(is_log_text(&out), "{out:?}");
        }
    }

    /// A log that takes `room` more bytes and then fails once, as a disk
    /// does that is full until something is removed.
    struct FullOnce {
        written: Vec<u8>,
        room: usize,
    }

    impl Write for FullOnce {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.room == 0 {
                self.room = usize::MAX;
                return Err(io::Error::other("no space left"));
            }
            let taken = buf.len().min(self.room);
            self.written.extend_from_slice(&buf[..taken]);
            self.room -= taken;
            Ok(taken)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_write_after_a_failed_one_starts_a_new_line() {
        // Cut between the CR and the LF of a line break: what comes next
        // would be written over the line.
        let mut log = FullOnce {
            written: Vec::new(),
            room: 3,
        };
        let mut failed = false;
        assert!(write_text(&mut log, &mut failed, "ab\r\ncd").is_err());
        assert!(failed);
        assert_eq!(log.written, b"ab\r");
        write_text(&mut log, &mut failed, "x").unwrap();
        assert!(!failed);
        write_text(&mut log, &mut failed, "y").unwrap();
        assert_eq!(log.written, b"ab\r\nxy");

        // Cut in the middle of a character.
        let mut log = FullOnce {
            written: Vec::new(),
            room: 2,
        };
        let mut failed = false;
        assert!(write_text(&mut log, &mut failed, "a€b").is_err());
        assert_eq!(log.written, b"a\xe2");
        write_text(&mut log, &mut failed, "c").unwrap();
        assert_eq!(log.written, b"a\xe2\nc");

        // A write that fails twice gets one line feed for it.
        let mut log = FullOnce {
            written: Vec::new(),
            room: 0,
        };
        let mut failed = false;
        assert!(write_text(&mut log, &mut failed, "a").is_err());
        log.room = 0;
        assert!(write_text(&mut log, &mut failed, "b").is_err());
        assert!(failed);
        write_text(&mut log, &mut failed, "c").unwrap();
        assert_eq!(log.written, b"\nc");

        // One that does not fail is written as it is.
        let mut log = FullOnce {
            written: Vec::new(),
            room: usize::MAX,
        };
        let mut failed = false;
        write_text(&mut log, &mut failed, "a\r\n").unwrap();
        write_text(&mut log, &mut failed, "b").unwrap();
        assert_eq!(log.written, b"a\r\nb");
    }

    /// The log's bytes, and not only what the stripper returns: DEL and a C1
    /// control sent by a host are not in the file.
    #[test]
    fn a_log_file_holds_no_del_and_no_c1_control() {
        let (mut mgr, dir) = manager();
        mgr.open_log("a", "router").unwrap();
        mgr.write_log("a", "shown\x7f\u{9b}2J\u{9b}H".as_bytes());
        // A C1 control split across two outputs of the session, and then
        // `ß`, which ends in a byte of that range (C3 9F).
        mgr.write_log("a", b"fake\xc2");
        mgr.write_log("a", b"\x8d gr\xc3\xb6\xc3\x9fe\r\n");
        mgr.close_log("a");
        assert_eq!(
            std::fs::read(dir.join("router.log")).unwrap(),
            "shownfake größe\r\n".as_bytes()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
