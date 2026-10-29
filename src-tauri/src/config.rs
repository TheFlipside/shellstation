use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Application configuration, persisted as JSON in the config directory.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub db_backend: DbBackend,
    /// Custom `SQLite` database path. When `None`, the default
    /// path `<config_dir>/sessions.db` is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sqlite_path: Option<String>,
    #[serde(default)]
    pub postgres: PostgresConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub app_logging: AppLoggingConfig,
    /// User identity for multi-user `PostgreSQL` mode. Used to key per-user
    /// credential mappings in the `session_credentials` table. Prompted on
    /// first launch in `PostgreSQL` mode, pre-filled with the `OS` username.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_ident: Option<String>,
    /// The private-address check, connect timeout and keepalive.
    #[serde(default)]
    pub connection: ConnectionConfig,
}

/// Settings every new SSH and Telnet connection uses. The connect commands
/// read them from here rather than taking them from the webview, so a
/// compromised webview cannot connect with the private-address check
/// switched off. `commands::connection` is the only way to change them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectionConfig {
    /// Refuse connections to loopback, link-local and private addresses.
    #[serde(default)]
    pub restrict_private_ips: bool,
    /// Seconds a connection may take to open, jump hosts included.
    #[serde(default = "default_connect_timeout_secs")]
    pub connect_timeout_secs: u64,
    /// Seconds between SSH keepalives; 0 sends none.
    #[serde(default = "default_keepalive_interval_secs")]
    pub keepalive_interval_secs: u64,
    /// Keepalives that may go unanswered before the connection is dropped.
    #[serde(default = "default_keepalive_max")]
    pub keepalive_max: u32,
}

fn default_connect_timeout_secs() -> u64 {
    10
}

fn default_keepalive_interval_secs() -> u64 {
    15
}

fn default_keepalive_max() -> u32 {
    3
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            restrict_private_ips: false,
            connect_timeout_secs: default_connect_timeout_secs(),
            keepalive_interval_secs: default_keepalive_interval_secs(),
            keepalive_max: default_keepalive_max(),
        }
    }
}

impl ConnectionConfig {
    // Mirrored by the validators in src/stores/settingsStore.ts.
    /// The connect timeouts allowed, in seconds.
    pub const CONNECT_TIMEOUT_SECS: RangeInclusive<u64> = 1..=300;
    /// The keepalive intervals allowed, in seconds.
    pub const KEEPALIVE_INTERVAL_SECS: RangeInclusive<u64> = 0..=3600;
    /// The numbers of unanswered keepalives allowed.
    pub const KEEPALIVE_MAX: RangeInclusive<u32> = 1..=100;

    /// Check a connect timeout against [`Self::CONNECT_TIMEOUT_SECS`].
    ///
    /// # Errors
    /// Says which range the value is outside of.
    pub fn validate_connect_timeout(secs: u64) -> Result<(), String> {
        check_range("connect timeout", secs, &Self::CONNECT_TIMEOUT_SECS)
    }

    /// Check a keepalive interval against [`Self::KEEPALIVE_INTERVAL_SECS`].
    ///
    /// # Errors
    /// Says which range the value is outside of.
    pub fn validate_keepalive_interval(secs: u64) -> Result<(), String> {
        check_range("keepalive interval", secs, &Self::KEEPALIVE_INTERVAL_SECS)
    }

    /// Check a keepalive count against [`Self::KEEPALIVE_MAX`].
    ///
    /// # Errors
    /// Says which range the value is outside of.
    pub fn validate_keepalive_max(count: u32) -> Result<(), String> {
        check_range("keepalive count", count, &Self::KEEPALIVE_MAX)
    }

    /// Put the default back for each value outside its range, as a
    /// hand-edited config file can hold. The private-address check is left
    /// as it is. Returns the names of the values that were reset.
    pub fn reset_invalid(&mut self) -> Vec<&'static str> {
        let defaults = Self::default();
        let mut reset = Vec::new();
        if Self::validate_connect_timeout(self.connect_timeout_secs).is_err() {
            self.connect_timeout_secs = defaults.connect_timeout_secs;
            reset.push("connect_timeout_secs");
        }
        if Self::validate_keepalive_interval(self.keepalive_interval_secs).is_err() {
            self.keepalive_interval_secs = defaults.keepalive_interval_secs;
            reset.push("keepalive_interval_secs");
        }
        if Self::validate_keepalive_max(self.keepalive_max).is_err() {
            self.keepalive_max = defaults.keepalive_max;
            reset.push("keepalive_max");
        }
        reset
    }
}

