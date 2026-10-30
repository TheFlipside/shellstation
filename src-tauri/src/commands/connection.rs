//! The connection settings: the private-address check, the connect
//! timeout and the SSH keepalive.
//!
//! The connect commands read these from [`ConfigState`] instead of taking
//! them as arguments, so a compromised webview cannot pass its own values.
//! It can still call the commands below, which is why switching the
//! private-address check off asks the user first, in a native dialog that
//! the backend opens and words itself, in the operating system's language:
//! the webview can neither answer it nor choose what it says, or in which
//! language. Switching the check on, or changing a timeout within its
//! limits, weakens nothing and needs no confirmation.
//!
//! The webview does choose when the dialog appears, for instance just as
//! the user presses Enter in a terminal. The button that keeps the check on
//! is therefore the dialog's first one, which Windows and macOS make the
//! default (GTK sets none, so Enter there only presses a focused button),
//! and the check goes off only on a click of the second.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use tauri::{State, Window};
use tauri_plugin_dialog::{
    DialogExt, MessageDialogButtons, MessageDialogKind, MessageDialogResult,
};

use super::connection_prompt;
use crate::config::{self, ConfigState, ConnectionConfig};

/// Set while the dialog asking to switch the check off is open, so a
/// looping caller cannot stack up dialogs.
static PROMPT_OPEN: AtomicBool = AtomicBool::new(false);

/// After the user keeps the check on, a request to switch it off is refused
/// without asking for this long, doubling with each further decline up to
/// [`MAX_DECLINE_COOLDOWN`]. A compromised webview could otherwise reopen
/// the dialog the moment it closes, until a click lands on the wrong button.
const DECLINE_COOLDOWN: Duration = Duration::from_secs(30);
const MAX_DECLINE_COOLDOWN: Duration = Duration::from_hours(1);

/// How often, and when last, the user chose to keep the check on.
#[derive(Debug, Clone, Copy)]
struct Declines {
    count: u32,
    last: Option<Instant>,
}

impl Declines {
    fn cooldown(self) -> Duration {
        let doublings = self.count.saturating_sub(1).min(16);
        DECLINE_COOLDOWN
            .saturating_mul(1 << doublings)
            .min(MAX_DECLINE_COOLDOWN)
    }

    /// How long until the user may be asked again; zero once they may.
    fn wait(self, now: Instant) -> Duration {
        self.last.map_or(Duration::ZERO, |at| {
            self.cooldown()
                .saturating_sub(now.saturating_duration_since(at))
        })
    }

    fn record(&mut self, now: Instant) {
        self.count = self.count.saturating_add(1);
        self.last = Some(now);
    }

    /// The user has just agreed, so earlier refusals no longer count.
    fn clear(&mut self) {
        self.count = 0;
        self.last = None;
    }
}

static DECLINES: Mutex<Declines> = Mutex::new(Declines {
    count: 0,
    last: None,
});

fn declines() -> std::sync::MutexGuard<'static, Declines> {
    DECLINES.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Holds [`PROMPT_OPEN`] and clears it when dropped, however the prompt ends.
struct PromptGuard;

impl PromptGuard {
    fn acquire() -> Result<Self, String> {
        PROMPT_OPEN
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self)
            .map_err(|_| "A confirmation is already open".to_string())
    }
}

impl Drop for PromptGuard {
    fn drop(&mut self) {
        PROMPT_OPEN.store(false, Ordering::Release);
    }
}

/// The settings a new connection reads.
pub(crate) fn current(state: &ConfigState) -> Result<ConnectionConfig, String> {
    state
        .config
        .lock()
        .map(|config| config.connection.clone())
        .map_err(|e| format!("Config lock poisoned: {e}"))
}

/// Apply `change` to the connection settings and write the config file.
/// The settings in memory change only once the file is written, and the
/// lock is held throughout, so two changes cannot undo each other.
fn update(
    state: &ConfigState,
    change: impl FnOnce(&mut ConnectionConfig),
) -> Result<ConnectionConfig, String> {
    let mut config = state
        .config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {e}"))?;
    let mut updated = config.clone();
    change(&mut updated.connection);
    config::save_config(&state.config_path, &updated)?;
    *config = updated;
    Ok(config.connection.clone())
}

