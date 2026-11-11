use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The app's own directory: `shellstation` in the platform's config
/// directory, or in the working directory when there is none. It holds the
/// config, the local database and, by default, the logs.
#[must_use]
pub fn app_config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("shellstation")
}

/// The app's directory for what belongs to this machine alone:
/// `shellstation` in the platform's local data directory, such as
/// `%LOCALAPPDATA%` on Windows, which does not roam with the user's
/// profile. `None` when there is none, never a directory relative to where
/// the app was started.
#[must_use]
pub fn local_app_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join("shellstation"))
}

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
    /// The size at which a log file is closed and the session's log goes on
    /// in a new one, in MiB: see [`Self::MAX_SIZE_MB`].
    #[serde(default = "default_log_max_size_mb")]
    pub max_size_mb: u32,
    /// How many files of a session's log are kept, the newest ones; older
    /// ones are deleted as new ones are started: see [`Self::MAX_FILES`].
    #[serde(default = "default_log_max_files")]
    pub max_files: u32,
}

fn default_log_filename_format() -> String {
    "{name}_{mm}-{hh}_{dd}{MM}{yy}.log".to_string()
}

fn default_log_max_size_mb() -> u32 {
    10
}

fn default_log_max_files() -> u32 {
    10
}

/// The placeholders the session logger fills in (see
/// `SessionLogManager::log_path`).
const FILENAME_PLACEHOLDERS: [&str; 6] = ["{name}", "{mm}", "{hh}", "{dd}", "{MM}", "{yy}"];

/// Characters a filename format may hold besides letters, digits and the
/// placeholders.
const FILENAME_PUNCTUATION: &str = " -_.()";

/// Longest filename format allowed, in characters, before the placeholders
/// are filled in.
const MAX_FILENAME_FORMAT_LEN: usize = 100;

impl LoggingConfig {
    /// The sizes a log file may be allowed to reach, in MiB. There is no
    /// value for "no limit": the limit is what keeps a remote host from
    /// filling the volume the logs are on with one session's output, and the
    /// webview, which saves these settings, must not be able to take it away.
    /// The largest size and the largest number of files together are what a
    /// webview can raise one session's log to: 12.5 GiB.
    /// Mirrored by `LOG_MAX_SIZE_MB` in `src/components/SettingsDialog.tsx`.
    pub const MAX_SIZE_MB: RangeInclusive<u32> = 1..=256;
    /// The numbers of files of one session's log that may be kept. Mirrored
    /// by `LOG_MAX_FILES` in `src/components/SettingsDialog.tsx`.
    pub const MAX_FILES: RangeInclusive<u32> = 1..=50;

    /// Set the limits that are given, each checked against its range; one
    /// that is `None` stays as it is. Nothing is changed when either is
    /// outside its range.
    ///
    /// # Errors
    /// Says which range a value is outside of.
    pub fn set_limits(
        &mut self,
        max_size_mb: Option<u32>,
        max_files: Option<u32>,
    ) -> Result<(), String> {
        if let Some(mb) = max_size_mb {
            Self::validate_max_size_mb(mb)?;
        }
        if let Some(count) = max_files {
            Self::validate_max_files(count)?;
        }
        if let Some(mb) = max_size_mb {
            self.max_size_mb = mb;
        }
        if let Some(count) = max_files {
            self.max_files = count;
        }
        Ok(())
    }

    /// Check a log file size limit against [`Self::MAX_SIZE_MB`].
    ///
    /// # Errors
    /// Says which range the value is outside of.
    pub fn validate_max_size_mb(mb: u32) -> Result<(), String> {
        check_range("log file size limit", mb, &Self::MAX_SIZE_MB)
    }

    /// Check a number of log files to keep against [`Self::MAX_FILES`].
    ///
    /// # Errors
    /// Says which range the value is outside of.
    pub fn validate_max_files(count: u32) -> Result<(), String> {
        check_range("number of log files kept", count, &Self::MAX_FILES)
    }

