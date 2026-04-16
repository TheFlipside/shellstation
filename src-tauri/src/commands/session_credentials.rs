use sqlx::Row;
use tauri::State;

use crate::config::ConfigState;
use crate::db::PgPoolState;

fn require_pg_pool(pg: &PgPoolState) -> Result<&sqlx::PgPool, String> {
    pg.0.as_ref()
        .ok_or_else(|| "Session credentials are only available in PostgreSQL mode".to_string())
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

fn parse_uuid(s: &str) -> Result<uuid::Uuid, String> {
    uuid::Uuid::parse_str(s).map_err(|e| format!("Invalid UUID '{s}': {e}"))
}

/// Set (or clear) the current user's credential profile for a session.
#[tauri::command]
pub async fn set_session_credential(
    pg: State<'_, PgPoolState>,
    config: State<'_, ConfigState>,
    session_id: String,
    credential_profile_id: Option<String>,
) -> Result<(), String> {
    let pool = require_pg_pool(&pg)?;
    let user_ident = require_user_ident(&config)?;
    let _ = parse_uuid(&session_id)?;

    match credential_profile_id {
        Some(ref profile_id) => {
            let _ = parse_uuid(profile_id)?;
            sqlx::query(
                "INSERT INTO session_credentials (session_id, user_ident, credential_profile_id) \
                 VALUES ($1, $2, $3) \
                 ON CONFLICT (session_id, user_ident) \
                 DO UPDATE SET credential_profile_id = EXCLUDED.credential_profile_id",
            )
            .bind(&session_id)
            .bind(&user_ident)
            .bind(profile_id)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to set session credential: {e}"))?;
        }
        None => {
            sqlx::query(
                "DELETE FROM session_credentials \
                 WHERE session_id = $1 AND user_ident = $2",
            )
            .bind(&session_id)
            .bind(&user_ident)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to clear session credential: {e}"))?;
        }
    }
    Ok(())
}

/// Get the current user's credential profile for a session.
#[tauri::command]
pub async fn get_session_credential(
    pg: State<'_, PgPoolState>,
    config: State<'_, ConfigState>,
    session_id: String,
) -> Result<Option<String>, String> {
    let pool = require_pg_pool(&pg)?;
    let user_ident = require_user_ident(&config)?;
    let _ = parse_uuid(&session_id)?;

    let row = sqlx::query(
        "SELECT credential_profile_id FROM session_credentials \
         WHERE session_id = $1 AND user_ident = $2",
    )
    .bind(&session_id)
    .bind(&user_ident)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to get session credential: {e}"))?;

    Ok(row.map(|r| r.get::<String, _>("credential_profile_id")))
}

/// Bulk-set credential profile for all sessions in a folder subtree.
#[tauri::command]
pub async fn bulk_set_session_credentials(
    pg: State<'_, PgPoolState>,
    config: State<'_, ConfigState>,
    folder_id: String,
    credential_profile_id: Option<String>,
) -> Result<u32, String> {
    let pool = require_pg_pool(&pg)?;
    let user_ident = require_user_ident(&config)?;
    let _ = parse_uuid(&folder_id)?;
    if let Some(ref pid) = credential_profile_id {
        let _ = parse_uuid(pid)?;
    }

    match credential_profile_id {
        Some(profile_id) => {
            let result = sqlx::query(
                "WITH RECURSIVE subtree(id) AS ( \
                     SELECT id FROM folders WHERE id = $3 \
                     UNION ALL \
                     SELECT f.id FROM folders f JOIN subtree s ON f.parent_id = s.id \
                 ) \
                 INSERT INTO session_credentials (session_id, user_ident, credential_profile_id) \
                 SELECT s.id, $1, $2 FROM sessions s \
                 WHERE s.folder_id IN (SELECT id FROM subtree) AND s.protocol != 'telnet' \
                 ON CONFLICT (session_id, user_ident) \
                 DO UPDATE SET credential_profile_id = EXCLUDED.credential_profile_id",
            )
            .bind(&user_ident)
            .bind(&profile_id)
            .bind(&folder_id)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to bulk set session credentials: {e}"))?;
            Ok(result.rows_affected() as u32)
        }
        None => {
            let result = sqlx::query(
                "WITH RECURSIVE subtree(id) AS ( \
                     SELECT id FROM folders WHERE id = $2 \
                     UNION ALL \
                     SELECT f.id FROM folders f JOIN subtree s ON f.parent_id = s.id \
                 ) \
                 DELETE FROM session_credentials \
                 WHERE user_ident = $1 AND session_id IN ( \
                     SELECT s.id FROM sessions s \
                     WHERE s.folder_id IN (SELECT id FROM subtree) AND s.protocol != 'telnet' \
                 )",
            )
            .bind(&user_ident)
            .bind(&folder_id)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to bulk clear session credentials: {e}"))?;
            Ok(result.rows_affected() as u32)
        }
    }
}

/// Query the current user's credential profile ID for a session.
/// Used internally by session_connect for PG-mode credential resolution.
pub async fn query_user_credential(
    pool: &sqlx::PgPool,
    session_id: &str,
    user_ident: &str,
) -> Result<Option<uuid::Uuid>, String> {
    let row = sqlx::query(
        "SELECT credential_profile_id FROM session_credentials \
         WHERE session_id = $1 AND user_ident = $2",
    )
    .bind(session_id)
    .bind(user_ident)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to query session credential: {e}"))?;

    match row {
        Some(r) => {
            let id_str: String = r.get("credential_profile_id");
            let uuid = uuid::Uuid::parse_str(&id_str)
                .map_err(|e| format!("Invalid credential profile UUID: {e}"))?;
            Ok(Some(uuid))
        }
        None => Ok(None),
    }
}
