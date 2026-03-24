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
    tags TEXT NOT NULL DEFAULT '[]'
);

CREATE INDEX idx_sessions_folder_id ON sessions(folder_id);
CREATE INDEX idx_sessions_hostname ON sessions(hostname);
