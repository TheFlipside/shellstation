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
mod session_login;
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
pub use session_login::*;
pub use session_login_sequences::*;
pub use ssh::*;
pub use telnet::*;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use tauri::{Emitter, Manager};

use crate::pty::{PtyHandle, PtyState};
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
    /// Which page of the main webview the sessions belong to.
    page: std::sync::Mutex<PageState>,
    /// Held while the announced page changes and the sessions it leaves
    /// behind are taken out of their managers: see [`change_page`].
    page_change: tokio::sync::Mutex<()>,
    /// Set once the app has begun to quit, and never cleared; see
    /// [`quit_app`].
    quitting: AtomicBool,
}

/// The page of the main webview that terminal sessions may belong to.
///
/// A page load discards the frontend's tabs, so a session of the page before
/// it can never be shown or closed. Each page therefore makes up a token when
/// it starts, announces it ([`page_announce`]) and passes it with every
/// connect, and a session is registered only for the page announced last.
/// The token arrives with the request, so a connect of an earlier page is
/// told apart whenever it arrives, however late.
///
/// Take this lock last and briefly: it is taken under the session managers'
/// locks, and nothing else is locked while it is held.
#[derive(Default)]
struct PageState {
    /// How many times the main webview has started loading a page.
    loads: u64,
    /// The page announced last, unless a page load has started since.
    current: Option<AnnouncedPage>,
}

/// A page that has announced itself.
struct AnnouncedPage {
    /// The token the page made up for itself.
    token: String,
    /// [`PageState::loads`] when the page announced itself.
    load: u64,
}

/// Longest page token accepted. The frontend sends a `UUID`, 36 characters.
const MAX_PAGE_TOKEN_LEN: usize = 64;

/// Whether `token` can be a page token: letters, digits and hyphens, so that
/// what is kept for the page stays small.
fn is_page_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= MAX_PAGE_TOKEN_LEN
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

impl TerminalReadyState {
    /// Count a page load that has started in the main webview, returning its
    /// number.
    fn count_page_load(&self) -> u64 {
        let mut page = lock_or_recover(&self.page);
        page.loads += 1;
        page.loads
    }

    /// Forget the announced page, for page load number `load`, returning
    /// whether it is forgotten. A page announced during that load or a later
    /// one is the page the load brought, or a newer one, and is kept.
    fn leave_page(&self, load: u64) -> bool {
        let mut page = lock_or_recover(&self.page);
        if page
            .current
            .as_ref()
            .is_some_and(|current| current.load >= load)
        {
            return false;
        }
        page.current = None;
        true
    }

    /// Make the page with `token` the announced one, returning whether that
    /// changed anything.
    fn enter_page(&self, token: &str) -> bool {
        let mut page = lock_or_recover(&self.page);
        if page
            .current
            .as_ref()
            .is_some_and(|current| current.token == token)
        {
            return false;
        }
        page.current = Some(AnnouncedPage {
            token: token.to_string(),
            load: page.loads,
        });
        true
    }

    /// Whether the page with `token` is the one announced last.
    fn is_current_page(&self, token: &str) -> bool {
        lock_or_recover(&self.page)
            .current
            .as_ref()
            .is_some_and(|current| current.token == token)
    }

    /// Fail unless the page with `token` may open a session: it must be the
    /// page announced last, as otherwise its tabs are gone and nothing would
    /// ever show or close the session.
    ///
    /// Fail as well once the app is quitting: [`quit_app`] has taken, or is
    /// about to take, the sessions it closes, and one registered after that
    /// would be left to the operating system.
    ///
    /// A connect command calls this first, so that a connect of an earlier
    /// page never reaches the remote host, and [`ConnectSetup`] calls it
    /// again right before registering.
    fn check_page(&self, token: &str) -> Result<(), String> {
        if !self.is_current_page(token) {
            return Err("The window was reloaded while the session was connecting".to_string());
        }
        if self.quitting.load(Ordering::SeqCst) {
            return Err("The app is quitting".to_string());
        }
        Ok(())
    }
}

/// A change of the announced page, which leaves the sessions of the page
/// before it with nothing to show or close them.
enum PageChange {
    /// The main webview started page load number `.0`.
    Load(u64),
    /// The page with this token announced itself.
    Announce(String),
}

