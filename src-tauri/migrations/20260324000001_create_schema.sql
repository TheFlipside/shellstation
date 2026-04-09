-- Highlight profiles (must precede sessions due to FK reference)
CREATE TABLE IF NOT EXISTS highlight_profiles (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    rules TEXT NOT NULL DEFAULT '[]',
    sort_order INTEGER NOT NULL DEFAULT 0
);

-- Folders
CREATE TABLE IF NOT EXISTS folders (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    parent_id TEXT REFERENCES folders(id) ON DELETE CASCADE,
    sort_order INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_folders_parent_id ON folders(parent_id);

-- Sessions
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY NOT NULL,
    folder_id TEXT NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    hostname TEXT NOT NULL,
    port INTEGER NOT NULL DEFAULT 22,
    protocol TEXT NOT NULL DEFAULT 'ssh',
    username TEXT NOT NULL,
    auth_method TEXT NOT NULL DEFAULT 'password',
    jump_host_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,
    tags TEXT NOT NULL DEFAULT '[]',
    icon TEXT NOT NULL DEFAULT 'desktop',
    sort_order INTEGER NOT NULL DEFAULT 0,
    highlight_profile_id TEXT REFERENCES highlight_profiles(id) ON DELETE SET NULL
);

CREATE INDEX idx_sessions_folder_id ON sessions(folder_id);
CREATE INDEX idx_sessions_hostname ON sessions(hostname);

-- Credentials (secrets stored in OS keychain, not in the database)
CREATE TABLE IF NOT EXISTS credentials (
    id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL UNIQUE REFERENCES sessions(id) ON DELETE CASCADE,
    auth_type TEXT NOT NULL,
    keychain_ref TEXT NOT NULL,
    username TEXT NOT NULL DEFAULT ''
);