fn check_range<T: PartialOrd + std::fmt::Display>(
    name: &str,
    value: T,
    range: &RangeInclusive<T>,
) -> Result<(), String> {
    if range.contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "Invalid {name}: {value} (must be {}\u{2013}{})",
            range.start(),
            range.end()
        ))
    }
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
    /// Password is stored in the `OS` keychain, not in config.json.
    /// This field is populated at runtime from the keychain and cleared
    /// before writing to disk in `save_config`. It must remain serializable
    /// because Tauri IPC uses serde to return the config to the frontend.
    #[serde(default)]
    pub password: String,
    /// `SSL` mode for the `PostgreSQL` connection.
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
    /// Build type-safe `PostgreSQL` connection options.
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

    /// Validate the `ssl_mode` value.
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

/// Keychain reference for the `PostgreSQL` password.
pub const PG_PASSWORD_KEYCHAIN_REF: &str = "postgres-password";

/// The private-address check for a config file that does not parse as a
/// whole: what the file says, if that much can still be read, and switched
/// on otherwise. Damage to the file must never switch the check off; the user
/// can do that in Settings, which asks first.
fn restrict_private_ips_in(contents: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(contents)
        .ok()
        .and_then(|value| {
            value
                .pointer("/connection/restrict_private_ips")
                .and_then(serde_json::Value::as_bool)
        })
        .unwrap_or(true)
}

/// The defaults, but with the private-address check as `restrict`.
fn defaults_restricting(restrict: bool) -> AppConfig {
    AppConfig {
        connection: ConnectionConfig {
            restrict_private_ips: restrict,
            ..ConnectionConfig::default()
        },
        ..AppConfig::default()
    }
}

/// Keep a config file that does not parse as `config.json.bad` beside it:
/// the next save replaces it with the defaults this start fell back to.
/// A copy kept by an earlier start is replaced: the file now there is the
/// one the user last had.
fn keep_damaged_copy(path: &Path, contents: &[u8]) {
    let copy = path.with_extension("json.bad");
    match crate::private_fs::write_private_file(&copy, contents) {
        Ok(()) => tracing::warn!(
            "Kept the config file that does not parse as {}",
            copy.display()
        ),
        Err(e) => tracing::error!("Could not keep a copy of {}: {e}", path.display()),
    }
}

