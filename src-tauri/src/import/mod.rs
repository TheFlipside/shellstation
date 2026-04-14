pub mod mremoteng;
pub mod securecrt;

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use uuid::Uuid;

use crate::db::models::{NewFolder, NewSession};
use crate::db::{DatabaseProvider, DbState};

/// Maximum allowed XML file size (100 MB).
const MAX_XML_SIZE: usize = 100 * 1024 * 1024;

/// Maximum length of any imported name (folder or session). Longer values
/// are truncated to protect downstream tools that may have stricter limits.
const MAX_IMPORTED_NAME_LEN: usize = 255;

/// Strip NUL bytes and control characters that could confuse log pipelines
/// or C-string-based downstream consumers, and cap the length.
fn sanitize_imported_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| *c != '\0' && (!c.is_control() || *c == ' '))
        .collect();
    if cleaned.chars().count() <= MAX_IMPORTED_NAME_LEN {
        cleaned
    } else {
        cleaned.chars().take(MAX_IMPORTED_NAME_LEN).collect()
    }
}

/// Clamp an imported port to the valid TCP range. Returns `None` if the
/// value cannot be represented as a valid port, signaling the caller to
/// skip the session.
fn validate_port(port: i32) -> Option<i32> {
    if (1..=65535).contains(&port) {
        Some(port)
    } else {
        None
    }
}

/// Return type shared by both parsers: (folders, sessions, warnings).
type ParseResult = (Vec<ImportedFolder>, Vec<ImportedSession>, Vec<String>);

// ── Common types ────────────────────────────────────────────────────

/// A folder extracted from an external session file.
pub struct ImportedFolder {
    /// Local ordinal used to reference parent folders before DB IDs exist.
    pub temp_id: usize,
    pub name: String,
    /// `None` only for the root import folder; all others reference a parent.
    pub parent_temp_id: Option<usize>,
}

/// A session extracted from an external session file.
pub struct ImportedSession {
    pub name: String,
    /// References an `ImportedFolder.temp_id`.
    pub folder_temp_id: usize,
    pub hostname: String,
    pub port: i32,
    /// `"ssh"` or `"telnet"`.
    pub protocol: String,
    /// Parsed but not yet used — credentials are configured per-session in the UI.
    #[allow(dead_code)]
    pub username: String,
    /// Jump host reference by name (resolved post-import).
    pub jump_host_name: Option<String>,
}

/// Result returned to the frontend after an import operation.
#[derive(Debug, Serialize)]
pub struct ImportResult {
    pub folders_created: u32,
    pub sessions_created: u32,
    pub skipped: u32,
    pub warnings: Vec<String>,
}

/// Tauri event name for streaming import progress to the frontend.
const IMPORT_PROGRESS_EVENT: &str = "import:progress";

#[derive(Debug, Clone, Serialize)]
struct ImportProgress {
    phase: &'static str,
    current: u32,
    total: u32,
}

fn emit_progress(app: &AppHandle, phase: &'static str, current: u32, total: u32) {
    let _ = app.emit(
        IMPORT_PROGRESS_EVENT,
        ImportProgress {
            phase,
            current,
            total,
        },
    );
}

// ── Tauri commands ──────────────────────────────────────────────────

#[tauri::command]
pub async fn import_mremoteng(
    app: AppHandle,
    state: State<'_, DbState>,
    xml: String,
) -> Result<ImportResult, String> {
    if xml.len() > MAX_XML_SIZE {
        return Err(format!(
            "File too large ({:.1} MB, max {:.0} MB)",
            xml.len() as f64 / 1_048_576.0,
            MAX_XML_SIZE as f64 / 1_048_576.0,
        ));
    }

    emit_progress(&app, "parsing", 0, 0);
    let (folders, sessions, warnings) = mremoteng::parse(&xml)?;
    persist_import(
        &app,
        &state.0,
        "mRemoteNG Import",
        folders,
        sessions,
        warnings,
    )
    .await
}

#[tauri::command]
pub async fn import_securecrt(
    app: AppHandle,
    state: State<'_, DbState>,
    xml: String,
) -> Result<ImportResult, String> {
    if xml.len() > MAX_XML_SIZE {
        return Err(format!(
            "File too large ({:.1} MB, max {:.0} MB)",
            xml.len() as f64 / 1_048_576.0,
            MAX_XML_SIZE as f64 / 1_048_576.0,
        ));
    }

    emit_progress(&app, "parsing", 0, 0);
    let (folders, sessions, warnings) = securecrt::parse(&xml)?;
    persist_import(
        &app,
        &state.0,
        "SecureCRT Import",
        folders,
        sessions,
        warnings,
    )
    .await
}

// ── Shared import-to-DB logic ───────────────────────────────────────