impl PageChange {
    /// Make the change, returning whether there was anything to change.
    fn apply(&self, ready: &TerminalReadyState) -> bool {
        match self {
            Self::Load(load) => ready.leave_page(*load),
            Self::Announce(token) => ready.enter_page(token),
        }
    }
}

/// Tell the backend which page the main webview now shows. The frontend
/// calls this when it starts, and again before every connect, with a token it
/// made up for its page and passes to the connect commands as `page`.
///
/// A token other than the announced one is a new page: every session is
/// closed, since they all belong to pages that are gone, and from then on
/// only that page may open one. Announcing the same token again changes
/// nothing, but returns only once a change of page still under way is
/// complete, so that a session opened after it is not taken by that change.
#[tauri::command]
pub async fn page_announce(
    ready: tauri::State<'_, TerminalReadyState>,
    logger: tauri::State<'_, SessionLogState>,
    ssh: tauri::State<'_, SshState>,
    telnet: tauri::State<'_, TelnetState>,
    pty: tauri::State<'_, PtyState>,
    page: String,
) -> Result<(), String> {
    if !is_page_token(&page) {
        return Err("Invalid page token".to_string());
    }
    let change = PageChange::Announce(page);
    let count = close_sessions_left_behind(&ready, &logger, &ssh, &telnet, &pty, change).await;
    if count > 0 {
        tracing::info!(count, "Closed the sessions left behind by an earlier page");
    }
    Ok(())
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
    /// The token of the page that asked for the session.
    page: String,
    armed: bool,
}

impl<'a> ConnectSetup<'a> {
    /// Open the session log, so output from the start is captured. `page`
    /// is the token the frontend passed with the connect.
    fn begin(
        id: &str,
        log_name: &str,
        page: String,
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
            page,
            armed: true,
        }
    }

    /// Store the signal `terminal_ready` fires once the tab is listening.
    fn add_ready_signal(&self, signal: impl FnOnce() + Send + 'static) {
        lock_or_recover(&self.ready.senders).insert(self.id.clone(), Box::new(signal));
    }

    /// Fail if the page that asked for the session is no longer the
    /// announced one, or the app is quitting: see
    /// [`TerminalReadyState::check_page`]. Call it under the manager lock,
    /// right before registering, so that a change of page either takes the
    /// session with the others or finds it refused.
    fn check_page_unchanged(&self) -> Result<(), String> {
        self.ready.check_page(&self.page)
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

/// Close every terminal session of the page before. Run when the main webview
/// starts loading a page: a reload throws away the frontend's tabs, so the
/// sessions from before it have nothing left to show or close them. Left
/// alone, they would stay connected for as long as the app runs, their
/// keepalives telling each remote device that the session is still in use.
///
/// The load is counted here, before the handler returns and so before the new
/// page can announce itself; the closing runs on a task. Should the new page
/// have announced itself by the time the task gets its turn, the task does
/// nothing: that announcement has closed the old sessions already, and the
/// ones there now are the new page's.
///
/// By the time the webview reports the load, the old page can send nothing
/// more. A connect it sent before is refused wherever it arrives, since no
/// page, and later another page, is the announced one. That leaves one case:
/// an announcement of the old page still on its way makes it the announced
/// page again, and a connect it sent in that same moment is then registered.
/// The new page's own announcement, sent as it starts, closes that session.
/// Should the old page's announcement arrive later still, after the new
/// page's, it closes the new page's sessions, and the new page's next
/// connect, which announces it again, closes what the old page opened.
pub fn page_load_started(app: &tauri::AppHandle) {
    let Some(ready) = app.try_state::<TerminalReadyState>() else {
        tracing::warn!("Terminal state not set up yet; no sessions to close");
        return;
    };
    let change = PageChange::Load(ready.count_page_load());
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let (Some(ready), Some(logger), Some(ssh), Some(telnet), Some(pty)) = (
            app.try_state::<TerminalReadyState>(),
            app.try_state::<SessionLogState>(),
            app.try_state::<SshState>(),
            app.try_state::<TelnetState>(),
            app.try_state::<PtyState>(),
        ) else {
            tracing::warn!("Terminal state not set up yet; no sessions to close");
            return;
        };
        let count = close_sessions_left_behind(&ready, &logger, &ssh, &telnet, &pty, change).await;
        if count > 0 {
            tracing::info!(count, "Closed the sessions left behind by a page load");
        }
    });
}

