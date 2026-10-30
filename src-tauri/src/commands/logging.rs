//! Settings for the session logs and the application log.
//!
//! The webview cannot name a new log directory itself: the user chooses one
//! in a folder dialog the backend opens ([`logging_pick_directory`],
//! [`app_logging_pick_directory`]; see [`path_dialog`]). Nor can it name a
//! log file freely: the filename format allows only plain characters and
//! must end in `.log` or `.txt` (`LoggingConfig::validate_filename_format`,
//! which also runs when the config is loaded). The logger appends terminal
//! output, which the remote host controls, so a compromised webview could
//! otherwise have it written to a file that something runs or reads, such
//! as one in a Startup folder.

use std::path::PathBuf;
use std::time::Instant;

use tauri::{State, Window};

use super::path_dialog::{self, PickSlot};
use super::path_dialog_text;
use crate::config::{self, AppLoggingConfig, ConfigState, LoggingConfig};
use crate::session_logger::SessionLogState;

/// The folder the user last chose for each log.
static SESSION_LOG_DIR: PickSlot = PickSlot::new("Choose a new log directory with Browse");
static APP_LOG_DIR: PickSlot = PickSlot::new("Choose a new log directory with Browse");

#[tauri::command]
pub async fn logging_get_config(state: State<'_, ConfigState>) -> Result<LoggingConfig, String> {
    let config = state.config.lock().map_err(|e| format!("{e}"))?;
    Ok(config.logging.clone())
}

/// Validate that a log directory path is absolute.
fn validate_log_directory(dir: &str) -> Result<(), String> {
    let path = PathBuf::from(dir);
    if !path.is_absolute() {
        return Err("Log directory must be an absolute path".to_string());
    }
    Ok(())
}

/// `<config_dir>/logs`, where both logs go unless the user chose otherwise.
fn default_log_dir(state: &ConfigState) -> PathBuf {
    state
        .config_path
        .parent()
        .map_or_else(|| PathBuf::from("logs"), |p| p.join("logs"))
}

/// Where the folder dialog opens: the configured directory, or the default.
/// Neither comes from the webview, so it cannot open the dialog in a folder
/// it would like the user to pick.
fn start_directory(state: &ConfigState, configured: Option<&str>) -> PathBuf {
    configured
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| default_log_dir(state))
}

/// Let the user choose a folder for the session logs in a native dialog.
/// Returns the folder, which `logging_save_config` then accepts as the log
/// directory, or `None` if the user cancelled.
#[tauri::command]
pub async fn logging_pick_directory(
    window: Window,
    state: State<'_, ConfigState>,
) -> Result<Option<String>, String> {
    let configured = state
        .config
        .lock()
        .map_err(|e| format!("{e}"))?
        .logging
        .log_directory
        .clone();
    let start = start_directory(&state, configured.as_deref());
    let title = path_dialog_text::for_system_language().session_logs;
    path_dialog::choose_folder(&window, title, &start, &SESSION_LOG_DIR).await
}

/// Save the session log settings. `log_directory` must be the default
/// (`None` or empty), the directory already configured, or the folder just
/// chosen with `logging_pick_directory`.
#[tauri::command]
pub async fn logging_save_config(
    config_state: State<'_, ConfigState>,
    logger_state: State<'_, SessionLogState>,
    enabled: bool,
    log_directory: Option<String>,
    filename_format: Option<String>,
) -> Result<(), String> {
    // Validate inputs before persisting.
    if let Some(ref dir) = log_directory {
        if !dir.is_empty() {
            validate_log_directory(dir)?;
        }
    }
    if let Some(ref fmt) = filename_format {
        if !fmt.is_empty() {
            LoggingConfig::validate_filename_format(fmt)?;
        }
    }

    let mut config = config_state.config.lock().map_err(|e| format!("{e}"))?;
    // The settings in memory change only once the file is written.
    let mut updated = config.clone();
    updated.logging.enabled = enabled;
    updated.logging.log_directory.clone_from(&log_directory);
    if let Some(fmt) = filename_format {
        if !fmt.is_empty() {
            updated.logging.filename_format = fmt;
        }
    }
    SESSION_LOG_DIR.save_with(
        log_directory.as_deref(),
        config.logging.log_directory.as_deref(),
        Instant::now(),
        || config::save_config(&config_state.config_path, &updated),
    )?;
    *config = updated;

    // Update the live logger state so new connections use the new settings.
    let log_dir = match &config.logging.log_directory {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => default_log_dir(&config_state),
    };
    let mut mgr = logger_state.0.lock().map_err(|e| format!("{e}"))?;
    mgr.enabled = config.logging.enabled;
    mgr.log_dir = log_dir;
    mgr.filename_format
        .clone_from(&config.logging.filename_format);

    Ok(())
}

#[tauri::command]
pub async fn app_logging_get_config(
    state: State<'_, ConfigState>,
) -> Result<AppLoggingConfig, String> {
    let config = state.config.lock().map_err(|e| format!("{e}"))?;
    Ok(config.app_logging.clone())
}

/// Let the user choose a folder for the application log in a native dialog.
/// Returns the folder, which `app_logging_save_config` then accepts as the
/// log directory, or `None` if the user cancelled.
#[tauri::command]
pub async fn app_logging_pick_directory(
    window: Window,
    state: State<'_, ConfigState>,
) -> Result<Option<String>, String> {
    let configured = state
        .config
        .lock()
        .map_err(|e| format!("{e}"))?
        .app_logging
        .log_directory
        .clone();
    let start = start_directory(&state, configured.as_deref());
    let title = path_dialog_text::for_system_language().app_log;
    path_dialog::choose_folder(&window, title, &start, &APP_LOG_DIR).await
}

/// Save the application log settings, which take effect at the next start.
/// `log_directory` must be the default (`None` or empty), the directory
/// already configured, or the folder just chosen with
/// `app_logging_pick_directory`.
#[tauri::command]
pub async fn app_logging_save_config(
    state: State<'_, ConfigState>,
    enabled: bool,
    log_directory: Option<String>,
    level: String,
) -> Result<(), String> {
    AppLoggingConfig::validate_level(&level)?;
    if let Some(ref dir) = log_directory {
        if !dir.is_empty() {
            validate_log_directory(dir)?;
        }
    }

    let mut config = state.config.lock().map_err(|e| format!("{e}"))?;
    // The settings in memory change only once the file is written.
    let mut updated = config.clone();
    updated.app_logging.enabled = enabled;
    updated.app_logging.log_directory.clone_from(&log_directory);
    updated.app_logging.level = level;
    APP_LOG_DIR.save_with(
        log_directory.as_deref(),
        config.app_logging.log_directory.as_deref(),
        Instant::now(),
        || config::save_config(&state.config_path, &updated),
    )?;
    *config = updated;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn a_relative_log_directory_is_refused() {
        // A hand-edited config file can hold one.
        let err = validate_log_directory("logs").unwrap_err();
        assert!(err.contains("absolute"), "{err}");
    }

    #[test]
    fn the_dialog_opens_in_the_configured_or_default_directory() {
        let state = ConfigState {
            config: Mutex::new(config::AppConfig::default()),
            config_path: std::env::temp_dir().join("config.json"),
        };
        let default = std::env::temp_dir().join("logs");
        assert_eq!(start_directory(&state, None), default);
        assert_eq!(start_directory(&state, Some("")), default);
        assert_eq!(start_directory(&state, Some("relative")), default);
        let configured = std::env::temp_dir().join("configured");
        assert_eq!(start_directory(&state, configured.to_str()), configured);
    }
}
