pub mod mremoteng;
pub mod securecrt;

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use tauri::State;
use uuid::Uuid;

use crate::db::models::{NewFolder, NewSession};
use crate::db::{DatabaseProvider, DbState};

/// Maximum allowed XML file size (100 MB).
const MAX_XML_SIZE: usize = 100 * 1024 * 1024;

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

// ── Tauri commands ──────────────────────────────────────────────────

#[tauri::command]
pub async fn import_mremoteng(
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

    let (folders, sessions, warnings) = mremoteng::parse(&xml)?;
    persist_import(&state.0, "mRemoteNG Import", folders, sessions, warnings).await
}

#[tauri::command]
pub async fn import_securecrt(
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

    let (folders, sessions, warnings) = securecrt::parse(&xml)?;
    persist_import(&state.0, "SecureCRT Import", folders, sessions, warnings).await
}

// ── Shared import-to-DB logic ───────────────────────────────────────

/// Persist parsed folders and sessions into the database.
///
/// Creates a root folder named `root_name` to contain all imported data,
/// then creates folders and sessions in order. Jump host references are
/// resolved in a second pass.
async fn persist_import(
    db: &Arc<dyn DatabaseProvider>,
    root_name: &str,
    folders: Vec<ImportedFolder>,
    sessions: Vec<ImportedSession>,
    mut warnings: Vec<String>,
) -> Result<ImportResult, String> {
    let mut folders_created = 0u32;
    let mut sessions_created = 0u32;
    let skipped = warnings.len() as u32;

    // Create the root import folder at the top level (no parent).
    let root_folder = db
        .create_folder(NewFolder {
            name: root_name.to_string(),
            parent_id: None,
        })
        .await
        .map_err(|e| format!("Failed to create root import folder: {e}"))?;
    folders_created += 1;

    // Map temp_id → real DB UUID. temp_id 0 = root folder.
    let mut id_map: HashMap<usize, Uuid> = HashMap::new();
    id_map.insert(0, root_folder.id);

    // Create folders in order (parents before children — parsers guarantee this).
    for folder in &folders {
        let parent_uuid = folder
            .parent_temp_id
            .and_then(|tid| id_map.get(&tid).copied())
            .unwrap_or(root_folder.id);

        match db
            .create_folder(NewFolder {
                name: folder.name.clone(),
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
    }

    // Create sessions and track (name → session_id) for jump host resolution.
    let mut session_name_map: HashMap<String, Uuid> = HashMap::new();
    let mut jump_host_pending: Vec<(Uuid, String)> = Vec::new();

    for session in &sessions {
        let folder_uuid = id_map
            .get(&session.folder_temp_id)
            .copied()
            .unwrap_or(root_folder.id);

        match db
            .create_session(NewSession {
                folder_id: folder_uuid,
                name: session.name.clone(),
                hostname: session.hostname.clone(),
                port: session.port,
                protocol: session.protocol.clone(),
                auth_method: "password".to_string(),
                jump_host_id: None,
                tags: String::new(),
                icon: String::new(),
            })
            .await
        {
            Ok(created) => {
                session_name_map.insert(session.name.clone(), created.id);
                if let Some(ref jump_name) = session.jump_host_name {
                    jump_host_pending.push((created.id, jump_name.clone()));
                }
                sessions_created += 1;
            }
            Err(e) => {
                warnings.push(format!(
                    "Failed to create session \"{}\": {e}",
                    session.name
                ));
            }
        }
    }

    // Second pass: resolve jump host references by name.
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

    Ok(ImportResult {
        folders_created,
        sessions_created,
        skipped,
        warnings,
    })
}
