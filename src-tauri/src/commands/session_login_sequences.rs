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

fn parse_uuid(s: &str) -> Result<uuid::Uuid, String> {
    uuid::Uuid::parse_str(s).map_err(|e| format!("Invalid UUID '{s}': {e}"))
}

// All operations filter by user_ident (the PG role of the caller), so each user
// can only read/write their own login sequence mappings. PG RLS on the sessions
// and folders tables applies within CTEs and subqueries (policy enforcement is at
// the table-scan level, not query-structure level).
#[tauri::command]
pub async fn set_session_login_sequence(
    pg: State<'_, PgPoolState>,
    config: State<'_, ConfigState>,
    session_id: String,
    login_sequence_id: Option<String>,
) -> Result<(), String> {
    let pool = require_pg_pool(&pg)?;
    let user_ident = require_user_ident(&config)?;
    let _ = parse_uuid(&session_id)?;

    match login_sequence_id {
        Some(ref seq_id) => {
            let _ = parse_uuid(seq_id)?;
            sqlx::query(
                "INSERT INTO session_login_sequences (session_id, user_ident, login_sequence_id) \
                 VALUES ($1, $2, $3) \
                 ON CONFLICT (session_id, user_ident) \
                 DO UPDATE SET login_sequence_id = EXCLUDED.login_sequence_id",
            )
            .bind(&session_id)
            .bind(&user_ident)
            .bind(seq_id)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to set session login sequence: {e}"))?;
        }
        None => {
            sqlx::query(
                "DELETE FROM session_login_sequences \
                 WHERE session_id = $1 AND user_ident = $2",
            )
            .bind(&session_id)
            .bind(&user_ident)
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
    let _ = parse_uuid(&session_id)?;

    let row = sqlx::query(
        "SELECT login_sequence_id FROM session_login_sequences \
         WHERE session_id = $1 AND user_ident = $2",
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
    let _ = parse_uuid(&folder_id)?;
    if let Some(ref sid) = login_sequence_id {
        let _ = parse_uuid(sid)?;
    }

    match login_sequence_id {
        Some(seq_id) => {
            let result = sqlx::query(
                "WITH RECURSIVE subtree(id) AS ( \
                     SELECT id FROM folders WHERE id = $3 \
                     UNION ALL \
                     SELECT f.id FROM folders f JOIN subtree s ON f.parent_id = s.id \
                 ) \
                 INSERT INTO session_login_sequences (session_id, user_ident, login_sequence_id) \
                 SELECT s.id, $1, $2 FROM sessions s \
                 WHERE s.folder_id IN (SELECT id FROM subtree) \
                 ON CONFLICT (session_id, user_ident) \
                 DO UPDATE SET login_sequence_id = EXCLUDED.login_sequence_id",
            )
            .bind(&user_ident)
            .bind(&seq_id)
            .bind(&folder_id)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to bulk set session login sequences: {e}"))?;
            Ok(result.rows_affected() as u32)
        }
        None => {
            let result = sqlx::query(
                "WITH RECURSIVE subtree(id) AS ( \
                     SELECT id FROM folders WHERE id = $2 \
                     UNION ALL \
                     SELECT f.id FROM folders f JOIN subtree s ON f.parent_id = s.id \
                 ) \
                 DELETE FROM session_login_sequences \
                 WHERE user_ident = $1 AND session_id IN ( \
                     SELECT s.id FROM sessions s \
                     WHERE s.folder_id IN (SELECT id FROM subtree) \
                 )",
            )
            .bind(&user_ident)
            .bind(&folder_id)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to bulk clear session login sequences: {e}"))?;
            Ok(result.rows_affected() as u32)
        }
    }
}

pub async fn query_user_login_sequence(
    pool: &sqlx::PgPool,
    session_id: &str,
    user_ident: &str,
) -> Result<Option<uuid::Uuid>, String> {
    let row = sqlx::query(
        "SELECT login_sequence_id FROM session_login_sequences \
         WHERE session_id = $1 AND user_ident = $2",
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
