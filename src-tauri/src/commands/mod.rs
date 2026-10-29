mod connection;
mod connection_prompt;
mod credential_profile;
mod data_transfer;
mod database;
mod highlight;
mod logging;
mod login_sequence;
mod pty;
mod session;
mod session_credentials;
mod session_login_sequences;
mod ssh;
mod telnet;

pub use connection::*;
pub use credential_profile::*;
pub use data_transfer::*;
pub use database::*;
pub use highlight::*;
pub use logging::*;
pub use login_sequence::*;
pub use pty::*;
pub use session::*;
pub use session_credentials::*;
pub use session_login_sequences::*;
pub use ssh::*;
pub use telnet::*;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use tauri::Manager;

use crate::pty::PtyState;
use crate::session_logger::SessionLogState;
use crate::ssh::SshState;
use crate::telnet::TelnetState;

/// Senders for the "terminal ready" signal. Each protocol's reader task/thread
/// waits on the corresponding receiver before entering its read loop. This
/// prevents early terminal output (banner, MOTD, prompt) from being emitted
/// before the frontend listener is registered.
///
/// Uses `Box<dyn FnOnce() + Send>` so both tokio oneshot (SSH/Telnet) and
/// std mpsc (PTY) senders can be stored uniformly.
pub type ReadySenders = std::sync::Mutex<HashMap<String, Box<dyn FnOnce() + Send>>>;

/// Tauri-managed state for terminals the frontend has yet to attach to.
#[derive(Default)]
pub struct TerminalReadyState {
    /// Pending terminal-ready signals, by session `ID`.
    senders: ReadySenders,
    /// How many times the main webview has started loading a page. A page
    /// load discards the frontend's tabs, so a session set up before it can
    /// never be shown or closed; see [`close_all_sessions`].
    page_loads: AtomicU64,
}

impl TerminalReadyState {
    /// The current page load, for [`ConnectSetup::begin`]. A connect command
    /// reads it before anything else, so that a reload during any of its
    /// work — database lookups included — gets the session refused.
    fn page_load(&self) -> u64 {
        self.page_loads.load(Ordering::SeqCst)
    }
}

/// Signal that the frontend event listener for a terminal session is ready.
/// The backend reader task/thread will start streaming output after this call.
#[tauri::command]
pub fn terminal_ready(
    state: tauri::State<'_, TerminalReadyState>,
    id: String,
) -> Result<(), String> {
    let sender = {
        let mut senders = state
            .senders
            .lock()
            .map_err(|e| format!("Ready sender lock poisoned: {e}"))?;
        senders.remove(&id)
    };
    if let Some(signal) = sender {
        signal();
    }
    Ok(())
}

/// What a connect command sets up before its session exists — the session
/// log and the terminal-ready signal — undone on every early return. Disarm
/// it once the session is registered, when the tab owns the session.
#[must_use = "dropping the guard undoes the setup at once"]
struct ConnectSetup<'a> {
    id: String,
    ready: &'a TerminalReadyState,
    logger: &'a SessionLogState,
    /// [`TerminalReadyState::page_load`] when the connect command started.
    page_load: u64,
    armed: bool,
}

impl<'a> ConnectSetup<'a> {
    /// Open the session log, so output from the start is captured.
    /// `page_load` is what the command read when it started.
    fn begin(
        id: &str,
        log_name: &str,
        page_load: u64,
        ready: &'a TerminalReadyState,
        logger: &'a SessionLogState,
    ) -> Self {
        if let Err(e) = lock_or_recover(&logger.0).open_log(id, log_name) {
            tracing::warn!("Failed to open session log: {e}");
        }
        Self {
            id: id.to_string(),
            ready,
            logger,
            page_load,
            armed: true,
        }
    }

    /// Store the signal `terminal_ready` fires once the tab is listening.
    fn add_ready_signal(&self, signal: impl FnOnce() + Send + 'static) {
        lock_or_recover(&self.ready.senders).insert(self.id.clone(), Box::new(signal));
    }

    /// Fail if the webview has loaded a page since the connect began: the
    /// tab that asked for the session is gone, and nothing would ever show
    /// or close it. Call it under the manager lock, right before registering,
    /// so that [`close_all_sessions`] either sees the session or refuses it.
    fn check_page_unchanged(&self) -> Result<(), String> {
        if self.ready.page_load() != self.page_load {
            return Err("The window was reloaded while the session was connecting".to_string());
        }
        Ok(())
    }

