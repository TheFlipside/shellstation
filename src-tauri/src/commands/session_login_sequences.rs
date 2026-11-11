use sqlx::Row;
use tauri::State;

use crate::config::ConfigState;
use crate::db::PgPoolState;

fn require_pg_pool(pg: &PgPoolState) -> Result<&sqlx::PgPool, String> {
    pg.0.as_ref()
        .ok_or_else(|| "Session login sequences are only available in PostgreSQL mode".to_string())
}

fn require_user_ident(config: &ConfigState) -> Result<String, String> {
    let cfg = config
        .config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {e}"))?;
    cfg.user_ident
        .clone()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            "User identity is not configured. Set it in Settings before using shared sessions."
                .to_string()
        })
}

/// The ids are stored as text, in the form `Uuid` prints. What is bound to a
/// statement is therefore the parsed id printed again, never the text as it
/// came: `Uuid` also reads upper case, braces and the form without hyphens,
/// none of which would match a stored id.
fn parse_uuid(s: &str) -> Result<uuid::Uuid, String> {
    uuid::Uuid::parse_str(s).map_err(|e| format!("Invalid UUID '{s}': {e}"))
}

// A mapping belongs to the database role that wrote it (`owner`), and under
// that role to a `user_ident`; see `session_credentials.rs`. Every statement
// names `owner = current_user`, and row-level security holds a role to its
// own rows whatever statement it sends. PG RLS on the sessions and folders
// tables applies within CTEs and subqueries (policy enforcement is at the
// table-scan level, not query-structure level).
#[tauri::command]
pub async fn set_session_login_sequence(
    pg: State<'_, PgPoolState>,
    config: State<'_, ConfigState>,
    session_id: String,
    login_sequence_id: Option<String>,
) -> Result<(), String> {
    let pool = require_pg_pool(&pg)?;
    let user_ident = require_user_ident(&config)?;
    let session_id = parse_uuid(&session_id)?.to_string();
    let login_sequence_id = login_sequence_id
        .as_deref()
        .map(parse_uuid)
        .transpose()?
        .map(|id| id.to_string());
    set_user_login_sequence(pool, &session_id, &user_ident, login_sequence_id.as_deref()).await
}

