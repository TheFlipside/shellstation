CREATE TABLE IF NOT EXISTS credentials (
    id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL UNIQUE REFERENCES sessions(id) ON DELETE CASCADE,
    auth_type TEXT NOT NULL,
    keychain_ref TEXT NOT NULL,
    secret TEXT NOT NULL DEFAULT '',
    username TEXT NOT NULL DEFAULT ''
);