    /// Keep the setup: the registered session owns it from here on.
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for ConnectSetup<'_> {
    fn drop(&mut self) {
        if self.armed {
            release_session(&self.id, self.ready, self.logger);
        }
    }
}

/// Lock a mutex that a panic elsewhere may have poisoned. The maps behind
/// these locks stay usable, and skipping the cleanup would leave sessions,
/// signals or logs behind.
fn lock_or_recover<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| {
        tracing::warn!("Recovering a poisoned lock");
        e.into_inner()
    })
}

/// Forget what is kept for a session outside its manager: the terminal-ready
/// signal, if the tab never fired it, and the session log.
fn release_session(id: &str, ready: &TerminalReadyState, logger: &SessionLogState) {
    lock_or_recover(&ready.senders).remove(id);
    lock_or_recover(&logger.0).close_log(id);
}

/// Close every terminal session. Run when the main webview starts loading a
/// page: a reload throws away the frontend's tabs, so the sessions from
/// before it have nothing left to show or close them. Left alone, they would
/// stay connected for as long as the app runs, their keepalives telling each
/// remote device that the session is still in use.
///
/// The page-load count goes up on the task this spawns, under the manager
/// locks, so that no SSH or Telnet session of the new page is ever closed
/// here (local shells: see below). In return,
/// two narrow cases are left: a connect the new page starts before the count
/// goes up is refused, and one the old page sends in the instant of the
/// reload can read the new count and stay connected (see Known issues in
/// `CHANGELOG.md`).
pub fn close_all_sessions(app: &tauri::AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let (Some(ready), Some(logger), Some(ssh), Some(telnet)) = (
            app.try_state::<TerminalReadyState>(),
            app.try_state::<SessionLogState>(),
            app.try_state::<SshState>(),
            app.try_state::<TelnetState>(),
        ) else {
            tracing::warn!("Terminal state not set up yet; no sessions to close");
            return;
        };

        // Count the page load and drain the SSH and Telnet managers while
        // holding both their locks. A connect checks the count under its
        // manager's lock right before registering, so one from before the
        // load is either drained here or refused, and one from the new page
        // is neither. Pending prompts belong to connects from before the load
        // too; dropping them ends those connects now rather than at their
        // timeout.
        let (ssh_sessions, telnet_sessions) = {
            let mut ssh_manager = ssh.manager.lock().await;
            let mut telnet_manager = telnet.0.lock().await;
            ready.page_loads.fetch_add(1, Ordering::SeqCst);
            lock_or_recover(&ssh.host_verify_senders).clear();
            lock_or_recover(&ssh.kbd_interactive_senders).clear();
            (ssh_manager.take_all(), telnet_manager.take_all())
        };
        let mut count = ssh_sessions.len() + telnet_sessions.len();
        // Dropping a session closes it (see the `Drop` impls); then forget
        // what the session kept outside its manager.
        for (id, session) in ssh_sessions {
            drop(session);
            release_session(&id, &ready, &logger);
        }
        for (id, session) in telnet_sessions {
            drop(session);
            release_session(&id, &ready, &logger);
        }

        // Local shells are drained apart, on a blocking thread: their lock
        // is a blocking one, which a stuck write to a shell can hold, and the
        // SSH and Telnet locks must not wait on it. `pty_spawn` checks the
        // count under this lock as well. A shell the new page starts in the
        // moment before this drain would be closed with the old ones; it
        // shows as an exited tab.
        let pty_app = app.clone();
        let killed = tauri::async_runtime::spawn_blocking(move || {
            let (Some(pty), Some(ready), Some(logger)) = (
                pty_app.try_state::<PtyState>(),
                pty_app.try_state::<TerminalReadyState>(),
                pty_app.try_state::<SessionLogState>(),
            ) else {
                return 0;
            };
            let shells = lock_or_recover(&pty.0).take_all();
            let killed = shells.len();
            for (id, shell) in shells {
                shell.shut_down(&id);
                release_session(&id, &ready, &logger);
            }
            killed
        })
        .await;
        match killed {
            Ok(killed) => count += killed,
            Err(e) => tracing::warn!(error = %e, "Closing the local shells failed"),
        }

        if count > 0 {
            tracing::info!(count, "Closed the sessions left behind by a page load");
        }
    });
}

/// Maximum terminal dimensions to prevent resource exhaustion.
const MAX_COLS: u16 = 500;
const MAX_ROWS: u16 = 500;

/// Maximum bytes per single write call.
const MAX_WRITE_SIZE: usize = 65536;

