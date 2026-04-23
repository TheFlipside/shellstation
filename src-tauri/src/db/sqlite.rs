use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::models::{
    Credential, CredentialProfile, DataFingerprint, Folder, HighlightProfile, LoginSequence,
    LoginSequenceStep, NewCredentialProfile, NewFolder, NewHighlightProfile, NewLoginSequence,
    NewSession, Session, UpdateCredentialProfile, UpdateHighlightProfile, UpdateLoginSequence,
    UpdateSession,
};
use super::{BulkSessionEdit, DatabaseProvider, DbResult};

pub struct SqliteProvider {
    pool: SqlitePool,
}

impl SqliteProvider {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

fn parse_uuid(s: &str) -> DbResult<Uuid> {
    Uuid::parse_str(s).map_err(|e| format!("Invalid UUID '{s}': {e}"))
}

fn parse_optional_uuid(s: Option<&str>) -> DbResult<Option<Uuid>> {
    match s {
        Some(v) => Ok(Some(parse_uuid(v)?)),
        None => Ok(None),
    }
}

fn row_to_folder(row: &SqliteRow) -> DbResult<Folder> {
    Ok(Folder {
        id: parse_uuid(row.get("id"))?,
        name: row.get("name"),
        parent_id: parse_optional_uuid(row.get("parent_id"))?,
        sort_order: row.get("sort_order"),
        owner: row.get("owner"),
        visibility: row.get("visibility"),
    })
}

fn row_to_session(row: &SqliteRow) -> DbResult<Session> {
    Ok(Session {
        id: parse_uuid(row.get("id"))?,
        folder_id: parse_uuid(row.get("folder_id"))?,
        name: row.get("name"),
        hostname: row.get("hostname"),
        port: row.get("port"),
        protocol: row.get("protocol"),
        username: row.get("username"),
        auth_method: row.get("auth_method"),
        jump_host_id: parse_optional_uuid(row.get("jump_host_id"))?,
        tags: row.get("tags"),
        icon: row.get("icon"),
        sort_order: row.get("sort_order"),
        highlight_profile_id: parse_optional_uuid(row.get("highlight_profile_id"))?,
        credential_profile_id: parse_optional_uuid(row.get("credential_profile_id"))?,
        legacy_algorithms: row.get::<i64, _>("legacy_algorithms") != 0,
        login_sequence_id: parse_optional_uuid(row.get("login_sequence_id"))?,
        owner: row.get("owner"),
        visibility: row.get("visibility"),
    })
}

fn row_to_credential_profile(row: &SqliteRow) -> DbResult<CredentialProfile> {
    Ok(CredentialProfile {
        id: parse_uuid(row.get("id"))?,
        name: row.get("name"),
        auth_type: row.get("auth_type"),
        username: row.get("username"),
        keychain_ref: row.get("keychain_ref"),
        key_path: row.get("key_path"),
        sort_order: row.get("sort_order"),
    })
}

fn row_to_highlight_profile(row: &SqliteRow) -> DbResult<HighlightProfile> {
    Ok(HighlightProfile {
        id: parse_uuid(row.get("id"))?,
        name: row.get("name"),
        rules: row.get("rules"),
        sort_order: row.get("sort_order"),
    })
}

fn row_to_login_sequence(row: &SqliteRow) -> DbResult<LoginSequence> {
    let steps_json: String = row.get("steps");
    let steps: Vec<LoginSequenceStep> = serde_json::from_str(&steps_json)
        .map_err(|e| format!("Failed to parse login sequence steps: {e}"))?;
    Ok(LoginSequence {
        id: parse_uuid(row.get("id"))?,
        name: row.get("name"),
        send_initial_cr: row.get::<i64, _>("send_initial_cr") != 0,
        steps,
        sort_order: row.get("sort_order"),
    })
}

fn row_to_credential(row: &SqliteRow) -> DbResult<Credential> {
    Ok(Credential {
        id: parse_uuid(row.get("id"))?,
        session_id: parse_uuid(row.get("session_id"))?,
        username: row.get("username"),
        auth_type: row.get("auth_type"),
        keychain_ref: row.get("keychain_ref"),
    })
}

#[async_trait]
impl DatabaseProvider for SqliteProvider {
    // ── Folders ──────────────────────────────────────────────────────────

    async fn create_folder(&self, folder: NewFolder) -> DbResult<Folder> {
        let id = Uuid::new_v4();
        let id_str = id.to_string();
        let parent_str = folder.parent_id.map(|u| u.to_string());

        // Place new folder at the end of its sibling list.
        let sort_order: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM folders WHERE parent_id IS ?",
        )
        .bind(&parent_str)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| format!("Failed to compute sort_order: {e}"))?;

