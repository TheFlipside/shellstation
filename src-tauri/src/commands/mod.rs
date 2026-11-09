mod connection;
mod connection_prompt;
mod credential_profile;
mod data_transfer;
mod database;
mod highlight;
mod logging;
mod login_sequence;
mod path_dialog;
mod path_dialog_text;
mod pty;
mod session;
mod session_credentials;
mod session_login_sequences;
mod ssh;
mod system_language;
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tauri::{Emitter, Manager};

use crate::pty::PtyState;
use crate::session_logger::SessionLogState;
use crate::ssh::{SshSession, SshState, DISCONNECT_TIMEOUT};
use crate::telnet::{TelnetSession, TelnetState};

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
    /// Set once the app has begun to quit, and never cleared; see
    /// [`quit_app`].
    quitting: AtomicBool,
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
    ///
    /// Fail as well once the app is quitting: [`quit_app`] has taken, or is
    /// about to take, the sessions it closes, and one registered after that
    /// would be left to the operating system.
    fn check_page_unchanged(&self) -> Result<(), String> {
        if self.ready.page_load() != self.page_load {
            return Err("The window was reloaded while the session was connecting".to_string());
        }
        if self.ready.quitting.load(Ordering::SeqCst) {
            return Err("The app is quitting".to_string());
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

        let (ssh_sessions, telnet_sessions) = take_remote_sessions(&ready, &ssh, &telnet).await;
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

        // Local shells are drained apart, after the SSH and Telnet locks are
        // released. A shell the new page starts in the moment before this
        // drain would be closed with the old ones; it shows as an exited tab.
        count += close_local_shells(&app).await;

        if count > 0 {
            tracing::info!(count, "Closed the sessions left behind by a page load");
        }
    });
}

/// The SSH and Telnet sessions [`take_remote_sessions`] takes, with their `ID`s.
type RemoteSessions = (Vec<(String, SshSession)>, Vec<(String, TelnetSession)>);

/// Take every SSH and Telnet session out of its manager, and end the connects
/// still in flight.
///
/// The page-load count goes up while both manager locks are held. A connect
/// checks the count under its manager's lock right before registering, so
/// one from before this is either taken here or refused, and one that reads
/// the new count is neither. Pending prompts belong to connects from before
/// this too; dropping them ends those connects now rather than at their
/// timeout.
async fn take_remote_sessions(
    ready: &TerminalReadyState,
    ssh: &SshState,
    telnet: &TelnetState,
) -> RemoteSessions {
    let mut ssh_manager = ssh.manager.lock().await;
    let mut telnet_manager = telnet.0.lock().await;
    ready.page_loads.fetch_add(1, Ordering::SeqCst);
    lock_or_recover(&ssh.host_verify_senders).clear();
    lock_or_recover(&ssh.kbd_interactive_senders).clear();
    (ssh_manager.take_all(), telnet_manager.take_all())
}

