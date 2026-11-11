use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use chrono::{Datelike, Local, Timelike};
use tracing::{info, warn};

use crate::config::LoggingConfig;
use crate::private_fs;

/// What the manager keeps for a session's log. The file is not here: it
/// belongs to the log's writer, a thread of its own (see [`LogWriter`]), so
/// that nothing done under the lock all sessions share touches the file
/// system.
enum LogEntry {
    /// A writer has the log. The session's text is handed to it here.
    Writing(LogQueue),
    /// The log could not even be handed to a writer when the session
    /// connected. Kept so that the session's first output says so in its
    /// tab, as a writer does for a file that cannot be created: the readers
    /// hold output back until the tab has said that it is listening.
    Unavailable,
}

/// Where a log's writer is with the file.
enum LogFile {
    /// The name the log is to get, but no file created yet: that waits for
    /// the first text, so failed connections leave no empty log files
    /// behind. The file may end up with a number after the name: see
    /// [`create_log_file`].
    Pending(PathBuf),
    /// File created on first write.
    Active(ActiveLog),
    /// The log directory could not be prepared, or the file not created.
    /// The session's first output says so in its tab.
    Unavailable,
}

/// Called with the connection ID of a session whose log is given up.
type GivenUpNotifier = Arc<dyn Fn(&str) + Send + Sync>;

/// How much text may wait for a log's writer, in bytes, what the writer has
/// taken and not yet written included, before the session waits for the
/// writer. A session's output is handed to the writer and does not wait for
/// the file, so text collects while a write takes its time. With more than
/// this waiting, the session's own reader waits until the writer has caught
/// up (see [`LogBehind`]): a log directory that is slow then slows the
/// sessions that are logged there by megabytes, as it did before each log
/// had a writer, and nothing is lost. No other session waits. It also bounds
/// the memory a log can take while its directory does not answer: this much
/// text and one output of the session, in buffers that can be up to twice as
/// large as the text they hold.
const MAX_WAITING_BYTES: usize = 1024 * 1024;

/// How long a session waits for a log that is behind and gets no further:
/// with nothing written in this time, the log is given up, and the session
/// goes on without one. A session's output is thus held up by its log once,
/// for this long, when its log directory stops answering.
const STALL_TIMEOUT: Duration = Duration::from_secs(5);

/// How much a writer hands to the file in one write: the outputs of the
/// session that wait, together, as far as they fit in this, and a single
/// output whatever its size. A file that is slow to write to then takes few
/// writes rather than one for each output. After each write the session is
/// told how far the writer has got, so that one waiting for it goes on as
/// soon as there is room, and a write that is slow but gets somewhere is not
/// taken for one that does not: this much in [`STALL_TIMEOUT`] is enough. A
/// write that fails loses no more than this, or the one output.
const WRITE_PIECE_BYTES: usize = 64 * 1024;

/// How long a test waits for a log's writer before it fails.
#[cfg(test)]
const TEST_WAIT: Duration = Duration::from_secs(30);

/// Lock a mutex that a panic elsewhere may have poisoned: what these locks
/// guard stays usable.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a session's reader and the writer of its log share. Its lock is
/// held only to add text or to take it, never while the file is written.
#[derive(Default)]
struct LogShared {
    state: Mutex<LogState>,
    /// Told when there is text to write, when some of it has been written,
    /// and when the log is closed or given up.
    changed: Condvar,
}

#[derive(Default)]
struct LogState {
    /// The outputs of the session, stripped, that the writer has not taken
    /// yet, the oldest first. Small ones are joined as they come (see
    /// [`LogQueue::hand_on`]), so that what the queue takes besides the text
    /// stays small however small the outputs are.
    queue: VecDeque<String>,
    /// How many bytes `queue` holds.
    queued: usize,
    /// How many bytes the writer has taken and is writing.
    writing: usize,
    /// Whether the session is done with the log: the writer writes what is
    /// left and ends.
    closed: bool,
    /// Whether the log has been given up: by the session's side, when it
    /// hands text on or waits for the writer, or by the writer. Whichever
    /// sets it tells the window, so that is done once.
    given_up: bool,
    /// Whether the writer has nothing to do: it waits for text, or has
    /// ended. For tests, which wait for it.
    #[cfg(test)]
    idle: bool,
}

impl LogState {
    /// How many bytes of text the file has not taken yet.
    fn waiting(&self) -> usize {
        self.queued + self.writing
    }

    /// Take what goes into the next write from the front of the queue: see
    /// [`WRITE_PIECE_BYTES`]. `None` when nothing waits.
    fn take_piece(&mut self) -> Option<String> {
        let mut piece = self.queue.pop_front()?;
        while let Some(next) = self.queue.front() {
            if piece.len() + next.len() > WRITE_PIECE_BYTES {
                break;
            }
            piece.push_str(next);
            self.queue.pop_front();
        }
        self.queued = self.queued.saturating_sub(piece.len());
        self.writing = piece.len();
        Some(piece)
    }

    /// Drop the text that waits.
    fn drop_text(&mut self) {
        self.queue = VecDeque::new();
        self.queued = 0;
        self.writing = 0;
    }
}

/// A session's end of its log: where its text is handed to the writer.
/// Dropping it closes the log, once the writer has written what it has.
struct LogQueue(Arc<LogShared>);

/// What became of text handed to a writer.
enum Handed {
    /// The writer has it, or will take it.
    Queued,
    /// As [`Handed::Queued`], and more than [`MAX_WAITING_BYTES`] wait now:
    /// the session is to wait for the writer before it hands on more.
    Behind,
    /// The log was given up before, and that was said then.
    GivenUp,
    /// More than [`MAX_WAITING_BYTES`] waited already, so the session did
    /// not wait for the writer when it was to: the log is given up here,
    /// which is still to be said.
    TooMuchWaiting,
}

impl LogQueue {
    /// Hand `text`, an output of the session, to the writer. It joins the
    /// last output that waits if the two fit in one write (see
    /// [`WRITE_PIECE_BYTES`]): a host that sends a byte at a time while the
    /// log directory does not answer would otherwise have each byte cost a
    /// buffer of its own, many times what the limit on waiting text counts.
    fn hand_on(&self, text: String) -> Handed {
        let mut state = lock(&self.0.state);
        if state.given_up {
            return Handed::GivenUp;
        }
        if text.is_empty() {
            return Handed::Queued;
        }
        if state.waiting() > MAX_WAITING_BYTES {
            state.given_up = true;
            return Handed::TooMuchWaiting;
        }
        state.queued += text.len();
        match state.queue.back_mut() {
            Some(last) if last.len() + text.len() <= WRITE_PIECE_BYTES => last.push_str(&text),
            _ => state.queue.push_back(text),
        }
        let behind = state.waiting() > MAX_WAITING_BYTES;
        drop(state);
        self.0.changed.notify_all();
        if behind {
            Handed::Behind
        } else {
            Handed::Queued
        }
    }
}

impl Drop for LogQueue {
    fn drop(&mut self) {
        lock(&self.0.state).closed = true;
        self.0.changed.notify_all();
    }
}

/// A log that is behind its session: more than [`MAX_WAITING_BYTES`] of the
/// session's text wait to be written. The session's reader waits here until
/// the writer has caught up, without the lock all sessions share, so that
/// only the session whose log is slow is held up.
pub struct LogBehind {
    shared: Arc<LogShared>,
    connection_id: String,
    on_given_up: Option<GivenUpNotifier>,
    stall_timeout: Duration,
}

impl LogBehind {
    /// Wait until no more than [`MAX_WAITING_BYTES`] wait to be written, or
    /// the log is closed or given up. A log whose writer completes no write
    /// for as long as the stall timeout is given up here: the session goes on
    /// without one, and the window is told. What waits is left to the
    /// writer: if the log directory answers again, the writer writes it,
    /// and the log ends there.
    ///
    /// This blocks: a reader that is a task waits through
    /// [`log_output_async`].
    fn wait(self) {
        let mut state = lock(&self.shared.state);
        loop {
            let waiting = state.waiting();
            if state.given_up || state.closed || waiting <= MAX_WAITING_BYTES {
                return;
            }
            // Only the writer changes what waits while the session is here,
            // and only downwards.
            let (next, wait) = self
                .shared
                .changed
                .wait_timeout_while(state, self.stall_timeout, |state| {
                    !state.given_up && !state.closed && state.waiting() == waiting
                })
                .unwrap_or_else(PoisonError::into_inner);
            state = next;
            if wait.timed_out() {
                break;
            }
        }
        state.given_up = true;
        drop(state);
        self.shared.changed.notify_all();
        warn!(
            connection_id = %self.connection_id,
            timeout = ?self.stall_timeout,
            "The session log takes no writes; this session is no longer logged"
        );
        if let Some(notify) = &self.on_given_up {
            notify(&self.connection_id);
        }
    }
}

/// Hand the output of a session to its log, for a reader that is a thread of
/// its own. It takes the lock all sessions share for as long as stripping
/// the output takes, and touches no file. Should the log be behind, it then
/// waits for the log's writer, without that lock: see [`LogBehind`].
pub fn log_output(logger: &Mutex<SessionLogManager>, connection_id: &str, data: &[u8]) {
    let behind = lock(logger).write_log(connection_id, data);
    if let Some(behind) = behind {
        behind.wait();
    }
}

/// [`log_output`] for a reader that is a task: the wait for a log that is
/// behind is on a blocking thread, so that it holds up this task and none of
/// the runtime's threads.
pub async fn log_output_async(logger: &Mutex<SessionLogManager>, connection_id: &str, data: &[u8]) {
    let behind = lock(logger).write_log(connection_id, data);
    if let Some(behind) = behind {
        // An error is a wait that panicked or a runtime that is shutting
        // down: the session goes on either way.
        if let Err(e) = tokio::task::spawn_blocking(move || behind.wait()).await {
            warn!(error = %e, "Waiting for a session log failed");
        }
    }
}