/// Make a change of page and close every session it leaves behind, returning
/// how many there were. Does nothing if the change changes nothing.
async fn close_sessions_left_behind(
    ready: &TerminalReadyState,
    logger: &SessionLogState,
    ssh: &SshState,
    telnet: &TelnetState,
    pty: &PtyState,
    change: PageChange,
) -> usize {
    let Some(((ssh_sessions, telnet_sessions), shells)) =
        change_page(ready, ssh, telnet, pty, &change).await
    else {
        return 0;
    };
    let count = ssh_sessions.len() + telnet_sessions.len();
    // Dropping a session closes it (see the `Drop` impls); then forget
    // what the session kept outside its manager.
    for (id, session) in ssh_sessions {
        drop(session);
        release_session(&id, ready, logger);
    }
    for (id, session) in telnet_sessions {
        drop(session);
        release_session(&id, ready, logger);
    }
    count + shut_down_shells(ready, logger, shells).await
}

/// The SSH and Telnet sessions [`take_remote_sessions`] takes, with their `ID`s.
type RemoteSessions = (Vec<(String, SshSession)>, Vec<(String, TelnetSession)>);

/// The local shells [`take_local_shells`] takes, with their `ID`s.
type LocalShells = Vec<(String, PtyHandle)>;

/// Make a change of page and take every session it leaves behind out of its
/// manager, or return `None` if the change changes nothing.
///
/// One change at a time. The local shells are taken after the page has
/// changed, under a lock of their own, and a second change in between could
/// bring a page whose shells the first would then take. For the same reason
/// an announcement that changes nothing still waits its turn here: the page
/// it announces opens sessions once it returns, and a change of that page
/// still taking shells would take those too.
///
/// The sessions are closed by the caller, after its turn: a shell slow to
/// exit must not keep the next page waiting. A held lock on the shells
/// keeps it waiting for [`SHELL_LOCK_TIMEOUT`] at most, after which
/// [`take_local_shells`] gives up.
async fn change_page(
    ready: &TerminalReadyState,
    ssh: &SshState,
    telnet: &TelnetState,
    pty: &PtyState,
    change: &PageChange,
) -> Option<(RemoteSessions, LocalShells)> {
    let _turn = ready.page_change.lock().await;
    let remote = take_remote_sessions(ready, ssh, telnet, Some(change)).await?;
    Some((
        remote,
        take_local_shells(pty, Some(SHELL_LOCK_TIMEOUT)).await,
    ))
}

/// Take every SSH and Telnet session out of its manager, and end the connects
/// still in flight. With a change of page, make the change first, and take
/// nothing (`None`) if it changes nothing, or if the app is quitting, when
/// [`quit_app`] closes the sessions and no page will connect again.
///
/// The page changes while both manager locks are held. A connect checks its
/// page under its manager's lock right before registering, so one of the
/// page before is either taken here or refused, and one of the page after is
/// neither. Pending prompts belong to connects from before this too; dropping
/// them ends those connects now rather than at their timeout.
async fn take_remote_sessions(
    ready: &TerminalReadyState,
    ssh: &SshState,
    telnet: &TelnetState,
    change: Option<&PageChange>,
) -> Option<RemoteSessions> {
    let mut ssh_manager = ssh.manager.lock().await;
    let mut telnet_manager = telnet.0.lock().await;
    if let Some(change) = change {
        if ready.quitting.load(Ordering::SeqCst) || !change.apply(ready) {
            return None;
        }
    }
    lock_or_recover(&ssh.host_verify_senders).clear();
    lock_or_recover(&ssh.kbd_interactive_senders).clear();
    Some((ssh_manager.take_all(), telnet_manager.take_all()))
}

/// How long a change of page waits for the local shells' lock: see
/// [`take_local_shells`]. Shorter under test, which waits it out.
const SHELL_LOCK_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(if cfg!(test) { 1 } else { 2 });

/// Who takes the local shells in [`take_local_shells`]: the thread, once it
/// has their lock, or nobody, once the wait for it is over. Settled once,
/// by whichever comes first; the other finds it settled.
struct ShellTake(AtomicU8);

