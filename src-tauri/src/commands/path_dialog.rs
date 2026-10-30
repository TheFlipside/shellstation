//! File and folder dialogs that choose where the app writes: the session
//! and application log directories and the `SQLite` database.
//!
//! These paths are not taken as typed from the webview. A compromised
//! webview could otherwise make the app write where it likes, such as
//! appending terminal output, which the remote host controls, to a file in
//! a Startup folder, or creating the database there. A command opens the
//! dialog here instead, starting in a folder and with a title of the
//! backend's choosing, and keeps the user's choice in a [`PickSlot`]. The
//! save command then accepts a path only if it is the default, the one
//! already configured, or that choice, made at most [`PICK_VALID_FOR`] ago.
//!
//! The webview can still open file dialogs of its own, worded as it likes:
//! the SSH key pickers need `dialog:allow-open`. A path from one of those is
//! never kept in a [`PickSlot`], so these save commands refuse it.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tauri::Window;
use tauri_plugin_dialog::{DialogExt, FilePath};
use tokio::sync::oneshot;

/// How long a choice stays good for the save command: long enough to look
/// over the other settings first, short enough that a choice the user never
/// saved is not left for a compromised webview to save later.
const PICK_VALID_FOR: Duration = Duration::from_secs(10 * 60);

/// Set while one of these dialogs is open, so a looping caller cannot stack
/// up dialogs.
static DIALOG_OPEN: AtomicBool = AtomicBool::new(false);

/// Holds [`DIALOG_OPEN`] and clears it when dropped, however the dialog ends.
struct DialogGuard;

impl DialogGuard {
    fn acquire() -> Result<Self, String> {
        DIALOG_OPEN
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self)
            .map_err(|_| "A file dialog is already open".to_string())
    }
}

impl Drop for DialogGuard {
    fn drop(&mut self) {
        DIALOG_OPEN.store(false, Ordering::Release);
    }
}

struct Pick {
    path: String,
    at: Instant,
}

/// The path the user last chose in a dialog for one setting.
pub(super) struct PickSlot {
    pick: Mutex<Option<Pick>>,
    /// The error for a path that is not the default, the configured one or
    /// the user's choice.
    refusal: &'static str,
}