/// Persist parsed folders and sessions into the database.
///
/// Creates a root folder named `root_name` to contain all imported data,
/// then creates folders and sessions in order. Jump host references are
/// resolved in a second pass.
async fn persist_import(
    app: &AppHandle,
    db: &Arc<dyn DatabaseProvider>,
    root_name: &str,
    folders: Vec<ImportedFolder>,
    sessions: Vec<ImportedSession>,
    mut warnings: Vec<String>,
) -> Result<ImportResult, String> {
    // Emit progress at most every N items to keep event traffic reasonable
    // for large imports.
    const PROGRESS_STEP: usize = 25;
    let mut folders_created = 0u32;
    let mut sessions_created = 0u32;
    let session_total_count = sessions.len() as u32;

    // Create the root import folder at the top level (no parent).
    let root_folder = db
        .create_folder(NewFolder {
            name: sanitize_imported_name(root_name),
            parent_id: None,
        })
        .await
        .map_err(|e| format!("Failed to create root import folder: {e}"))?;
    folders_created += 1;

    // Map temp_id → real DB UUID. temp_id 0 = root folder.
    let mut id_map: HashMap<usize, Uuid> = HashMap::new();
    id_map.insert(0, root_folder.id);

    let folder_total = folders.len() as u32;
    emit_progress(app, "folders", 0, folder_total);

    // Create folders in order (parents before children — parsers guarantee this).
    for (i, folder) in folders.iter().enumerate() {
        let parent_uuid = folder
            .parent_temp_id
            .and_then(|tid| id_map.get(&tid).copied())
            .unwrap_or(root_folder.id);

        match db
            .create_folder(NewFolder {
                name: sanitize_imported_name(&folder.name),
                parent_id: Some(parent_uuid),
            })
            .await
        {
            Ok(created) => {
                id_map.insert(folder.temp_id, created.id);
                folders_created += 1;
            }
            Err(e) => {
                warnings.push(format!("Failed to create folder \"{}\": {e}", folder.name));
            }
        }
        if (i + 1) % PROGRESS_STEP == 0 {
            emit_progress(app, "folders", (i + 1) as u32, folder_total);
        }
    }
    emit_progress(app, "folders", folder_total, folder_total);

    // Create sessions and track (name → session_id) for jump host resolution.
    let mut session_name_map: HashMap<String, Uuid> = HashMap::new();
    let mut jump_host_pending: Vec<(Uuid, String)> = Vec::new();

    let session_total = sessions.len() as u32;
    emit_progress(app, "sessions", 0, session_total);

    for (i, session) in sessions.iter().enumerate() {
        let folder_uuid = id_map
            .get(&session.folder_temp_id)
            .copied()
            .unwrap_or(root_folder.id);

        let safe_name = sanitize_imported_name(&session.name);
        let safe_hostname = sanitize_imported_name(&session.hostname);
        let Some(safe_port) = validate_port(session.port) else {
            warnings.push(format!(
                "Skipped session \"{}\": invalid port {}",
                safe_name, session.port
            ));
            if (i + 1) % PROGRESS_STEP == 0 {
                emit_progress(app, "sessions", (i + 1) as u32, session_total);
            }
            continue;
        };

        match db
            .create_session(NewSession {
                folder_id: folder_uuid,
                name: safe_name.clone(),
                hostname: safe_hostname,
                port: safe_port,
                protocol: session.protocol.clone(),
                auth_method: "password".to_string(),
                jump_host_id: None,
                tags: String::new(),
                icon: String::new(),
                highlight_profile_id: None,
                credential_profile_id: None,
                legacy_algorithms: false,
            })
            .await
        {
            Ok(created) => {
                session_name_map.insert(safe_name.clone(), created.id);
                if let Some(ref jump_name) = session.jump_host_name {
                    jump_host_pending.push((created.id, sanitize_imported_name(jump_name)));
                }
                sessions_created += 1;
            }
            Err(e) => {
                warnings.push(format!("Failed to create session \"{safe_name}\": {e}"));
            }
        }
        if (i + 1) % PROGRESS_STEP == 0 {
            emit_progress(app, "sessions", (i + 1) as u32, session_total);
        }
    }
    emit_progress(app, "sessions", session_total, session_total);

    // Second pass: resolve jump host references by name.
    let jump_total = jump_host_pending.len() as u32;
    if jump_total > 0 {
        emit_progress(app, "jump_hosts", 0, jump_total);
    }
    for (session_id, jump_name) in &jump_host_pending {
        if let Some(&jump_id) = session_name_map.get(jump_name) {
            if let Err(e) = db
                .update_session(
                    *session_id,
                    crate::db::models::UpdateSession {
                        jump_host_id: Some(Some(jump_id)),
                        ..Default::default()
                    },
                )
                .await
            {
                warnings.push(format!(
                    "Failed to set jump host \"{jump_name}\" on session: {e}"
                ));
            }
        } else {
            warnings.push(format!(
                "Jump host \"{jump_name}\" not found among imported sessions"
            ));
        }
    }

    emit_progress(app, "done", 1, 1);

    Ok(ImportResult {
        folders_created,
        sessions_created,
        skipped: session_total_count.saturating_sub(sessions_created),
        warnings,
    })
}