/// Set (or clear) the mapping of `session_id` that the current role has
/// under `user_ident`.
pub(crate) async fn set_user_login_sequence(
    pool: &sqlx::PgPool,
    session_id: &str,
    user_ident: &str,
    login_sequence_id: Option<&str>,
) -> Result<(), String> {
    match login_sequence_id {
        Some(seq_id) => {
            // From `sessions`, so that only a session the role can see gets
            // a mapping, as in the folder-wide statement below. The foreign
            // key alone would take the id of any session there is.
            let result = sqlx::query(
                "INSERT INTO session_login_sequences (session_id, owner, user_ident, login_sequence_id) \
                 SELECT s.id, current_user, $2, $3 FROM sessions s WHERE s.id = $1 \
                 ON CONFLICT (session_id, owner, user_ident) \
                 DO UPDATE SET login_sequence_id = EXCLUDED.login_sequence_id",
            )
            .bind(session_id)
            .bind(user_ident)
            .bind(seq_id)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to set session login sequence: {e}"))?;
            if result.rows_affected() == 0 {
                return Err(format!("Session {session_id} not found"));
            }
        }
        None => {
            sqlx::query(
                "DELETE FROM session_login_sequences \
                 WHERE session_id = $1 AND owner = current_user AND user_ident = $2",
            )
            .bind(session_id)
            .bind(user_ident)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to clear session login sequence: {e}"))?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn get_session_login_sequence(
    pg: State<'_, PgPoolState>,
    config: State<'_, ConfigState>,
    session_id: String,
) -> Result<Option<String>, String> {
    let pool = require_pg_pool(&pg)?;
    let user_ident = require_user_ident(&config)?;
    let session_id = parse_uuid(&session_id)?.to_string();

    let row = sqlx::query(
        "SELECT login_sequence_id FROM session_login_sequences \
         WHERE session_id = $1 AND owner = current_user AND user_ident = $2",
    )
    .bind(&session_id)
    .bind(&user_ident)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to get session login sequence: {e}"))?;

    Ok(row.map(|r| r.get::<String, _>("login_sequence_id")))
}

#[tauri::command]
pub async fn bulk_set_session_login_sequences(
    pg: State<'_, PgPoolState>,
    config: State<'_, ConfigState>,
    folder_id: String,
    login_sequence_id: Option<String>,
) -> Result<u32, String> {
    let pool = require_pg_pool(&pg)?;
    let user_ident = require_user_ident(&config)?;
    let folder_id = parse_uuid(&folder_id)?.to_string();
    let login_sequence_id = login_sequence_id
        .as_deref()
        .map(parse_uuid)
        .transpose()?
        .map(|id| id.to_string());
    bulk_set_user_login_sequences(pool, &folder_id, &user_ident, login_sequence_id.as_deref()).await
}

/// Set (or clear) the mapping that the current role has under `user_ident`
/// for every session in `folder_id` and the folders below it, and return
/// how many sessions that was.
pub(crate) async fn bulk_set_user_login_sequences(
    pool: &sqlx::PgPool,
    folder_id: &str,
    user_ident: &str,
    login_sequence_id: Option<&str>,
) -> Result<u32, String> {
    if let Some(seq_id) = login_sequence_id {
        let result = sqlx::query(
            "WITH RECURSIVE subtree(id) AS ( \
                 SELECT id FROM folders WHERE id = $3 \
                 UNION ALL \
                 SELECT f.id FROM folders f JOIN subtree s ON f.parent_id = s.id \
             ) \
             INSERT INTO session_login_sequences (session_id, owner, user_ident, login_sequence_id) \
             SELECT s.id, current_user, $1, $2 FROM sessions s \
             WHERE s.folder_id IN (SELECT id FROM subtree) \
             ON CONFLICT (session_id, owner, user_ident) \
             DO UPDATE SET login_sequence_id = EXCLUDED.login_sequence_id",
        )
        .bind(user_ident)
        .bind(seq_id)
        .bind(folder_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to bulk set session login sequences: {e}"))?;
        Ok(result.rows_affected() as u32)
    } else {
        let result = sqlx::query(
            "WITH RECURSIVE subtree(id) AS ( \
                 SELECT id FROM folders WHERE id = $2 \
                 UNION ALL \
                 SELECT f.id FROM folders f JOIN subtree s ON f.parent_id = s.id \
             ) \
             DELETE FROM session_login_sequences \
             WHERE owner = current_user AND user_ident = $1 AND session_id IN ( \
                 SELECT s.id FROM sessions s \
                 WHERE s.folder_id IN (SELECT id FROM subtree) \
             )",
        )
        .bind(user_ident)
        .bind(folder_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to bulk clear session login sequences: {e}"))?;
        Ok(result.rows_affected() as u32)
    }
}

pub(crate) async fn query_user_login_sequence(
    pool: &sqlx::PgPool,
    session_id: &str,
    user_ident: &str,
) -> Result<Option<uuid::Uuid>, String> {
    let row = sqlx::query(
        "SELECT login_sequence_id FROM session_login_sequences \
         WHERE session_id = $1 AND owner = current_user AND user_ident = $2",
    )
    .bind(session_id)
    .bind(user_ident)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to query session login sequence: {e}"))?;

    match row {
        Some(r) => {
            let id_str: String = r.get("login_sequence_id");
            let uuid = uuid::Uuid::parse_str(&id_str)
                .map_err(|e| format!("Invalid login sequence UUID: {e}"))?;
            Ok(Some(uuid))
        }
        None => Ok(None),
    }
}