/// Load the application config from `config_dir/config.json`.
///
/// A missing file gives `AppConfig::default()`. A file that does not parse
/// gives the defaults too, after a copy of it is kept, but keeps the
/// private-address check as the file has it, or on if that cannot be read;
/// a file that cannot be read gives the defaults with the check on.
/// Connection settings outside their range get their defaults.
/// The `PostgreSQL` password is loaded from the `OS` keychain (not from the file).
pub fn load_config(config_dir: &Path) -> AppConfig {
    let path = config_dir.join("config.json");
    // Read as bytes, so a file that is not UTF-8 counts as one that does not
    // parse, and is kept, rather than as one that cannot be read.
    let mut config = match std::fs::read(&path) {
        Ok(contents) => match serde_json::from_slice::<AppConfig>(&contents) {
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
                let restrict = restrict_private_ips_in(&contents);
                tracing::error!(
                    restrict_private_ips = restrict,
                    "Failed to parse {}: {e} — using defaults",
                    path.display()
                );
                keep_damaged_copy(&path, &contents);
                // Saving later writes this back, rather than the default.
                defaults_restricting(restrict)
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!(
                "Config file not found ({}) — using defaults",
                path.display()
            );
            AppConfig::default()
        }
        Err(e) => {
            // There is a file, so it may switch the check on.
            tracing::error!(
                "Config file unreadable ({}): {e} — using defaults, blocking private addresses",
                path.display()
            );
            defaults_restricting(true)
        }
    };

    let reset = config.connection.reset_invalid();
    if !reset.is_empty() {
        tracing::warn!(
            settings = ?reset,
            "{} holds connection settings outside their range — using their defaults",
            path.display()
        );
    }

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
                config.postgres.password.clone_from(&*pw);
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
/// The file is owner-only (0600 on Unix) from the moment it is created,
/// since the config names the database server and user.
pub fn save_config(config_path: &Path, config: &AppConfig) -> Result<(), String> {
    // Clone and strip the password so it never lands on disk.
    let mut safe = config.clone();
    safe.postgres.password.clear();
    let json = serde_json::to_string_pretty(&safe)
        .map_err(|e| format!("Failed to serialize config: {e}"))?;
    crate::private_fs::write_private_file(config_path, json.as_bytes())
        .map_err(|e| format!("Failed to write config: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_without_connection_section_gets_defaults() {
        // A config.json written before the connection settings moved here.
        let config: AppConfig = serde_json::from_str(r#"{"db_backend":"sqlite"}"#).unwrap();
        assert_eq!(config.connection, ConnectionConfig::default());
        assert!(!config.connection.restrict_private_ips);
        assert_eq!(config.connection.connect_timeout_secs, 10);
        assert_eq!(config.connection.keepalive_interval_secs, 15);
        assert_eq!(config.connection.keepalive_max, 3);
    }

    #[test]
    fn partial_connection_section_keeps_what_it_has() {
        let config: AppConfig =
            serde_json::from_str(r#"{"connection":{"restrict_private_ips":true}}"#).unwrap();
        assert!(config.connection.restrict_private_ips);
        assert_eq!(config.connection.connect_timeout_secs, 10);
    }

    #[test]
    fn a_config_that_does_not_parse_keeps_what_it_says_or_blocks() {
        let bad_port = br#"{"postgres":{"port":"x"},"connection":{"restrict_private_ips":true}}"#;
        assert!(serde_json::from_slice::<AppConfig>(bad_port).is_err());
        assert!(restrict_private_ips_in(bad_port));
        // An explicit "off" that can still be read stays off.
        assert!(!restrict_private_ips_in(
            br#"{"postgres":{"port":"x"},"connection":{"restrict_private_ips":false}}"#
        ));

        // Anything that cannot say "off" for certain blocks.
        for damaged in [
            &br#"{"postgres":{"port":"x"}}"#[..],
            br#"{"connection":{"restrict_private_ips":"false"}}"#,
            br#"{"connection":{"restrict_private_ips":"#,
            b"{\"connection\":{\"restrict_private_ips\":false},\"user_ident\":\"\xff\"}",
            b"",
        ] {
            assert!(restrict_private_ips_in(damaged), "{damaged:?}");
        }
        assert!(defaults_restricting(true).connection.restrict_private_ips);
    }

    fn scratch_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shellstation-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_missing_config_file_keeps_the_default() {
        let dir = scratch_dir();
        let config = load_config(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(!config.connection.restrict_private_ips);
    }

    #[test]
    fn a_config_file_that_does_not_parse_is_kept_and_blocks() {
        for damaged in [
            &br#"{"postgres":{"host":"db.example","port":"x"}}"#[..],
            // Not UTF-8.
            b"{\"postgres\":{\"host\":\"db.\xffexample\"}}",
        ] {
            let dir = scratch_dir();
            std::fs::write(dir.join("config.json"), damaged).unwrap();
            let config = load_config(&dir);
            let kept = std::fs::read(dir.join("config.json.bad"));
            std::fs::remove_dir_all(&dir).unwrap();
            assert!(config.connection.restrict_private_ips, "{damaged:?}");
            assert_eq!(kept.unwrap(), damaged);
        }
    }

    #[test]
    fn a_config_file_that_cannot_be_read_blocks() {
        let dir = scratch_dir();
        // Reading a directory fails, but not with NotFound.
        std::fs::create_dir(dir.join("config.json")).unwrap();
        let config = load_config(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(config.connection.restrict_private_ips);
    }

    #[test]
    fn connection_section_round_trips() {
        let config = AppConfig {
            connection: ConnectionConfig {
                restrict_private_ips: true,
                connect_timeout_secs: 60,
                keepalive_interval_secs: 0,
                keepalive_max: 10,
            },
            ..AppConfig::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        let back: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.connection, config.connection);
    }

    #[test]
    fn connection_limits_accept_their_bounds_only() {
        assert!(ConnectionConfig::validate_connect_timeout(1).is_ok());
        assert!(ConnectionConfig::validate_connect_timeout(300).is_ok());
        assert!(ConnectionConfig::validate_connect_timeout(0).is_err());
        assert!(ConnectionConfig::validate_connect_timeout(301).is_err());

        assert!(ConnectionConfig::validate_keepalive_interval(0).is_ok());
        assert!(ConnectionConfig::validate_keepalive_interval(3600).is_ok());
        assert!(ConnectionConfig::validate_keepalive_interval(3601).is_err());

        assert!(ConnectionConfig::validate_keepalive_max(1).is_ok());
        assert!(ConnectionConfig::validate_keepalive_max(100).is_ok());
        assert!(ConnectionConfig::validate_keepalive_max(0).is_err());
        assert!(ConnectionConfig::validate_keepalive_max(101).is_err());
    }

    #[test]
    fn reset_invalid_fixes_only_the_values_out_of_range() {
        let mut connection = ConnectionConfig {
            restrict_private_ips: true,
            connect_timeout_secs: 0,
            keepalive_interval_secs: 30,
            keepalive_max: 1000,
        };
        assert_eq!(
            connection.reset_invalid(),
            ["connect_timeout_secs", "keepalive_max"]
        );
        assert_eq!(
            connection,
            ConnectionConfig {
                restrict_private_ips: true,
                connect_timeout_secs: 10,
                keepalive_interval_secs: 30,
                keepalive_max: 3,
            }
        );
        assert_eq!(connection.reset_invalid(), Vec::<&str>::new());
    }
}
