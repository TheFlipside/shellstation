//! Data export file: any mix of the database, the client settings and the
//! command bar buttons in one versioned JSON document.
//!
//! The database comes from the backend, but settings and command buttons
//! live in the webview's localStorage, so the frontend hands those two over
//! and this command only checks their shape before writing the file. Import
//! runs the other way round: the frontend reads the file, applies settings
//! and buttons itself, and sends the database section to `db_import`.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tauri::{State, Window};
use tauri_plugin_dialog::DialogExt;
use uuid::Uuid;

use super::database::build_export_data;
use crate::db::models::ExportData;
use crate::db::{CredentialDbState, DbState, LoginSequenceDbState};

/// Marks a file as a `ShellStation` export. Files written before the
/// envelope existed are a bare [`ExportData`] object; the import side still
/// reads those as database-only exports.
const EXPORT_FORMAT: &str = "shellstation-export";

/// Bump only when an older app would misread a newer file. Adding another
/// optional section does not need a bump: older apps ignore unknown keys.
const EXPORT_VERSION: u32 = 1;

// Settings are a flat map of scalars (see `SETTING_IMPORTERS` in
// src/stores/settingsStore.ts); these caps sit well above what it holds.
const MAX_SETTINGS_ENTRIES: usize = 64;
const MAX_SETTING_KEY_LEN: usize = 64;
const MAX_SETTING_STRING_LEN: usize = 256;

// Mirror the command button limits in src/stores/settingsStore.ts.
const MAX_COMMAND_BUTTONS: usize = 1000;
const MAX_BUTTON_NAME_LEN: usize = 32;
const MAX_BUTTON_COMMAND_LEN: usize = 1024;

/// File name the save dialog suggests.
const DEFAULT_EXPORT_FILE_NAME: &str = "shellstation-export.json";

/// The webview chooses the save dialog's title, since it is translated.
/// Anything longer than this, or with control characters, gets the default.
const MAX_DIALOG_TITLE_LEN: usize = 128;
const DEFAULT_DIALOG_TITLE: &str = "Export Data";

/// Set while an export is running, from its save dialog to the written
/// file, so a looping caller cannot stack up save dialogs.
static EXPORT_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Holds [`EXPORT_IN_PROGRESS`] and clears it when dropped, whichever way
/// the export ends.
struct ExportGuard;

impl ExportGuard {
    fn acquire() -> Result<Self, String> {
        EXPORT_IN_PROGRESS
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self)
            .map_err(|_| "An export is already in progress".to_string())
    }
}

impl Drop for ExportGuard {
    fn drop(&mut self) {
        EXPORT_IN_PROGRESS.store(false, Ordering::Release);
    }
}

/// One command bar button, as stored by the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportCommandButton {
    pub id: Uuid,
    pub name: String,
    pub command: String,
    pub color: String,
}

/// The file written by [`data_export_file`]. Sections the user did not pick
/// are left out entirely rather than written as `null` or empty, so the
/// import side can tell "not exported" apart from "exported, but empty".
#[derive(Debug, Serialize)]
struct ExportFile {
    format: &'static str,
    version: u32,
    app_version: &'static str,
    exported_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    database: Option<ExportData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    settings: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command_buttons: Option<Vec<ExportCommandButton>>,
}

/// Database counts for the frontend's success message. Both are zero when
/// the database was not part of the export.
#[derive(Debug, Serialize)]
pub struct ExportSummary {
    pub folders: usize,
    pub sessions: usize,
}

fn validate_settings(settings: &Map<String, Value>) -> Result<(), String> {
    if settings.len() > MAX_SETTINGS_ENTRIES {
        return Err(format!(
            "Too many settings to export ({}, max {MAX_SETTINGS_ENTRIES})",
            settings.len()
        ));
    }
    for (key, value) in settings {
        if key.is_empty() || key.chars().count() > MAX_SETTING_KEY_LEN {
            return Err("Invalid setting name in export".to_string());
        }
        let valid = match value {
            Value::Null | Value::Bool(_) | Value::Number(_) => true,
            Value::String(s) => s.chars().count() <= MAX_SETTING_STRING_LEN,
            Value::Array(_) | Value::Object(_) => false,
        };
        if !valid {
            return Err(format!("Invalid value for setting \"{key}\""));
        }
    }
    Ok(())
}

fn is_hex_color(color: &str) -> bool {
    // Slicing at byte 1 is safe: the string starts with the one-byte '#'.
    color.len() == 7 && color.starts_with('#') && color[1..].chars().all(|c| c.is_ascii_hexdigit())
}