impl ShellTake {
    const WAITING: u8 = 0;
    const TAKING: u8 = 1;
    const GIVEN_UP: u8 = 2;

    /// Settle it as `outcome`, returning whether it was still open.
    fn settle(&self, outcome: u8) -> bool {
        self.0
            .compare_exchange(Self::WAITING, outcome, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
}

/// Gives up a [`ShellTake`] when the wait for it ends without the shells,
/// whether it timed out or its future was dropped. Left to itself, the
/// thread would take the shells once the lock comes free, with nobody left
/// to shut them down.
struct GiveUpOnDrop(std::sync::Arc<ShellTake>);

impl Drop for GiveUpOnDrop {
    fn drop(&mut self) {
        self.0.settle(ShellTake::GIVEN_UP);
    }
}

/// Take every local shell out of its manager.
///
/// This runs on a blocking thread: the shells' lock is a blocking one, which
/// a stuck write to a shell can hold, and the SSH and Telnet locks must not
/// wait on it. So the shells are taken after the page has changed, under
/// their own lock, where `pty_spawn` checks its page as well: a shell of the
/// page before is either taken here or refused.
///
/// With a `limit`, as a change of page gives, no shell is taken should the
/// lock not come free within it, then or later: every connect waits for the
/// change of page, and by the time the lock comes free the shells there may
/// be the next page's. The shells of the page before then stay, without a
/// tab, until the next change of page takes them or the app quits. The quit
/// gives no limit: it has its own, and nothing comes after it.
async fn take_local_shells(pty: &PtyState, limit: Option<std::time::Duration>) -> LocalShells {
    let manager = std::sync::Arc::clone(&pty.0);
    let take = std::sync::Arc::new(ShellTake(AtomicU8::new(ShellTake::WAITING)));
    let thread_take = std::sync::Arc::clone(&take);
    let mut taking = tauri::async_runtime::spawn_blocking(move || {
        let mut manager = lock_or_recover(&manager);
        if thread_take.settle(ShellTake::TAKING) {
            manager.take_all()
        } else {
            Vec::new()
        }
    });
    let abandon = GiveUpOnDrop(take);

    let waited = match limit {
        Some(limit) => tokio::time::timeout(limit, &mut taking).await.ok(),
        None => Some((&mut taking).await),
    };
    let taken = if let Some(taken) = waited {
        taken
    } else if abandon.0.settle(ShellTake::GIVEN_UP) {
        // Settled here, not left to the end of the function: from now on
        // the thread takes nothing.
        tracing::warn!(
            ?limit,
            "The local shells' lock is held; leaving the shells to the next change of page"
        );
        return Vec::new();
    } else {
        // The thread got the lock at the last moment: it has the shells, or
        // will in a moment.
        taking.await
    };
    taken.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Taking the local shells failed");
        Vec::new()
    })
}

/// Kill the local shells taken out of their manager and forget what they
/// kept outside it, returning how many there were. Killing waits for the
/// shell to exit, so it runs on a blocking thread. The kill comes before the
/// release, so the shell's reader cannot start reading on the way out.
async fn shut_down_shells(
    ready: &TerminalReadyState,
    logger: &SessionLogState,
    shells: LocalShells,
) -> usize {
    if shells.is_empty() {
        return 0;
    }
    let ids: Vec<String> = shells.iter().map(|(id, _)| id.clone()).collect();
    let killed = tauri::async_runtime::spawn_blocking(move || {
        for (id, shell) in shells {
            shell.shut_down(&id);
        }
    })
    .await;
    if let Err(e) = killed {
        tracing::warn!(error = %e, "Closing the local shells failed");
    }
    // Released even if a kill panicked: the shells are out of their manager.
    for id in &ids {
        release_session(id, ready, logger);
    }
    ids.len()
}

/// Event telling the frontend that the app is closing its SSH and Telnet
/// sessions before it quits, so that it can say so while that takes.
const QUITTING_EVENT: &str = "app:quitting";

/// How long quitting waits, in all, for the sessions to close and their
/// logs to be written before it exits anyway. The SSH chains close side by
/// side, each giving up after [`DISCONNECT_TIMEOUT`]; the extra second is
/// for a local shell slow to exit, a manager lock held by a stuck write, or
/// a session log that is still being written.
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
/// each session log closed and waited for until it is written. Called by
/// [`app_quit`], and by the menu's quit item directly, so that the item
/// works even when the webview does not.
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
/// connect is refused (see [`TerminalReadyState::check_page`]).
fn begin_quit(ready: &TerminalReadyState) -> bool {
    !ready.quitting.swap(true, Ordering::SeqCst)
}