/// Counts the log writers that are still running, so that quitting can wait
/// until the logs are written: see [`SessionLogManager::close_all`].
#[derive(Default)]
pub struct LogWriters {
    running: Mutex<usize>,
    all_done: Condvar,
}

/// How many log writers may run at a time. Each session with a log has one,
/// and at most 300 sessions are open at a time (100 each of SSH, Telnet and
/// local shell), so this is reached only when writers outlive their
/// sessions: a writer stuck in a write to a log directory that does not
/// answer stays, with the text it has, until the write returns. Sessions
/// opened one after the other could otherwise leave any number of them
/// behind. With every one of them stuck and holding all the text a log may
/// have waiting (see [`MAX_WAITING_BYTES`]), that is somewhat over half a
/// gigabyte of text, in buffers of up to twice that.
const MAX_WRITERS: usize = 512;

impl LogWriters {
    /// Count one more writer, until what is returned is dropped. `None`
    /// when [`MAX_WRITERS`] are running already.
    fn count(self: &Arc<Self>) -> Option<RunningWriter> {
        let mut running = lock(&self.running);
        if *running >= MAX_WRITERS {
            return None;
        }
        *running += 1;
        Some(RunningWriter(Arc::clone(self)))
    }

    /// Wait until every writer has ended, for at most `timeout`. Returns
    /// whether they all have. A writer ends once its log is closed and
    /// written, so this is for when the logs have been closed.
    pub fn wait_until_done(&self, timeout: Duration) -> bool {
        let (running, _) = self
            .all_done
            .wait_timeout_while(lock(&self.running), timeout, |running| *running > 0)
            .unwrap_or_else(PoisonError::into_inner);
        *running == 0
    }
}

/// One writer among those [`LogWriters`] counts, for as long as it lives.
struct RunningWriter(Arc<LogWriters>);

impl Drop for RunningWriter {
    fn drop(&mut self) {
        let mut running = lock(&self.0.running);
        *running = running.saturating_sub(1);
        if *running == 0 {
            self.0.all_done.notify_all();
        }
    }
}

/// Writes one session's log, on a thread of its own, so that no session's
/// output waits for the file system under the lock all sessions share: a
/// log directory that is slow, such as one on a network share that does not
/// answer, holds up this thread, and the session whose log it is once that
/// log is far behind (see [`LogBehind`]), and nothing else. It makes sure
/// the log directory is there when the session connects, creates the file
/// on the session's first text, writes the text that waits a piece at a
/// time (see [`WRITE_PIECE_BYTES`]), goes on in a new file when the one it
/// writes is full and deletes the oldest (see [`LogLimits`]), and ends when
/// the session has closed the log and nothing is left to write, or when it
/// has to give the log up.
struct LogWriter {
    connection_id: String,
    shared: Arc<LogShared>,
    on_given_up: Option<GivenUpNotifier>,
    /// How large a file of this log may get, and how many are kept: what
    /// was set when the session connected.
    limits: LogLimits,
    /// Counts this writer among the running ones until it ends.
    _running: RunningWriter,
    /// Held by a test for as long as writing is to take.
    #[cfg(test)]
    write_gate: Arc<Mutex<()>>,
}

/// A writer that ends in a panic gives its log up on the way out: nothing
/// would take the session's text any more, and nothing else would say that
/// the session is no longer logged.
impl Drop for LogWriter {
    fn drop(&mut self) {
        if thread::panicking() {
            warn!(
                connection_id = %self.connection_id,
                "The session log's writer panicked; this session is no longer logged"
            );
            self.give_up();
        }
    }
}

impl LogWriter {
    fn run(self, log_dir: &Path, base: PathBuf) {
        self.write_until_closed(log_dir, base);
        #[cfg(test)]
        {
            lock(&self.shared.state).idle = true;
            self.shared.changed.notify_all();
        }
    }

    fn write_until_closed(&self, log_dir: &Path, base: PathBuf) {
        let mut log = match prepare_log_dir(&self.connection_id, log_dir, &base) {
            Ok(()) => LogFile::Pending(base),
            Err(e) => {
                warn!(
                    connection_id = %self.connection_id,
                    error = %e,
                    "Failed to prepare session log; this session is not logged"
                );
                LogFile::Unavailable
            }
        };

        while let Some(text) = self.next_text() {
            // Promote Pending → Active on first real data.
            if let LogFile::Pending(base) = &log {
                log = match create_log_file(base) {
                    Ok((file, path)) => {
                        info!(
                            connection_id = %self.connection_id,
                            path = %path.display(),
                            "Session log file created"
                        );
                        LogFile::Active(ActiveLog::new(file, path))
                    }
                    Err(e) => {
                        warn!(
                            connection_id = %self.connection_id,
                            path = %base.display(),
                            error = %e,
                            "Failed to create session log file; this session is not logged"
                        );
                        LogFile::Unavailable
                    }
                };
            }
            let LogFile::Active(active) = &mut log else {
                // The reason is in the application log by now. Trying again
                // with each output would also mean up to a thousand failed
                // creates per output where every name is taken.
                self.give_up();
                return;
            };

            if let Err(e) = self.write(active, &text) {
                // What the log holds so far stays. As above, the next
                // output does not try again.
                warn!(
                    connection_id = %self.connection_id,
                    error = %e,
                    "The session log cannot go on in a new file; \
                     this session is no longer logged"
                );
                self.give_up();
                return;
            }
        }

        match log {
            LogFile::Active(mut active) => {
                if let Err(e) = active.file.flush() {
                    warn!(
                        connection_id = %self.connection_id,
                        error = %e,
                        "Failed to flush session log"
                    );
                }
                info!(connection_id = %self.connection_id, "Session log closed");
            }
            LogFile::Pending(_) => {
                info!(
                    connection_id = %self.connection_id,
                    "Session log closed (no data written, no file created)"
                );
            }
            LogFile::Unavailable => {}
        }
    }

    /// Write a piece of the session's text to the log: to the file that is
    /// being written as far as it has room, and the rest to a new one, or to
    /// several (see [`LogLimits`]). Text that cannot be written is lost, and
    /// said in the application log.
    ///
    /// # Errors
    /// The log cannot go on in a new file: see [`Self::start_next_part`].
    /// The rest of the piece is not written then.
    fn write(&self, log: &mut ActiveLog, piece: &str) -> io::Result<()> {
        #[cfg(test)]
        let _writing = lock(&self.write_gate);
        let mut rest = piece;
        while !rest.is_empty() {
            let (now, later) = log.split_for_room(rest, self.limits.max_file_bytes);
            if now.is_empty() {
                self.start_next_part(log)?;
                continue;
            }
            if let Err(e) = write_text(&mut log.file, &mut log.after_failed_write, now) {
                warn!(
                    connection_id = %self.connection_id,
                    error = %e,
                    "Failed to write session log"
                );
            }
            rest = later;
        }
        Ok(())
    }

    /// Close the file that is being written, which is full, and go on in a
    /// new one. Then delete the oldest files of this log, so that no more
    /// than [`LogLimits::max_files`] are left.
    ///
    /// Only files this writer created are deleted, by the names it gave
    /// them. As for creating a log, that protects the name and not the log
    /// directory: someone able to replace that directory, or one above it,
    /// with a link decides where a name leads.
    ///
    /// A file that cannot be deleted stays, and is tried again when the next
    /// file is full: one that another program holds open, on Windows, may be
    /// free by then. The application log has a warning for it the first
    /// time, and not for each time it is tried again: a host that puts out
    /// text fast enough fills a file many times a second. A file that is
    /// gone already counts as deleted.
    ///
    /// # Errors
    /// The new file cannot be created, or too many files could not be
    /// deleted: more than twice [`LogLimits::max_files`] would be left. The
    /// log has a limit only as long as its oldest files go, so it ends here
    /// rather than grow without one.
    fn start_next_part(&self, log: &mut ActiveLog) -> io::Result<()> {
        if let Err(e) = log.file.flush() {
            warn!(
                connection_id = %self.connection_id,
                error = %e,
                "Failed to flush session log"
            );
        }
        let (file, path, part) = create_next_part(&log.first, log.part)?;
        info!(
            connection_id = %self.connection_id,
            path = %path.display(),
            "The session log file is full; the log goes on in a new file"
        );
        // The file that was written is closed here, before any is deleted:
        // with one file kept, it is the one to go.
        log.file = Counted::new(file);
        log.after_failed_write = false;
        log.part = part;
        log.kept.push_back(path);

        // The file being written is always kept, whatever the limit says.
        let max_files = self.limits.max_files.max(1);
        let excess = log.kept.len().saturating_sub(max_files);
        // Those that could not be deleted before are the first ones again.
        let tried_before = std::mem::take(&mut log.not_deleted);
        let mut not_deleted = Vec::new();
        for (index, path) in log.kept.drain(..excess).enumerate() {
            match std::fs::remove_file(&path) {
                Ok(()) => info!(
                    connection_id = %self.connection_id,
                    path = %path.display(),
                    "Oldest file of the session log deleted"
                ),
                // Someone has removed it already.
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => {
                    if index >= tried_before {
                        warn!(
                            connection_id = %self.connection_id,
                            path = %path.display(),
                            error = %e,
                            "Failed to delete the oldest file of the session log; \
                             it is tried again when the next file is full"
                        );
                    }
                    not_deleted.push(path);
                }
            }
        }
        log.not_deleted = not_deleted.len();
        for path in not_deleted.into_iter().rev() {
            log.kept.push_front(path);
        }
        if log.kept.len() > max_files.saturating_mul(2) {
            return Err(io::Error::other(
                "too many files of this log could not be deleted",
            ));
        }
        Ok(())
    }

