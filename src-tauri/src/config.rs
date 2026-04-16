use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Application configuration, persisted as JSON in the config directory.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub db_backend: DbBackend,
    /// Custom SQLite database path. When `None`, the default
    /// `<config_dir>/sessions.db` is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sqlite_path: Option<String>,
    #[serde(default)]
    pub postgres: PostgresConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub app_logging: AppLoggingConfig,
    /// User identity for multi-user PostgreSQL mode. Used to key per-user
    /// credential mappings in the `session_credentials` table. Prompted on
    /// first launch in PostgreSQL mode, pre-filled with the OS username.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_ident: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppLoggingConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Directory where application log files are written. When `None`,
    /// defaults to `<config_dir>/logs/`. Files are rotated daily as
    /// `shellstation.YYYY-MM-DD.log`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_directory: Option<String>,
    /// Log level filter: "error", "warn", "info", "debug", "trace".
    /// The `RUST_LOG` environment variable, if set, overrides this value.
    #[serde(default = "default_app_log_level")]
    pub level: String,
}

fn default_app_log_level() -> String {
    "info".to_string()
}

impl Default for AppLoggingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            log_directory: None,
            level: default_app_log_level(),
        }
    }
}

impl AppLoggingConfig {
    /// Validate the level value.
    pub fn validate_level(level: &str) -> Result<(), String> {
        match level {
            "error" | "warn" | "info" | "debug" | "trace" => Ok(()),
            other => Err(format!(
                "Invalid log level: \"{other}\". Must be one of error, warn, info, debug, trace."
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Directory where log files are written. When `None`, defaults to
    /// `<config_dir>/logs/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_directory: Option<String>,
    /// Filename format template. Placeholders: {name}, {mm}, {hh}, {dd}, {MM}, {yy}.
    #[serde(default = "default_log_filename_format")]
    pub filename_format: String,
}

fn default_log_filename_format() -> String {
    "{name}_{mm}-{hh}_{dd}{MM}{yy}.log".to_string()
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            log_directory: None,
            filename_format: default_log_filename_format(),
        }
    }
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
    /// Password is stored in the OS keychain, not in config.json.
    /// This field is populated at runtime from the keychain and
    /// excluded when writing to disk via `save_config`.
    #[serde(default)]
    pub password: String,
    /// SSL mode for the PostgreSQL connection.
    /// Accepted values: "disable", "prefer" (default), "require".
    #[serde(default = "default_ssl_mode")]
    pub ssl_mode: String,
}

fn default_pg_port() -> u16 {
    5432
}

fn default_ssl_mode() -> String {
    "prefer".to_string()
}

impl Default for PostgresConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: default_pg_port(),
            database: String::new(),
            username: String::new(),
            password: String::new(),
            ssl_mode: default_ssl_mode(),
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
        use sqlx::postgres::PgSslMode;

        let ssl_mode = match self.ssl_mode.as_str() {
            "disable" => PgSslMode::Disable,
            "require" => PgSslMode::Require,
            _ => PgSslMode::Prefer,
        };

        sqlx::postgres::PgConnectOptions::new()
            .host(&self.host)
            .port(self.port)
            .database(&self.database)
            .username(&self.username)
            .password(&self.password)
            .ssl_mode(ssl_mode)
    }

    /// Validate the ssl_mode value.
    pub fn validate_ssl_mode(mode: &str) -> Result<(), String> {
        match mode {
            "disable" | "prefer" | "require" => Ok(()),
            other => Err(format!(
                "Invalid SSL mode: \"{other}\". Must be \"disable\", \"prefer\", or \"require\"."
            )),
        }
    }
}

/// Tauri managed state wrapping the config and its file path.
pub struct ConfigState {
    pub config: std::sync::Mutex<AppConfig>,
    pub config_path: PathBuf,
}

/// Keychain reference for the PostgreSQL password.
pub const PG_PASSWORD_KEYCHAIN_REF: &str = "postgres-password";

/// Load the application config from `config_dir/config.json`.
/// Returns `AppConfig::default()` if the file is missing or unreadable.
/// The PostgreSQL password is loaded from the OS keychain (not from the file).
pub fn load_config(config_dir: &Path) -> AppConfig {
    let path = config_dir.join("config.json");
    let mut config = match std::fs::read_to_string(&path) {
        Ok(contents) => match serde_json::from_str::<AppConfig>(&contents) {
            Ok(config) => {
                tracing::info!(
                    backend = ?config.db_backend,
                    pg_host = %config.postgres.host,
                    pg_port = config.postgres.port,
                    pg_ssl_mode = %config.postgres.ssl_mode,
                    "Loaded config from {}",
                    path.display()
                );
                config
            }
            Err(e) => {
                tracing::error!("Failed to parse {}: {e} — using defaults", path.display());
                AppConfig::default()
            }
        },
        Err(e) => {
            tracing::info!(
                "Config file not found or unreadable ({}): {e} — using defaults",
                path.display()
            );
            AppConfig::default()
        }
    };

    // Populate PostgreSQL password from OS keychain.
    if config.db_backend == DbBackend::Postgres {
        match crate::credentials::retrieve(PG_PASSWORD_KEYCHAIN_REF) {
            Ok(pw) if pw.is_empty() => {
                tracing::error!(
                    "Keychain returned an empty PostgreSQL password — connection will fail. \
                     This often indicates the OS keychain was not ready during startup. \
                     Try opening Settings and re-saving the database configuration."
                );
            }
            Ok(pw) => {
                tracing::info!(
                    "Retrieved PostgreSQL password from keychain ({} bytes)",
                    pw.len()
                );
                config.postgres.password = pw;
            }
            Err(e) => {
                tracing::error!(
                    "Failed to retrieve PostgreSQL password from keychain: {e} \
                     — connection will fail. This often indicates the OS keychain \
                     was not ready during startup. Try opening Settings and re-saving \
                     the database configuration."
                );
            }
        }
    }

    config
}

/// Save the application config to the given path.
/// Sets restrictive file permissions (0600 on Unix) since the config may
/// contain database credentials.
pub fn save_config(config_path: &Path, config: &AppConfig) -> Result<(), String> {
    // Clone and strip the password so it never lands on disk.
    let mut safe = config.clone();
    safe.postgres.password.clear();
    let json = serde_json::to_string_pretty(&safe)
        .map_err(|e| format!("Failed to serialize config: {e}"))?;
    std::fs::write(config_path, &json).map_err(|e| format!("Failed to write config: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) =
            std::fs::set_permissions(config_path, std::fs::Permissions::from_mode(0o600))
        {
            tracing::warn!(
                "Failed to set 0600 permissions on config file {}: {e} \
                 — file may be readable by other users on this system.",
                config_path.display()
            );
        }
    }
    Ok(())
}