    /// Put the default back for a size limit or a number of files outside
    /// its range, as a hand-edited config file can hold. Returns the names
    /// of the values that were reset.
    pub fn reset_invalid_limits(&mut self) -> Vec<&'static str> {
        let mut reset = Vec::new();
        if Self::validate_max_size_mb(self.max_size_mb).is_err() {
            self.max_size_mb = default_log_max_size_mb();
            reset.push("max_size_mb");
        }
        if Self::validate_max_files(self.max_files).is_err() {
            self.max_files = default_log_max_files();
            reset.push("max_files");
        }
        reset
    }

    /// Validate a filename format template: letters, digits, spaces,
    /// `-_.()` and the placeholders only, not starting with a dot or a
    /// space, and ending in `.log` or `.txt`. The webview chooses the format and the
    /// logger appends text the remote host sends, so it must not be able to
    /// name a file something would run or read as configuration. A session
    /// name goes in as letters, digits and `-_.` only.
    pub fn validate_filename_format(fmt: &str) -> Result<(), String> {
        if fmt.is_empty() {
            return Err("Filename format must not be empty".to_string());
        }
        if fmt.chars().count() > MAX_FILENAME_FORMAT_LEN {
            return Err(format!(
                "Filename format is too long (max {MAX_FILENAME_FORMAT_LEN} characters)"
            ));
        }
        let mut rest = fmt;
        while let Some(c) = rest.chars().next() {
            if let Some(placeholder) = FILENAME_PLACEHOLDERS.iter().find(|p| rest.starts_with(*p)) {
                rest = &rest[placeholder.len()..];
            } else if c.is_alphanumeric() || FILENAME_PUNCTUATION.contains(c) {
                rest = &rest[c.len_utf8()..];
            } else {
                return Err(format!(
                    "Filename format must not contain {c:?}: use letters, digits, spaces, \
                     - _ . ( ) and the placeholders"
                ));
            }
        }
        // Windows treats a name starting with a space inconsistently.
        if fmt.starts_with(['.', ' ']) || fmt.contains("..") {
            return Err(
                "Filename format must not start with '.' or a space, or contain '..'".to_string(),
            );
        }
        let extension = Path::new(fmt).extension().and_then(|ext| ext.to_str());
        if !extension
            .is_some_and(|ext| ext.eq_ignore_ascii_case("log") || ext.eq_ignore_ascii_case("txt"))
        {
            return Err("Filename format must end in .log or .txt".to_string());
        }
        Ok(())
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            log_directory: None,
            filename_format: default_log_filename_format(),
            max_size_mb: default_log_max_size_mb(),
            max_files: default_log_max_files(),
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
    /// A value in the config file that is none of these loads as
    /// [`FALLBACK_SSL_MODE`], not as the default.
    #[serde(default = "default_ssl_mode")]
    pub ssl_mode: String,
}

