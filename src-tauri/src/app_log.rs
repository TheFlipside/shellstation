//! The files of the application log: one for each day, in UTC, named
//! `shellstation.YYYY-MM-DD.log`.
//!
//! The app opens them itself. A logging library's rolling writer opens
//! whatever holds the day's name, follows a link there and waits on a FIFO,
//! and a log directory may be one where others can put either: one that
//! could not be restricted to its owner. Here a day's file is created for
//! its owner alone, as a session log is, and one that is already there, from
//! an earlier start on the same day, is appended to only if it is a plain
//! file of the user's own: see `open_own_append_file` in `private_fs.rs`.
//! A name that holds anything else is passed over for the next of `_1` to
//! `_999` before the extension.
//!
//! As with a session log, this protects the names of the files and not the
//! log directory: someone able to replace that directory, or one above it,
//! with a link decides where the log goes.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::NaiveDate;

use crate::private_fs;

/// How many more names a day's file may get, as `_1` to `_999` after the
/// date, when the names before hold something that is not appended to.
const MAX_OTHER_NAMES: u16 = 999;

/// How long the names after the first may be looked through. A start waits
/// for the day's file before there is a window, and an open can be made to
/// take long by whoever holds the name, on Windows by a lock of theirs on a
/// file of their own: a thousand such names are not waited for.
const SEARCH_TIME: Duration = Duration::from_secs(2);

/// The name of a day's file: `shellstation.2026-03-01.log`, or with
/// `other` from 1 on, `shellstation.2026-03-01_1.log`.
fn file_name(day: NaiveDate, other: u16) -> String {
    if other == 0 {
        format!("shellstation.{day}.log")
    } else {
        format!("shellstation.{day}_{other}.log")
    }
}

/// A day's file as [`open_for_day`] opened it.
struct DayFile {
    file: File,
    /// Its name in the log directory.
    name: String,
    /// The first name that was passed over for it, and why.
    passed_over: Option<(String, io::Error)>,
}

impl DayFile {
    /// What to tell the user when the file is not the one of the day's own
    /// name. The names are the app's own, so they need no escaping.
    fn note(&self) -> Option<String> {
        self.passed_over.as_ref().map(|(name, why)| {
            format!(
                "The application log is written to {}: {name} could not be used ({why})",
                self.name
            )
        })
    }
}

/// What [`open_existing`] found at a name.
enum AtName {
    /// A file that is appended to.
    Opened(File),
    /// Nothing, as far as can be told.
    Free,
    /// Something that is not appended to, and why.
    Refused(io::Error),
}

