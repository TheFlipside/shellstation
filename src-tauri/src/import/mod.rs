pub mod mremoteng;
pub mod securecrt;

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use uuid::Uuid;

use crate::db::models::{NewFolder, NewSession};
use crate::db::{DatabaseProvider, DbState};

/// Maximum allowed `XML` file size (100 MB).
const MAX_XML_SIZE: usize = 100 * 1024 * 1024;

/// Maximum length of any imported name (folder or session). Longer values
/// are truncated to protect downstream tools that may have stricter limits.
const MAX_IMPORTED_NAME_LEN: usize = 255;

/// Maximum length of a warning or error message returned to the UI.
const MAX_MESSAGE_LEN: usize = 500;

/// Maximum number of warnings returned to the UI, where each one becomes a
/// toast. Any further warnings are summarized in a single final entry.
const MAX_WARNINGS: usize = 20;

/// Format and filler characters that `char::is_control` misses but that let a
/// name display as something it is not: the bidi embedding, override and
/// isolate controls (U+202A–U+202E, U+2066–U+2069), the line and paragraph
/// separators (U+2028, U+2029), and invisible characters with no place in a
/// host, user or session name — zero-width space and BOM (U+200B, U+FEFF),
/// soft hyphen (U+00AD), word joiner and invisible operators (U+2060–U+2064),
/// interlinear annotation marks (U+FFF9–U+FFFB) and the standalone Hangul
/// fillers (U+3164, U+FFA0). Directional marks (U+200E, U+200F, U+061C) and
/// joiners (U+200C, U+200D) are kept: right-to-left and Indic names and emoji
/// sequences need them, and marks cannot reorder text the way overrides can.
///
/// This is a denylist aimed at reordering and the common blank-name tricks,
/// not at every invisible code point. Rarer invisibles that belong to a script
/// or to emoji sequences — conjoining Hangul fillers (U+115F, U+1160),
/// Mongolian free variation selectors and vowel separator (U+180B–U+180E),
/// the combining grapheme joiner (U+034F), variation selectors (U+FE00–U+FE0F)
/// and emoji tag characters (U+E0000–U+E007F) — are deliberately left alone.
fn is_display_spoofing_char(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{200B}'
            | '\u{FEFF}'
            | '\u{00AD}'
            | '\u{2060}'..='\u{2064}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{3164}'
            | '\u{FFA0}'
    )
}

/// Strip NUL bytes and control characters that could confuse log pipelines
/// or C-string-based downstream consumers, drop characters that would let the
/// text display as something it is not, and cap the length at `max_len`
/// characters.
fn strip_and_cap(raw: &str, max_len: usize) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| *c != '\0' && (!c.is_control() || *c == ' ') && !is_display_spoofing_char(*c))
        .collect();
    if cleaned.chars().count() <= max_len {
        cleaned
    } else {
        cleaned.chars().take(max_len).collect()
    }
}

/// Sanitize an imported folder, session, host, user or jump-host name.
fn sanitize_imported_name(raw: &str) -> String {
    strip_and_cap(raw, MAX_IMPORTED_NAME_LEN)
}

/// Sanitize a warning or error message for display. Parser messages quote
/// raw file content (session and key names, protocol strings, XML fragments),
/// so they get the same character filtering as names.
fn sanitize_message(raw: &str) -> String {
    strip_and_cap(raw, MAX_MESSAGE_LEN)
}