/// Kill every local shell and forget what it kept outside its manager,
/// returning how many there were.
///
/// This runs on a blocking thread: the shells' lock is a blocking one, which
/// a stuck write to a shell can hold, and the SSH and Telnet locks must not
/// wait on it. `pty_spawn` checks the page-load count under this lock as well.
async fn close_local_shells(app: &tauri::AppHandle) -> usize {
    let app = app.clone();
    let killed = tauri::async_runtime::spawn_blocking(move || {
        let (Some(pty), Some(ready), Some(logger)) = (
            app.try_state::<PtyState>(),
            app.try_state::<TerminalReadyState>(),
            app.try_state::<SessionLogState>(),
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
    killed.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Closing the local shells failed");
        0
    })
}

/// Event telling the frontend that the app is closing its SSH and Telnet
/// sessions before it quits, so that it can say so while that takes.
const QUITTING_EVENT: &str = "app:quitting";

/// How long quitting waits, in all, for the sessions to close before it
/// exits anyway. The SSH chains close side by side, each giving up after
/// [`DISCONNECT_TIMEOUT`]; the extra second is for a local shell slow to
/// exit, or a manager lock held by a stuck write.
const QUIT_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(DISCONNECT_TIMEOUT.as_secs() + 1);

/// Quit the app, once its sessions are closed: see [`quit_app`]. The frontend
/// calls this for Ctrl+Q / Cmd+Q and the window's close button, after asking
/// to confirm where the settings say so.
#[tauri::command]
pub fn app_quit(app: tauri::AppHandle) {
    quit_app(&app);
}

/// Quit the app once its sessions are closed. Each SSH connection, the jump
/// hosts included, is sent `SSH_MSG_DISCONNECT` and waited for, as when its
/// tab is closed, so a device sees the session end and does not go on
/// counting it; each Telnet socket is closed, each local shell killed, and
/// each session log flushed. Called by [`app_quit`], and by the menu's quit
/// item directly, so that the item works even when the webview does not.
///
/// Returns at once; the closing runs on a task of its own, and the app exits
/// within [`QUIT_TIMEOUT`], whatever is still closing then. A session still
/// closing at that point gets only what its `Drop` can send before the
/// process ends. A second call while this runs does nothing.
pub fn quit_app(app: &tauri::AppHandle) {
    let Some(ready) = app.try_state::<TerminalReadyState>() else {
        app.exit(0);
        return;
    };
    if !begin_quit(&ready) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let started = std::time::Instant::now();
        // On a task of its own, so that a panic there still ends in the exit
        // below rather than leaving an app that refuses every connect and
        // every further quit.
        let closing_app = app.clone();
        let closing =
            tauri::async_runtime::spawn(async move { close_sessions_for_quit(&closing_app).await });
        match tokio::time::timeout(QUIT_TIMEOUT, closing).await {
            Ok(Ok(0)) => {}
            Ok(Ok(count)) => {
                tracing::info!(count, elapsed = ?started.elapsed(), "Closed the sessions before quitting");
            }
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "Closing the sessions at quit failed; quitting anyway");
            }
            Err(_) => {
                tracing::warn!(timeout = ?QUIT_TIMEOUT, "Sessions still closing at quit; quitting anyway");
            }
        }
        app.exit(0);
    });
}

/// Mark the app as quitting, returning whether this call did so. The mark is
/// never taken back, since the app exits after it: from then on every
/// connect is refused (see [`ConnectSetup::check_page_unchanged`]).
fn begin_quit(ready: &TerminalReadyState) -> bool {
    !ready.quitting.swap(true, Ordering::SeqCst)
}

/// Close every session for [`quit_app`], all at the same time, so quitting
/// takes as long as the slowest close rather than all of them in turn.
/// Returns how many there were. When some are SSH or Telnet sessions, the
/// frontend is told first.
async fn close_sessions_for_quit(app: &tauri::AppHandle) -> usize {
    let (Some(ready), Some(logger), Some(ssh), Some(telnet)) = (
        app.try_state::<TerminalReadyState>(),
        app.try_state::<SessionLogState>(),
        app.try_state::<SshState>(),
        app.try_state::<TelnetState>(),
    ) else {
        tracing::warn!("Terminal state not set up yet; no sessions to close");
        return 0;
    };

    let (ssh_sessions, telnet_sessions) = take_remote_sessions(&ready, &ssh, &telnet).await;
    let remote = ssh_sessions.len() + telnet_sessions.len();
    if remote > 0 {
        if let Err(e) = app.emit(QUITTING_EVENT, ()) {
            tracing::warn!(error = %e, "Failed to tell the frontend that the app is quitting");
        }
    }

    let mut closing = tokio::task::JoinSet::new();
    for (id, session) in ssh_sessions {
        closing.spawn(async move {
            session.close().await;
            id
        });
    }
    for (id, session) in telnet_sessions {
        closing.spawn(async move {
            session.close().await;
            id
        });
    }
    let close_remote = async {
        while let Some(closed) = closing.join_next().await {
            match closed {
                Ok(id) => release_session(&id, &ready, &logger),
                Err(e) => tracing::warn!(error = %e, "Closing a session failed"),
            }
        }
    };
    let ((), shells) = tokio::join!(close_remote, close_local_shells(app));
    remote + shells
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
    fn connect_finishing_while_the_app_quits_is_refused() {
        // `quit_app` closes the sessions registered so far; one registered
        // after it began would be left to the operating system.
        let fx = Fixture::new();
        let setup = fx.begin("c");
        assert!(setup.check_page_unchanged().is_ok());

        assert!(begin_quit(&fx.ready));
        assert!(setup.check_page_unchanged().is_err());
        assert!(fx.begin("d").check_page_unchanged().is_err());
    }

    #[test]
    fn quit_begins_only_once() {
        // A second request — the menu item and Ctrl+Q together, say — must
        // not start a second closing.
        let ready = TerminalReadyState::default();
        assert!(begin_quit(&ready));
        assert!(!begin_quit(&ready));
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
