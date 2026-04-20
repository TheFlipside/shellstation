use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use tauri::{AppHandle, Emitter};
use tracing::{error, info};

/// Handle to a single PTY session, holding the writer and child process.
struct PtyHandle {
    writer: Box<dyn Write + Send>,
    pair: Box<dyn MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send>,
    shutdown: Arc<AtomicBool>,
}

/// Manages all active PTY sessions, keyed by session ID.
#[derive(Default)]
pub struct PtyManager {
    sessions: HashMap<String, PtyHandle>,
}

impl PtyManager {
    /// Maximum number of concurrent PTY sessions.
    const MAX_SESSIONS: usize = 100;

    /// Spawn a new PTY session and start streaming output via Tauri events.
    ///
    /// Returns the session ID on success.
    pub fn spawn(
        &mut self,
        id: &str,
        cols: u16,
        rows: u16,
        app_handle: AppHandle,
        logger: Option<std::sync::Arc<std::sync::Mutex<crate::session_logger::SessionLogManager>>>,
        ready_rx: std::sync::mpsc::Receiver<()>,
    ) -> Result<(), String> {
        if self.sessions.len() >= Self::MAX_SESSIONS {
            return Err(format!(
                "Session limit reached (max {})",
                Self::MAX_SESSIONS
            ));
        }

        let pty_system = native_pty_system();

        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };

        let pair = pty_system.openpty(size).map_err(|e| e.to_string())?;

        let shell = default_shell();
        let mut cmd = CommandBuilder::new(&shell);
        cmd.env("TERM", "xterm-256color");

        let child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
        drop(pair.slave);

        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;

        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_flag = Arc::clone(&shutdown);

        let event_name = format!("terminal-output-{id}");
        let session_id = id.to_string();

        thread::spawn(move || {
            // Wait for the frontend listener before reading. The PTY pipe
            // buffers any output the shell produces during this wait, so
            // nothing is lost. Check shutdown flag periodically so the thread
            // can exit promptly if the session is killed during setup.
            // Cap the wait at 10s as a safety net — if the frontend never
            // signals, start reading anyway to avoid a permanently stuck session.
            let start = std::time::Instant::now();
            let max_wait = Duration::from_secs(10);
            let ready = loop {
                if shutdown_flag.load(Ordering::Relaxed) {
                    break false;
                }
                match ready_rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(()) => {
                        info!(session_id = %session_id, "Frontend listener ready, starting PTY reader");
                        break true;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if start.elapsed() >= max_wait {
                            info!(session_id = %session_id, "Frontend ready signal timed out, starting PTY reader anyway");
                            break true;
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        info!(session_id = %session_id, "Ready signal sender dropped, starting PTY reader anyway");
                        break true;
                    }
                }
            };
            if !ready {
                return;
            }

            let mut buf = [0u8; 4096];
            loop {
                if shutdown_flag.load(Ordering::Relaxed) {
                    info!(session_id = %session_id, "PTY reader received shutdown signal");
                    break;
                }
                match reader.read(&mut buf) {
                    Ok(0) => {
                        info!(session_id = %session_id, "PTY reader EOF");
                        break;
                    }
                    Ok(n) => {
                        let data = &buf[..n];
                        if let Some(ref lg) = logger {
                            if let Ok(mut mgr) = lg.lock() {
                                mgr.write_log(&session_id, data);
                            }
                        }
                        let payload = base64_encode(data);
                        if app_handle.emit(&event_name, &payload).is_err() {
                            break;
                        }
                    }
                    Err(ref e) if is_timeout_error(e) => {
                        continue;
                    }
                    Err(e) => {
                        if !shutdown_flag.load(Ordering::Relaxed) {
                            error!(session_id = %session_id, error = %e, "PTY read error");
                        }
                        break;
                    }
                }
            }

            if let Some(ref lg) = logger {
                if let Ok(mut mgr) = lg.lock() {
                    mgr.close_log(&session_id);
                }
            }
            let _ = app_handle.emit(&format!("terminal-exit-{session_id}"), ());
        });

        let handle = PtyHandle {
            writer,
            pair: pair.master,
            child,
            shutdown,
        };
        self.sessions.insert(id.to_string(), handle);

        info!(session_id = %id, "PTY session spawned");
        Ok(())
    }

    /// Write data to a PTY session's stdin.
    pub fn write(&mut self, id: &str, data: &[u8]) -> Result<(), String> {
        let handle = self
            .sessions
            .get_mut(id)
            .ok_or_else(|| format!("Session {id} not found"))?;
        handle.writer.write_all(data).map_err(|e| e.to_string())?;
        handle.writer.flush().map_err(|e| e.to_string())
    }

    /// Resize a PTY session.
    pub fn resize(&mut self, id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let handle = self
            .sessions
            .get_mut(id)
            .ok_or_else(|| format!("Session {id} not found"))?;
        handle
            .pair
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())
    }

    /// Kill a PTY session and clean up resources.
    pub fn kill(&mut self, id: &str) -> Result<(), String> {
        let mut handle = self
            .sessions
            .remove(id)
            .ok_or_else(|| format!("Session {id} not found"))?;

        // Signal the reader thread to stop.
        handle.shutdown.store(true, Ordering::Relaxed);

        // Kill may fail if the child already exited — that's fine.
        if let Err(e) = handle.child.kill() {
            info!(session_id = %id, error = %e, "Child kill returned error (may have already exited)");
        }
        let _ = handle.child.wait();

        // Drop the master PTY handle explicitly. On Windows (ConPTY) this
        // closes the pseudoconsole output pipe, which unblocks the reader
        // thread's read() call. On Unix, killing the child already causes
        // the slave side to hang up, but dropping the master ensures cleanup.
        drop(handle.writer);
        drop(handle.pair);

        info!(session_id = %id, "PTY session killed");
        Ok(())
    }
}

/// Thread-safe wrapper around `PtyManager` for use as Tauri managed state.
pub struct PtyState(pub Arc<Mutex<PtyManager>>);

impl Default for PtyState {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(PtyManager::default())))
    }
}

/// Return the default shell for the current platform.
fn default_shell() -> String {
    #[cfg(unix)]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }
    #[cfg(windows)]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
    }
}

/// Base64-encode bytes for safe transit over Tauri events.
fn base64_encode(data: &[u8]) -> String {
    use base64::prelude::{Engine, BASE64_STANDARD};
    BASE64_STANDARD.encode(data)
}

/// Check if an IO error is a timeout/would-block error.
fn is_timeout_error(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    )
}