/// Close every session for [`quit_app`], all at the same time, so quitting
/// takes as long as the slowest close rather than all of them in turn.
/// Returns how many there were. When some are SSH or Telnet sessions, the
/// frontend is told first.
async fn close_sessions_for_quit(app: &tauri::AppHandle) -> usize {
    let (Some(ready), Some(logger), Some(ssh), Some(telnet), Some(pty)) = (
        app.try_state::<TerminalReadyState>(),
        app.try_state::<SessionLogState>(),
        app.try_state::<SshState>(),
        app.try_state::<TelnetState>(),
        app.try_state::<PtyState>(),
    ) else {
        tracing::warn!("Terminal state not set up yet; no sessions to close");
        return 0;
    };

    // Without a change of page, the sessions are always taken.
    let (ssh_sessions, telnet_sessions) = take_remote_sessions(&ready, &ssh, &telnet, None)
        .await
        .unwrap_or_default();
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
    let close_local = async {
        let shells = take_local_shells(&pty, None).await;
        shut_down_shells(&ready, &logger, shells).await
    };
    let ((), shells) = tokio::join!(close_remote, close_local);
    wait_for_session_logs(&logger).await;
    remote + shells
}

/// Close every session log and wait until what the sessions put out is
/// written. Each log is written by a thread of its own, which the exit would
/// stop wherever it is. Closing them all takes in the log of a connection
/// still being set up, whose writer would otherwise never end. The wait is on
/// a blocking thread, and ends with [`QUIT_TIMEOUT`] at the latest: a log
/// directory that does not answer must not keep the app from quitting. It
/// comes after the sessions have closed, and [`quit_app`] gives the two
/// [`QUIT_TIMEOUT`] between them: after an SSH close that took its three
/// seconds, the logs have what is left. Most are written by then, as a log
/// is closed as soon as its session is.
async fn wait_for_session_logs(logger: &SessionLogState) {
    let writers = lock_or_recover(&logger.0).close_all();
    let written =
        tauri::async_runtime::spawn_blocking(move || writers.wait_until_done(QUIT_TIMEOUT)).await;
    if !matches!(written, Ok(true)) {
        tracing::warn!("Session logs still being written at quit; quitting anyway");
    }
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

    /// Ready and logger state for one test. No log gets a file, as no
    /// session has any output; only the log directory is created, by the
    /// logs' writers, and removed again on drop.
    struct Fixture {
        ready: TerminalReadyState,
        logger: SessionLogState,
        dir: std::path::PathBuf,
    }

    /// The token of the page that connects in these tests.
    const PAGE: &str = "page-a";

    impl Fixture {
        /// A fixture whose page [`PAGE`] has loaded and announced itself.
        fn new() -> Self {
            let fx = Self::unannounced();
            fx.load_page();
            assert!(fx.ready.enter_page(PAGE));
            fx
        }

        fn unannounced() -> Self {
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

        /// Set up a connect that the page [`PAGE`] sent.
        fn begin(&self, id: &str) -> ConnectSetup<'_> {
            ConnectSetup::begin(id, "host", PAGE.to_string(), &self.ready, &self.logger)
        }

        /// A page load starts and the task it spawns runs, with nothing in
        /// between.
        fn load_page(&self) {
            let load = self.ready.count_page_load();
            assert!(PageChange::Load(load).apply(&self.ready));
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
            // Each log's writer creates the log directory when it starts:
            // wait until they have ended, or one could create it again.
            let writers = lock_or_recover(&self.logger.0).close_all();
            writers.wait_until_done(std::time::Duration::from_secs(30));
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

        fx.load_page();
        assert!(setup.check_page_unchanged().is_err());

        // Nor may the new page connect before it has announced itself.
        let early = ConnectSetup::begin("d", "host", "page-b".to_string(), &fx.ready, &fx.logger);
        assert!(early.check_page_unchanged().is_err());
        assert!(fx.ready.enter_page("page-b"));
        assert!(early.check_page_unchanged().is_ok());
        assert!(setup.check_page_unchanged().is_err());
    }

    #[test]
    fn connect_of_an_earlier_page_is_refused_whenever_it_arrives() {
        // The known issue this replaces: a connect the old page sent in the
        // instant of a reload started running after the load was counted,
        // and passed for one of the new page. Its token tells it apart.
        let fx = Fixture::new();
        fx.load_page();
        assert!(fx.ready.check_page(PAGE).is_err());

        assert!(fx.ready.enter_page("page-b"));
        assert!(fx.ready.check_page(PAGE).is_err());
        assert!(fx.ready.check_page("page-b").is_ok());
    }

    #[test]
    fn connect_before_any_announcement_is_refused() {
        let fx = Fixture::unannounced();
        assert!(fx.ready.check_page(PAGE).is_err());
        assert!(fx.ready.check_page("").is_err());
    }

    #[test]
    fn page_token_is_short_and_plain() {
        assert!(is_page_token("3f2b8c1e-9a4d-4e7b-8f6a-0c1d2e3f4a5b"));
        assert!(is_page_token(&"a".repeat(MAX_PAGE_TOKEN_LEN)));
        assert!(!is_page_token(""));
        assert!(!is_page_token(&"a".repeat(MAX_PAGE_TOKEN_LEN + 1)));
        assert!(!is_page_token("page a"));
        assert!(!is_page_token("pag\u{e9}"));
    }

    #[test]
    fn announcing_the_same_page_again_changes_nothing() {
        // The frontend announces before every connect. Only a new page may
        // close the sessions, or each connect would close the ones before.
        let fx = Fixture::new();
        let again = PageChange::Announce(PAGE.to_string());
        assert!(!again.apply(&fx.ready));
        assert!(fx.ready.check_page(PAGE).is_ok());

        let other = PageChange::Announce("page-b".to_string());
        assert!(other.apply(&fx.ready));
        assert!(fx.ready.check_page(PAGE).is_err());
    }

    #[test]
    fn page_load_task_running_late_keeps_the_new_page() {
        // The load is counted when the webview reports it; the task that
        // closes the sessions runs later. A new page that announces itself
        // in between has closed the old sessions itself, and the sessions
        // there when the task runs are its own.
        let fx = Fixture::new();
        let load = PageChange::Load(fx.ready.count_page_load());
        assert!(fx.ready.enter_page("page-b"));

        assert!(!load.apply(&fx.ready));
        assert!(fx.ready.check_page("page-b").is_ok());

        // The next load leaves that page like any other.
        let next = PageChange::Load(fx.ready.count_page_load());
        assert!(next.apply(&fx.ready));
        assert!(fx.ready.check_page("page-b").is_err());
    }

    #[test]
    fn late_announcement_of_the_old_page_yields_to_the_new_page() {
        // An announcement the old page sent in the instant of a reload makes
        // it the announced page once more, and a connect it sent in that
        // moment is opened. The new page's announcement, sent as it starts,
        // is a change again, which closes whatever the old page opened.
        let fx = Fixture::new();
        fx.load_page();
        assert!(fx.ready.check_page(PAGE).is_err());
        assert!(fx.ready.enter_page(PAGE));
        assert!(fx.ready.check_page(PAGE).is_ok());

        let new_page = PageChange::Announce("page-b".to_string());
        assert!(new_page.apply(&fx.ready));
        assert!(fx.ready.check_page(PAGE).is_err());
    }

    /// The session managers a change of page takes from, all empty.
    #[derive(Default)]
    struct Managers {
        ssh: SshState,
        telnet: TelnetState,
        pty: PtyState,
    }

    impl Managers {
        async fn announce(&self, fx: &Fixture, token: &str) -> usize {
            self.change(fx, PageChange::Announce(token.to_string()))
                .await
        }

        async fn change(&self, fx: &Fixture, change: PageChange) -> usize {
            close_sessions_left_behind(
                &fx.ready,
                &fx.logger,
                &self.ssh,
                &self.telnet,
                &self.pty,
                change,
            )
            .await
        }

        /// Hold the local shells' lock on a thread of its own until the
        /// returned sender is dropped, as a stuck write to a shell does.
        fn hold_shells(&self) -> std::sync::mpsc::Sender<()> {
            let manager = Arc::clone(&self.pty.0);
            let (release, released) = std::sync::mpsc::channel::<()>();
            let (held, is_held) = std::sync::mpsc::channel::<()>();
            std::thread::spawn(move || {
                let _guard = manager.lock().unwrap();
                held.send(()).unwrap();
                let _ = released.recv();
            });
            is_held.recv().unwrap();
            release
        }
    }

    #[tokio::test]
    async fn announcement_waits_for_a_change_of_page_under_way() {
        // The frontend announces its page as it starts and again before each
        // connect. The first makes the page the announced one and then takes
        // the local shells; a second that returned meanwhile would let the
        // page open a shell for the first to take.
        const WAIT: std::time::Duration = std::time::Duration::from_millis(100);
        let fx = Fixture::new();
        let managers = Managers::default();
        let release = managers.hold_shells();

        let first = managers.announce(&fx, "page-b");
        let second = managers.announce(&fx, "page-b");
        tokio::pin!(first, second);

        assert!(tokio::time::timeout(WAIT, &mut first).await.is_err());
        assert!(fx.ready.is_current_page("page-b"));
        assert!(tokio::time::timeout(WAIT, &mut second).await.is_err());

        drop(release);
        assert_eq!(first.await, 0);
        assert_eq!(second.await, 0);
        assert!(fx.ready.check_page("page-b").is_ok());
    }

    #[tokio::test]
    async fn page_does_not_change_once_the_app_is_quitting() {
        // Quitting closes the sessions itself, waiting for each to end. A
        // change of page then must not take them first and drop them.
        let fx = Fixture::new();
        let managers = Managers::default();
        assert!(begin_quit(&fx.ready));

        assert_eq!(managers.announce(&fx, "page-b").await, 0);
        assert!(fx.ready.is_current_page(PAGE));
        assert!(!fx.ready.is_current_page("page-b"));

        let load = PageChange::Load(fx.ready.count_page_load());
        assert_eq!(managers.change(&fx, load).await, 0);
        assert!(fx.ready.is_current_page(PAGE));
    }

    #[tokio::test]
    async fn change_of_page_does_not_wait_for_a_held_shell_lock() {
        // A stuck write to a local shell holds the shells' lock. Every
        // connect announces its page first, so a change of page that waited
        // on that lock would keep SSH and Telnet from connecting as well.
        let fx = Fixture::new();
        let managers = Managers::default();
        let release = managers.hold_shells();

        let started = std::time::Instant::now();
        assert_eq!(managers.announce(&fx, "page-b").await, 0);
        assert!(started.elapsed() >= SHELL_LOCK_TIMEOUT);
        assert!(fx.ready.check_page("page-b").is_ok());

        // Nor does the next announcement, before a connect, wait again.
        let again = tokio::time::timeout(SHELL_LOCK_TIMEOUT / 2, managers.announce(&fx, "page-b"));
        assert_eq!(again.await, Ok(0));
        drop(release);
    }

    #[tokio::test]
    async fn quit_waits_for_a_held_shell_lock() {
        // Nothing comes after the quit to take the shells, and it has a
        // time limit of its own, so it waits for the lock.
        let managers = Managers::default();
        let release = managers.hold_shells();

        let taking = take_local_shells(&managers.pty, None);
        tokio::pin!(taking);
        let waited = tokio::time::timeout(SHELL_LOCK_TIMEOUT * 2, &mut taking);
        assert!(waited.await.is_err());

        drop(release);
        assert!(taking.await.is_empty());
    }

    #[tokio::test]
    async fn abandoned_shell_take_takes_nothing_later() {
        // Should the wait be dropped while the lock is held, the thread must
        // not take the shells once it gets the lock: nobody would shut them
        // down, and they may be the next page's by then.
        let take = Arc::new(ShellTake(AtomicU8::new(ShellTake::WAITING)));
        drop(GiveUpOnDrop(Arc::clone(&take)));
        assert!(!take.settle(ShellTake::TAKING));

        // A take the thread has begun stays the thread's.
        let take = Arc::new(ShellTake(AtomicU8::new(ShellTake::WAITING)));
        assert!(take.settle(ShellTake::TAKING));
        drop(GiveUpOnDrop(Arc::clone(&take)));
        assert_eq!(take.0.load(Ordering::SeqCst), ShellTake::TAKING);
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
}