fn validate_command_buttons(buttons: &[ExportCommandButton]) -> Result<(), String> {
    if buttons.len() > MAX_COMMAND_BUTTONS {
        return Err(format!(
            "Too many command buttons to export ({}, max {MAX_COMMAND_BUTTONS})",
            buttons.len()
        ));
    }
    let mut ids = HashSet::with_capacity(buttons.len());
    for button in buttons {
        if !ids.insert(button.id) {
            return Err(format!("Duplicate command button {}", button.id));
        }
        let name_len = button.name.chars().count();
        let command_len = button.command.chars().count();
        if name_len == 0
            || name_len > MAX_BUTTON_NAME_LEN
            || command_len == 0
            || command_len > MAX_BUTTON_COMMAND_LEN
            || !is_hex_color(&button.color)
        {
            return Err(format!("Invalid command button {}", button.id));
        }
    }
    Ok(())
}

/// Check what the webview asked to export before anything is written.
fn validate_export_request(
    include_database: bool,
    settings: Option<&Map<String, Value>>,
    command_buttons: Option<&[ExportCommandButton]>,
) -> Result<(), String> {
    if !include_database && settings.is_none() && command_buttons.is_none() {
        return Err("Nothing selected to export".to_string());
    }
    if let Some(settings) = settings {
        validate_settings(settings)?;
    }
    if let Some(buttons) = command_buttons {
        validate_command_buttons(buttons)?;
    }
    Ok(())
}

fn dialog_title(title: String) -> String {
    if title.is_empty()
        || title.chars().count() > MAX_DIALOG_TITLE_LEN
        || title.chars().any(char::is_control)
    {
        DEFAULT_DIALOG_TITLE.to_string()
    } else {
        title
    }
}

/// Ask the user where to save the export; `None` means they cancelled.
///
/// The dialog is opened here rather than in the webview so that the file
/// written is always one the user picked: a compromised webview cannot name
/// its own target and overwrite an arbitrary file.
async fn pick_export_path(window: &Window, title: String) -> Result<Option<PathBuf>, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .file()
        .set_parent(window)
        .set_title(dialog_title(title))
        .set_file_name(DEFAULT_EXPORT_FILE_NAME)
        .add_filter("JSON", &["json"])
        .save_file(move |path| {
            // The receiver only goes away if this command was dropped.
            let _ = tx.send(path);
        });
    let Some(path) = rx
        .await
        .map_err(|_| "Save dialog closed unexpectedly".to_string())?
    else {
        return Ok(None);
    };
    path.into_path()
        .map(Some)
        .map_err(|e| format!("Invalid export path: {e}"))
}

/// Write `contents` to `path` through a temporary file in the same
/// directory. An existing file is replaced in one step, so a failed export
/// never leaves it half-written, and a symlink at `path` is replaced rather
/// than followed.
fn write_atomically(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let (Some(dir), Some(_)) = (path.parent(), path.file_name()) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "export path has no file name",
        ));
    };
    // A fixed short name, so a target name near the file system's length
    // limit does not make the temporary one too long.
    let tmp = dir.join(format!(".shellstation-export-{}.tmp", Uuid::new_v4()));
    let result = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut file| {
            file.write_all(contents)?;
            file.sync_all()
        })
        .and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn build_export_file(
    database: Option<ExportData>,
    settings: Option<Map<String, Value>>,
    command_buttons: Option<Vec<ExportCommandButton>>,
) -> ExportFile {
    ExportFile {
        format: EXPORT_FORMAT,
        version: EXPORT_VERSION,
        app_version: env!("CARGO_PKG_VERSION"),
        exported_at: chrono::Utc::now().to_rfc3339(),
        database,
        settings,
        command_buttons,
    }
}