/// Sanitize warnings for display and cap how many are returned, so a file
/// full of bad entries cannot flood the UI with one toast each.
fn finalize_warnings(warnings: Vec<String>) -> Vec<String> {
    let total = warnings.len();
    let mut shown: Vec<String> = warnings
        .iter()
        .take(MAX_WARNINGS)
        .map(|w| sanitize_message(w))
        .collect();
    if total > MAX_WARNINGS {
        shown.push(format!("…and {} more warnings", total - MAX_WARNINGS));
    }
    shown
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
#[derive(Debug)]
pub struct ImportedFolder {
    /// Local ordinal used to reference parent folders before DB IDs exist.
    pub temp_id: usize,
    pub name: String,
    /// `None` only for the root import folder; all others reference a parent.
    pub parent_temp_id: Option<usize>,
}

/// A session extracted from an external session file.
#[derive(Debug)]
pub struct ImportedSession {
    pub name: String,
    /// References an `ImportedFolder.temp_id`.
    pub folder_temp_id: usize,
    pub hostname: String,
    pub port: i32,
    /// `"ssh"` or `"telnet"`.
    pub protocol: String,
    pub username: Option<String>,
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

// The import log is limited to sizes and counts. A parse failure is logged as
// a fixed message: quick-xml errors can quote fragments of the input (tag
// names, an unrecognized entity name inside an attribute value), so the full
// error goes back to the UI only, through `sanitize_message`. Per-session
// problems that name a session go into `warnings`, which the summary line
// counts but never writes out; `finalize_warnings` sanitizes and caps them on
// the way to the UI.

#[tauri::command]
pub async fn import_mremoteng(
    app: AppHandle,
    state: State<'_, DbState>,
    xml: String,
) -> Result<ImportResult, String> {
    tracing::info!("mRemoteNG import started ({} bytes)", xml.len());
    if xml.len() > MAX_XML_SIZE {
        return Err(format!(
            "File too large ({:.1} MB, max {:.0} MB)",
            xml.len() as f64 / 1_048_576.0,
            MAX_XML_SIZE as f64 / 1_048_576.0,
        ));
    }

    emit_progress(&app, "parsing", 0, 0);
    let (folders, sessions, warnings) = mremoteng::parse(&xml)
        .inspect_err(|_| tracing::warn!("mRemoteNG import failed to parse"))
        .map_err(|e| sanitize_message(&e))?;
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
    tracing::info!("SecureCRT import started ({} bytes)", xml.len());
    if xml.len() > MAX_XML_SIZE {
        return Err(format!(
            "File too large ({:.1} MB, max {:.0} MB)",
            xml.len() as f64 / 1_048_576.0,
            MAX_XML_SIZE as f64 / 1_048_576.0,
        ));
    }

    emit_progress(&app, "parsing", 0, 0);
    let (folders, sessions, warnings) = securecrt::parse(&xml)
        .inspect_err(|_| tracing::warn!("SecureCRT import failed to parse"))
        .map_err(|e| sanitize_message(&e))?;
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

        let safe_name = sanitize_imported_name(&folder.name);
        match db
            .create_folder(NewFolder {
                name: safe_name.clone(),
                parent_id: Some(parent_uuid),
            })
            .await
        {
            Ok(created) => {
                id_map.insert(folder.temp_id, created.id);
                folders_created += 1;
            }
            Err(e) => {
                warnings.push(format!("Failed to create folder \"{safe_name}\": {e}"));
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
                username: session
                    .username
                    .as_deref()
                    .map(sanitize_imported_name)
                    .unwrap_or_default(),
                auth_method: "password".to_string(),
                jump_host_id: None,
                tags: String::new(),
                icon: String::new(),
                highlight_profile_id: None,
                credential_profile_id: None,
                login_sequence_id: None,
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

    let skipped = session_total_count.saturating_sub(sessions_created);
    tracing::info!(
        "{root_name} finished: {folders_created} folders, {sessions_created} sessions, \
         {skipped} skipped, {} warnings",
        warnings.len()
    );

    Ok(ImportResult {
        folders_created,
        sessions_created,
        skipped,
        warnings: finalize_warnings(warnings),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_control_characters_but_keeps_spaces() {
        // A resolved `&#x1b;` or `&#0;` must not reach a stored name or
        // username as a raw control character.
        assert_eq!(sanitize_imported_name("ad\u{1b}[31mmin\0 x"), "ad[31mmin x");
        assert_eq!(sanitize_imported_name("a\tb\r\nc"), "abc");
    }

    #[test]
    fn sanitize_strips_reordering_and_invisible_format_characters() {
        assert_eq!(
            sanitize_imported_name("ad\u{202E}min\u{2066}x\u{2069}\u{200B}\u{FEFF}\u{2028}y"),
            "adminxy"
        );
        assert_eq!(
            sanitize_imported_name("p\u{00AD}r\u{2060}o\u{FFF9}d\u{3164}"),
            "prod"
        );
        // Marks and joiners that real right-to-left text and emoji need survive.
        assert_eq!(
            sanitize_imported_name("a\u{200F}b\u{200D}c\u{061C}d"),
            "a\u{200F}b\u{200D}c\u{061C}d"
        );
    }

    #[test]
    fn sanitize_caps_length_in_characters() {
        let long = "é".repeat(MAX_IMPORTED_NAME_LEN + 10);
        let capped = sanitize_imported_name(&long);
        assert_eq!(capped.chars().count(), MAX_IMPORTED_NAME_LEN);
    }

    #[test]
    fn finalize_warnings_sanitizes_and_summarizes_the_overflow() {
        // Parser warnings quote raw file content, and each one is a toast.
        let warnings = (0..MAX_WARNINGS + 5)
            .map(|i| format!("Skipped \"x\u{202E}{i}\u{1b}\""))
            .collect();
        let shown = finalize_warnings(warnings);
        assert_eq!(shown.len(), MAX_WARNINGS + 1);
        assert_eq!(shown[0], "Skipped \"x0\"");
        assert_eq!(shown[MAX_WARNINGS], "…and 5 more warnings");
    }

    #[test]
    fn finalize_warnings_keeps_a_short_list_whole() {
        let shown = finalize_warnings(vec!["a".into(), "b".into()]);
        assert_eq!(shown, ["a", "b"]);
        assert!(finalize_warnings(Vec::new()).is_empty());
    }

    #[test]
    fn sanitize_message_caps_length() {
        let long = "x".repeat(MAX_MESSAGE_LEN + 1);
        assert_eq!(sanitize_message(&long).chars().count(), MAX_MESSAGE_LEN);
    }
}
