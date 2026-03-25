use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Application configuration, persisted as JSON in the config directory.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub db_backend: DbBackend,
    #[serde(default)]
    pub postgres: PostgresConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum DbBackend {
    #[default]
    Sqlite,
    Postgres,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostgresConfig {
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_pg_port")]
    pub port: u16,
    #[serde(default)]
    pub database: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

fn default_pg_port() -> u16 {
    5432
}

impl Default for PostgresConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: default_pg_port(),
            database: String::new(),
            username: String::new(),
            password: String::new(),
        }
    }
}

impl PostgresConfig {
    /// Build type-safe PostgreSQL connection options.
    ///
    /// Uses `PgConnectOptions` instead of string interpolation to avoid
    /// parameter injection when the password contains special characters
    /// (`@`, `:`, `/`, etc.) and to prevent credentials leaking into logs.
    pub fn connect_options(&self) -> sqlx::postgres::PgConnectOptions {
        sqlx::postgres::PgConnectOptions::new()
            .host(&self.host)
            .port(self.port)
            .database(&self.database)
            .username(&self.username)
            .password(&self.password)
    }
}

/// Tauri managed state wrapping the config and its file path.
pub struct ConfigState {
    pub config: std::sync::Mutex<AppConfig>,
    pub config_path: PathBuf,
}

/// Load the application config from `config_dir/config.json`.
/// Returns `AppConfig::default()` if the file is missing or unreadable.
pub fn load_config(config_dir: &Path) -> AppConfig {
    let path = config_dir.join("config.json");
    match std::fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
        Err(_) => AppConfig::default(),
    }
}

/// Save the application config to the given path.
pub fn save_config(config_path: &Path, config: &AppConfig) -> Result<(), String> {
    let json = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Failed to serialize config: {e}"))?;
    std::fs::write(config_path, json).map_err(|e| format!("Failed to write config: {e}"))
}