/// Ask where to save, then write an export file containing the sections
/// the user picked. Returns `None` when the save dialog was cancelled.
///
/// `settings` and `command_buttons` are `None` when the user left that
/// section out; `include_database` does the same for the database, which
/// the backend reads itself. `title` is the save dialog's (translated) title.
#[tauri::command]
// The window and the three states are injected by Tauri; the webview sends
// the other four.
#[allow(clippy::too_many_arguments)]
pub async fn data_export_file(
    window: Window,
    state: State<'_, DbState>,
    cred_db: State<'_, CredentialDbState>,
    login_seq_db: State<'_, LoginSequenceDbState>,
    title: String,
    include_database: bool,
    settings: Option<Map<String, Value>>,
    command_buttons: Option<Vec<ExportCommandButton>>,
) -> Result<Option<ExportSummary>, String> {
    validate_export_request(
        include_database,
        settings.as_ref(),
        command_buttons.as_deref(),
    )?;
    let _guard = ExportGuard::acquire()?;
    let Some(path) = pick_export_path(&window, title).await? else {
        return Ok(None);
    };

    let database = if include_database {
        Some(build_export_data(&state, &cred_db, &login_seq_db).await?)
    } else {
        None
    };
    let summary = ExportSummary {
        folders: database.as_ref().map_or(0, |d| d.folders.len()),
        sessions: database.as_ref().map_or(0, |d| d.sessions.len()),
    };

    let file = build_export_file(database, settings, command_buttons);
    let json = serde_json::to_string_pretty(&file)
        .map_err(|e| format!("Failed to serialize export data: {e}"))?;
    tokio::task::spawn_blocking(move || write_atomically(&path, json.as_bytes()))
        .await
        .map_err(|e| format!("Export task failed: {e}"))?
        .map_err(|e| format!("Failed to write export file: {e}"))?;
    Ok(Some(summary))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn button(name: &str, command: &str, color: &str) -> ExportCommandButton {
        ExportCommandButton {
            id: Uuid::new_v4(),
            name: name.to_string(),
            command: command.to_string(),
            color: color.to_string(),
        }
    }

    fn settings(value: &Value) -> Map<String, Value> {
        value
            .as_object()
            .expect("test settings must be an object")
            .clone()
    }

    #[test]
    fn scalar_settings_are_valid() {
        let s = settings(&json!({
            "themeMode": "dark",
            "uiScale": 110,
            "copyOnSelect": true,
            "terminalBackgroundColor": null,
        }));
        assert!(validate_settings(&s).is_ok());
    }

    #[test]
    fn nested_setting_values_are_rejected() {
        for value in [json!({ "a": [1] }), json!({ "a": { "b": 1 } })] {
            let err = validate_settings(&settings(&value)).unwrap_err();
            assert!(err.contains("Invalid value for setting \"a\""), "{err}");
        }
    }

    #[test]
    fn overlong_setting_string_is_rejected() {
        let s = settings(&json!({ "language": "x".repeat(MAX_SETTING_STRING_LEN + 1) }));
        assert!(validate_settings(&s).is_err());
        let s = settings(&json!({ "language": "x".repeat(MAX_SETTING_STRING_LEN) }));
        assert!(validate_settings(&s).is_ok());
    }

    #[test]
    fn bad_setting_keys_are_rejected() {
        assert!(validate_settings(&settings(&json!({ "": 1 }))).is_err());
        let long_key = "k".repeat(MAX_SETTING_KEY_LEN + 1);
        let mut s = Map::new();
        s.insert(long_key, json!(1));
        assert!(validate_settings(&s).is_err());
    }

    #[test]
    fn too_many_settings_are_rejected() {
        let s: Map<String, Value> = (0..=MAX_SETTINGS_ENTRIES)
            .map(|i| (format!("k{i}"), json!(i)))
            .collect();
        let err = validate_settings(&s).unwrap_err();
        assert!(err.contains("Too many settings"), "{err}");
    }

    #[test]
    fn valid_command_buttons_pass() {
        let buttons = vec![
            button("Uptime", "uptime\\r", "#89b4fa"),
            button(&"n".repeat(MAX_BUTTON_NAME_LEN), "x", "#ABCDEF"),
        ];
        assert!(validate_command_buttons(&buttons).is_ok());
    }

    #[test]
    fn invalid_command_buttons_are_rejected() {
        let cases = [
            button("", "uptime", "#89b4fa"),
            button(&"n".repeat(MAX_BUTTON_NAME_LEN + 1), "uptime", "#89b4fa"),
            button("Uptime", "", "#89b4fa"),
            button("Uptime", &"c".repeat(MAX_BUTTON_COMMAND_LEN + 1), "#89b4fa"),
            button("Uptime", "uptime", "89b4fa"),
            button("Uptime", "uptime", "#89b4fg"),
            button("Uptime", "uptime", "#fff"),
            button("Uptime", "uptime", "red"),
        ];
        for b in cases {
            assert!(
                validate_command_buttons(std::slice::from_ref(&b)).is_err(),
                "{b:?} should be rejected"
            );
        }
    }

    #[test]
    fn too_many_command_buttons_are_rejected() {
        let buttons = vec![button("b", "c", "#000000"); MAX_COMMAND_BUTTONS + 1];
        let err = validate_command_buttons(&buttons).unwrap_err();
        assert!(err.contains("Too many command buttons"), "{err}");
    }

    #[test]
    fn export_file_omits_sections_that_were_not_picked() {
        let file = build_export_file(None, None, Some(vec![button("b", "c", "#000000")]));
        let value = serde_json::to_value(&file).unwrap();
        let obj = value.as_object().unwrap();
        assert_eq!(obj["format"], EXPORT_FORMAT);
        assert_eq!(obj["version"], EXPORT_VERSION);
        assert_eq!(obj["app_version"], env!("CARGO_PKG_VERSION"));
        assert!(obj["exported_at"].is_string());
        assert!(obj.contains_key("command_buttons"));
        assert!(!obj.contains_key("database"));
        assert!(!obj.contains_key("settings"));
    }

    #[test]
    fn export_file_keeps_empty_sections_that_were_picked() {
        let file = build_export_file(Some(ExportData::default()), Some(Map::new()), Some(vec![]));
        let value = serde_json::to_value(&file).unwrap();
        assert_eq!(value["database"]["folders"], json!([]));
        assert_eq!(value["database"]["sessions"], json!([]));
        assert_eq!(value["settings"], json!({}));
        assert_eq!(value["command_buttons"], json!([]));
    }

    #[test]
    fn duplicate_command_button_ids_are_rejected() {
        let a = button("a", "uptime", "#89b4fa");
        let b = ExportCommandButton {
            name: "b".to_string(),
            ..a.clone()
        };
        let err = validate_command_buttons(&[a, b]).unwrap_err();
        assert!(err.starts_with("Duplicate command button"), "{err}");
    }

    #[test]
    fn empty_selection_is_rejected() {
        let err = validate_export_request(false, None, None).unwrap_err();
        assert_eq!(err, "Nothing selected to export");
        assert!(validate_export_request(true, None, None).is_ok());
        assert!(validate_export_request(false, Some(&Map::new()), None).is_ok());
        assert!(validate_export_request(false, None, Some(&[])).is_ok());
    }

    #[test]
    fn export_request_checks_each_picked_section() {
        let bad_settings = settings(&json!({ "a": [1] }));
        assert!(validate_export_request(true, Some(&bad_settings), None).is_err());
        let bad_buttons = [button("", "uptime", "#89b4fa")];
        assert!(validate_export_request(true, None, Some(&bad_buttons)).is_err());
    }

    #[test]
    fn database_section_reads_back_for_db_import() {
        let file = build_export_file(Some(ExportData::default()), None, None);
        let value = serde_json::to_value(&file).unwrap();
        let back: ExportData = serde_json::from_value(value["database"].clone()).unwrap();
        assert!(back.folders.is_empty());
        assert!(back.sessions.is_empty());
    }

    #[test]
    fn atomic_write_replaces_existing_file_and_cleans_up() {
        let dir = std::env::temp_dir().join(format!("ss-export-{}", Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("export.json");
        std::fs::write(&path, "old contents that are longer than the new ones").unwrap();

        write_atomically(&path, b"{}").unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
        let entries = std::fs::read_dir(&dir).unwrap().count();
        assert_eq!(entries, 1, "temporary file left behind");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dialog_title_falls_back_on_unusable_input() {
        assert_eq!(
            dialog_title("Daten exportieren".into()),
            "Daten exportieren"
        );
        assert_eq!(dialog_title(String::new()), DEFAULT_DIALOG_TITLE);
        let long = "t".repeat(MAX_DIALOG_TITLE_LEN + 1);
        assert_eq!(dialog_title(long), DEFAULT_DIALOG_TITLE);
        assert_eq!(dialog_title("Export\nData".into()), DEFAULT_DIALOG_TITLE);
    }

    #[test]
    fn only_one_export_runs_at_a_time() {
        let guard = ExportGuard::acquire().unwrap();
        assert!(ExportGuard::acquire().is_err());
        drop(guard);
        assert!(ExportGuard::acquire().is_ok());
    }

    #[test]
    fn atomic_write_fails_in_a_missing_directory() {
        let dir = std::env::temp_dir().join(format!("ss-missing-{}", Uuid::new_v4()));
        assert!(write_atomically(&dir.join("export.json"), b"{}").is_err());
        assert!(!dir.exists());
    }
}
