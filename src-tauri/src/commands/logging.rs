use std::path::PathBuf;

use tauri::State;

use crate::config::{self, AppLoggingConfig, ConfigState, LoggingConfig};
use crate::session_logger::SessionLogState;

#[tauri::command]
pub async fn logging_get_config(state: State<'_, ConfigState>) -> Result<LoggingConfig, String> {
    let config = state.config.lock().map_err(|e| format!("{e}"))?;
    Ok(config.logging.clone())
}

/// Validate that a filename format template does not contain path traversal.
fn validate_filename_format(fmt: &str) -> Result<(), String> {
    if fmt.contains('/') || fmt.contains('\\') || fmt.contains("..") {
        return Err("Filename format must not contain path separators (/, \\) or '..'".to_string());
    }
    if fmt.is_empty() {
        return Err("Filename format must not be empty".to_string());
    }
    Ok(())
}

/// Validate that a log directory path is absolute.
fn validate_log_directory(dir: &str) -> Result<(), String> {
    let path = PathBuf::from(dir);
    if !path.is_absolute() {
        return Err("Log directory must be an absolute path".to_string());
    }
    Ok(())
}

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
            validate_filename_format(fmt)?;
        }
    }

    let mut config = config_state.config.lock().map_err(|e| format!("{e}"))?;
    config.logging.enabled = enabled;
    config.logging.log_directory = log_directory;
    if let Some(fmt) = filename_format {
        if !fmt.is_empty() {
            config.logging.filename_format = fmt;
        }
    }
    config::save_config(&config_state.config_path, &config)?;

    // Update the live logger state so new connections use the new settings.
    let log_dir = match &config.logging.log_directory {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => config_state
            .config_path
            .parent()
            .map(|p| p.join("logs"))
            .unwrap_or_else(|| PathBuf::from("logs")),
    };
    let mut mgr = logger_state.0.lock().map_err(|e| format!("{e}"))?;
    mgr.enabled = config.logging.enabled;
    mgr.log_dir = log_dir;
    mgr.filename_format = config.logging.filename_format.clone();

    Ok(())
}

#[tauri::command]
pub async fn app_logging_get_config(
    state: State<'_, ConfigState>,
) -> Result<AppLoggingConfig, String> {
    let config = state.config.lock().map_err(|e| format!("{e}"))?;
    Ok(config.app_logging.clone())
}

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
    config.app_logging.enabled = enabled;
    config.app_logging.log_directory = log_directory;
    config.app_logging.level = level;
    config::save_config(&state.config_path, &config)?;
    Ok(())
}