impl PickSlot {
    pub(super) const fn new(refusal: &'static str) -> Self {
        Self {
            pick: Mutex::new(None),
            refusal,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Option<Pick>> {
        self.pick.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn remember(&self, path: String, at: Instant) {
        *self.lock() = Some(Pick { path, at });
    }

    fn check_locked(
        &self,
        pick: Option<&Pick>,
        requested: Option<&str>,
        configured: Option<&str>,
        now: Instant,
    ) -> Result<(), String> {
        let Some(path) = requested.filter(|p| !p.is_empty()) else {
            return Ok(());
        };
        let chosen = pick
            .filter(|pick| now.saturating_duration_since(pick.at) < PICK_VALID_FOR)
            .map(|pick| pick.path.as_str());
        if Some(path) == configured || Some(path) == chosen {
            Ok(())
        } else {
            Err(self.refusal.to_string())
        }
    }

    /// Check `requested`, the path the webview asks to save. It must be the
    /// default (`None` or empty), `configured`, or the path the user chose
    /// at most [`PICK_VALID_FOR`] before `now`.
    pub(super) fn check(
        &self,
        requested: Option<&str>,
        configured: Option<&str>,
        now: Instant,
    ) -> Result<(), String> {
        self.check_locked(self.lock().as_ref(), requested, configured, now)
    }

    /// [`check`](Self::check) `requested`, then run `save`. Once it has
    /// succeeded the choice is forgotten: it is now the configured path, or
    /// was not used, so the webview cannot save it later on its own.
    pub(super) fn save_with(
        &self,
        requested: Option<&str>,
        configured: Option<&str>,
        now: Instant,
        save: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        // Held throughout, so a choice made meanwhile is not forgotten
        // unchecked.
        let mut pick = self.lock();
        self.check_locked(pick.as_ref(), requested, configured, now)?;
        save()?;
        *pick = None;
        Ok(())
    }
}

/// Wait for the dialog's answer and remember it in `slot`. `None` means the
/// user cancelled.
async fn finish(
    answer: oneshot::Receiver<Option<FilePath>>,
    slot: &PickSlot,
) -> Result<Option<String>, String> {
    let Some(path) = answer
        .await
        .map_err(|_| "The dialog closed unexpectedly".to_string())?
    else {
        return Ok(None);
    };
    // The config keeps paths as strings, so one that is not valid Unicode
    // cannot be stored.
    let path = path
        .into_path()
        .map_err(|e| format!("Invalid path: {e}"))?
        .into_os_string()
        .into_string()
        .map_err(|_| "The chosen path cannot be stored: it is not valid Unicode".to_string())?;
    slot.remember(path.clone(), Instant::now());
    Ok(Some(path))
}

/// Let the user choose a folder, starting in `start` if it exists, and
/// remember it in `slot`. `None` means they cancelled.
pub(super) async fn choose_folder(
    window: &Window,
    title: &str,
    start: &Path,
    slot: &PickSlot,
) -> Result<Option<String>, String> {
    let guard = DialogGuard::acquire()?;
    let (tx, rx) = oneshot::channel();
    let mut dialog = window.dialog().file().set_parent(window).set_title(title);
    if start.is_dir() {
        dialog = dialog.set_directory(start);
    }
    dialog.pick_folder(move |path| {
        // The guard lives in here rather than in the command, so it lasts
        // until the dialog has closed even if the command is dropped.
        drop(guard);
        // The receiver only goes away if the command was dropped.
        let _ = tx.send(path);
    });
    finish(rx, slot).await
}

/// What a file dialog asks for.
pub(super) enum FileDialog<'a> {
    /// An existing file.
    Open,
    /// A file to create; `name` is the name it suggests.
    Create { name: &'a str },
}

/// Let the user choose a file matching `filter` (a name and its
/// extensions), starting in `start` if it exists, and remember it in
/// `slot`. `None` means they cancelled.
pub(super) async fn choose_file(
    window: &Window,
    title: &str,
    start: &Path,
    filter: (&str, &[&str]),
    kind: FileDialog<'_>,
    slot: &PickSlot,
) -> Result<Option<String>, String> {
    let guard = DialogGuard::acquire()?;
    let (tx, rx) = oneshot::channel();
    let mut dialog = window
        .dialog()
        .file()
        .set_parent(window)
        .set_title(title)
        .add_filter(filter.0, filter.1);
    if start.is_dir() {
        dialog = dialog.set_directory(start);
    }
    let answer = move |path| {
        // As in `choose_folder`.
        drop(guard);
        let _ = tx.send(path);
    };
    match kind {
        FileDialog::Open => dialog.pick_file(answer),
        FileDialog::Create { name } => dialog.set_file_name(name).save_file(answer),
    }
    finish(rx, slot).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFUSAL: &str = "Choose it with Browse";

    fn absolute(name: &str) -> String {
        std::env::temp_dir()
            .join(name)
            .to_string_lossy()
            .into_owned()
    }

    fn slot_with(path: &str, at: Instant) -> PickSlot {
        let slot = PickSlot::new(REFUSAL);
        slot.remember(path.to_string(), at);
        slot
    }

    fn chosen(slot: &PickSlot) -> Option<String> {
        slot.lock().as_ref().map(|pick| pick.path.clone())
    }

    #[test]
    fn the_default_is_always_accepted() {
        let slot = PickSlot::new(REFUSAL);
        let now = Instant::now();
        assert!(slot.check(None, None, now).is_ok());
        assert!(slot.check(Some(""), Some(&absolute("a")), now).is_ok());
    }

    #[test]
    fn the_configured_or_chosen_path_is_accepted() {
        let now = Instant::now();
        let configured = absolute("configured");
        let picked = absolute("picked");
        let slot = slot_with(&picked, now);
        assert!(slot
            .check(Some(&configured), Some(&configured), now)
            .is_ok());
        assert!(slot.check(Some(&picked), Some(&configured), now).is_ok());
        assert!(slot.check(Some(&picked), None, now).is_ok());
    }

    #[test]
    fn a_path_the_user_did_not_choose_is_refused() {
        let now = Instant::now();
        let picked = absolute("picked");
        let slot = slot_with(&picked, now);
        for requested in [absolute("Startup"), format!("{picked}x")] {
            let err = slot
                .check(Some(&requested), Some(&absolute("configured")), now)
                .unwrap_err();
            assert_eq!(err, REFUSAL, "{requested}");
        }
        assert!(PickSlot::new(REFUSAL)
            .check(Some(&picked), None, now)
            .is_err());
    }

    #[test]
    fn a_choice_expires() {
        let at = Instant::now();
        let picked = absolute("picked");
        let slot = slot_with(&picked, at);
        let almost = at + PICK_VALID_FOR - Duration::from_secs(1);
        assert!(slot.check(Some(&picked), None, almost).is_ok());
        let err = slot
            .check(Some(&picked), None, at + PICK_VALID_FOR)
            .unwrap_err();
        assert_eq!(err, REFUSAL);
        // Once configured, it no longer depends on the choice.
        assert!(slot
            .check(Some(&picked), Some(&picked), at + PICK_VALID_FOR)
            .is_ok());
    }

    #[test]
    fn a_choice_is_good_for_one_successful_save() {
        let now = Instant::now();
        let picked = absolute("picked");
        let slot = slot_with(&picked, now);
        let saves = std::cell::Cell::new(0);
        let save = || {
            saves.set(saves.get() + 1);
            Ok(())
        };

        assert!(slot.save_with(Some(&picked), None, now, save).is_ok());
        assert_eq!(saves.get(), 1);
        assert_eq!(chosen(&slot), None);
        // Neither configured nor still the choice, it is now refused.
        assert!(slot.save_with(Some(&picked), None, now, save).is_err());
        assert_eq!(saves.get(), 1);
    }

    #[test]
    fn a_failed_or_refused_save_keeps_the_choice() {
        let now = Instant::now();
        let picked = absolute("picked");
        let slot = slot_with(&picked, now);

        let failed = slot.save_with(Some(&picked), None, now, || Err("disk full".into()));
        assert_eq!(failed.unwrap_err(), "disk full");
        assert_eq!(chosen(&slot), Some(picked.clone()));

        let refused = slot.save_with(Some(&absolute("Startup")), None, now, || {
            panic!("a refused path must not be saved")
        });
        assert_eq!(refused.unwrap_err(), REFUSAL);
        assert_eq!(chosen(&slot), Some(picked));
    }

    #[test]
    fn saving_the_default_also_forgets_the_choice() {
        let now = Instant::now();
        let slot = slot_with(&absolute("picked"), now);
        assert!(slot.save_with(None, None, now, || Ok(())).is_ok());
        assert_eq!(chosen(&slot), None);
    }

    #[test]
    fn only_one_dialog_opens_at_a_time() {
        let guard = DialogGuard::acquire().unwrap();
        assert!(DialogGuard::acquire().is_err());
        drop(guard);
        assert!(DialogGuard::acquire().is_ok());
    }
}
