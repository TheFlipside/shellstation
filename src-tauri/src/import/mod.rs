pub mod mremoteng;
pub mod securecrt;

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use uuid::Uuid;

use crate::commands::{MAX_IMPORT_FOLDERS, MAX_IMPORT_SESSIONS};
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

/// Sanitize a jump host reference, or a session name as a reference matches
/// it, with one character more than the name cap. A string still longer than
/// the cap afterwards was over-long in the file: `JumpHostIndex` neither
/// tracks such a reference nor can such a name equal a tracked one. Cutting
/// both to the cap instead would match whatever shares the first 255
/// characters.
fn sanitize_reference(raw: &str) -> String {
    strip_and_cap(raw, MAX_IMPORTED_NAME_LEN + 1)
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

/// Reject an import with more folders or sessions than the JSON database
/// import accepts. The 100 MB file cap alone allows hundreds of thousands of
/// entries, each one a database write.
fn check_import_counts(folders: usize, sessions: usize) -> Result<(), String> {
    if folders > MAX_IMPORT_FOLDERS {
        return Err(format!(
            "Too many folders in import ({folders}, max {MAX_IMPORT_FOLDERS})"
        ));
    }
    if sessions > MAX_IMPORT_SESSIONS {
        return Err(format!(
            "Too many sessions in import ({sessions}, max {MAX_IMPORT_SESSIONS})"
        ));
    }
    Ok(())
}

/// Stop a parse as soon as it holds more entries than an import may take.
/// The 100 MB file cap alone allows millions of tiny entries, and without
/// this a file would be parsed in full before `check_import_counts` saw it.
/// Warnings get the session limit: every session dropped on its own is one,
/// and so is every malformed element, which a crafted file can repeat at
/// will. The dropped sessions kept for jump host resolution get it too, since
/// an element dropped with everything in it takes one warning for all of them.
fn check_parsed_counts(
    folders: usize,
    sessions: usize,
    skipped: usize,
    warnings: usize,
) -> Result<(), String> {
    for (count, max, what) in [
        (folders, MAX_IMPORT_FOLDERS, "folders"),
        (sessions, MAX_IMPORT_SESSIONS, "sessions"),
        (
            warnings,
            MAX_IMPORT_SESSIONS,
            "skipped or unreadable entries",
        ),
        (skipped, MAX_IMPORT_SESSIONS, "skipped sessions"),
    ] {
        if count > max {
            return Err(format!("Too many {what} in import (more than {max})"));
        }
    }
    Ok(())
}

/// The end of a warning about an element dropped with everything in it,
/// saying how many sessions went with it.
fn dropped_sessions_note(sessions: usize) -> String {
    match sessions {
        0 => String::new(),
        1 => " (with 1 session in it)".to_string(),
        n => format!(" (with {n} sessions in it)"),
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

/// Return type shared by both parsers: (folders, sessions, skipped sessions,
/// warnings).
type ParseResult = (
    Vec<ImportedFolder>,
    Vec<ImportedSession>,
    Vec<SkippedSession>,
    Vec<String>,
);

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

/// A session the parser dropped (unsupported protocol, no hostname, or inside
/// an element dropped with everything in it), kept by name and folder only: a
/// jump host reference that also matches it is then reported as ambiguous
/// instead of being linked to the copy that remained.
#[derive(Debug)]
pub struct SkippedSession {
    pub name: String,
    /// References an `ImportedFolder.temp_id`. `None` inside a dropped
    /// element, whose folders are never imported: such a session has no
    /// folder path and matches a reference by its name only.
    pub folder_temp_id: Option<usize>,
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
    let (folders, sessions, skipped, warnings) = mremoteng::parse(&xml)
        .inspect_err(|_| tracing::warn!("mRemoteNG import failed to parse"))
        .map_err(|e| sanitize_message(&e))?;
    persist_import(
        &app,
        &state.0,
        "mRemoteNG Import",
        folders,
        sessions,
        &skipped,
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
    let (folders, sessions, skipped, warnings) = securecrt::parse(&xml)
        .inspect_err(|_| tracing::warn!("SecureCRT import failed to parse"))
        .map_err(|e| sanitize_message(&e))?;
    persist_import(
        &app,
        &state.0,
        "SecureCRT Import",
        folders,
        sessions,
        &skipped,
        warnings,
    )
    .await
}

// ── Shared import-to-DB logic ───────────────────────────────────────

/// The sessions in the file that jump host references point at. A session
/// answers to its bare name (mRemoteNG's `SSHTunnelConnectionName`) and to
/// its folder path ending in that name (`Prod/EU/bastion`), so a reference
/// that carries a path can pick out one of several sessions sharing a name.
/// Sessions that were skipped or failed to create are counted too, so a name
/// the file uses twice stays ambiguous even when only one copy was imported.
///
/// Only references that appear in the file are tracked, each with a match
/// count and the first match, so the index is sized by the references and not
/// by the sessions, of which dropped ones are not covered by the import caps.
struct JumpHostIndex(HashMap<String, JumpHostMatches>);

#[derive(Default)]
struct JumpHostMatches {
    count: usize,
    /// Database id of the first match, `None` if it was not imported.
    first: Option<Uuid>,
}

impl JumpHostIndex {
    /// An index that will count the sessions matching `references`, which
    /// come from `sanitize_reference`. One that sanitized to nothing names
    /// nothing, and one over the name cap names nothing that can be matched
    /// safely.
    fn new(references: impl IntoIterator<Item = String>) -> Self {
        Self(
            references
                .into_iter()
                .filter(|r| !r.is_empty() && r.chars().count() <= MAX_IMPORTED_NAME_LEN)
                .map(|r| (r, JumpHostMatches::default()))
                .collect(),
        )
    }

    /// `path` is the session's folder path joined with its name, or `None`
    /// when the session sits directly under the import root (its name is its
    /// path) or the path is too long for a reference to match. The name and
    /// the path always differ, so a session counts at most once per reference.
    fn insert(&mut self, name: &str, path: Option<&str>, id: Option<Uuid>) {
        for key in [Some(name), path].into_iter().flatten() {
            if let Some(matches) = self.0.get_mut(key) {
                if matches.count == 0 {
                    matches.first = id;
                }
                matches.count += 1;
            }
        }
    }

    /// The session that `reference` on session `session_id` names, or the
    /// warning to report instead. A reference that matches several sessions
    /// is never guessed at: linking the wrong bastion routes the connection
    /// through a host the user did not choose.
    fn resolve(
        &self,
        session_id: Uuid,
        session_name: &str,
        reference: &str,
    ) -> Result<Uuid, String> {
        match self.0.get(reference) {
            Some(JumpHostMatches {
                count: 1,
                first: Some(id),
            }) if *id == session_id => Err(format!(
                "Session \"{session_name}\" names itself as its jump host; left unset"
            )),
            Some(JumpHostMatches {
                count: 1,
                first: Some(id),
            }) => Ok(*id),
            Some(JumpHostMatches {
                count: 1,
                first: None,
            }) => Err(format!(
                "Jump host \"{reference}\" for \"{session_name}\" was not imported; left unset"
            )),
            Some(JumpHostMatches { count, .. }) if *count > 1 => Err(format!(
                "Jump host \"{reference}\" for \"{session_name}\" matches {count} sessions in \
                 the file; left unset"
            )),
            // Matched nothing, or was empty and never tracked.
            _ => Err(format!(
                "Jump host \"{reference}\" for \"{session_name}\" not found among \
                 imported sessions"
            )),
        }
    }
}

/// `parent` and `name` joined into a jump host reference path, or `None` when
/// the result is longer than a reference can be. References are capped like
/// names, so a longer path could never match one, and not building it keeps a
/// deeply nested file from costing a long path string per folder and session.
/// A name that sanitized to nothing has no path.
fn join_reference_path(parent: &str, name: &str) -> Option<String> {
    let len = parent.chars().count() + 1 + name.chars().count();
    (!name.is_empty() && len <= MAX_IMPORTED_NAME_LEN).then(|| format!("{parent}/{name}"))
}

/// The reference path of a session named `name` in folder `folder_temp_id`:
/// `None` directly under the import root, where the name is the path, and
/// in a folder whose path is already too long for a reference.
fn session_reference_path(
    folder_paths: &HashMap<usize, Option<String>>,
    folder_temp_id: usize,
    name: &str,
) -> Option<String> {
    match folder_paths.get(&folder_temp_id) {
        Some(Some(folder_path)) => join_reference_path(folder_path, name),
        _ => None,
    }
}

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
    skipped: &[SkippedSession],
    mut warnings: Vec<String>,
) -> Result<ImportResult, String> {
    // Emit progress at most every N items to keep event traffic reasonable
    // for large imports.
    const PROGRESS_STEP: usize = 25;

    // Checked before anything is written, so an oversized file leaves no
    // partial import behind.
    check_import_counts(folders.len(), sessions.len()).inspect_err(|e| {
        tracing::warn!("{root_name} rejected: {e}");
    })?;

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
        .map_err(|e| {
            // A database error, not file content, so it can be logged whole.
            // Without this line the log would show the start of the import but
            // no outcome.
            tracing::warn!("{root_name} failed to create the root import folder: {e}");
            sanitize_message(&format!("Failed to create root import folder: {e}"))
        })?;
    folders_created += 1;

    // Map temp_id → real DB UUID. temp_id 0 = root folder.
    let mut id_map: HashMap<usize, Uuid> = HashMap::new();
    id_map.insert(0, root_folder.id);
    // temp_id → folder path for jump host references, `None` once it is too
    // long for a reference to match. Paths follow the file even where a folder
    // failed to create and its contents fell back to the import root, since
    // the file's references are written against the file's layout.
    let mut folder_paths: HashMap<usize, Option<String>> = HashMap::new();

    let folder_total = folders.len() as u32;
    emit_progress(app, "folders", 0, folder_total);

    // Create folders in order (parents before children — parsers guarantee this).
    for (i, folder) in folders.iter().enumerate() {
        let parent_uuid = folder
            .parent_temp_id
            .and_then(|tid| id_map.get(&tid).copied())
            .unwrap_or(root_folder.id);

        let safe_name = sanitize_imported_name(&folder.name);
        let path = match folder.parent_temp_id.and_then(|tid| folder_paths.get(&tid)) {
            Some(Some(parent_path)) => join_reference_path(parent_path, &safe_name),
            Some(None) => None,
            // A top-level folder: its parent is the import root.
            None => (!safe_name.is_empty()).then(|| safe_name.clone()),
        };
        folder_paths.insert(folder.temp_id, path);
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

    // Create sessions and index them for jump host resolution, starting with
    // the ones the parser already dropped.
    let mut jump_targets = JumpHostIndex::new(
        sessions
            .iter()
            .filter_map(|s| s.jump_host_name.as_deref())
            .map(sanitize_reference),
    );
    for dropped in skipped {
        let name = sanitize_reference(&dropped.name);
        let path = dropped
            .folder_temp_id
            .and_then(|tid| session_reference_path(&folder_paths, tid, &name));
        jump_targets.insert(&name, path.as_deref(), None);
    }
    // (session id, session name, jump host reference)
    let mut jump_host_pending: Vec<(Uuid, String, String)> = Vec::new();

    let session_total = sessions.len() as u32;
    emit_progress(app, "sessions", 0, session_total);

    for (i, session) in sessions.iter().enumerate() {
        let folder_uuid = id_map
            .get(&session.folder_temp_id)
            .copied()
            .unwrap_or(root_folder.id);

        let safe_name = sanitize_imported_name(&session.name);
        let safe_hostname = sanitize_imported_name(&session.hostname);
        let created_id = match validate_port(session.port) {
            None => {
                warnings.push(format!(
                    "Skipped session \"{}\": invalid port {}",
                    safe_name, session.port
                ));
                None
            }
            // The parsers reject an empty hostname, but one made only of
            // control or formatting characters is empty only from here on.
            Some(_) if safe_hostname.is_empty() => {
                warnings.push(format!(
                    "Skipped session \"{safe_name}\": no hostname once control and \
                     formatting characters are removed"
                ));
                None
            }
            Some(safe_port) => match db
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
                    if let Some(ref jump_name) = session.jump_host_name {
                        jump_host_pending.push((
                            created.id,
                            safe_name.clone(),
                            sanitize_reference(jump_name),
                        ));
                    }
                    sessions_created += 1;
                    Some(created.id)
                }
                Err(e) => {
                    warnings.push(format!("Failed to create session \"{safe_name}\": {e}"));
                    None
                }
            },
        };
        // Matched as a reference sees it, not as stored: see `sanitize_reference`.
        let name = sanitize_reference(&session.name);
        let path = session_reference_path(&folder_paths, session.folder_temp_id, &name);
        jump_targets.insert(&name, path.as_deref(), created_id);
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
    for (session_id, session_name, jump_name) in &jump_host_pending {
        let jump_id = match jump_targets.resolve(*session_id, session_name, jump_name) {
            Ok(id) => id,
            Err(warning) => {
                warnings.push(warning);
                continue;
            }
        };
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
                "Failed to set jump host \"{jump_name}\" on \"{session_name}\": {e}"
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
        assert_eq!(finalize_warnings(Vec::new()), Vec::<String>::new());
    }

    /// An index over `references`, as `persist_import` builds it.
    fn index_for(references: &[&str]) -> JumpHostIndex {
        JumpHostIndex::new(references.iter().map(|r| (*r).to_string()))
    }

    /// Index a session as `persist_import` does, from its folder path
    /// (empty for the import root).
    fn index_session(index: &mut JumpHostIndex, folder: &str, name: &str, id: Option<Uuid>) {
        let path = (!folder.is_empty())
            .then(|| join_reference_path(folder, name))
            .flatten();
        index.insert(name, path.as_deref(), id);
    }

    #[test]
    fn jump_host_resolves_a_unique_name_and_a_folder_path() {
        let (bastion, target) = (Uuid::new_v4(), Uuid::new_v4());
        let mut index = index_for(&["bastion", "Prod/EU/bastion", "EU/bastion"]);
        index_session(&mut index, "Prod/EU", "bastion", Some(bastion));
        index_session(&mut index, "", "target", Some(target));
        assert_eq!(index.resolve(target, "target", "bastion"), Ok(bastion));
        assert_eq!(
            index.resolve(target, "target", "Prod/EU/bastion"),
            Ok(bastion)
        );
        assert_eq!(
            index.resolve(target, "target", "EU/bastion"),
            Err("Jump host \"EU/bastion\" for \"target\" not found among imported sessions".into())
        );
    }

    #[test]
    fn jump_host_shared_name_is_left_unset_unless_a_path_picks_one() {
        // Last-one-wins by name used to link whichever duplicate came last.
        let (eu, us, target) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut index = index_for(&["bastion", "EU/bastion", "US/bastion"]);
        index_session(&mut index, "EU", "bastion", Some(eu));
        index_session(&mut index, "US", "bastion", Some(us));
        assert_eq!(
            index.resolve(target, "target", "bastion"),
            Err(
                "Jump host \"bastion\" for \"target\" matches 2 sessions in the file; left unset"
                    .into()
            )
        );
        assert_eq!(index.resolve(target, "target", "EU/bastion"), Ok(eu));
        assert_eq!(index.resolve(target, "target", "US/bastion"), Ok(us));
    }

    #[test]
    fn jump_host_shared_name_stays_ambiguous_when_one_copy_was_not_imported() {
        // The file is what is ambiguous; a duplicate that was skipped — by
        // the parser or by `persist_import` — must not leave the other copy
        // looking unique.
        let (us, target) = (Uuid::new_v4(), Uuid::new_v4());
        let mut index = index_for(&["bastion", "EU/bastion", "US/bastion"]);
        index_session(&mut index, "EU", "bastion", None);
        index_session(&mut index, "US", "bastion", Some(us));
        assert!(index.resolve(target, "target", "bastion").is_err());
        assert_eq!(
            index.resolve(target, "target", "EU/bastion"),
            Err("Jump host \"EU/bastion\" for \"target\" was not imported; left unset".into())
        );
        assert_eq!(index.resolve(target, "target", "US/bastion"), Ok(us));
    }

    #[test]
    fn jump_host_path_and_slashed_name_collide_as_ambiguous() {
        // A root-level session literally named "EU/bastion" and "bastion" in
        // folder "EU" both answer to "EU/bastion".
        let (slashed, nested, target) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut index = index_for(&["EU/bastion"]);
        index_session(&mut index, "", "EU/bastion", Some(slashed));
        index_session(&mut index, "EU", "bastion", Some(nested));
        assert!(index.resolve(target, "target", "EU/bastion").is_err());
    }

    #[test]
    fn jump_host_naming_its_own_session_is_left_unset() {
        let web = Uuid::new_v4();
        let mut index = index_for(&["web"]);
        index_session(&mut index, "", "web", Some(web));
        assert_eq!(
            index.resolve(web, "web", "web"),
            Err("Session \"web\" names itself as its jump host; left unset".into())
        );
    }

    #[test]
    fn jump_host_empty_name_is_not_a_target() {
        // A name made only of control characters sanitizes to "", and so
        // does a reference made the same way.
        let (blank, target) = (Uuid::new_v4(), Uuid::new_v4());
        let mut index = index_for(&["", "EU/"]);
        index_session(&mut index, "EU", "", Some(blank));
        assert!(index.resolve(target, "target", "").is_err());
        assert!(index.resolve(target, "target", "EU/").is_err());
    }

    #[test]
    fn jump_host_index_keeps_only_what_a_reference_uses() {
        // Skipped sessions are not covered by the import caps; the index
        // must not grow with sessions nothing points at.
        let mut index = index_for(&["bastion"]);
        for i in 0..1000 {
            index_session(&mut index, "Deep/Folder", &format!("rdp-{i}"), None);
        }
        index_session(&mut index, "Deep/Folder", "bastion", Some(Uuid::new_v4()));
        assert_eq!(index.0.len(), 1);
        assert_eq!(index.0["bastion"].count, 1);
    }

    #[test]
    fn jump_host_overlong_name_or_reference_matches_nothing_by_prefix() {
        // Stored names are cut to the cap. Neither an over-long reference nor
        // an over-long name may match the other side by its first 255
        // characters alone.
        let long = "x".repeat(MAX_IMPORTED_NAME_LEN + 10);
        let at_cap = "x".repeat(MAX_IMPORTED_NAME_LEN);
        let (bastion, target) = (Uuid::new_v4(), Uuid::new_v4());

        let mut index = JumpHostIndex::new([sanitize_reference(&long)]);
        index_session(&mut index, "", &sanitize_reference(&at_cap), Some(bastion));
        assert!(index
            .resolve(target, "target", &sanitize_reference(&long))
            .is_err());

        let mut index = JumpHostIndex::new([sanitize_reference(&at_cap)]);
        index_session(&mut index, "", &sanitize_reference(&long), Some(bastion));
        assert!(index
            .resolve(target, "target", &sanitize_reference(&at_cap))
            .is_err());

        // At the cap, the reference and the name are the same string.
        let mut index = JumpHostIndex::new([sanitize_reference(&at_cap)]);
        index_session(&mut index, "", &sanitize_reference(&at_cap), Some(bastion));
        assert_eq!(
            index.resolve(target, "target", &sanitize_reference(&at_cap)),
            Ok(bastion)
        );
    }

    #[test]
    fn check_parsed_counts_stops_past_each_limit() {
        assert!(check_parsed_counts(
            MAX_IMPORT_FOLDERS,
            MAX_IMPORT_SESSIONS,
            MAX_IMPORT_SESSIONS,
            MAX_IMPORT_SESSIONS
        )
        .is_ok());
        assert_eq!(
            check_parsed_counts(MAX_IMPORT_FOLDERS + 1, 0, 0, 0),
            Err(format!(
                "Too many folders in import (more than {MAX_IMPORT_FOLDERS})"
            ))
        );
        assert!(check_parsed_counts(0, MAX_IMPORT_SESSIONS + 1, 0, 0).is_err());
        assert_eq!(
            check_parsed_counts(0, 0, MAX_IMPORT_SESSIONS + 1, 0),
            Err(format!(
                "Too many skipped sessions in import (more than {MAX_IMPORT_SESSIONS})"
            ))
        );
        // A session dropped on its own counts in both, and stops the parse
        // with the message it always did.
        assert_eq!(
            check_parsed_counts(0, 0, MAX_IMPORT_SESSIONS + 1, MAX_IMPORT_SESSIONS + 1),
            Err(format!(
                "Too many skipped or unreadable entries in import (more than {MAX_IMPORT_SESSIONS})"
            ))
        );
    }

    #[test]
    fn join_reference_path_stops_at_the_reference_length_cap() {
        assert_eq!(
            join_reference_path("EU", "bastion").as_deref(),
            Some("EU/bastion")
        );
        // Exactly at the cap in characters, multi-byte ones included.
        let parent = "é".repeat(MAX_IMPORTED_NAME_LEN - 2);
        assert_eq!(
            join_reference_path(&parent, "x").map(|p| p.chars().count()),
            Some(MAX_IMPORTED_NAME_LEN)
        );
        assert_eq!(join_reference_path(&parent, "xy"), None);
    }

    #[test]
    fn check_import_counts_accepts_the_limits_and_rejects_beyond() {
        assert!(check_import_counts(MAX_IMPORT_FOLDERS, MAX_IMPORT_SESSIONS).is_ok());
        assert_eq!(
            check_import_counts(MAX_IMPORT_FOLDERS + 1, 0),
            Err(format!(
                "Too many folders in import ({}, max {MAX_IMPORT_FOLDERS})",
                MAX_IMPORT_FOLDERS + 1
            ))
        );
        assert_eq!(
            check_import_counts(0, MAX_IMPORT_SESSIONS + 1),
            Err(format!(
                "Too many sessions in import ({}, max {MAX_IMPORT_SESSIONS})",
                MAX_IMPORT_SESSIONS + 1
            ))
        );
    }

    #[test]
    fn dropped_sessions_note_counts_only_when_there_were_sessions() {
        assert_eq!(dropped_sessions_note(0), "");
        assert_eq!(dropped_sessions_note(1), " (with 1 session in it)");
        assert_eq!(dropped_sessions_note(3), " (with 3 sessions in it)");
    }

    #[test]
    fn sanitize_message_caps_length() {
        let long = "x".repeat(MAX_MESSAGE_LEN + 1);
        assert_eq!(sanitize_message(&long).chars().count(), MAX_MESSAGE_LEN);
    }
}
