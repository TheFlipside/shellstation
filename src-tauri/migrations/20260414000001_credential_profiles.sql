-- Named credential profiles. Secrets live in the OS keychain under
-- `credprofile-<uuid>`; this table holds only metadata.
--
-- This migration is additive: the old `credentials` table is left in place
-- so that an automated one-shot migration (see `migrate_legacy.rs`) can read
-- existing per-session secrets from the keychain, group them into profiles,
-- and link sessions to the new rows. A later release will drop that table.
CREATE TABLE IF NOT EXISTS credential_profiles (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL UNIQUE,
    auth_type TEXT NOT NULL,
    username TEXT NOT NULL DEFAULT '',
    keychain_ref TEXT NOT NULL,
    key_path TEXT NOT NULL DEFAULT '',
    sort_order INTEGER NOT NULL DEFAULT 0
);

-- No foreign key: in PostgreSQL deployments the central sessions DB has this
-- column but `credential_profiles` lives in each user's local SQLite, so a
-- cross-database FK cannot be enforced. Treat this as a soft reference —
-- delete_credential_profile leaves dangling IDs, which connect flow handles
-- by falling back to "no profile assigned".
ALTER TABLE sessions ADD COLUMN credential_profile_id TEXT;

CREATE INDEX idx_sessions_credential_profile_id
    ON sessions(credential_profile_id);