/// Maximum number of jump host hops.
const MAX_JUMP_HOPS: usize = 10;

fn validate_dimensions(cols: u16, rows: u16) -> Result<(), String> {
    if cols == 0 || rows == 0 || cols > MAX_COLS || rows > MAX_ROWS {
        return Err(format!(
            "Invalid terminal dimensions: cols and rows must be 1\u{2013}{MAX_COLS}/{MAX_ROWS}"
        ));
    }
    Ok(())
}

fn validate_port(port: i32) -> Result<(), String> {
    if !(1..=65535).contains(&port) {
        return Err(format!("Invalid port {port}: must be 1\u{2013}65535"));
    }
    Ok(())
}

/// Maximum length for session string fields to prevent resource exhaustion.
const MAX_NAME_LEN: usize = 255;
const MAX_HOSTNAME_LEN: usize = 255;
const MAX_TAGS_LEN: usize = 1024;
const MAX_ICON_LEN: usize = 128;

fn validate_session_fields(
    name: Option<&str>,
    hostname: Option<&str>,
    tags: Option<&str>,
) -> Result<(), String> {
    if let Some(v) = name {
        if v.len() > MAX_NAME_LEN {
            return Err(format!("Name too long (max {MAX_NAME_LEN} characters)"));
        }
    }
    if let Some(v) = hostname {
        if v.len() > MAX_HOSTNAME_LEN {
            return Err(format!(
                "Hostname too long (max {MAX_HOSTNAME_LEN} characters)"
            ));
        }
    }
    if let Some(v) = tags {
        if v.len() > MAX_TAGS_LEN {
            return Err(format!("Tags too long (max {MAX_TAGS_LEN} characters)"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_logger::SessionLogManager;
    use std::sync::{Arc, Mutex};

    /// Ready and logger state for one test. Pending logs are kept in memory;
    /// only the log directory is created, and removed again on drop.
    struct Fixture {
        ready: TerminalReadyState,
        logger: SessionLogState,
        dir: std::path::PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "shellstation-connect-setup-{}",
                uuid::Uuid::new_v4()
            ));
            Self {
                ready: TerminalReadyState::default(),
                logger: SessionLogState(Arc::new(Mutex::new(SessionLogManager::new(
                    true,
                    dir.clone(),
                    "{name}.log".to_string(),
                )))),
                dir,
            }
        }

        /// Set up a connect that starts now.
        fn begin(&self, id: &str) -> ConnectSetup<'_> {
            ConnectSetup::begin(
                id,
                "host",
                self.ready.page_load(),
                &self.ready,
                &self.logger,
            )
        }

        fn has_ready_signal(&self, id: &str) -> bool {
            self.ready.senders.lock().unwrap().contains_key(id)
        }

        fn has_log(&self, id: &str) -> bool {
            self.logger.0.lock().unwrap().has_log(id)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn connect_setup_is_undone_on_early_return() {
        let fx = Fixture::new();
        {
            let setup = fx.begin("c");
            setup.add_ready_signal(|| {});
            assert!(fx.has_ready_signal("c"));
            assert!(fx.has_log("c"));
        }
        assert!(!fx.has_ready_signal("c"));
        assert!(!fx.has_log("c"));
    }

    #[test]
    fn disarmed_connect_setup_is_kept() {
        let fx = Fixture::new();
        let setup = fx.begin("c");
        setup.add_ready_signal(|| {});
        setup.disarm();
        assert!(fx.has_ready_signal("c"));
        assert!(fx.has_log("c"));
    }

    #[test]
    fn page_load_during_connect_is_refused() {
        let fx = Fixture::new();
        let setup = fx.begin("c");
        assert!(setup.check_page_unchanged().is_ok());

        fx.ready.page_loads.fetch_add(1, Ordering::SeqCst);
        assert!(setup.check_page_unchanged().is_err());

        // A connect that starts after the load is unaffected.
        let later = fx.begin("d");
        assert!(later.check_page_unchanged().is_ok());
    }

    #[test]
    fn page_load_before_setup_begins_is_refused() {
        // session_connect reads the database before its setup begins. A
        // reload during those reads must count, so the command reads the
        // page load first and hands it to `begin`.
        let fx = Fixture::new();
        let page_load = fx.ready.page_load();
        fx.ready.page_loads.fetch_add(1, Ordering::SeqCst);

        let setup = ConnectSetup::begin("c", "host", page_load, &fx.ready, &fx.logger);
        assert!(setup.check_page_unchanged().is_err());
    }
}