/// What an `ssl_mode` the app does not know connects as. Never the default,
/// `prefer`, which falls back to an unencrypted connection: a hand-edited
/// `required` or `verify-full` must not allow one.
pub const FALLBACK_SSL_MODE: &str = "require";

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

        // `load_config` and the commands only let the three known modes get
        // here. Should anything else get here, it is treated as
        // `FALLBACK_SSL_MODE`: an unknown mode must never be the one that may
        // connect unencrypted.
        let ssl_mode = match self.ssl_mode.as_str() {
            "disable" => PgSslMode::Disable,
            "prefer" => PgSslMode::Prefer,
            _ => PgSslMode::Require,
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

    /// Bring an `ssl_mode` from the config file into the form the app saves:
    /// surrounding whitespace and letter case are dropped, so a hand-edited
    /// `"Require"` is `require`. A value that is still not a known mode is
    /// replaced by [`FALLBACK_SSL_MODE`] and returned; otherwise `None`.
    pub fn reset_invalid_ssl_mode(&mut self) -> Option<String> {
        let mode = self.ssl_mode.trim().to_ascii_lowercase();
        if Self::validate_ssl_mode(&mode).is_ok() {
            self.ssl_mode = mode;
            None
        } else {
            Some(std::mem::replace(
                &mut self.ssl_mode,
                FALLBACK_SSL_MODE.to_string(),
            ))
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

/// How a [`ConfigNote`] is logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteLevel {
    /// What the load did, with nothing wrong.
    Info,
    /// The app changed or set aside something the file holds.
    Warn,
    /// The file, or a copy of it, could not be read, used or kept.
    Error,
}

/// A line [`load_config`] has for the application log.
///
/// At startup `load_config` runs before the tracing subscriber exists,
/// because the subscriber is set up from the config, so a `tracing` call in
/// it would write nothing. It returns its lines instead, and the caller logs
/// them once there is a log.
///
/// A note is logged as its text, so whatever comes from the config file, an
/// error that quotes the file included, goes into the text escaped (`{:?}`):
/// a line break in it must not start a log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigNote {
    /// The level the note is logged at.
    pub level: NoteLevel,
    /// The log message, with anything from the config file escaped.
    pub text: String,
}

impl ConfigNote {
    fn info(text: String) -> Self {
        Self {
            level: NoteLevel::Info,
            text,
        }
    }

    fn warn(text: String) -> Self {
        Self {
            level: NoteLevel::Warn,
            text,
        }
    }

    fn error(text: String) -> Self {
        Self {
            level: NoteLevel::Error,
            text,
        }
    }

    /// Write the note to the application log.
    pub fn log(&self) {
        match self.level {
            NoteLevel::Info => tracing::info!("{}", self.text),
            NoteLevel::Warn => tracing::warn!("{}", self.text),
            NoteLevel::Error => tracing::error!("{}", self.text),
        }
    }
}

/// Keep a config file that does not parse as `config.json.bad` beside it:
/// the next save replaces it with the defaults this start fell back to.
/// A copy kept by an earlier start is replaced: the file now there is the
/// one the user last had.
fn keep_damaged_copy(path: &Path, contents: &[u8]) -> ConfigNote {
    let copy = path.with_extension("json.bad");
    match crate::private_fs::write_private_file(&copy, contents) {
        Ok(()) => ConfigNote::warn(format!(
            "Kept the config file that does not parse as {}",
            copy.display()
        )),
        Err(e) => ConfigNote::error(format!("Could not keep a copy of {}: {e}", path.display())),
    }
}

/// Load the application config from `config_dir/config.json`.
///
/// A missing file gives `AppConfig::default()`. A file that does not parse
/// gives the defaults too, after a copy of it is kept, but keeps the
/// private-address check as the file has it, or on if that cannot be read;
/// a file that cannot be read gives the defaults with the check on.
/// Connection settings outside their range get their defaults.
/// An `ssl_mode` that is not a known mode becomes [`FALLBACK_SSL_MODE`].
/// The `PostgreSQL` password is not in the file: see
/// [`load_postgres_password`].
///
/// What there is to say about the load is returned and not logged here,
/// since at startup there is no log yet: see [`ConfigNote`].
pub fn load_config(config_dir: &Path) -> (AppConfig, Vec<ConfigNote>) {
    let path = config_dir.join("config.json");
    let mut notes = Vec::new();
    // Read as bytes, so a file that is not UTF-8 counts as one that does not
    // parse, and is kept, rather than as one that cannot be read.
    let mut config = match std::fs::read(&path) {
        Ok(contents) => match serde_json::from_slice::<AppConfig>(&contents) {
            Ok(mut config) => {
                notes.push(ConfigNote::info(format!(
                    "Loaded config from {}",
                    path.display()
                )));
                // The other arms give the defaults, which are valid.
                if let Some(was) = config.postgres.reset_invalid_ssl_mode() {
                    notes.push(ConfigNote::warn(format!(
                        "{} holds an ssl_mode the app does not know ({was:?}) — using \
                         \"{FALLBACK_SSL_MODE}\", which never connects unencrypted",
                        path.display()
                    )));
                }
                config
            }
            Err(e) => {
                let restrict = restrict_private_ips_in(&contents);
                // The error can quote the file (an unknown `db_backend`).
                notes.push(ConfigNote::error(format!(
                    "Failed to parse {} ({:?}) — using defaults, restrict_private_ips={restrict}",
                    path.display(),
                    e.to_string()
                )));
                notes.push(keep_damaged_copy(&path, &contents));
                // Saving later writes this back, rather than the default.
                defaults_restricting(restrict)
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            notes.push(ConfigNote::info(format!(
                "Config file not found ({}) — using defaults",
                path.display()
            )));
            AppConfig::default()
        }
        Err(e) => {
            // There is a file, so it may switch the check on.
            notes.push(ConfigNote::error(format!(
                "Config file unreadable ({}): {e} — using defaults, blocking private addresses",
                path.display()
            )));
            defaults_restricting(true)
        }
    };

    let reset = config.connection.reset_invalid();
    if !reset.is_empty() {
        notes.push(ConfigNote::warn(format!(
            "{} holds connection settings outside their range ({}) — using their defaults",
            path.display(),
            reset.join(", ")
        )));
    }

    // A format saved before the format rules existed, possibly by a
    // compromised webview, could name a file such as `x.bat`. The error
    // quotes at most one character of it, escaped.
    if let Err(e) = LoggingConfig::validate_filename_format(&config.logging.filename_format) {
        notes.push(ConfigNote::warn(format!(
            "{} holds a session log filename format the app no longer accepts ({e}) — \
             using the default",
            path.display()
        )));
        config.logging.filename_format = default_log_filename_format();
    }

    let reset = config.logging.reset_invalid_limits();
    if !reset.is_empty() {
        notes.push(ConfigNote::warn(format!(
            "{} holds session log limits outside their range ({}) — using their defaults",
            path.display(),
            reset.join(", ")
        )));
    }

    (config, notes)
}

/// Put the `PostgreSQL` password from the `OS` keychain into `config`, if the
/// backend is `PostgreSQL`. The password is never in the config file.
///
/// Not part of [`load_config`], so that it can run once there is a log: this
/// and the keychain code say what went wrong through `tracing`. Nothing
/// about the password itself is logged, its length included.
pub fn load_postgres_password(config: &mut AppConfig) {
    if config.db_backend != DbBackend::Postgres {
        return;
    }
    match crate::credentials::retrieve(PG_PASSWORD_KEYCHAIN_REF) {
        Ok(pw) if pw.is_empty() => {
            tracing::error!(
                "Keychain returned an empty PostgreSQL password — connection will fail. \
                 This often indicates the OS keychain was not ready during startup. \
                 Try opening Settings and re-saving the database configuration."
            );
        }
        Ok(pw) => {
            tracing::info!("Retrieved PostgreSQL password from keychain");
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

    /// The texts of the notes of one level, in order.
    fn texts(notes: &[ConfigNote], level: NoteLevel) -> Vec<String> {
        notes
            .iter()
            .filter(|note| note.level == level)
            .map(|note| note.text.clone())
            .collect()
    }

    fn scratch_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shellstation-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_default_filename_format_is_valid() {
        let default = LoggingConfig::default().filename_format;
        assert_eq!(LoggingConfig::validate_filename_format(&default), Ok(()));
        for fmt in [
            "{name}.txt",
            "Session {name} ({dd}.{MM}.{yy}).LOG",
            "sitzung-ä_{hh}{mm}.log",
        ] {
            assert_eq!(
                LoggingConfig::validate_filename_format(fmt),
                Ok(()),
                "{fmt}"
            );
        }
    }

    #[test]
    fn filename_formats_that_name_other_files_are_refused() {
        for fmt in [
            "",
            "{name}",
            "{name}.bat",
            "{name}.log.bat",
            "{name}.log.",
            "{name}.log ",
            "{name}.ps1",
            ".bashrc",
            ".log",
            " {name}.log",
            "{name}..log",
            "../{name}.log",
            "logs/{name}.log",
            "logs\\{name}.log",
            "C:{name}.log",
            "{name}:stream.log",
            "{name}\u{202e}gol.exe",
            "{name}\n.log",
            "{other}.log",
            "{name.log",
            "$name.log",
        ] {
            assert!(
                LoggingConfig::validate_filename_format(fmt).is_err(),
                "{fmt:?}"
            );
        }
        let long = format!("{}.log", "a".repeat(MAX_FILENAME_FORMAT_LEN));
        assert!(LoggingConfig::validate_filename_format(&long).is_err());
    }

    #[test]
    fn a_saved_filename_format_the_rules_refuse_gets_the_default() {
        let dir = scratch_dir();
        std::fs::write(
            dir.join("config.json"),
            r#"{"logging":{"enabled":true,"filename_format":"{name}.bat"}}"#,
        )
        .unwrap();
        let (config, notes) = load_config(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(config.logging.enabled);
        assert_eq!(
            config.logging.filename_format,
            LoggingConfig::default().filename_format
        );
        let warnings = texts(&notes, NoteLevel::Warn);
        assert_eq!(warnings.len(), 1, "{notes:?}");
        assert!(warnings[0].contains("filename format"), "{}", warnings[0]);
    }

    #[test]
    fn a_config_without_log_limits_gets_the_defaults() {
        // A config file written before the limits existed.
        let dir = scratch_dir();
        std::fs::write(dir.join("config.json"), r#"{"logging":{"enabled":true}}"#).unwrap();
        let (config, notes) = load_config(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(config.logging.max_size_mb, 10);
        assert_eq!(config.logging.max_files, 10);
        assert!(texts(&notes, NoteLevel::Warn).is_empty(), "{notes:?}");
    }

    #[test]
    fn saved_log_limits_outside_their_range_get_the_defaults() {
        // Neither can be switched off in a hand-edited config file.
        let dir = scratch_dir();
        std::fs::write(
            dir.join("config.json"),
            r#"{"logging":{"enabled":true,"max_size_mb":0,"max_files":4000000000}}"#,
        )
        .unwrap();
        let (config, notes) = load_config(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(config.logging.enabled);
        assert_eq!(config.logging.max_size_mb, 10);
        assert_eq!(config.logging.max_files, 10);
        let warnings = texts(&notes, NoteLevel::Warn);
        assert_eq!(warnings.len(), 1, "{notes:?}");
        assert!(
            warnings[0].contains("max_size_mb, max_files"),
            "{}",
            warnings[0]
        );
    }

    /// A limit that is no whole number from 0 to 4294967295 is not a value
    /// outside its range but a file that cannot be read, as with every
    /// other number in the config: the whole file is set aside, and the
    /// limits are the defaults with the rest.
    #[test]
    fn a_log_limit_of_the_wrong_type_makes_the_config_file_unreadable() {
        for limit in ["-1", "1.5", "\"10\"", "null", "5000000000"] {
            let dir = scratch_dir();
            let file =
                format!(r#"{{"logging":{{"enabled":true,"max_size_mb":{limit},"max_files":3}}}}"#);
            std::fs::write(dir.join("config.json"), &file).unwrap();
            let (config, notes) = load_config(&dir);
            let kept = std::fs::read(dir.join("config.json.bad"));
            std::fs::remove_dir_all(&dir).unwrap();
            assert_eq!(config.logging.max_size_mb, 10, "{limit}");
            assert_eq!(config.logging.max_files, 10, "{limit}");
            assert!(!config.logging.enabled, "{limit}");
            assert_eq!(kept.unwrap(), file.as_bytes(), "{limit}");
            assert_eq!(texts(&notes, NoteLevel::Error).len(), 1, "{notes:?}");
        }
    }

    /// What `logging_save_config` does with the limits the webview passes.
    #[test]
    fn limits_are_set_only_when_all_that_are_given_are_in_range() {
        let mut logging = LoggingConfig::default();
        assert_eq!(logging.set_limits(Some(25), Some(4)), Ok(()));
        assert_eq!((logging.max_size_mb, logging.max_files), (25, 4));
        // One that is not passed stays as it is.
        assert_eq!(logging.set_limits(None, Some(7)), Ok(()));
        assert_eq!((logging.max_size_mb, logging.max_files), (25, 7));
        assert_eq!(logging.set_limits(Some(256), None), Ok(()));
        assert_eq!((logging.max_size_mb, logging.max_files), (256, 7));
        assert_eq!(logging.set_limits(None, None), Ok(()));
        assert_eq!((logging.max_size_mb, logging.max_files), (256, 7));
        // Neither limit can be switched off or put out of its range, and a
        // value in range is not taken together with one that is not.
        for (mb, count) in [
            (Some(0), Some(5)),
            (Some(5), Some(0)),
            (Some(257), None),
            (None, Some(51)),
            (Some(u32::MAX), Some(u32::MAX)),
        ] {
            assert!(logging.set_limits(mb, count).is_err(), "{mb:?} {count:?}");
            assert_eq!((logging.max_size_mb, logging.max_files), (256, 7));
        }
    }

    #[test]
    fn log_limits_are_checked_against_their_ranges() {
        for mb in [1, 10, 256] {
            assert_eq!(LoggingConfig::validate_max_size_mb(mb), Ok(()), "{mb}");
        }
        for mb in [0, 257, 1024, u32::MAX] {
            assert!(LoggingConfig::validate_max_size_mb(mb).is_err(), "{mb}");
        }
        for count in [1, 10, 50] {
            assert_eq!(LoggingConfig::validate_max_files(count), Ok(()), "{count}");
        }
        for count in [0, 51, 100, u32::MAX] {
            assert!(LoggingConfig::validate_max_files(count).is_err(), "{count}");
        }
    }

    /// The `ssl_mode` a config file with `json_value` for it loads as, and
    /// the warnings the load returns.
    fn load_with_ssl_mode(json_value: &str) -> (String, Vec<String>) {
        let dir = scratch_dir();
        std::fs::write(
            dir.join("config.json"),
            format!(r#"{{"postgres":{{"host":"db.example","ssl_mode":{json_value}}}}}"#),
        )
        .unwrap();
        let (config, notes) = load_config(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        // The rest of the file is kept, unlike for one that does not parse.
        assert_eq!(config.postgres.host, "db.example");
        assert!(texts(&notes, NoteLevel::Error).is_empty(), "{notes:?}");
        (config.postgres.ssl_mode, texts(&notes, NoteLevel::Warn))
    }

    #[test]
    fn an_unknown_ssl_mode_in_the_config_file_never_loads_as_prefer() {
        for unknown in [
            r#""required""#,
            r#""verify-full""#,
            r#""verify-ca""#,
            r#""allow""#,
            r#""disabled""#,
            r#""off""#,
            r#""prefer;""#,
            r#""""#,
            r#""  ""#,
        ] {
            let (mode, warnings) = load_with_ssl_mode(unknown);
            assert_eq!(mode, FALLBACK_SSL_MODE, "{unknown}");
            // The caller is told, with what the file said: `load_config`
            // runs before there is a log, so it cannot say so itself.
            assert_eq!(warnings.len(), 1, "{unknown}");
            assert!(warnings[0].contains(unknown), "{}", warnings[0]);
            assert!(warnings[0].contains("ssl_mode"), "{}", warnings[0]);
        }
    }

    #[test]
    fn the_warning_about_an_unknown_ssl_mode_quotes_it_escaped() {
        let (mode, warnings) = load_with_ssl_mode(r#""x\n2026-01-01 ERROR forged\u001b[2J""#);
        assert_eq!(mode, FALLBACK_SSL_MODE);
        assert_eq!(warnings.len(), 1);
        assert!(!warnings[0].contains('\n'), "{:?}", warnings[0]);
        assert!(!warnings[0].contains('\u{1b}'), "{:?}", warnings[0]);
        assert!(warnings[0].contains(r"x\n2026-01-01 ERROR forged"));
    }

    #[test]
    fn a_known_ssl_mode_in_the_config_file_is_kept_in_any_case() {
        for (written, loaded) in [
            (r#""disable""#, "disable"),
            (r#""prefer""#, "prefer"),
            (r#""require""#, "require"),
            (r#""Require""#, "require"),
            (r#""REQUIRE""#, "require"),
            (r#"" require\n""#, "require"),
            (r#""Disable""#, "disable"),
        ] {
            let (mode, warnings) = load_with_ssl_mode(written);
            assert_eq!(mode, loaded, "{written}");
            assert!(warnings.is_empty(), "{written}: {warnings:?}");
        }
    }

    #[test]
    fn a_config_file_without_an_ssl_mode_gets_the_default() {
        let dir = scratch_dir();
        std::fs::write(
            dir.join("config.json"),
            r#"{"postgres":{"host":"db.example"}}"#,
        )
        .unwrap();
        let (config, notes) = load_config(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(config.postgres.ssl_mode, "prefer");
        assert!(texts(&notes, NoteLevel::Warn).is_empty(), "{notes:?}");
        // What every other way of loading falls back to is valid as well.
        let mut defaults = PostgresConfig::default();
        assert_eq!(defaults.reset_invalid_ssl_mode(), None);
        assert_eq!(defaults.ssl_mode, "prefer");
    }

    #[test]
    fn a_config_file_whose_ssl_mode_is_no_string_is_not_used_at_all() {
        let file = |ssl_mode: &str| {
            format!(
                r#"{{"db_backend":"postgres","postgres":{{"host":"db.example","ssl_mode":{ssl_mode}}}}}"#
            )
        };
        // With a string in that place the file parses, so it is the value
        // alone that the cases below fail on.
        let parsed: AppConfig = serde_json::from_str(&file(r#""require""#)).unwrap();
        assert_eq!(parsed.db_backend, DbBackend::Postgres);
        assert_eq!(parsed.postgres.host, "db.example");

        // Such a file does not parse, so nothing of it is used: the app
        // starts on the default backend, SQLite, with no PostgreSQL server
        // to connect to, and so with no `ssl_mode` in use.
        for value in [
            "null",
            "1",
            "true",
            r#"["require"]"#,
            r#"{"mode":"require"}"#,
        ] {
            let dir = scratch_dir();
            std::fs::write(dir.join("config.json"), file(value)).unwrap();
            let (config, _) = load_config(&dir);
            let kept = dir.join("config.json.bad").exists();
            std::fs::remove_dir_all(&dir).unwrap();
            assert_eq!(config.db_backend, DbBackend::Sqlite, "{value}");
            assert_eq!(config.postgres.host, "", "{value}");
            assert!(kept, "{value}");
        }

        // A key given twice does not parse either, whichever value comes
        // last, so the second cannot replace the first.
        let twice =
            br#"{"db_backend":"postgres","postgres":{"ssl_mode":"require","ssl_mode":"prefer"}}"#;
        let error = serde_json::from_slice::<AppConfig>(twice).unwrap_err();
        assert!(error.to_string().contains("duplicate field"), "{error}");
    }

    #[test]
    fn resetting_an_ssl_mode_returns_what_the_file_said() {
        let mut postgres = PostgresConfig {
            ssl_mode: "Required".to_string(),
            ..PostgresConfig::default()
        };
        assert_eq!(
            postgres.reset_invalid_ssl_mode(),
            Some("Required".to_string())
        );
        assert_eq!(postgres.ssl_mode, FALLBACK_SSL_MODE);
        assert_eq!(PostgresConfig::validate_ssl_mode(FALLBACK_SSL_MODE), Ok(()));
    }

    #[test]
    fn connect_options_never_turn_an_unknown_ssl_mode_into_prefer() {
        use sqlx::postgres::PgSslMode;

        let mode_for = |ssl_mode: &str| {
            PostgresConfig {
                ssl_mode: ssl_mode.to_string(),
                ..PostgresConfig::default()
            }
            .connect_options()
            .get_ssl_mode()
        };
        assert!(matches!(mode_for("disable"), PgSslMode::Disable));
        assert!(matches!(mode_for("prefer"), PgSslMode::Prefer));
        assert!(matches!(mode_for("require"), PgSslMode::Require));
        // Not reachable through `load_config` or the commands, which is why
        // it is checked here.
        for unknown in ["required", "Require", "verify-full", "disabled", ""] {
            assert!(
                matches!(mode_for(unknown), PgSslMode::Require),
                "{unknown:?}"
            );
        }
    }

    #[test]
    fn a_missing_config_file_keeps_the_default() {
        let dir = scratch_dir();
        let (config, notes) = load_config(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(!config.connection.restrict_private_ips);
        // A first start is nothing to warn about.
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(notes[0].level, NoteLevel::Info);
        assert!(notes[0].text.contains("not found"), "{}", notes[0].text);
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
            let (config, notes) = load_config(&dir);
            let kept = std::fs::read(dir.join("config.json.bad"));
            std::fs::remove_dir_all(&dir).unwrap();
            assert!(config.connection.restrict_private_ips, "{damaged:?}");
            assert_eq!(kept.unwrap(), damaged);
            // Both are said: that the file is not used, and where it went.
            let errors = texts(&notes, NoteLevel::Error);
            assert_eq!(errors.len(), 1, "{notes:?}");
            assert!(errors[0].contains("Failed to parse"), "{}", errors[0]);
            let warnings = texts(&notes, NoteLevel::Warn);
            assert_eq!(warnings.len(), 1, "{notes:?}");
            assert!(warnings[0].contains("config.json.bad"), "{}", warnings[0]);
        }
    }

    #[test]
    fn what_a_damaged_config_file_holds_cannot_start_a_log_line() {
        // serde quotes an unknown `db_backend` in its error as it is.
        let dir = scratch_dir();
        std::fs::write(
            dir.join("config.json"),
            r#"{"db_backend":"x\n2026-01-01 ERROR forged"}"#,
        )
        .unwrap();
        let (_, notes) = load_config(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        let errors = texts(&notes, NoteLevel::Error);
        assert_eq!(errors.len(), 1, "{notes:?}");
        assert!(
            errors[0].contains(r"x\n2026-01-01 ERROR forged"),
            "{}",
            errors[0]
        );
        for note in &notes {
            assert!(!note.text.contains('\n'), "{:?}", note.text);
        }
    }

    #[test]
    fn connection_settings_out_of_range_are_named_in_a_warning() {
        let dir = scratch_dir();
        std::fs::write(
            dir.join("config.json"),
            r#"{"connection":{"connect_timeout_secs":0,"keepalive_max":1000}}"#,
        )
        .unwrap();
        let (config, notes) = load_config(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(config.connection.connect_timeout_secs, 10);
        let warnings = texts(&notes, NoteLevel::Warn);
        assert_eq!(warnings.len(), 1, "{notes:?}");
        assert!(
            warnings[0].contains("connect_timeout_secs, keepalive_max"),
            "{}",
            warnings[0]
        );
    }

    #[test]
    fn a_config_file_that_cannot_be_read_blocks() {
        let dir = scratch_dir();
        // Reading a directory fails, but not with NotFound.
        std::fs::create_dir(dir.join("config.json")).unwrap();
        let (config, notes) = load_config(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(config.connection.restrict_private_ips);
        let errors = texts(&notes, NoteLevel::Error);
        assert_eq!(errors.len(), 1, "{notes:?}");
        assert!(errors[0].contains("unreadable"), "{}", errors[0]);
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