    /// Wait for text of the session and take the next piece of it to write:
    /// see [`WRITE_PIECE_BYTES`]. `None` once the session has closed the log
    /// and nothing is left.
    fn next_text(&self) -> Option<String> {
        let mut state = lock(&self.shared.state);
        // What was taken before is written, or could not be: a session
        // that waits for the writer is told that less waits now.
        if std::mem::take(&mut state.writing) > 0 {
            self.shared.changed.notify_all();
        }
        loop {
            if let Some(piece) = state.take_piece() {
                return Some(piece);
            }
            if state.closed {
                return None;
            }
            #[cfg(test)]
            {
                state.idle = true;
                self.shared.changed.notify_all();
            }
            state = self
                .shared
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
            #[cfg(test)]
            {
                state.idle = false;
            }
        }
    }

    /// Give the log up: drop the text that waits, have the session's side
    /// hand on no more and wait no longer, and tell the window, unless that
    /// side gave the log up first and has told it.
    fn give_up(&self) {
        let said = {
            let mut state = lock(&self.shared.state);
            state.drop_text();
            std::mem::replace(&mut state.given_up, true)
        };
        self.shared.changed.notify_all();
        if !said {
            if let Some(notify) = &self.on_given_up {
                notify(&self.connection_id);
            }
        }
    }
}

/// How large a session's log may get on disk: the size a file of it may
/// reach, and how many files of it are kept.
///
/// A log has no end of its own, and what goes into it is what the remote
/// host sends, so without a limit a host could fill the volume the log
/// directory is on. A file that is full is therefore closed, and the log
/// goes on in a new one (see [`create_next_part`] for its name); once more
/// than `max_files` are there, the oldest is deleted. One session's log
/// thus takes no more than `max_files` times `max_file_bytes`, and holds the
/// session's latest output. The exception is a file that cannot be deleted:
/// with such files a log can take up to twice that before it is given up
/// (see [`LogWriter::start_next_part`]).
///
/// This limits each log by itself, not the log directory: every session has
/// a log of its own, and the logs of sessions that have ended stay until the
/// user removes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogLimits {
    /// The size a log file may reach, in bytes. No file is larger, unless
    /// this is less than a single character takes. A write that fails does
    /// not take a file over it either, but may leave part of its text there.
    pub max_file_bytes: u64,
    /// How many files of one session's log are kept, the one being written
    /// included.
    pub max_files: usize,
}

impl LogLimits {
    /// The limits for a file size in MiB and a number of files, as the
    /// config holds them.
    #[must_use]
    pub fn new(max_size_mb: u32, max_files: u32) -> Self {
        Self {
            max_file_bytes: u64::from(max_size_mb) * 1024 * 1024,
            max_files: usize::try_from(max_files).unwrap_or(usize::MAX),
        }
    }
}

impl From<&LoggingConfig> for LogLimits {
    fn from(config: &LoggingConfig) -> Self {
        Self::new(config.max_size_mb, config.max_files)
    }
}

impl Default for LogLimits {
    fn default() -> Self {
        Self::from(&LoggingConfig::default())
    }
}

/// Counts the bytes that a file has taken, whether the write they were part
/// of then failed or not: a log file's size is held against its limit by
/// what went into it, without asking the file system.
struct Counted<W> {
    inner: W,
    written: u64,
}

impl<W> Counted<W> {
    fn new(inner: W) -> Self {
        Self { inner, written: 0 }
    }
}

impl<W: Write> Write for Counted<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.written = self
            .written
            .saturating_add(u64::try_from(written).unwrap_or(u64::MAX));
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// A log that has a file, and what its writer knows about it.
struct ActiveLog {
    /// The file that is being written: the newest of the log.
    file: Counted<File>,
    /// Whether the last write to the file failed: see [`write_text`].
    after_failed_write: bool,
    /// The name the log's first file got, which the later ones are named
    /// after: see [`create_next_part`].
    first: PathBuf,
    /// Which file of the log is being written, counted from 1.
    part: u32,
    /// The files of this log that this writer created and has not deleted,
    /// the oldest first and the one being written last.
    kept: VecDeque<PathBuf>,
    /// How many of `kept`, its first ones, are files that could not be
    /// deleted when they were to go: see [`LogWriter::start_next_part`].
    not_deleted: usize,
}

impl ActiveLog {
    /// A log whose first file, `file`, has just been created at `path`.
    fn new(file: File, path: PathBuf) -> Self {
        Self {
            file: Counted::new(file),
            after_failed_write: false,
            first: path.clone(),
            part: 1,
            kept: VecDeque::from([path]),
            not_deleted: 0,
        }
    }

    /// Split `text` into what the file being written still has room for,
    /// with `max_file_bytes` as its limit, and what has to go into the next
    /// file. The first is empty when the file is full.
    ///
    /// The split is between two characters, and never between a CR and its
    /// LF: a file that ended in a CR would have whatever a terminal shows
    /// after it written over its last line (see [`AnsiStripperState::emit`]).
    /// That is for what is written: a write that fails part of the way can
    /// still leave a file ending there, as it can in a log of one file.
    /// A file that holds nothing yet takes one character whatever the limit,
    /// so that a limit smaller than a character cannot keep a log from
    /// getting anywhere.
    fn split_for_room<'a>(&self, text: &'a str, max_file_bytes: u64) -> (&'a str, &'a str) {
        // The write after a failed one starts with a line feed of its own.
        let taken = self
            .file
            .written
            .saturating_add(u64::from(self.after_failed_write));
        let room = usize::try_from(max_file_bytes.saturating_sub(taken)).unwrap_or(usize::MAX);
        if text.len() <= room {
            return (text, "");
        }
        let mut end = room;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if text.get(..end).is_some_and(|start| start.ends_with('\r')) {
            end -= 1;
        }
        if end == 0 && self.file.written == 0 {
            end = if text.starts_with("\r\n") {
                2
            } else {
                text.chars().next().map_or(0, char::len_utf8)
            };
        }
        text.split_at(end)
    }
}

/// How many names are tried for the next file of a log before the log is
/// given up: the names that are taken are passed over.
const MAX_PART_NAMES_TRIED: u32 = 1000;