/// Open what is at `path` for appending, if it is there and is appended to.
/// An error is one that is not about the name, and ends the search.
fn open_existing(path: &Path) -> io::Result<AtName> {
    match private_fs::open_own_append_file(path) {
        Ok(file) => Ok(AtName::Opened(file)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(AtName::Free),
        // Refused for what it is, or not opened. That is about the name
        // only if something holds it, which a look at it tells, without
        // following a link. If the look fails as well, the directory cannot
        // be searched or does not answer, and the next name would fare no
        // better.
        Err(e) => match std::fs::symlink_metadata(path) {
            Ok(_) => Ok(AtName::Refused(e)),
            // Gone in the moment between; another name will do.
            Err(look) if look.kind() == io::ErrorKind::NotFound => Ok(AtName::Refused(e)),
            Err(_) => Err(e),
        },
    }
}

/// Open the file at `path` for appending, or create it. The inner error is
/// why the name is passed over; the outer one is not about the name.
fn open_or_create(path: &Path) -> io::Result<Result<File, io::Error>> {
    let mut second_round = false;
    loop {
        match open_existing(path)? {
            AtName::Opened(file) => return Ok(Ok(file)),
            AtName::Refused(why) => return Ok(Err(why)),
            AtName::Free => {}
        }
        match private_fs::create_private_append_file(path) {
            Ok(file) => return Ok(Ok(file)),
            // Taken in the moment since the look. By another instance of
            // the app, most likely, which has just created the day's file:
            // a second round opens that one. Whatever else took the name
            // is refused there, or holds it against the create once more.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if second_round {
                    return Ok(Err(e));
                }
                second_round = true;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Open the file of `day` in `dir`, which must be there: the file of the
/// day's name if it can be appended to or created, and else the first of the
/// other names that can.
///
/// A name is passed over when something holds it that is not appended to: a
/// link, a FIFO, a directory, a file of another user's, one that others
/// could open or one with a second name, or anything else there that cannot
/// be opened. An error that is not about the name ends the search: a
/// directory that is missing or cannot be searched or written to, a full
/// disk. So does a search that takes longer than [`SEARCH_TIME`].
fn open_for_day(dir: &Path, day: NaiveDate) -> io::Result<DayFile> {
    open_for_day_within(dir, day, SEARCH_TIME)
}

fn open_for_day_within(dir: &Path, day: NaiveDate, time: Duration) -> io::Result<DayFile> {
    let started = Instant::now();
    let mut passed_over: Option<(String, io::Error)> = None;
    for other in 0..=MAX_OTHER_NAMES {
        if let Some((name, why)) = &passed_over {
            if started.elapsed() > time {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "{name} could not be used ({why}), and looking for another name \
                         took too long"
                    ),
                ));
            }
        }
        let name = file_name(day, other);
        match open_or_create(&dir.join(&name))? {
            Ok(file) => {
                return Ok(DayFile {
                    file,
                    name,
                    passed_over,
                })
            }
            Err(why) => {
                passed_over.get_or_insert((name, why));
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        match passed_over {
            Some((name, why)) => {
                format!("{name} and the {MAX_OTHER_NAMES} names after it could not be used ({why})")
            }
            None => "no name could be used".to_owned(),
        },
    ))
}

/// The writer of the application log: it appends to the file of the current
/// day, and goes on to the next day's file with the first write of that day.
///
/// For the thread that `tracing_appender::non_blocking` writes the log on.
/// When a day's file cannot be opened, the log stays in the file it has for
/// that day, so that no line is lost, and the next day's is tried again.
pub(crate) struct DailyLog {
    dir: PathBuf,
    file: File,
    /// The day `file` was opened for.
    day: NaiveDate,
    /// A day whose file could not be opened. Not tried again, or every
    /// write of that day would try up to a thousand names.
    gave_up_on: Option<NaiveDate>,
    /// The current day; the tests have their own.
    today: Box<dyn Fn() -> NaiveDate + Send>,
}

impl DailyLog {
    /// Open today's file in `dir`, which must be there. Also returns what
    /// there is to tell the user, if the file is not the one of the day's
    /// own name; the caller logs it, since there is no log before this.
    pub(crate) fn open(dir: &Path) -> io::Result<(Self, Option<String>)> {
        Self::open_with(dir, Box::new(|| chrono::Utc::now().date_naive()))
    }

    fn open_with(
        dir: &Path,
        today: Box<dyn Fn() -> NaiveDate + Send>,
    ) -> io::Result<(Self, Option<String>)> {
        let day = today();
        let opened = open_for_day(dir, day)?;
        let note = opened.note();
        let log = Self {
            dir: dir.to_path_buf(),
            file: opened.file,
            day,
            gave_up_on: None,
            today,
        };
        Ok((log, note))
    }

    /// Go on to the file of the current day, if that is a later one than
    /// the one that is open.
    fn roll(&mut self) {
        let today = (self.today)();
        // Forward only: a clock that is set back does not bring an earlier
        // day's file back, or one that goes back and forth across midnight
        // would open a file each time.
        if today <= self.day || self.gave_up_on == Some(today) {
            return;
        }
        // The directory is treated as at a start. One that was removed in
        // the meantime is created again for its owner alone, and one that
        // is there is restricted to its owner, without following a link at
        // its name. One that cannot be restricted is still used, and the
        // log says so, as it does at a start.
        let opened = private_fs::create_private_dir(&self.dir).and_then(|()| {
            if let Err(e) = private_fs::restrict_dir_to_owner_nofollow(&self.dir) {
                tracing::warn!(
                    error = %e,
                    "Failed to restrict the application log directory to its owner \
                     at the change of day"
                );
            }
            open_for_day(&self.dir, today)
        });
        // What is logged here is written by this writer in turn. That is
        // sound on the thread it runs on, which is not inside a log call,
        // and with the `non_blocking` writer as the app sets it up, which
        // drops a line where its queue is full and never waits.
        match opened {
            Ok(opened) => {
                if let Some(note) = opened.note() {
                    tracing::warn!("{note}");
                }
                self.file = opened.file;
                self.day = today;
                self.gave_up_on = None;
            }
            Err(e) => {
                self.gave_up_on = Some(today);
                tracing::warn!(
                    error = %e,
                    "Failed to open the application log's file for {today}; \
                     the log stays in the file for {} until the next day",
                    self.day
                );
            }
        }
    }
}

impl io::Write for DailyLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.roll();
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ss-app-log-{}", uuid::Uuid::new_v4()));
        private_fs::create_private_dir(&dir).unwrap();
        dir
    }

    fn day(day_of_march: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 3, day_of_march).unwrap()
    }

    /// A log in `dir` whose day is what the returned handle holds.
    fn log_on(dir: &Path, start: NaiveDate) -> (DailyLog, Arc<Mutex<NaiveDate>>) {
        let clock = Arc::new(Mutex::new(start));
        let read = Arc::clone(&clock);
        let (log, note) =
            DailyLog::open_with(dir, Box::new(move || *read.lock().unwrap())).unwrap();
        assert_eq!(note, None);
        (log, clock)
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_days_file_has_the_date_in_its_name() {
        assert_eq!(file_name(day(1), 0), "shellstation.2026-03-01.log");
        assert_eq!(file_name(day(1), 1), "shellstation.2026-03-01_1.log");
        assert_eq!(file_name(day(1), 999), "shellstation.2026-03-01_999.log");
    }

    #[test]
    fn a_new_file_is_created_for_its_owner_alone() {
        let dir = scratch();
        let (mut log, _clock) = log_on(&dir, day(1));
        log.write_all(b"one\n").unwrap();
        let file = dir.join("shellstation.2026-03-01.log");
        assert_eq!(std::fs::read(&file).unwrap(), b"one\n");
        private_fs::assert_private(&file);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A second start on the same day goes on in the same file.
    #[test]
    fn a_file_of_the_same_day_is_appended_to() {
        let dir = scratch();
        let (mut log, _clock) = log_on(&dir, day(1));
        log.write_all(b"one\n").unwrap();
        drop(log);
        let (mut log, _clock) = log_on(&dir, day(1));
        log.write_all(b"two\n").unwrap();
        assert_eq!(names(&dir), ["shellstation.2026-03-01.log"]);
        assert_eq!(
            std::fs::read(dir.join("shellstation.2026-03-01.log")).unwrap(),
            b"one\ntwo\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_first_write_of_a_new_day_goes_to_a_new_file() {
        let dir = scratch();
        let (mut log, clock) = log_on(&dir, day(1));
        log.write_all(b"one\n").unwrap();
        *clock.lock().unwrap() = day(2);
        log.write_all(b"two\n").unwrap();
        log.write_all(b"three\n").unwrap();
        // A clock that is set back does not bring the earlier file back.
        *clock.lock().unwrap() = day(1);
        log.write_all(b"four\n").unwrap();
        let read = |name: &str| std::fs::read(dir.join(name)).unwrap();
        assert_eq!(read("shellstation.2026-03-01.log"), b"one\n");
        assert_eq!(read("shellstation.2026-03-02.log"), b"two\nthree\nfour\n");
        private_fs::assert_private(&dir.join("shellstation.2026-03-02.log"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A name that holds something else is passed over, at the start and at
    /// the change of day, and the caller is told.
    #[test]
    fn a_name_held_by_a_directory_is_passed_over() {
        let dir = scratch();
        std::fs::create_dir(dir.join("shellstation.2026-03-01.log")).unwrap();
        std::fs::create_dir(dir.join("shellstation.2026-03-01_1.log")).unwrap();
        std::fs::create_dir(dir.join("shellstation.2026-03-02.log")).unwrap();
        let clock = Arc::new(Mutex::new(day(1)));
        let read = Arc::clone(&clock);
        let (mut log, note) =
            DailyLog::open_with(&dir, Box::new(move || *read.lock().unwrap())).unwrap();
        let note = note.unwrap();
        assert!(
            note.starts_with(
                "The application log is written to shellstation.2026-03-01_2.log: \
                 shellstation.2026-03-01.log could not be used ("
            ),
            "{note}"
        );
        log.write_all(b"one\n").unwrap();
        *clock.lock().unwrap() = day(2);
        log.write_all(b"two\n").unwrap();
        let read = |name: &str| std::fs::read(dir.join(name)).unwrap();
        assert_eq!(read("shellstation.2026-03-01_2.log"), b"one\n");
        assert_eq!(read("shellstation.2026-03-02_1.log"), b"two\n");
        // The next start on that day finds the same file.
        drop(log);
        let opened = open_for_day(&dir, day(2)).unwrap();
        assert_eq!(opened.name, "shellstation.2026-03-02_1.log");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_day_with_every_name_taken_has_no_file() {
        let dir = scratch();
        for other in 0..=MAX_OTHER_NAMES {
            std::fs::create_dir(dir.join(file_name(day(2), other))).unwrap();
        }
        let error = open_for_day(&dir, day(2)).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(
            error
                .to_string()
                .starts_with("shellstation.2026-03-02.log and the 999 names after it"),
            "{error}"
        );

        // At the change of day the log stays in the file it has, and does
        // not try that day again.
        let (mut log, clock) = log_on(&dir, day(1));
        log.write_all(b"one\n").unwrap();
        *clock.lock().unwrap() = day(2);
        log.write_all(b"two\n").unwrap();
        assert_eq!(log.gave_up_on, Some(day(2)));
        std::fs::remove_dir(dir.join(file_name(day(2), 0))).unwrap();
        log.write_all(b"three\n").unwrap();
        let read = |name: &str| std::fs::read(dir.join(name)).unwrap();
        assert_eq!(read("shellstation.2026-03-01.log"), b"one\ntwo\nthree\n");
        // The day after has a file again.
        *clock.lock().unwrap() = day(3);
        log.write_all(b"four\n").unwrap();
        assert_eq!(read("shellstation.2026-03-03.log"), b"four\n");
        assert_eq!(log.gave_up_on, None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An error that is not about the name is not answered with a thousand
    /// more names.
    #[test]
    fn a_missing_directory_ends_the_search() {
        let dir = scratch().join("missing");
        let error = open_for_day(&dir, day(1)).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    /// Nor is a directory that cannot be searched: whether anything holds
    /// the name cannot be told there, for any name.
    #[cfg(unix)]
    #[test]
    fn a_directory_that_cannot_be_searched_ends_the_search() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: `geteuid` takes no arguments, touches no memory and
        // cannot fail.
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipped: root can search any directory");
            return;
        }
        let dir = scratch();
        std::fs::write(dir.join("shellstation.2026-03-01.log"), b"").unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        let error = open_for_day(&dir, day(1)).err().unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(!error.to_string().contains("names after it"), "{error}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The names after the first are looked through for a time only, since
    /// whoever holds a name can make its open take long.
    #[test]
    fn a_search_that_takes_too_long_ends() {
        let dir = scratch();
        std::fs::create_dir(dir.join("shellstation.2026-03-01.log")).unwrap();
        let error = open_for_day_within(&dir, day(1), Duration::ZERO)
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(
            error
                .to_string()
                .starts_with("shellstation.2026-03-01.log could not be used ("),
            "{error}"
        );
        assert_eq!(names(&dir), ["shellstation.2026-03-01.log"]);
        // With the time it has otherwise, the next name is taken.
        let opened = open_for_day(&dir, day(1)).unwrap();
        assert_eq!(opened.name, "shellstation.2026-03-01_1.log");
        drop(opened);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A log directory that is gone at the change of day is created again
    /// for its owner alone, with the day's file in it.
    #[test]
    fn a_removed_directory_is_created_again_owner_only() {
        let root = scratch();
        let dir = root.join("logs");
        private_fs::create_private_dir(&dir).unwrap();
        let (mut log, clock) = log_on(&dir, day(1));
        log.write_all(b"one\n").unwrap();
        // Not every Windows file system lets go of a directory with an open
        // file in it, so the log gets a handle elsewhere first.
        log.file = private_fs::create_private_append_file(&root.join("held.log")).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        *clock.lock().unwrap() = day(2);
        log.write_all(b"two\n").unwrap();
        private_fs::assert_private(&dir);
        let file = dir.join("shellstation.2026-03-02.log");
        assert_eq!(std::fs::read(&file).unwrap(), b"two\n");
        private_fs::assert_private(&file);
        drop(log);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A file with a second name could be a file of the user's that someone
    /// else linked to the day's name.
    #[test]
    fn a_file_with_another_name_is_not_appended_to() {
        let dir = scratch();
        let elsewhere = dir.join("the-users-own.txt");
        // Its owner's alone, so that the second name is all that is wrong
        // with it.
        private_fs::write_private_file(&elsewhere, b"kept\n").unwrap();
        std::fs::hard_link(&elsewhere, dir.join("shellstation.2026-03-01.log")).unwrap();
        let clock = day(1);
        let (mut log, note) = DailyLog::open_with(&dir, Box::new(move || clock)).unwrap();
        assert!(note.unwrap().contains("hard link"));
        log.write_all(b"one\n").unwrap();
        assert_eq!(std::fs::read(&elsewhere).unwrap(), b"kept\n");
        assert_eq!(
            std::fs::read(dir.join("shellstation.2026-03-01_1.log")).unwrap(),
            b"one\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A link at the day's name is not written through, whether it leads to
    /// a file or nowhere; the link stays as it is.
    #[test]
    fn a_link_at_the_name_is_not_followed() {
        let dir = scratch();
        let target = dir.join("the-users-own.txt");
        // Its owner's alone, so that it would be appended to if the link
        // were followed.
        private_fs::write_private_file(&target, b"kept\n").unwrap();
        let link = |to: &Path, name: &str| {
            #[cfg(unix)]
            return std::os::unix::fs::symlink(to, dir.join(name));
            #[cfg(windows)]
            return std::os::windows::fs::symlink_file(to, dir.join(name));
        };
        if let Err(e) = link(&target, "shellstation.2026-03-01.log") {
            // Windows asks for a privilege to create a symbolic link.
            eprintln!("skipped: symbolic links cannot be created here ({e})");
            std::fs::remove_dir_all(&dir).unwrap();
            return;
        }
        link(&dir.join("nowhere"), "shellstation.2026-03-01_1.log").unwrap();
        let clock = day(1);
        let (mut log, note) = DailyLog::open_with(&dir, Box::new(move || clock)).unwrap();
        assert!(note.is_some());
        log.write_all(b"one\n").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"kept\n");
        assert!(!dir.join("nowhere").exists());
        assert_eq!(
            std::fs::read(dir.join("shellstation.2026-03-01_2.log")).unwrap(),
            b"one\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A FIFO at the day's name is neither waited on nor written to, with a
    /// reader or without one.
    #[cfg(unix)]
    #[test]
    fn a_fifo_at_the_name_is_not_waited_on() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::OpenOptionsExt;
        let dir = scratch();
        let fifo = |name: &str| {
            let path = dir.join(name);
            let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            // SAFETY: `c_path` is a NUL-terminated string that outlives the
            // call.
            assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
            path
        };
        fifo("shellstation.2026-03-01.log");
        let with_reader = fifo("shellstation.2026-03-01_1.log");
        let mut reader = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&with_reader)
            .unwrap();
        let clock = day(1);
        let (mut log, note) = DailyLog::open_with(&dir, Box::new(move || clock)).unwrap();
        assert!(note.is_some());
        log.write_all(b"one\n").unwrap();
        assert_eq!(
            std::fs::read(dir.join("shellstation.2026-03-01_2.log")).unwrap(),
            b"one\n"
        );
        // Nothing went into the FIFO, whose writer is gone again.
        let mut read = Vec::new();
        std::io::Read::read_to_end(&mut reader, &mut read).unwrap();
        assert_eq!(read, b"");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A file that others could open, as the logging library of an earlier
    /// version created it with the umask's mode, is not appended to: someone
    /// may have it open, and would go on reading. It is taken from everyone
    /// else and from the log, at this start and at the next.
    #[cfg(unix)]
    #[test]
    fn a_file_that_others_could_open_is_not_appended_to() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch();
        let file = dir.join("shellstation.2026-03-01.log");
        std::fs::write(&file, b"one\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        for (line, written) in [
            (&b"two\n"[..], &b"two\n"[..]),
            (b"three\n", b"two\nthree\n"),
        ] {
            let clock = day(1);
            let (mut log, note) = DailyLog::open_with(&dir, Box::new(move || clock)).unwrap();
            let note = note.unwrap();
            assert!(
                note.starts_with(
                    "The application log is written to shellstation.2026-03-01_1.log: \
                     shellstation.2026-03-01.log could not be used ("
                ),
                "{note}"
            );
            log.write_all(line).unwrap();
            assert_eq!(std::fs::read(&file).unwrap(), b"one\n");
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o400);
            assert_eq!(
                std::fs::read(dir.join("shellstation.2026-03-01_1.log")).unwrap(),
                written
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