/// Ask whether to switch the private-address check off. `true` only when
/// the user clicked `allow`; any other answer keeps the check on.
///
/// The buttons are `keep`, `allow` and `cancel`, in that order. Closing the
/// dialog comes back as the last one: with only two buttons, the plugin
/// would report Escape as a click on the second, so the second could not be
/// `allow` and the safe button could not be the first.
async fn confirm_allow_private(window: &Window) -> Result<bool, String> {
    let guard = PromptGuard::acquire()?;
    // Checked only once the guard is held: a decline is recorded before its
    // dialog lets go of the guard, so no request can slip in between.
    let wait = declines().wait(Instant::now());
    if !wait.is_zero() {
        // Like the declines, many of these suggest something keeps asking.
        tracing::info!(
            wait_secs = wait.as_secs(),
            "Refused to ask about private addresses again so soon after a decline"
        );
        return Err(format!(
            "Switching off the private-address check was declined a moment ago; \
             try again in {} s",
            wait.as_secs().max(1)
        ));
    }
    let prompt = connection_prompt::for_system_language();
    let (tx, rx) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .message(prompt.message)
        .title(prompt.title)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::YesNoCancelCustom(
            prompt.keep.to_string(),
            prompt.allow.to_string(),
            prompt.cancel.to_string(),
        ))
        .parent(window)
        .show_with_result(move |result| {
            let allowed = result == MessageDialogResult::Custom(prompt.allow.to_string());
            // The decline is recorded before the guard goes, so no new
            // dialog can open in between. The guard lives in here rather
            // than in the command, so it lasts until the dialog has closed
            // even if the command is dropped meanwhile.
            if allowed {
                declines().clear();
            } else {
                declines().record(Instant::now());
            }
            drop(guard);
            // The receiver only goes away if this command was dropped.
            let _ = tx.send(allowed);
        });
    rx.await
        .map_err(|_| "Confirmation dialog closed unexpectedly".to_string())
}

/// The connection settings in effect.
#[tauri::command]
pub async fn connection_get_config(
    state: State<'_, ConfigState>,
) -> Result<ConnectionConfig, String> {
    current(&state)
}

/// Change the connect timeout and the keepalive settings. A value left out
/// keeps its current setting. Returns the settings now in effect.
#[tauri::command]
pub async fn connection_save_config(
    state: State<'_, ConfigState>,
    connect_timeout_secs: Option<u64>,
    keepalive_interval_secs: Option<u64>,
    keepalive_max: Option<u32>,
) -> Result<ConnectionConfig, String> {
    if let Some(secs) = connect_timeout_secs {
        ConnectionConfig::validate_connect_timeout(secs)?;
    }
    if let Some(secs) = keepalive_interval_secs {
        ConnectionConfig::validate_keepalive_interval(secs)?;
    }
    if let Some(count) = keepalive_max {
        ConnectionConfig::validate_keepalive_max(count)?;
    }
    update(&state, |connection| {
        if let Some(secs) = connect_timeout_secs {
            connection.connect_timeout_secs = secs;
        }
        if let Some(secs) = keepalive_interval_secs {
            connection.keepalive_interval_secs = secs;
        }
        if let Some(count) = keepalive_max {
            connection.keepalive_max = count;
        }
    })
}

/// Switch the private-address check on or off, and return whether it is on
/// afterwards. Switching it off asks the user first, and is refused for a
/// while after they said no.
#[tauri::command]
pub async fn connection_set_restrict_private_ips(
    window: Window,
    state: State<'_, ConfigState>,
    enabled: bool,
) -> Result<bool, String> {
    if !enabled && current(&state)?.restrict_private_ips && !confirm_allow_private(&window).await? {
        // Many refusals in one session suggest something keeps asking.
        tracing::info!(
            declines = declines().count,
            "Kept blocking private addresses: the user declined to stop"
        );
        return Ok(current(&state)?.restrict_private_ips);
    }
    // Saved even if the check was switched on while the dialog was open:
    // the user has just confirmed switching it off.
    // A failure is logged here: when an older "on" is handed over at
    // startup, the webview tells the user the check is off, not why.
    let connection = update(&state, |connection| {
        connection.restrict_private_ips = enabled;
    })
    .inspect_err(|e| {
        tracing::warn!(enabled, error = %e, "Could not save the private-address check");
    })?;
    tracing::info!(
        restrict_private_ips = connection.restrict_private_ips,
        "Saved the private-address check"
    );
    Ok(connection.restrict_private_ips)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_prompt_is_refused_while_one_is_open() {
        let first = PromptGuard::acquire().expect("no prompt is open yet");
        assert!(PromptGuard::acquire().is_err());
        drop(first);
        assert!(PromptGuard::acquire().is_ok());
    }

    #[test]
    fn a_decline_holds_off_asking_again_for_the_cooldown() {
        let start = Instant::now();
        let mut declines = Declines {
            count: 0,
            last: None,
        };
        assert_eq!(declines.wait(start), Duration::ZERO);

        declines.record(start);
        assert_eq!(declines.wait(start), DECLINE_COOLDOWN);
        assert_eq!(
            declines.wait(start + Duration::from_secs(10)),
            Duration::from_secs(20)
        );
        assert_eq!(declines.wait(start + DECLINE_COOLDOWN), Duration::ZERO);
    }

    #[test]
    fn each_further_decline_doubles_the_cooldown_up_to_its_cap() {
        let start = Instant::now();
        let mut declines = Declines {
            count: 0,
            last: None,
        };
        let mut expected = Vec::new();
        for _ in 0..10 {
            declines.record(start);
            expected.push(declines.cooldown().as_secs());
        }
        assert_eq!(
            expected,
            [30, 60, 120, 240, 480, 960, 1920, 3600, 3600, 3600]
        );
        declines.count = u32::MAX;
        assert_eq!(declines.cooldown(), MAX_DECLINE_COOLDOWN);

        declines.clear();
        assert_eq!(declines.wait(start), Duration::ZERO);
        declines.record(start);
        assert_eq!(declines.cooldown(), DECLINE_COOLDOWN);
    }
}