        sqlx::query("INSERT INTO folders (id, name, parent_id, sort_order) VALUES (?, ?, ?, ?)")
            .bind(&id_str)
            .bind(&folder.name)
            .bind(&parent_str)
            .bind(sort_order)
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to create folder: {e}"))?;

        Ok(Folder {
            id,
            name: folder.name,
            parent_id: folder.parent_id,
            sort_order,
            owner: "local".to_string(),
            visibility: "personal".to_string(),
        })
    }

    async fn list_folders(&self) -> DbResult<Vec<Folder>> {
        let rows = sqlx::query(
            "SELECT id, name, parent_id, sort_order, owner, visibility \
             FROM folders ORDER BY sort_order ASC, name ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("Failed to list folders: {e}"))?;

        rows.iter().map(row_to_folder).collect()
    }

    async fn rename_folder(&self, id: Uuid, name: &str) -> DbResult<()> {
        let result = sqlx::query("UPDATE folders SET name = ? WHERE id = ?")
            .bind(name)
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to rename folder: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Folder {id} not found"));
        }
        Ok(())
    }

    async fn move_folder(&self, id: Uuid, new_parent_id: Option<Uuid>) -> DbResult<()> {
        let parent_str = new_parent_id.map(|u| u.to_string());

        // Append at end of new parent's sibling list.
        let sort_order: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM folders WHERE parent_id IS ?",
        )
        .bind(&parent_str)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| format!("Failed to compute sort_order: {e}"))?;

        let result = sqlx::query("UPDATE folders SET parent_id = ?, sort_order = ? WHERE id = ?")
            .bind(&parent_str)
            .bind(sort_order)
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to move folder: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Folder {id} not found"));
        }
        Ok(())
    }

    async fn delete_folder(&self, id: Uuid) -> DbResult<()> {
        let result = sqlx::query("DELETE FROM folders WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to delete folder: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Folder {id} not found"));
        }
        Ok(())
    }

    // ── Sessions ─────────────────────────────────────────────────────────

    async fn create_session(&self, session: NewSession) -> DbResult<Session> {
        let id = Uuid::new_v4();
        let id_str = id.to_string();
        let folder_str = session.folder_id.to_string();
        let jump_str = session.jump_host_id.map(|u| u.to_string());

        // Place new session at the end of its folder.
        let sort_order: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM sessions WHERE folder_id = ?",
        )
        .bind(&folder_str)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| format!("Failed to compute sort_order: {e}"))?;

        sqlx::query(
            "INSERT INTO sessions (id, folder_id, name, hostname, port, protocol, username, auth_method, jump_host_id, tags, icon, sort_order, highlight_profile_id, credential_profile_id, legacy_algorithms, login_sequence_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id_str)
        .bind(&folder_str)
        .bind(&session.name)
        .bind(&session.hostname)
        .bind(session.port)
        .bind(&session.protocol)
        .bind(&session.username)
        .bind(&session.auth_method)
        .bind(&jump_str)
        .bind(&session.tags)
        .bind(&session.icon)
        .bind(sort_order)
        .bind(session.highlight_profile_id.map(|u| u.to_string()))
        .bind(session.credential_profile_id.map(|u| u.to_string()))
        .bind(i64::from(session.legacy_algorithms))
        .bind(session.login_sequence_id.map(|u| u.to_string()))
        .execute(&self.pool)
        .await
        .map_err(|e| format!("Failed to create session: {e}"))?;

        Ok(Session {
            id,
            folder_id: session.folder_id,
            name: session.name,
            hostname: session.hostname,
            port: session.port,
            protocol: session.protocol,
            username: session.username,
            auth_method: session.auth_method,
            jump_host_id: session.jump_host_id,
            tags: session.tags,
            icon: session.icon,
            sort_order,
            highlight_profile_id: session.highlight_profile_id,
            credential_profile_id: session.credential_profile_id,
            legacy_algorithms: session.legacy_algorithms,
            login_sequence_id: session.login_sequence_id,
            owner: "local".to_string(),
            visibility: "personal".to_string(),
        })
    }

    async fn get_session(&self, id: Uuid) -> DbResult<Option<Session>> {
        let row = sqlx::query(
            "SELECT id, folder_id, name, hostname, port, protocol, username, auth_method, jump_host_id, tags, icon, sort_order, highlight_profile_id, credential_profile_id, legacy_algorithms, owner, visibility, login_sequence_id \
             FROM sessions WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| format!("Failed to get session: {e}"))?;

        match row {
            Some(r) => Ok(Some(row_to_session(&r)?)),
            None => Ok(None),
        }
    }

    async fn list_all_sessions(&self) -> DbResult<Vec<Session>> {
        let rows = sqlx::query(
            "SELECT id, folder_id, name, hostname, port, protocol, username, auth_method, jump_host_id, tags, icon, sort_order, highlight_profile_id, credential_profile_id, legacy_algorithms, owner, visibility, login_sequence_id \
             FROM sessions ORDER BY sort_order ASC, name ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("Failed to list sessions: {e}"))?;

        rows.iter().map(row_to_session).collect()
    }

    async fn update_session(&self, id: Uuid, update: UpdateSession) -> DbResult<()> {
        let mut sets = Vec::new();

        // Use typed bind values so port binds as integer and
        // jump_host_id = None binds as SQL NULL (not empty string).
        enum BindVal {
            Text(String),
            Int(i32),
            Null,
        }
        let mut values: Vec<BindVal> = Vec::new();

        if let Some(ref name) = update.name {
            sets.push("name = ?");
            values.push(BindVal::Text(name.clone()));
        }
        if let Some(ref hostname) = update.hostname {
            sets.push("hostname = ?");
            values.push(BindVal::Text(hostname.clone()));
        }
        if let Some(port) = update.port {
            sets.push("port = ?");
            values.push(BindVal::Int(port));
        }
        if let Some(ref protocol) = update.protocol {
            sets.push("protocol = ?");
            values.push(BindVal::Text(protocol.clone()));
        }
        if let Some(ref username) = update.username {
            sets.push("username = ?");
            values.push(BindVal::Text(username.clone()));
        }
        if let Some(ref auth_method) = update.auth_method {
            sets.push("auth_method = ?");
            values.push(BindVal::Text(auth_method.clone()));
        }
        if let Some(ref jump_host_id) = update.jump_host_id {
            sets.push("jump_host_id = ?");
            match jump_host_id {
                Some(u) => values.push(BindVal::Text(u.to_string())),
                None => values.push(BindVal::Null),
            }
        }
        if let Some(ref tags) = update.tags {
            sets.push("tags = ?");
            values.push(BindVal::Text(tags.clone()));
        }
        if let Some(ref icon) = update.icon {
            sets.push("icon = ?");
            values.push(BindVal::Text(icon.clone()));
        }
        if let Some(ref highlight_profile_id) = update.highlight_profile_id {
            sets.push("highlight_profile_id = ?");
            match highlight_profile_id {
                Some(u) => values.push(BindVal::Text(u.to_string())),
                None => values.push(BindVal::Null),
            }
        }
        if let Some(ref credential_profile_id) = update.credential_profile_id {
            sets.push("credential_profile_id = ?");
            match credential_profile_id {
                Some(u) => values.push(BindVal::Text(u.to_string())),
                None => values.push(BindVal::Null),
            }
        }
        if let Some(legacy) = update.legacy_algorithms {
            sets.push("legacy_algorithms = ?");
            values.push(BindVal::Int(i32::from(legacy)));
        }
        if let Some(ref login_sequence_id) = update.login_sequence_id {
            sets.push("login_sequence_id = ?");
            match login_sequence_id {
                Some(u) => values.push(BindVal::Text(u.to_string())),
                None => values.push(BindVal::Null),
            }
        }

        if sets.is_empty() {
            return Ok(());
        }

        let sql = format!("UPDATE sessions SET {} WHERE id = ?", sets.join(", "));
        let mut query = sqlx::query(&sql);
        for val in &values {
            match val {
                BindVal::Text(s) => query = query.bind(s.as_str()),
                BindVal::Int(n) => query = query.bind(*n),
                BindVal::Null => query = query.bind(None::<String>),
            }
        }
        query = query.bind(id.to_string());

        let result = query
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to update session: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Session {id} not found"));
        }
        Ok(())
    }

    async fn move_session(&self, id: Uuid, new_folder_id: Uuid) -> DbResult<()> {
        let folder_str = new_folder_id.to_string();

        // Append at end of new folder's session list.
        let sort_order: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM sessions WHERE folder_id = ?",
        )
        .bind(&folder_str)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| format!("Failed to compute sort_order: {e}"))?;

        let result = sqlx::query("UPDATE sessions SET folder_id = ?, sort_order = ? WHERE id = ?")
            .bind(&folder_str)
            .bind(sort_order)
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to move session: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Session {id} not found"));
        }
        Ok(())
    }

    async fn delete_session(&self, id: Uuid) -> DbResult<()> {
        let result = sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to delete session: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Session {id} not found"));
        }
        Ok(())
    }

    // SQLite LIKE is case-insensitive for ASCII; PostgreSQL uses ILIKE.
    async fn search_sessions(&self, query: &str) -> DbResult<Vec<Session>> {
        let escaped = query.replace('%', "\\%").replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        let rows = sqlx::query(
            "SELECT id, folder_id, name, hostname, port, protocol, username, auth_method, jump_host_id, tags, icon, sort_order, highlight_profile_id, credential_profile_id, legacy_algorithms, owner, visibility, login_sequence_id \
             FROM sessions \
             WHERE name LIKE ? ESCAPE '\\' \
                OR hostname LIKE ? ESCAPE '\\' \
                OR username LIKE ? ESCAPE '\\' \
                OR tags LIKE ? ESCAPE '\\' \
             ORDER BY sort_order ASC, name ASC",
        )
        .bind(&pattern)
        .bind(&pattern)
        .bind(&pattern)
        .bind(&pattern)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("Failed to search sessions: {e}"))?;

        rows.iter().map(row_to_session).collect()
    }

    // ── Reordering ─────────────────────────────────────────────────────

    async fn reorder_folders(
        &self,
        parent_id: Option<Uuid>,
        ordered_ids: Vec<Uuid>,
    ) -> DbResult<()> {
        let parent_str = parent_id.map(|u| u.to_string());
        for (i, id) in ordered_ids.iter().enumerate() {
            sqlx::query("UPDATE folders SET sort_order = ? WHERE id = ? AND parent_id IS ?")
                .bind(i as i32)
                .bind(id.to_string())
                .bind(&parent_str)
                .execute(&self.pool)
                .await
                .map_err(|e| format!("Failed to reorder folders: {e}"))?;
        }
        Ok(())
    }

    async fn reorder_sessions(&self, folder_id: Uuid, ordered_ids: Vec<Uuid>) -> DbResult<()> {
        let folder_str = folder_id.to_string();
        for (i, id) in ordered_ids.iter().enumerate() {
            sqlx::query("UPDATE sessions SET sort_order = ? WHERE id = ? AND folder_id = ?")
                .bind(i as i32)
                .bind(id.to_string())
                .bind(&folder_str)
                .execute(&self.pool)
                .await
                .map_err(|e| format!("Failed to reorder sessions: {e}"))?;
        }
        Ok(())
    }

    async fn sort_folders_alphabetically(&self, parent_id: Option<Uuid>) -> DbResult<()> {
        let parent_str = parent_id.map(|u| u.to_string());
        let rows = sqlx::query("SELECT id FROM folders WHERE parent_id IS ? ORDER BY name ASC")
            .bind(&parent_str)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| format!("Failed to sort folders: {e}"))?;

        for (i, row) in rows.iter().enumerate() {
            let id: String = row.get("id");
            sqlx::query("UPDATE folders SET sort_order = ? WHERE id = ?")
                .bind(i as i32)
                .bind(&id)
                .execute(&self.pool)
                .await
                .map_err(|e| format!("Failed to sort folders: {e}"))?;
        }
        Ok(())
    }

    async fn sort_sessions_alphabetically(&self, folder_id: Uuid) -> DbResult<()> {
        let folder_str = folder_id.to_string();
        let rows = sqlx::query("SELECT id FROM sessions WHERE folder_id = ? ORDER BY name ASC")
            .bind(&folder_str)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| format!("Failed to sort sessions: {e}"))?;

        for (i, row) in rows.iter().enumerate() {
            let id: String = row.get("id");
            sqlx::query("UPDATE sessions SET sort_order = ? WHERE id = ?")
                .bind(i as i32)
                .bind(&id)
                .execute(&self.pool)
                .await
                .map_err(|e| format!("Failed to sort sessions: {e}"))?;
        }
        Ok(())
    }

    // ── Credentials ──────────────────────────────────────────────────────

    async fn upsert_credential(&self, cred: Credential) -> DbResult<()> {
        sqlx::query(
            "INSERT INTO credentials (id, session_id, username, auth_type, keychain_ref) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(session_id) DO UPDATE SET \
               username = excluded.username, \
               auth_type = excluded.auth_type, \
               keychain_ref = excluded.keychain_ref",
        )
        .bind(cred.id.to_string())
        .bind(cred.session_id.to_string())
        .bind(&cred.username)
        .bind(&cred.auth_type)
        .bind(&cred.keychain_ref)
        .execute(&self.pool)
        .await
        .map_err(|e| format!("Failed to upsert credential: {e}"))?;

        Ok(())
    }

    async fn get_credential(&self, session_id: Uuid) -> DbResult<Option<Credential>> {
        let row = sqlx::query(
            "SELECT id, session_id, username, auth_type, keychain_ref FROM credentials WHERE session_id = ?",
        )
        .bind(session_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| format!("Failed to get credential: {e}"))?;

        match row {
            Some(r) => Ok(Some(row_to_credential(&r)?)),
            None => Ok(None),
        }
    }

    async fn delete_credential(&self, session_id: Uuid) -> DbResult<()> {
        sqlx::query("DELETE FROM credentials WHERE session_id = ?")
            .bind(session_id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to delete credential: {e}"))?;

        Ok(())
    }

    async fn list_all_credentials(&self) -> DbResult<Vec<Credential>> {
        let rows = sqlx::query(
            "SELECT id, session_id, username, auth_type, keychain_ref FROM credentials ORDER BY session_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("Failed to list credentials: {e}"))?;

        rows.iter().map(row_to_credential).collect()
    }

    // ── Credential Profiles ──────────────────────────────────────────────

    async fn create_credential_profile(
        &self,
        profile: NewCredentialProfile,
    ) -> DbResult<CredentialProfile> {
        let id = Uuid::new_v4();
        let id_str = id.to_string();
        let keychain_ref = format!("credprofile-{id}");

        let sort_order: i32 =
            sqlx::query_scalar("SELECT COALESCE(MAX(sort_order), -1) + 1 FROM credential_profiles")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| format!("Failed to compute sort_order: {e}"))?;

        sqlx::query(
            "INSERT INTO credential_profiles (id, name, auth_type, username, keychain_ref, key_path, sort_order) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id_str)
        .bind(&profile.name)
        .bind(&profile.auth_type)
        .bind(&profile.username)
        .bind(&keychain_ref)
        .bind(&profile.key_path)
        .bind(sort_order)
        .execute(&self.pool)
        .await
        .map_err(|e| format!("Failed to create credential profile: {e}"))?;

        Ok(CredentialProfile {
            id,
            name: profile.name,
            auth_type: profile.auth_type,
            username: profile.username,
            keychain_ref,
            key_path: profile.key_path,
            sort_order,
        })
    }

    async fn list_credential_profiles(&self) -> DbResult<Vec<CredentialProfile>> {
        let rows = sqlx::query(
            "SELECT id, name, auth_type, username, keychain_ref, key_path, sort_order \
             FROM credential_profiles ORDER BY sort_order ASC, name ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("Failed to list credential profiles: {e}"))?;

        rows.iter().map(row_to_credential_profile).collect()
    }

    async fn get_credential_profile(&self, id: Uuid) -> DbResult<Option<CredentialProfile>> {
        let row = sqlx::query(
            "SELECT id, name, auth_type, username, keychain_ref, key_path, sort_order \
             FROM credential_profiles WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| format!("Failed to get credential profile: {e}"))?;

        match row {
            Some(r) => Ok(Some(row_to_credential_profile(&r)?)),
            None => Ok(None),
        }
    }

    async fn update_credential_profile(
        &self,
        id: Uuid,
        update: UpdateCredentialProfile,
    ) -> DbResult<()> {
        let mut sets = Vec::new();
        let mut values: Vec<String> = Vec::new();

        if let Some(ref name) = update.name {
            sets.push("name = ?");
            values.push(name.clone());
        }
        if let Some(ref auth_type) = update.auth_type {
            sets.push("auth_type = ?");
            values.push(auth_type.clone());
        }
        if let Some(ref username) = update.username {
            sets.push("username = ?");
            values.push(username.clone());
        }
        if let Some(ref key_path) = update.key_path {
            sets.push("key_path = ?");
            values.push(key_path.clone());
        }

        if sets.is_empty() {
            return Ok(());
        }

        let sql = format!(
            "UPDATE credential_profiles SET {} WHERE id = ?",
            sets.join(", ")
        );
        let mut query = sqlx::query(&sql);
        for val in &values {
            query = query.bind(val.as_str());
        }
        query = query.bind(id.to_string());

        let result = query
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to update credential profile: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Credential profile {id} not found"));
        }
        Ok(())
    }

    async fn delete_credential_profile(&self, id: Uuid) -> DbResult<()> {
        let result = sqlx::query("DELETE FROM credential_profiles WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to delete credential profile: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Credential profile {id} not found"));
        }
        Ok(())
    }

    async fn bulk_set_session_credential_profile(
        &self,
        folder_id: Uuid,
        profile_id: Option<Uuid>,
    ) -> DbResult<u32> {
        // Collect folder_id and all descendant folder IDs via BFS.
        let mut folder_ids: Vec<String> = vec![folder_id.to_string()];
        let mut frontier: Vec<String> = vec![folder_id.to_string()];
        while !frontier.is_empty() {
            let placeholders = vec!["?"; frontier.len()].join(",");
            let sql = format!("SELECT id FROM folders WHERE parent_id IN ({placeholders})");
            let mut q = sqlx::query(&sql);
            for f in &frontier {
                q = q.bind(f);
            }
            let rows = q
                .fetch_all(&self.pool)
                .await
                .map_err(|e| format!("Failed to walk folder tree: {e}"))?;
            frontier = rows.iter().map(|r| r.get::<String, _>("id")).collect();
            folder_ids.extend(frontier.clone());
        }

        let placeholders = vec!["?"; folder_ids.len()].join(",");
        let sql = format!(
            "UPDATE sessions SET credential_profile_id = ? \
             WHERE folder_id IN ({placeholders}) AND protocol != 'telnet'"
        );
        let mut q = sqlx::query(&sql);
        q = q.bind(profile_id.map(|u| u.to_string()));
        for f in &folder_ids {
            q = q.bind(f);
        }
        let result = q
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to bulk set credential profile: {e}"))?;

        Ok(result.rows_affected() as u32)
    }

    async fn bulk_edit_sessions(&self, folder_id: Uuid, edit: BulkSessionEdit) -> DbResult<u32> {
        if edit.jump_host_id.is_none()
            && edit.highlight_profile_id.is_none()
            && edit.icon.is_none()
            && edit.login_sequence_id.is_none()
        {
            return Ok(0);
        }

        // Collect folder_id and all descendant folder IDs via BFS.
        let mut folder_ids: Vec<String> = vec![folder_id.to_string()];
        let mut frontier: Vec<String> = vec![folder_id.to_string()];
        while !frontier.is_empty() {
            let placeholders = vec!["?"; frontier.len()].join(",");
            let sql = format!("SELECT id FROM folders WHERE parent_id IN ({placeholders})");
            let mut q = sqlx::query(&sql);
            for f in &frontier {
                q = q.bind(f);
            }
            let rows = q
                .fetch_all(&self.pool)
                .await
                .map_err(|e| format!("Failed to walk folder tree: {e}"))?;
            frontier = rows.iter().map(|r| r.get::<String, _>("id")).collect();
            folder_ids.extend(frontier.clone());
        }

        // jump_host_id applies only to non-telnet sessions. If only
        // jump_host_id is being set, skip telnet rows; otherwise run two
        // UPDATEs so icon/highlight still reach telnet sessions.
        let setters_all: Vec<&str> = {
            let mut v: Vec<&str> = Vec::new();
            if edit.highlight_profile_id.is_some() {
                v.push("highlight_profile_id = ?");
            }
            if edit.icon.is_some() {
                v.push("icon = ?");
            }
            if edit.login_sequence_id.is_some() {
                v.push("login_sequence_id = ?");
            }
            v
        };

        let placeholders = vec!["?"; folder_ids.len()].join(",");
        let mut total: u64 = 0;

        // First pass: apply highlight_profile_id and/or icon to all rows
        // (including telnet) in the subtree.
        if !setters_all.is_empty() {
            let sql = format!(
                "UPDATE sessions SET {} WHERE folder_id IN ({placeholders})",
                setters_all.join(", "),
            );
            let mut q = sqlx::query(&sql);
            if let Some(hp) = &edit.highlight_profile_id {
                q = q.bind(hp.map(|u| u.to_string()));
            }
            if let Some(icon) = &edit.icon {
                q = q.bind(icon);
            }
            if let Some(ls) = &edit.login_sequence_id {
                q = q.bind(ls.map(|u| u.to_string()));
            }
            for f in &folder_ids {
                q = q.bind(f);
            }
            let result = q
                .execute(&self.pool)
                .await
                .map_err(|e| format!("Failed to bulk edit sessions: {e}"))?;
            total = result.rows_affected();
        }

        // Second pass: jump_host_id — SSH only.
        if let Some(jh) = &edit.jump_host_id {
            let sql = format!(
                "UPDATE sessions SET jump_host_id = ? \
                 WHERE folder_id IN ({placeholders}) AND protocol != 'telnet'",
            );
            let mut q = sqlx::query(&sql);
            q = q.bind(jh.map(|u| u.to_string()));
            for f in &folder_ids {
                q = q.bind(f);
            }
            let result = q
                .execute(&self.pool)
                .await
                .map_err(|e| format!("Failed to bulk set jump host: {e}"))?;
            // We report *distinct* sessions touched. When the first pass
            // ran, it already counted the SSH rows this pass is updating
            // (both passes overlap on the SSH subset), so we only adopt
            // this count when the first pass was skipped.
            if setters_all.is_empty() {
                total = result.rows_affected();
            }
        }

        Ok(total as u32)
    }

    // ── Highlight Profiles ────────────────────────────────────────────────

    async fn create_highlight_profile(
        &self,
        profile: NewHighlightProfile,
    ) -> DbResult<HighlightProfile> {
        let id = Uuid::new_v4();
        let id_str = id.to_string();

        let sort_order: i32 =
            sqlx::query_scalar("SELECT COALESCE(MAX(sort_order), -1) + 1 FROM highlight_profiles")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| format!("Failed to compute sort_order: {e}"))?;

        sqlx::query(
            "INSERT INTO highlight_profiles (id, name, rules, sort_order) VALUES (?, ?, ?, ?)",
        )
        .bind(&id_str)
        .bind(&profile.name)
        .bind(&profile.rules)
        .bind(sort_order)
        .execute(&self.pool)
        .await
        .map_err(|e| format!("Failed to create highlight profile: {e}"))?;

        Ok(HighlightProfile {
            id,
            name: profile.name,
            rules: profile.rules,
            sort_order,
        })
    }

    async fn list_highlight_profiles(&self) -> DbResult<Vec<HighlightProfile>> {
        let rows = sqlx::query(
            "SELECT id, name, rules, sort_order FROM highlight_profiles ORDER BY sort_order ASC, name ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("Failed to list highlight profiles: {e}"))?;

        rows.iter().map(row_to_highlight_profile).collect()
    }

    async fn get_highlight_profile(&self, id: Uuid) -> DbResult<Option<HighlightProfile>> {
        let row =
            sqlx::query("SELECT id, name, rules, sort_order FROM highlight_profiles WHERE id = ?")
                .bind(id.to_string())
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| format!("Failed to get highlight profile: {e}"))?;

        match row {
            Some(r) => Ok(Some(row_to_highlight_profile(&r)?)),
            None => Ok(None),
        }
    }

    async fn update_highlight_profile(
        &self,
        id: Uuid,
        update: UpdateHighlightProfile,
    ) -> DbResult<()> {
        let mut sets = Vec::new();
        let mut values: Vec<String> = Vec::new();

        if let Some(ref name) = update.name {
            sets.push("name = ?");
            values.push(name.clone());
        }
        if let Some(ref rules) = update.rules {
            sets.push("rules = ?");
            values.push(rules.clone());
        }

        if sets.is_empty() {
            return Ok(());
        }

        let sql = format!(
            "UPDATE highlight_profiles SET {} WHERE id = ?",
            sets.join(", ")
        );
        let mut query = sqlx::query(&sql);
        for val in &values {
            query = query.bind(val.as_str());
        }
        query = query.bind(id.to_string());

        let result = query
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to update highlight profile: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Highlight profile {id} not found"));
        }
        Ok(())
    }

    async fn delete_highlight_profile(&self, id: Uuid) -> DbResult<()> {
        let result = sqlx::query("DELETE FROM highlight_profiles WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to delete highlight profile: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Highlight profile {id} not found"));
        }
        Ok(())
    }

    // ── Login Sequences ──────────────────────────────────────────────────

    async fn create_login_sequence(&self, sequence: NewLoginSequence) -> DbResult<LoginSequence> {
        let id = Uuid::new_v4();
        let id_str = id.to_string();

        let sort_order: i32 =
            sqlx::query_scalar("SELECT COALESCE(MAX(sort_order), -1) + 1 FROM login_sequences")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| format!("Failed to compute sort_order: {e}"))?;

        let steps_json = serde_json::to_string(&sequence.steps)
            .map_err(|e| format!("Failed to serialize steps: {e}"))?;

        sqlx::query(
            "INSERT INTO login_sequences (id, name, send_initial_cr, steps, sort_order) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&id_str)
        .bind(&sequence.name)
        .bind(i64::from(sequence.send_initial_cr))
        .bind(&steps_json)
        .bind(sort_order)
        .execute(&self.pool)
        .await
        .map_err(|e| format!("Failed to create login sequence: {e}"))?;

        Ok(LoginSequence {
            id,
            name: sequence.name,
            send_initial_cr: sequence.send_initial_cr,
            steps: sequence.steps,
            sort_order,
        })
    }

    async fn list_login_sequences(&self) -> DbResult<Vec<LoginSequence>> {
        let rows = sqlx::query(
            "SELECT id, name, send_initial_cr, steps, sort_order \
             FROM login_sequences ORDER BY sort_order ASC, name ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("Failed to list login sequences: {e}"))?;

        rows.iter().map(row_to_login_sequence).collect()
    }

    async fn get_login_sequence(&self, id: Uuid) -> DbResult<Option<LoginSequence>> {
        let row = sqlx::query(
            "SELECT id, name, send_initial_cr, steps, sort_order \
             FROM login_sequences WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| format!("Failed to get login sequence: {e}"))?;

        match row {
            Some(r) => Ok(Some(row_to_login_sequence(&r)?)),
            None => Ok(None),
        }
    }

    async fn update_login_sequence(&self, id: Uuid, update: UpdateLoginSequence) -> DbResult<()> {
        let mut sets = Vec::new();

        enum BindVal {
            Text(String),
            Int(i64),
        }
        let mut values: Vec<BindVal> = Vec::new();

        if let Some(ref name) = update.name {
            sets.push("name = ?");
            values.push(BindVal::Text(name.clone()));
        }
        if let Some(send_initial_cr) = update.send_initial_cr {
            sets.push("send_initial_cr = ?");
            values.push(BindVal::Int(i64::from(send_initial_cr)));
        }
        if let Some(ref steps) = update.steps {
            let json = serde_json::to_string(steps)
                .map_err(|e| format!("Failed to serialize steps: {e}"))?;
            sets.push("steps = ?");
            values.push(BindVal::Text(json));
        }

        if sets.is_empty() {
            return Ok(());
        }

        let sql = format!(
            "UPDATE login_sequences SET {} WHERE id = ?",
            sets.join(", ")
        );
        let mut query = sqlx::query(&sql);
        for val in &values {
            match val {
                BindVal::Text(s) => query = query.bind(s.as_str()),
                BindVal::Int(n) => query = query.bind(*n),
            }
        }
        query = query.bind(id.to_string());

        let result = query
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to update login sequence: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Login sequence {id} not found"));
        }
        Ok(())
    }

    async fn delete_login_sequence(&self, id: Uuid) -> DbResult<()> {
        let result = sqlx::query("DELETE FROM login_sequences WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to delete login sequence: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Login sequence {id} not found"));
        }
        Ok(())
    }

    async fn bulk_set_session_login_sequence(
        &self,
        folder_id: Uuid,
        sequence_id: Option<Uuid>,
    ) -> DbResult<u32> {
        let mut folder_ids: Vec<String> = vec![folder_id.to_string()];
        let mut frontier: Vec<String> = vec![folder_id.to_string()];
        while !frontier.is_empty() {
            let placeholders = vec!["?"; frontier.len()].join(",");
            let sql = format!("SELECT id FROM folders WHERE parent_id IN ({placeholders})");
            let mut q = sqlx::query(&sql);
            for f in &frontier {
                q = q.bind(f);
            }
            let rows = q
                .fetch_all(&self.pool)
                .await
                .map_err(|e| format!("Failed to walk folder tree: {e}"))?;
            frontier = rows.iter().map(|r| r.get::<String, _>("id")).collect();
            folder_ids.extend(frontier.clone());
        }

        let placeholders = vec!["?"; folder_ids.len()].join(",");
        let sql = format!(
            "UPDATE sessions SET login_sequence_id = ? \
             WHERE folder_id IN ({placeholders})"
        );
        let mut q = sqlx::query(&sql);
        q = q.bind(sequence_id.map(|u| u.to_string()));
        for f in &folder_ids {
            q = q.bind(f);
        }
        let result = q
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to bulk set login sequence: {e}"))?;

        Ok(result.rows_affected() as u32)
    }

    async fn data_fingerprint(&self) -> DbResult<DataFingerprint> {
        // Fetch only id + name from both tables — minimal data transfer.
        let folder_rows =
            sqlx::query("SELECT id, name, sort_order, visibility FROM folders ORDER BY id")
                .fetch_all(&self.pool)
                .await
                .map_err(|e| format!("Failed to fingerprint folders: {e}"))?;
        let session_rows = sqlx::query(
            "SELECT id, name, hostname, port, folder_id, sort_order, visibility \
             FROM sessions ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("Failed to fingerprint sessions: {e}"))?;

        let mut hasher = DefaultHasher::new();
        folder_rows.len().hash(&mut hasher);
        for row in &folder_rows {
            let id: String = row.get("id");
            let name: String = row.get("name");
            let sort_order: i32 = row.get("sort_order");
            let visibility: String = row.get("visibility");
            id.hash(&mut hasher);
            name.hash(&mut hasher);
            sort_order.hash(&mut hasher);
            visibility.hash(&mut hasher);
        }
        session_rows.len().hash(&mut hasher);
        for row in &session_rows {
            let id: String = row.get("id");
            let name: String = row.get("name");
            let hostname: String = row.get("hostname");
            let port: i32 = row.get("port");
            let folder_id: String = row.get("folder_id");
            let sort_order: i32 = row.get("sort_order");
            let visibility: String = row.get("visibility");
            id.hash(&mut hasher);
            name.hash(&mut hasher);
            hostname.hash(&mut hasher);
            port.hash(&mut hasher);
            folder_id.hash(&mut hasher);
            sort_order.hash(&mut hasher);
            visibility.hash(&mut hasher);
        }
        Ok(DataFingerprint {
            hash: format!("{:x}", hasher.finish()),
        })
    }
}