/// Create the next file of the log whose first file is `first`, after its
/// file number `part`. Returns the file, its name and its number.
///
/// The file is named after the first one, with `.part0002`, `.part0003`, …
/// before the extension: `router.log` goes on in `router.part0002.log`. The
/// numbers only go up, so a name is not used again once its file has been
/// deleted, and the names sort in the order the files were written up to
/// `.part9999`: the numbers after it are longer and sort before it. Which
/// file is the oldest is not read from the names. The file is created as the
/// first one is (see
/// [`create_log_file`]): never opened if it is already there, and for its
/// owner alone. A name that is taken, by a file of an earlier log or by
/// whatever someone else put there, is passed over for the next number.
fn create_next_part(first: &Path, part: u32) -> io::Result<(File, PathBuf, u32)> {
    let stem = first.file_stem().unwrap_or_default().to_string_lossy();
    let extension = first.extension().map(|ext| ext.to_string_lossy());
    let mut part = part;
    for _ in 0..MAX_PART_NAMES_TRIED {
        let Some(next) = part.checked_add(1) else {
            break;
        };
        part = next;
        let name = match &extension {
            Some(ext) => format!("{stem}.part{part:04}.{ext}"),
            None => format!("{stem}.part{part:04}"),
        };
        let path = first.with_file_name(name);
        match private_fs::create_private_append_file(&path) {
            Ok(file) => return Ok((file, path, part)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many log files with this name",
    ))
}

/// Make sure the log directory is there for the log at `path`, restricted
/// to its owner where it can be.
fn prepare_log_dir(connection_id: &str, log_dir: &Path, path: &Path) -> Result<(), String> {
    // A missing log directory is created for its owner alone (0700 on
    // Unix, an access list of its own on Windows, as part of the create),
    // so it is never open to others.
    private_fs::create_private_dir(log_dir)
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
    if let Err(e) = private_fs::restrict_dir_to_owner_nofollow(log_dir) {
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

    // Said in the application log, as nothing else tells the user.
    if private_fs::on_network_share(path) {
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
    Ok(())
}

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
    /// How large a log file may get, and how many files of a session's log
    /// are kept. A log keeps what was set when its session connected.
    pub limits: LogLimits,
    /// Tells the window that a session goes on without a log: see
    /// [`Self::notify_when_given_up`].
    on_given_up: Option<GivenUpNotifier>,
    /// Counts the writers of this manager's logs that are still running.
    writers: Arc<LogWriters>,
    /// How long a session waits for a log that is behind and gets no
    /// further: [`STALL_TIMEOUT`], and less in tests.
    stall_timeout: Duration,
    /// Held by a test for as long as writing a log is to take.
    #[cfg(test)]
    write_gate: Arc<Mutex<()>>,
}

impl SessionLogManager {
    pub fn new(enabled: bool, log_dir: PathBuf, filename_format: String) -> Self {
        Self {
            entries: HashMap::new(),
            enabled,
            log_dir,
            filename_format,
            limits: LogLimits::default(),
            on_given_up: None,
            writers: Arc::default(),
            stall_timeout: STALL_TIMEOUT,
            #[cfg(test)]
            write_gate: Arc::default(),
        }
    }

    /// Have `notify` called with the connection ID of each session whose log
    /// is given up, so that the window can say that the session is not
    /// logged; the application log alone is not where a user looks. It is
    /// called once per session. For a log that could not be prepared at
    /// connect, or whose file cannot be created, that is on the session's
    /// first output, which is when the log would have been created. By then
    /// the tab is listening, as the readers hold output back until it says
    /// so. They give up waiting after a few seconds, though: a tab that is
    /// later than that can miss the notice, as it misses the output. For a
    /// log that is behind its session and gets no further, it is when the
    /// session has waited for it in vain: see [`LogBehind`].
    ///
    /// `notify` runs on the log's writer thread, where a session waits for
    /// its log, or inside [`write_log`], and so at times under the lock all
    /// sessions share: it must not wait for anything, nor use the logger.
    /// Whatever takes time, such as sending an event, it has to hand on. Set
    /// it before the first log is opened: a log keeps what was set when it
    /// was opened.
    ///
    /// [`write_log`]: Self::write_log
    pub fn notify_when_given_up(&mut self, notify: impl Fn(&str) + Send + Sync + 'static) {
        self.on_given_up = Some(Arc::new(notify));
    }

    /// Prepare a log for a new session connection.
    ///
    /// Works out the name the log is to get and hands it to a writer thread
    /// of its own, which makes sure the log directory is there. Nothing here
    /// touches the file system, so a log directory that is slow holds up
    /// neither this connect nor the output of other sessions. The file is
    /// created by the writer on the first [`write_log`] call that has text,
    /// so failed connections never leave empty log files on disk.
    ///
    /// A log that cannot be handed to a writer is an error, and the session
    /// goes on without one. It is stored as [`LogEntry::Unavailable`], for
    /// the first output to report: see [`Self::notify_when_given_up`]. A log
    /// directory that cannot be created is no error here: the writer finds
    /// out, and the first output reports that as well.
    ///
    /// [`write_log`]: Self::write_log
    pub fn open_log(&mut self, connection_id: &str, session_name: &str) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }

        let (entry, result) = match self.start_writer(connection_id, session_name) {
            Ok(queue) => (LogEntry::Writing(queue), Ok(())),
            Err(e) => (LogEntry::Unavailable, Err(e)),
        };
        self.entries.insert(
            connection_id.to_string(),
            (entry, AnsiStripperState::default()),
        );
        result
    }

    /// Work out the name the log of this session is to get and start the
    /// thread that writes it.
    fn start_writer(&self, connection_id: &str, session_name: &str) -> Result<LogQueue, String> {
        let path = self.log_path(session_name)?;
        let running = self.writers.count().ok_or_else(|| {
            format!("{MAX_WRITERS} session logs are still being written, which is the limit")
        })?;
        let log_dir = self.log_dir.clone();
        let shared = Arc::new(LogShared::default());
        let writer = LogWriter {
            connection_id: connection_id.to_string(),
            shared: Arc::clone(&shared),
            on_given_up: self.on_given_up.clone(),
            limits: self.limits,
            _running: running,
            #[cfg(test)]
            write_gate: Arc::clone(&self.write_gate),
        };
        thread::Builder::new()
            .name("session-log".to_string())
            .spawn(move || writer.run(&log_dir, path))
            .map_err(|e| format!("Failed to start the thread that writes the log: {e}"))?;
        Ok(LogQueue(shared))
    }

    /// Hand terminal output to the log's writer, stripped of `ANSI` escape
    /// sequences and control characters so the log contains only readable
    /// text: see [`strip_ansi_stateful`] for what is left.
    ///
    /// The `ANSI` stripper state is preserved across calls so that escape
    /// sequences split across read boundaries (common with Windows `ConPTY`)
    /// are handled correctly.
    ///
    /// Nothing here touches the file system, so this does not wait for a
    /// log directory that is slow: every session's reader calls it under one
    /// lock, through [`log_output`] or [`log_output_async`]. The writer
    /// creates the file on the first text and writes what it is handed: see
    /// [`LogWriter`].
    ///
    /// What is returned is for the caller to wait on once it has let go of
    /// that lock: the log is more than [`MAX_WAITING_BYTES`] behind, because
    /// its file takes writes more slowly than the session puts text out, or
    /// not at all. See [`LogBehind`].
    ///
    /// A log whose file cannot be created is given up by its writer: the
    /// session goes on without one, and the reason is logged once rather
    /// than on every output. One that gets no further while its session
    /// waits for it is given up there. And one is given up here, should a
    /// caller hand on more without having waited: what waits to be written
    /// has a limit whatever a caller does. Each time the window is told,
    /// once: see [`Self::notify_when_given_up`].
    fn write_log(&mut self, connection_id: &str, data: &[u8]) -> Option<LogBehind> {
        let (entry, stripper) = self.entries.get_mut(connection_id)?;

        // Run the stripper with per-connection persistent state.
        let clean = strip_ansi_stateful(stripper, data);
        if clean.is_empty() {
            return None;
        }

        let still_to_say = match entry {
            LogEntry::Writing(queue) => match queue.hand_on(clean) {
                Handed::Queued => return None,
                Handed::Behind => {
                    return Some(LogBehind {
                        shared: Arc::clone(&queue.0),
                        connection_id: connection_id.to_string(),
                        on_given_up: self.on_given_up.clone(),
                        stall_timeout: self.stall_timeout,
                    });
                }
                Handed::GivenUp => false,
                Handed::TooMuchWaiting => {
                    warn!(
                        connection_id = %connection_id,
                        limit = MAX_WAITING_BYTES,
                        "Too much text is waiting to be written to the session log; \
                         this session is no longer logged"
                    );
                    true
                }
            },
            // The reason was logged when the session connected.
            LogEntry::Unavailable => true,
        };
        // Dropping what was kept for the log closes it: a writer that still
        // has text writes it and ends.
        self.entries.remove(connection_id);
        if still_to_say {
            if let Some(notify) = &self.on_given_up {
                notify(connection_id);
            }
        }
        None
    }

    /// Whether a log is open, or prepared, for this connection.
    #[cfg(test)]
    pub fn has_log(&self, connection_id: &str) -> bool {
        match self.entries.get(connection_id) {
            Some((LogEntry::Writing(queue), _)) => !lock(&queue.0.state).given_up,
            Some((LogEntry::Unavailable, _)) | None => false,
        }
    }

    /// Close the log for a disconnected session. Its writer writes what it
    /// still has and closes the file; this does not wait for that.
    ///
    /// If nothing was ever written, no file exists on disk and nothing needs
    /// to be cleaned up.
    pub fn close_log(&mut self, connection_id: &str) {
        self.entries.remove(connection_id);
    }

    /// Close every log, as [`Self::close_log`] closes one, and return what
    /// tells when their writers have written what they had. For quitting:
    /// the writers are threads that the exit stops wherever they are, so
    /// the app waits for them first, without this lock.
    pub fn close_all(&mut self) -> Arc<LogWriters> {
        self.entries.clear();
        Arc::clone(&self.writers)
    }

    /// Wait until the writer of this connection's log has done all it was
    /// handed so far: prepared the log directory, written the text, or given
    /// the log up and said so.
    #[cfg(test)]
    fn settle(&self, connection_id: &str) {
        let Some((LogEntry::Writing(queue), _)) = self.entries.get(connection_id) else {
            return;
        };
        let (_state, timed_out) = queue
            .0
            .changed
            .wait_timeout_while(lock(&queue.0.state), TEST_WAIT, |state| {
                !(state.idle && state.queue.is_empty())
            })
            .unwrap();
        assert!(!timed_out.timed_out(), "the log's writer is still busy");
    }

    /// Wait until the writers of all logs that were closed have ended, and
    /// so have written what they had. Every log has to be closed.
    #[cfg(test)]
    fn wait_until_written(&self) {
        assert!(
            self.writers.wait_until_done(TEST_WAIT),
            "a log's writer is still running"
        );
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
    /// handed to a writer. Its log directory is never created.
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
        mgr.wait_until_written();
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
        assert!(!dir.exists());
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
        assert!(!dir.exists());
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
        assert!(!dir.exists());
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
        assert!(!dir.exists());
    }

    #[test]
    fn the_event_is_named_after_the_session() {
        assert_eq!(not_logged_event("abc-1"), "terminal-log-failed-abc-1");
    }

    #[test]
    fn no_file_is_created_before_the_first_output() {
        let (mut mgr, dir) = manager();
        mgr.open_log("a", "router").unwrap();
        mgr.settle("a");
        assert!(dir.is_dir());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        // Escape sequences alone are no output.
        mgr.write_log("a", b"\x1b[0m");
        mgr.settle("a");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        mgr.write_log("a", b"hello\n");
        mgr.write_log("a", b"again\n");
        mgr.close_log("a");
        mgr.wait_until_written();
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
        // Each log has a writer of its own: this one has its file before
        // the other asks for the name.
        mgr.settle("b");
        mgr.write_log("a", b"first session\n");
        mgr.write_log("b", b"more\n");
        mgr.close_log("a");
        mgr.close_log("b");
        mgr.wait_until_written();
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
        mgr.settle("a");
        // Put there after the log was prepared, as someone else who can
        // write to the directory might; a directory holds a name as well.
        std::fs::write(dir.join("router.log"), b"not ours").unwrap();
        std::fs::create_dir(dir.join("router_1.log")).unwrap();
        mgr.write_log("a", b"output\n");
        mgr.close_log("a");
        mgr.wait_until_written();
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
        mgr.settle("a");
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
        mgr.wait_until_written();

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
        mgr.settle("a");
        std::fs::write(dir.join("router.log"), b"").unwrap();
        for number in 1..=MAX_SAME_NAME {
            std::fs::write(dir.join(format!("router_{number}.log")), b"").unwrap();
        }
        let taken = usize::from(MAX_SAME_NAME) + 1;
        mgr.write_log("a", b"output\n");
        mgr.settle("a");
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
        mgr.settle("a");
        mgr.settle("b");
        std::fs::remove_dir(&dir).unwrap();
        mgr.write_log("a", b"output\n");
        mgr.settle("a");
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
        mgr.wait_until_written();
        assert_eq!(*reported.lock().unwrap(), ["a"]);
        assert_eq!(std::fs::read(dir.join("switch.log")).unwrap(), b"output\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A manager as [`manager`] gives, whose logs get files of at most
    /// `max_file_bytes` and keep `max_files` of them.
    fn manager_with_limits(max_file_bytes: u64, max_files: usize) -> (SessionLogManager, PathBuf) {
        let (mut mgr, dir) = manager();
        mgr.limits = LogLimits {
            max_file_bytes,
            max_files,
        };
        (mgr, dir)
    }

    /// The names in `dir`, sorted.
    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn text_of(path: &Path) -> String {
        String::from_utf8(std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn the_limits_are_those_of_the_config() {
        assert_eq!(
            LogLimits::new(10, 10),
            LogLimits {
                max_file_bytes: 10 * 1024 * 1024,
                max_files: 10,
            }
        );
        assert_eq!(LogLimits::default(), LogLimits::new(10, 10));
        // The largest values the config allows.
        let largest = LogLimits::new(
            *LoggingConfig::MAX_SIZE_MB.end(),
            *LoggingConfig::MAX_FILES.end(),
        );
        assert_eq!(largest.max_file_bytes, 256 * 1024 * 1024);
        assert_eq!(largest.max_files, 50);
    }

    /// However the outputs are put together for writing, a file takes what
    /// it has room for and no more: each output in a write of its own, and
    /// outputs that waited for the writer and were joined.
    #[test]
    fn a_log_that_is_full_goes_on_in_a_new_file() {
        for joined in [false, true] {
            let (mut mgr, dir) = manager_with_limits(16, 10);
            let reported = record_given_up(&mut mgr);
            mgr.open_log("a", "router").unwrap();
            let gate = Arc::clone(&mgr.write_gate);
            // Held, the writer gets no further than its first write, and
            // what is handed on after that waits and is joined.
            let held = joined.then(|| lock(&gate));
            for _ in 0..3 {
                mgr.write_log("a", b"0123456789\n");
                if !joined {
                    mgr.settle("a");
                }
            }
            drop(held);
            mgr.close_log("a");
            mgr.wait_until_written();
            assert_eq!(
                names_in(&dir),
                ["router.log", "router.part0002.log", "router.part0003.log"],
                "joined: {joined}"
            );
            assert_eq!(text_of(&dir.join("router.log")), "0123456789\n01234");
            assert_eq!(
                text_of(&dir.join("router.part0002.log")),
                "56789\n0123456789"
            );
            assert_eq!(text_of(&dir.join("router.part0003.log")), "\n");
            assert!(reported.lock().unwrap().is_empty());
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    /// A file that is exactly full is not followed by an empty one: the
    /// next file is created for text, not ahead of it.
    #[test]
    fn no_new_file_is_started_without_text_for_it() {
        let (mut mgr, dir) = manager_with_limits(8, 10);
        mgr.open_log("a", "router").unwrap();
        mgr.write_log("a", b"abcdefg\n");
        mgr.settle("a");
        assert_eq!(names_in(&dir), ["router.log"]);
        mgr.write_log("a", b"h");
        mgr.close_log("a");
        mgr.wait_until_written();
        assert_eq!(names_in(&dir), ["router.log", "router.part0002.log"]);
        assert_eq!(text_of(&dir.join("router.part0002.log")), "h");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_oldest_files_of_a_log_are_deleted() {
        let (mut mgr, dir) = manager_with_limits(8, 2);
        let reported = record_given_up(&mut mgr);
        mgr.open_log("a", "router").unwrap();
        mgr.open_log("b", "router").unwrap();
        mgr.settle("a");
        // Not this log's, so not this log's to delete: a file of another
        // session, and a file at a name this log would have taken, which
        // would be its oldest by now.
        mgr.write_log("b", b"other\n");
        mgr.settle("b");
        mgr.close_log("b");
        std::fs::write(dir.join("router_1.part0003.log"), b"not ours").unwrap();

        for part in [
            "one....\n",
            "two....\n",
            "three..\n",
            "four...\n",
            "five...\n",
        ] {
            mgr.write_log("a", part.as_bytes());
        }
        mgr.close_log("a");
        mgr.wait_until_written();
        // The log of `b` took the plain name, so this one is `router_1`,
        // and it passed over the name that was taken.
        assert_eq!(
            names_in(&dir),
            [
                "router.log",
                "router_1.part0003.log",
                "router_1.part0005.log",
                "router_1.part0006.log",
            ]
        );
        assert_eq!(text_of(&dir.join("router.log")), "other\n");
        assert_eq!(text_of(&dir.join("router_1.part0003.log")), "not ours");
        assert_eq!(text_of(&dir.join("router_1.part0005.log")), "four...\n");
        assert_eq!(text_of(&dir.join("router_1.part0006.log")), "five...\n");
        assert!(reported.lock().unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// With one file kept, the file that was just full is the one deleted.
    #[test]
    fn a_log_that_keeps_one_file_holds_the_latest_output() {
        let (mut mgr, dir) = manager_with_limits(8, 1);
        mgr.open_log("a", "router").unwrap();
        for part in ["one....\n", "two....\n", "three"] {
            mgr.write_log("a", part.as_bytes());
        }
        mgr.close_log("a");
        mgr.wait_until_written();
        assert_eq!(names_in(&dir), ["router.part0003.log"]);
        assert_eq!(text_of(&dir.join("router.part0003.log")), "three");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_ends_neither_inside_a_character_nor_inside_a_line_break() {
        // Between the CR and the LF there is room for the CR alone.
        let (mut mgr, dir) = manager_with_limits(3, 10);
        mgr.open_log("a", "router").unwrap();
        mgr.write_log("a", b"ab\r\ncd\r\n");
        mgr.close_log("a");
        mgr.wait_until_written();
        assert_eq!(text_of(&dir.join("router.log")), "ab");
        assert_eq!(text_of(&dir.join("router.part0002.log")), "\r\nc");
        assert_eq!(text_of(&dir.join("router.part0003.log")), "d\r\n");
        assert_eq!(names_in(&dir).len(), 3);
        std::fs::remove_dir_all(&dir).unwrap();

        // Two bytes to a character: the third byte of room stays unused.
        let (mut mgr, dir) = manager_with_limits(3, 10);
        mgr.open_log("a", "router").unwrap();
        mgr.write_log("a", "äöü".as_bytes());
        mgr.close_log("a");
        mgr.wait_until_written();
        assert_eq!(text_of(&dir.join("router.log")), "ä");
        assert_eq!(text_of(&dir.join("router.part0002.log")), "ö");
        assert_eq!(text_of(&dir.join("router.part0003.log")), "ü");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A limit that is smaller than a character, which the config does not
    /// allow, still lets a log get on: a character to a file.
    #[test]
    fn a_limit_smaller_than_a_character_does_not_stop_a_log() {
        let (mut mgr, dir) = manager_with_limits(1, 10);
        mgr.open_log("a", "router").unwrap();
        mgr.write_log("a", "ä\r\nx".as_bytes());
        mgr.close_log("a");
        mgr.wait_until_written();
        assert_eq!(text_of(&dir.join("router.log")), "ä");
        assert_eq!(text_of(&dir.join("router.part0002.log")), "\r\n");
        assert_eq!(text_of(&dir.join("router.part0003.log")), "x");
        assert_eq!(names_in(&dir).len(), 3);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_room_in_a_file_counts_what_went_into_it() {
        let dir = std::env::temp_dir().join(format!("ss-sessionlog-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("router.log");
        let file = private_fs::create_private_append_file(&path).unwrap();
        let mut log = ActiveLog::new(file, path);

        assert_eq!(log.split_for_room("abcd", 4), ("abcd", ""));
        assert_eq!(log.split_for_room("abcde", 4), ("abcd", "e"));
        // The write after a failed one starts with a line feed, which takes
        // room as well.
        log.after_failed_write = true;
        assert_eq!(log.split_for_room("abcd", 4), ("abc", "d"));
        log.after_failed_write = false;

        log.file.write_all(b"ab").unwrap();
        assert_eq!(log.file.written, 2);
        assert_eq!(log.split_for_room("cd", 4), ("cd", ""));
        assert_eq!(log.split_for_room("cde", 4), ("cd", "e"));
        assert_eq!(log.split_for_room("c\r\n", 4), ("c", "\r\n"));
        assert_eq!(log.split_for_room("cä", 4), ("c", "ä"));
        // A file that holds something takes nothing it has no room for.
        log.file.write_all(b"cd").unwrap();
        assert_eq!(log.split_for_room("e", 4), ("", "e"));
        assert_eq!(log.split_for_room("e", 2), ("", "e"));
        drop(log);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Bytes that went into the file count, also those of a write that then
    /// failed: they take room in the file all the same.
    #[test]
    fn bytes_of_a_failed_write_are_counted() {
        struct TakesThree(usize);
        impl Write for TakesThree {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                if self.0 == 3 {
                    return Err(io::Error::other("full"));
                }
                let taken = buf.len().min(3 - self.0);
                self.0 += taken;
                Ok(taken)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut counted = Counted::new(TakesThree(0));
        assert!(counted.write_all(b"abcdef").is_err());
        assert_eq!(counted.written, 3);
        assert!(counted.write_all(b"g").is_err());
        assert_eq!(counted.written, 3);
    }

    #[test]
    fn a_name_that_is_taken_is_passed_over_for_the_next_file() {
        let (mut mgr, dir) = manager_with_limits(4, 1);
        mgr.open_log("a", "router").unwrap();
        mgr.settle("a");
        // As a log of an earlier session with this name may have left it,
        // or someone else who can write to the directory.
        std::fs::write(dir.join("router.part0002.log"), b"not ours").unwrap();
        std::fs::create_dir(dir.join("router.part0003.log")).unwrap();
        mgr.write_log("a", b"aaaabbbb");
        mgr.close_log("a");
        mgr.wait_until_written();
        // Neither written to nor deleted, although one file is kept.
        assert_eq!(text_of(&dir.join("router.part0002.log")), "not ours");
        assert!(dir.join("router.part0003.log").is_dir());
        assert_eq!(text_of(&dir.join("router.part0004.log")), "bbbb");
        assert_eq!(names_in(&dir).len(), 3);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_symlink_at_the_name_of_the_next_file_is_not_written_through() {
        let (mut mgr, dir) = manager_with_limits(4, 10);
        mgr.open_log("a", "router").unwrap();
        mgr.settle("a");
        let elsewhere = std::env::temp_dir().join(format!("ss-elsewhere-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&elsewhere).unwrap();
        let existing = elsewhere.join("existing");
        std::fs::write(&existing, b"untouched").unwrap();
        let missing = elsewhere.join("missing");
        for (name, target) in [
            ("router.part0002.log", &existing),
            ("router.part0003.log", &missing),
        ] {
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

        mgr.write_log("a", b"aaaabbbb");
        mgr.close_log("a");
        mgr.wait_until_written();

        assert_eq!(std::fs::read(&existing).unwrap(), b"untouched");
        assert!(!missing.exists());
        assert_eq!(text_of(&dir.join("router.part0004.log")), "bbbb");
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&elsewhere).unwrap();
    }

    #[test]
    fn a_log_that_cannot_go_on_in_a_new_file_is_given_up() {
        let (mut mgr, dir) = manager_with_limits(4, 10);
        let reported = record_given_up(&mut mgr);
        mgr.open_log("a", "router").unwrap();
        mgr.write_log("a", b"aaaa");
        mgr.settle("a");
        for number in 2..MAX_PART_NAMES_TRIED + 2 {
            std::fs::write(dir.join(format!("router.part{number:04}.log")), b"").unwrap();
        }
        assert!(mgr.has_log("a"));
        mgr.write_log("a", b"bbbb");
        mgr.settle("a");
        assert!(!mgr.has_log("a"));
        // Not tried again with the next output, and said once.
        std::fs::remove_file(dir.join("router.part0002.log")).unwrap();
        mgr.write_log("a", b"cccc");
        mgr.close_log("a");
        mgr.wait_until_written();
        assert_eq!(*reported.lock().unwrap(), ["a"]);
        // What the log held stays.
        assert_eq!(text_of(&dir.join("router.log")), "aaaa");
        assert!(!dir.join("router.part0002.log").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Put a directory where a file of a log was, so that deleting the file
    /// by its name fails.
    fn make_undeletable(path: &Path) {
        std::fs::remove_file(path).unwrap();
        std::fs::create_dir(path).unwrap();
    }

    #[test]
    fn a_file_that_could_not_be_deleted_is_tried_again() {
        let (mut mgr, dir) = manager_with_limits(4, 2);
        let reported = record_given_up(&mut mgr);
        mgr.open_log("a", "router").unwrap();
        mgr.write_log("a", b"aaaabbbb");
        mgr.settle("a");
        make_undeletable(&dir.join("router.log"));

        mgr.write_log("a", b"cccc");
        mgr.settle("a");
        // One file more than is kept, and the log goes on.
        assert!(mgr.has_log("a"));
        assert_eq!(
            names_in(&dir),
            ["router.log", "router.part0002.log", "router.part0003.log"]
        );

        // Whatever kept it from being deleted is over.
        std::fs::remove_dir(dir.join("router.log")).unwrap();
        std::fs::write(dir.join("router.log"), b"aaaa").unwrap();
        mgr.write_log("a", b"dddd");
        mgr.close_log("a");
        mgr.wait_until_written();
        assert_eq!(
            names_in(&dir),
            ["router.part0003.log", "router.part0004.log"]
        );
        assert!(reported.lock().unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A log whose oldest files cannot be deleted has no limit, so it ends
    /// once more than twice the files it may keep would be left.
    #[test]
    fn a_log_with_too_many_files_that_cannot_be_deleted_is_given_up() {
        let (mut mgr, dir) = manager_with_limits(4, 1);
        let reported = record_given_up(&mut mgr);
        mgr.open_log("a", "router").unwrap();
        mgr.write_log("a", b"aaaabbbb");
        mgr.settle("a");
        // The first file went when the second was started.
        assert_eq!(names_in(&dir), ["router.part0002.log"]);

        // The file being written is the newest, so the one before it is
        // closed and can be swapped for something that cannot be deleted.
        mgr.limits.max_files = 2;
        mgr.open_log("b", "switch").unwrap();
        mgr.write_log("b", b"aaaabbbb");
        mgr.settle("b");
        make_undeletable(&dir.join("switch.log"));
        mgr.write_log("b", b"cccc");
        mgr.settle("b");
        make_undeletable(&dir.join("switch.part0002.log"));
        mgr.write_log("b", b"dddd");
        mgr.settle("b");
        // Two that could not be deleted and two that are kept: the limit.
        assert!(mgr.has_log("b"));
        assert_eq!(names_in(&dir).len(), 5);
        make_undeletable(&dir.join("switch.part0003.log"));
        mgr.write_log("b", b"eeee");
        mgr.settle("b");
        assert!(!mgr.has_log("b"));
        assert_eq!(*reported.lock().unwrap(), ["b"]);
        // The other log is not affected.
        assert!(mgr.has_log("a"));

        mgr.write_log("b", b"ffff");
        mgr.close_log("a");
        mgr.close_log("b");
        mgr.wait_until_written();
        assert_eq!(*reported.lock().unwrap(), ["b"]);
        assert_eq!(text_of(&dir.join("switch.part0004.log")), "dddd");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Settings saved while a session is connected are for the sessions
    /// that connect after: a log keeps the limits it started with.
    #[test]
    fn a_log_keeps_the_limits_it_was_opened_with() {
        let (mut mgr, dir) = manager_with_limits(4, 10);
        mgr.open_log("a", "router").unwrap();
        mgr.limits = LogLimits::default();
        mgr.open_log("b", "switch").unwrap();
        mgr.write_log("a", b"aaaabbbb");
        mgr.write_log("b", b"aaaabbbb");
        mgr.close_log("a");
        mgr.close_log("b");
        mgr.wait_until_written();
        assert_eq!(
            names_in(&dir),
            ["router.log", "router.part0002.log", "switch.log"]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A log directory that cannot be created is found out by the writer,
    /// not at connect, and said with the first output all the same.
    #[test]
    fn a_log_directory_that_cannot_be_created_is_reported_on_the_first_output() {
        let file = std::env::temp_dir().join(format!("ss-notadir-{}", uuid::Uuid::new_v4()));
        std::fs::write(&file, b"").unwrap();
        let mut mgr = SessionLogManager::new(true, file.join("logs"), "{name}.log".to_string());
        let reported = record_given_up(&mut mgr);
        mgr.open_log("a", "router").unwrap();
        mgr.settle("a");
        assert!(reported.lock().unwrap().is_empty());
        mgr.write_log("a", b"\x1b[0m");
        mgr.settle("a");
        assert!(reported.lock().unwrap().is_empty());
        mgr.write_log("a", b"output\n");
        mgr.settle("a");
        assert!(!mgr.has_log("a"));
        assert_eq!(*reported.lock().unwrap(), ["a"]);
        mgr.write_log("a", b"more\n");
        mgr.close_log("a");
        mgr.wait_until_written();
        assert_eq!(*reported.lock().unwrap(), ["a"]);
        std::fs::remove_file(&file).unwrap();
    }

    /// Wait until the log at `path` has been created, which is when its
    /// writer has taken the session's first text.
    fn wait_for_file(path: &Path) {
        let started = std::time::Instant::now();
        while !path.exists() {
            assert!(
                started.elapsed() < TEST_WAIT,
                "{} is not there",
                path.display()
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// The known issue this replaces: a log was written under the lock all
    /// sessions share, so a write that took its time held up the output of
    /// every session. Here a write takes for as long as the test holds the
    /// gate, and output goes on being taken meanwhile, for the session with
    /// the slow log and for others.
    #[test]
    fn output_does_not_wait_for_a_log_that_is_slow() {
        let (mut mgr, dir) = manager();
        let reported = record_given_up(&mut mgr);
        let gate = Arc::clone(&mgr.write_gate);
        let writing = gate.lock().unwrap();

        mgr.open_log("a", "router").unwrap();
        mgr.write_log("a", b"one\n");
        // The writer has the text and is at the write.
        wait_for_file(&dir.join("router.log"));

        mgr.write_log("a", b"two\n");
        mgr.write_log("a", b"three\n");
        // A session that connects meanwhile, and its output.
        mgr.open_log("b", "switch").unwrap();
        mgr.write_log("b", b"other\n");
        wait_for_file(&dir.join("switch.log"));
        // Closing does not wait either.
        mgr.close_log("b");
        assert_eq!(std::fs::read(dir.join("router.log")).unwrap(), b"");
        assert_eq!(std::fs::read(dir.join("switch.log")).unwrap(), b"");

        // Once the writes go through, nothing is missing.
        drop(writing);
        mgr.close_log("a");
        mgr.wait_until_written();
        assert_eq!(
            std::fs::read(dir.join("router.log")).unwrap(),
            b"one\ntwo\nthree\n"
        );
        assert_eq!(std::fs::read(dir.join("switch.log")).unwrap(), b"other\n");
        assert!(reported.lock().unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Half of what may wait for a log's writer, as a line of output.
    fn half_the_limit() -> Vec<u8> {
        let mut chunk = vec![b'x'; MAX_WAITING_BYTES / 2 - 1];
        chunk.push(b'\n');
        chunk
    }

    /// Open the log `router.log` for session `a` and put it behind: its
    /// writer is at the write of `first`, which takes for as long as the
    /// caller holds the gate, and two more outputs wait, which is more than
    /// may. Returns what the session is to wait on.
    fn log_that_is_behind(mgr: &mut SessionLogManager, dir: &Path) -> LogBehind {
        mgr.open_log("a", "router").unwrap();
        assert!(mgr.write_log("a", b"first\n").is_none());
        wait_for_file(&dir.join("router.log"));
        let chunk = half_the_limit();
        assert!(mgr.write_log("a", &chunk).is_none());
        mgr.write_log("a", &chunk).expect("the log is behind")
    }

    /// What `router.log` holds once [`log_that_is_behind`] is written.
    fn assert_all_that_waited_is_written(dir: &Path) {
        let log = std::fs::read(dir.join("router.log")).unwrap();
        assert_eq!(log.len(), b"first\n".len() + MAX_WAITING_BYTES);
        assert!(log.starts_with(b"first\nxxx"));
        assert!(log.ends_with(b"xxx\n"));
    }

    /// Wait on `behind` on a thread of its own, as a reader does. The
    /// receiver hears when the wait is over.
    fn wait_on_a_thread(behind: LogBehind) -> std::sync::mpsc::Receiver<()> {
        let (done, is_done) = std::sync::mpsc::channel();
        thread::spawn(move || {
            behind.wait();
            let _ = done.send(());
        });
        is_done
    }

    /// A log that is slow slows its own session once it is far behind, and
    /// loses nothing: the session waits until the writer has caught up.
    #[test]
    fn a_session_waits_for_a_log_that_is_behind() {
        let (mut mgr, dir) = manager();
        let reported = record_given_up(&mut mgr);
        mgr.stall_timeout = TEST_WAIT;
        let gate = Arc::clone(&mgr.write_gate);
        let writing = gate.lock().unwrap();

        let waited = wait_on_a_thread(log_that_is_behind(&mut mgr, &dir));
        assert_eq!(
            waited.recv_timeout(Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        );
        // Another session does not wait, whatever it puts out meanwhile.
        mgr.open_log("b", "switch").unwrap();
        assert!(mgr.write_log("b", b"other\n").is_none());

        // The writes go through: the wait is over, and the log goes on.
        drop(writing);
        waited.recv_timeout(TEST_WAIT).unwrap();
        assert!(mgr.has_log("a"));
        // The writer may still be at what waited, and the log behind again
        // with this.
        if let Some(behind) = mgr.write_log("a", b"last\n") {
            behind.wait();
        }
        mgr.close_log("a");
        mgr.close_log("b");
        mgr.wait_until_written();
        let log = std::fs::read(dir.join("router.log")).unwrap();
        assert_eq!(
            log.len(),
            b"first\n".len() + MAX_WAITING_BYTES + b"last\n".len()
        );
        assert!(log.ends_with(b"xxx\nlast\n"));
        assert_eq!(std::fs::read(dir.join("switch.log")).unwrap(), b"other\n");
        assert!(reported.lock().unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A log directory that takes nothing holds its session up once, for the
    /// stall timeout, and then the log is given up.
    #[test]
    fn a_log_that_gets_no_further_is_given_up() {
        let (mut mgr, dir) = manager();
        let reported = record_given_up(&mut mgr);
        mgr.stall_timeout = Duration::from_millis(100);
        let gate = Arc::clone(&mgr.write_gate);
        let writing = gate.lock().unwrap();

        mgr.open_log("b", "switch").unwrap();
        let behind = log_that_is_behind(&mut mgr, &dir);
        assert!(reported.lock().unwrap().is_empty());
        let started = std::time::Instant::now();
        behind.wait();
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert!(!mgr.has_log("a"));
        assert_eq!(*reported.lock().unwrap(), ["a"]);
        // Said once, and the session's output is no longer kept or waited
        // for.
        assert!(mgr.write_log("a", &half_the_limit()).is_none());
        assert!(mgr.write_log("a", &half_the_limit()).is_none());
        assert!(!mgr.entries.contains_key("a"));
        assert_eq!(*reported.lock().unwrap(), ["a"]);
        // The log of another session is not given up with it.
        assert!(mgr.write_log("b", b"other\n").is_none());
        assert!(mgr.has_log("b"));

        // What was waiting is still written, should the directory answer
        // again, and the log ends there.
        drop(writing);
        mgr.close_log("b");
        mgr.wait_until_written();
        assert_all_that_waited_is_written(&dir);
        assert_eq!(std::fs::read(dir.join("switch.log")).unwrap(), b"other\n");
        assert_eq!(*reported.lock().unwrap(), ["a"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A write that is slow but gets somewhere is not taken for one that
    /// does not: whatever is written starts the stall timeout anew. The test
    /// is the writer here, and writes a little at a time for longer than
    /// the timeout in all.
    #[test]
    fn a_log_that_is_slow_but_gets_on_is_not_given_up() {
        const STEP: Duration = Duration::from_millis(250);
        let reported = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&reported);
        let shared = Arc::new(LogShared::default());
        lock(&shared.state).writing = MAX_WAITING_BYTES + 100;
        let waited = wait_on_a_thread(LogBehind {
            shared: Arc::clone(&shared),
            connection_id: "a".to_string(),
            on_given_up: Some(Arc::new(move |id: &str| {
                sink.lock().unwrap().push(id.to_string());
            })),
            stall_timeout: 6 * STEP,
        });
        for _ in 0..8 {
            thread::sleep(STEP);
            assert_eq!(waited.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
            lock(&shared.state).writing -= 1;
            shared.changed.notify_all();
        }
        assert_eq!(
            waited.recv_timeout(STEP),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        );
        assert!(!lock(&shared.state).given_up);
        assert!(reported.lock().unwrap().is_empty());

        // And then nothing for as long as the timeout.
        waited.recv_timeout(TEST_WAIT).unwrap();
        assert!(lock(&shared.state).given_up);
        assert_eq!(*reported.lock().unwrap(), ["a"]);
    }

    /// Closing a tab ends its session's wait for the log at once.
    #[test]
    fn a_wait_for_a_log_ends_when_the_log_is_closed() {
        let (mut mgr, dir) = manager();
        let reported = record_given_up(&mut mgr);
        mgr.stall_timeout = TEST_WAIT;
        let gate = Arc::clone(&mgr.write_gate);
        let writing = gate.lock().unwrap();

        let waited = wait_on_a_thread(log_that_is_behind(&mut mgr, &dir));
        assert_eq!(
            waited.recv_timeout(Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        );
        mgr.close_log("a");
        waited.recv_timeout(TEST_WAIT).unwrap();
        assert!(reported.lock().unwrap().is_empty());

        drop(writing);
        mgr.wait_until_written();
        assert_all_that_waited_is_written(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// What waits to be written has a limit whatever a reader does: one that
    /// hands on more without having waited has its log given up.
    #[test]
    fn a_log_handed_more_without_waiting_is_given_up() {
        let (mut mgr, dir) = manager();
        let reported = record_given_up(&mut mgr);
        let gate = Arc::clone(&mgr.write_gate);
        let writing = gate.lock().unwrap();

        drop(log_that_is_behind(&mut mgr, &dir));
        assert!(mgr.has_log("a"));
        assert!(reported.lock().unwrap().is_empty());
        assert!(mgr.write_log("a", b"one too many\n").is_none());
        assert!(!mgr.has_log("a"));
        assert_eq!(*reported.lock().unwrap(), ["a"]);
        assert!(mgr.write_log("a", b"more\n").is_none());
        assert_eq!(*reported.lock().unwrap(), ["a"]);

        drop(writing);
        mgr.wait_until_written();
        assert_all_that_waited_is_written(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The readers' way in: a reader waits for its log without the lock all
    /// sessions share, so the output of another session is taken meanwhile.
    #[test]
    fn a_reader_waits_for_its_log_without_the_shared_lock() {
        let (mut mgr, dir) = manager();
        let reported = record_given_up(&mut mgr);
        mgr.stall_timeout = TEST_WAIT;
        let gate = Arc::clone(&mgr.write_gate);
        let writing = gate.lock().unwrap();
        mgr.open_log("a", "router").unwrap();
        mgr.open_log("b", "switch").unwrap();
        let logger = Arc::new(Mutex::new(mgr));

        log_output(&logger, "a", b"first\n");
        wait_for_file(&dir.join("router.log"));
        log_output(&logger, "a", &half_the_limit());
        let (done, is_done) = std::sync::mpsc::channel();
        let reader = Arc::clone(&logger);
        thread::spawn(move || {
            log_output(&reader, "a", &half_the_limit());
            let _ = done.send(());
        });
        assert_eq!(
            is_done.recv_timeout(Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        );
        // On a thread, so that a reader that held the shared lock while it
        // waited would fail the test and not hang it.
        let (taken, is_taken) = std::sync::mpsc::channel();
        let other = Arc::clone(&logger);
        thread::spawn(move || {
            log_output(&other, "b", b"other\n");
            let _ = taken.send(());
        });
        is_taken.recv_timeout(TEST_WAIT).unwrap();
        assert_eq!(
            is_done.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        );

        drop(writing);
        is_done.recv_timeout(TEST_WAIT).unwrap();
        let mut mgr = logger.lock().unwrap();
        mgr.close_all();
        mgr.wait_until_written();
        assert_all_that_waited_is_written(&dir);
        assert_eq!(std::fs::read(dir.join("switch.log")).unwrap(), b"other\n");
        assert!(reported.lock().unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A reader that is a task waits on a blocking thread, so that a
    /// runtime with a single thread, as this test's is, goes on meanwhile.
    #[tokio::test]
    async fn a_task_waits_for_its_log_off_the_runtime() {
        let (mut mgr, dir) = manager();
        let reported = record_given_up(&mut mgr);
        mgr.stall_timeout = Duration::from_millis(300);
        // The gate is held on a thread, until `release` is dropped: a lock
        // is not for holding across an await.
        let gate = Arc::clone(&mgr.write_gate);
        let (release, released) = std::sync::mpsc::channel::<()>();
        let (held, is_held) = std::sync::mpsc::channel::<()>();
        thread::spawn(move || {
            let _writing = gate.lock().unwrap();
            held.send(()).unwrap();
            let _ = released.recv();
        });
        is_held.recv().unwrap();
        mgr.open_log("a", "router").unwrap();
        let logger = Arc::new(Mutex::new(mgr));

        log_output_async(&logger, "a", b"first\n").await;
        wait_for_file(&dir.join("router.log"));
        log_output_async(&logger, "a", &half_the_limit()).await;
        let chunk = half_the_limit();
        let log_wait = log_output_async(&logger, "a", &chunk);
        tokio::pin!(log_wait);
        // Other tasks get their turn while this one waits for its log.
        let mut turns = 0;
        loop {
            tokio::select! {
                () = &mut log_wait => break,
                () = tokio::time::sleep(Duration::from_millis(10)) => turns += 1,
            }
        }
        assert!(turns > 0);
        assert_eq!(*reported.lock().unwrap(), ["a"]);

        drop(release);
        let writers = logger.lock().unwrap().close_all();
        assert!(writers.wait_until_done(TEST_WAIT));
        assert_all_that_waited_is_written(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// What goes into one write: the outputs that wait, in their order, as
    /// far as they fit in a piece together, and a single output whole.
    #[test]
    fn outputs_that_wait_are_written_a_piece_at_a_time() {
        let mut state = LogState::default();
        let outputs = [
            "a".repeat(WRITE_PIECE_BYTES / 2),
            "b".repeat(WRITE_PIECE_BYTES / 2),
            "c".to_string(),
            "d".repeat(2 * WRITE_PIECE_BYTES),
            "e".to_string(),
        ];
        let all: usize = outputs.iter().map(String::len).sum();
        for output in &outputs {
            state.queued += output.len();
            state.queue.push_back(output.clone());
        }
        assert_eq!(state.waiting(), all);

        let mut pieces: Vec<String> = Vec::new();
        let mut written = 0;
        while let Some(piece) = state.take_piece() {
            // What the writer is at still counts as waiting.
            assert_eq!(state.writing, piece.len());
            assert_eq!(state.waiting(), all - written);
            written += piece.len();
            pieces.push(piece);
            state.writing = 0;
        }
        assert_eq!(
            pieces,
            [
                [outputs[0].as_str(), outputs[1].as_str()].concat(),
                outputs[2].clone(),
                outputs[3].clone(),
                outputs[4].clone(),
            ]
        );
        assert_eq!(state.waiting(), 0);
        assert!(state.queue.is_empty());
    }

    /// Outputs of many writes' worth, with characters of several bytes, are
    /// in the file whole and in their order, and nothing is left waiting.
    /// While they wait they are joined, so that small outputs do not take a
    /// buffer each.
    #[test]
    fn text_of_many_writes_is_written_whole() {
        let (mut mgr, dir) = manager();
        let gate = Arc::clone(&mgr.write_gate);
        let writing = gate.lock().unwrap();
        mgr.open_log("a", "router").unwrap();
        let shared = match mgr.entries.get("a") {
            Some((LogEntry::Writing(queue), _)) => Arc::clone(&queue.0),
            _ => panic!("the log has no writer"),
        };

        // The writer is at the first output for as long as the gate is
        // held, and the others wait.
        assert!(mgr.write_log("a", b"first\n").is_none());
        wait_for_file(&dir.join("router.log"));
        let mut text = "first\n".to_string();
        for line in 0..400 {
            let output = format!("{line} {}\n", "é".repeat(500));
            assert!(mgr.write_log("a", output.as_bytes()).is_none());
            text.push_str(&output);
        }
        assert!(text.len() > 6 * WRITE_PIECE_BYTES);
        {
            let state = lock(&shared.state);
            assert_eq!(state.waiting(), text.len());
            assert_eq!(state.writing, b"first\n".len());
            let lengths: Vec<usize> = state.queue.iter().map(String::len).collect();
            assert_eq!(lengths.iter().sum::<usize>(), state.queued);
            assert_eq!(lengths.len(), 7, "{lengths:?}");
            assert!(lengths.iter().all(|len| *len <= WRITE_PIECE_BYTES));
        }
        // An output without text takes no place. This end of the log is
        // not dropped, which would close the log.
        let session = std::mem::ManuallyDrop::new(LogQueue(Arc::clone(&shared)));
        assert!(matches!(session.hand_on(String::new()), Handed::Queued));
        assert_eq!(lock(&shared.state).queue.len(), 7);

        drop(writing);
        mgr.close_log("a");
        mgr.wait_until_written();
        assert_eq!(
            std::fs::read(dir.join("router.log")).unwrap(),
            text.as_bytes()
        );
        assert_eq!(lock(&shared.state).waiting(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A writer that ends in a panic gives its log up on the way out, so
    /// that the session is told and hands on no more.
    #[test]
    fn a_writer_that_panics_gives_its_log_up() {
        let reported = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&reported);
        let shared = Arc::new(LogShared::default());
        let writers = Arc::new(LogWriters::default());
        let writer = LogWriter {
            connection_id: "a".to_string(),
            shared: Arc::clone(&shared),
            on_given_up: Some(Arc::new(move |id: &str| {
                sink.lock().unwrap().push(id.to_string());
            })),
            limits: LogLimits::default(),
            _running: writers.count().unwrap(),
            write_gate: Arc::default(),
        };
        let queue = LogQueue(Arc::clone(&shared));
        assert!(matches!(
            queue.hand_on("output\n".to_string()),
            Handed::Queued
        ));
        // As when the panic comes while the writer is at a write.
        lock(&shared.state).writing = 100;

        let panicked = thread::spawn(move || {
            let _writer = writer;
            panic!("a panic in the writer, for the test");
        })
        .join();
        assert!(panicked.is_err());
        assert_eq!(*reported.lock().unwrap(), ["a"]);
        assert_eq!(lock(&shared.state).waiting(), 0);
        assert!(matches!(
            queue.hand_on("more\n".to_string()),
            Handed::GivenUp
        ));
        // It no longer counts as running either.
        assert!(writers.wait_until_done(Duration::ZERO));
        assert_eq!(*reported.lock().unwrap(), ["a"]);
    }

    /// Writers that are stuck outlive their sessions, so their number has a
    /// limit of its own: at the limit, a session gets no log and says so.
    #[test]
    fn a_session_gets_no_log_while_too_many_are_still_being_written() {
        let (mut mgr, dir) = manager();
        let reported = record_given_up(&mut mgr);
        *mgr.writers.running.lock().unwrap() = MAX_WRITERS;
        assert!(mgr.open_log("a", "router").is_err());
        assert!(!mgr.has_log("a"));
        mgr.write_log("a", b"output\n");
        assert_eq!(*reported.lock().unwrap(), ["a"]);
        assert!(!dir.exists());

        // Once one of them has ended, the next session has a log again.
        *mgr.writers.running.lock().unwrap() = MAX_WRITERS - 1;
        mgr.open_log("b", "router").unwrap();
        mgr.write_log("b", b"output\n");
        mgr.close_log("b");
        assert!(!mgr.writers.wait_until_done(Duration::ZERO));
        *mgr.writers.running.lock().unwrap() -= MAX_WRITERS - 1;
        mgr.wait_until_written();
        assert_eq!(std::fs::read(dir.join("router.log")).unwrap(), b"output\n");
        assert_eq!(*reported.lock().unwrap(), ["a"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Quitting closes every log and waits for the writers, which the exit
    /// would stop wherever they are.
    #[test]
    fn closing_every_log_tells_when_they_are_written() {
        let (mut mgr, dir) = manager();
        let gate = Arc::clone(&mgr.write_gate);
        let writing = gate.lock().unwrap();

        mgr.open_log("a", "router").unwrap();
        mgr.open_log("b", "switch").unwrap();
        // A session that never put anything out.
        mgr.open_log("c", "idle").unwrap();
        mgr.write_log("a", b"output\n");
        mgr.write_log("b", b"other\n");
        let writers = mgr.close_all();
        assert!(!mgr.has_log("a"));
        // Not while a write is under way, however long that is waited for.
        assert!(!writers.wait_until_done(Duration::from_millis(100)));

        drop(writing);
        assert!(writers.wait_until_done(TEST_WAIT));
        assert_eq!(std::fs::read(dir.join("router.log")).unwrap(), b"output\n");
        assert_eq!(std::fs::read(dir.join("switch.log")).unwrap(), b"other\n");
        assert!(!dir.join("idle.log").exists());
        // With no log open, there is nothing to wait for.
        assert!(mgr.close_all().wait_until_done(Duration::ZERO));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_new_log_and_its_directory_are_owner_only() {
        let (mut mgr, dir) = manager();
        mgr.open_log("a", "router").unwrap();
        mgr.write_log("a", b"output\n");
        mgr.close_log("a");
        mgr.wait_until_written();
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
        mgr.wait_until_written();
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
        mgr.wait_until_written();
        assert_eq!(
            std::fs::read(dir.join("router.log")).unwrap(),
            "shownfake größe\r\n".as_bytes()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
