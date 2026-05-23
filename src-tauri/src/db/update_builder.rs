//! Dynamic SQL `UPDATE` builder.
//!
//! ## Why this exists
//!
//! sqlx is compile-time-checked when the query string is a `&'static str`,
//! but the session-edit IPC has thirteen `Option<T>` fields where each
//! `Some(_)` adds one column to `SET` and `None` skips it. With static
//! queries you would need 2^13 prepared statements, so the builder
//! constructs the statement at runtime and binds typed values in order.
//!
//! ## Why a builder instead of inline `if let Some(...)` chains
//!
//! Before this module, each provider open-coded the same thirteen
//! `if let Some` branches twice (once to build the `SET` fragment, once
//! to bind the values), with the only differences being the placeholder
//! syntax (`?` vs `$N`) and a per-provider WHERE clause. The two copies
//! drifted: `PostgreSQL` needed explicit type tags to avoid implicit text
//! conversion, `SQLite` didn't, and the bool→int casting lived in only
//! one. Centralising the construction here:
//!
//! - removes the duplication between `postgres.rs` and `sqlite.rs`,
//! - keeps the typed `BindVal` enum in one place so future column
//!   additions cannot accidentally bind the wrong SQL type,
//! - makes the per-field logic unit-testable without a live database.
//!
//! ## Why column names are an enum, not `&str`
//!
//! The set/where fragments embed the column name into the SQL string via
//! `format!("{col} = {placeholder}")`. If `col` ever became a runtime
//! string (e.g. from an IPC payload), this would be classic SQL injection
//! against a table where the only remaining safeguard is `PostgreSQL`
//! row-level security. `SessionColumn` makes that injection impossible
//! by construction: the compiler enforces that every column name is one
//! of the thirteen variants below, each mapped to a hardcoded
//! `&'static str` in `as_sql()`. Adding a new column is a deliberate
//! decision — extending the enum — rather than something that can be
//! introduced accidentally by passing a string somewhere.
//!
//! If a future column must be added, extend `SessionColumn` and
//! `SessionColumn::as_sql`; the call sites in `postgres.rs` and
//! `sqlite.rs` will fail to compile until the new variant is wired up.

use uuid::Uuid;

/// Columns of the `sessions` table that the session-edit IPC may update.
///
/// Every name returned by [`SessionColumn::as_sql`] is a compile-time
/// `&'static str`. The enum cannot represent any other column name, so
/// the `format!` calls that embed it into SQL cannot be subverted by
/// runtime input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionColumn {
    Name,
    Hostname,
    Port,
    Protocol,
    Username,
    AuthMethod,
    JumpHostId,
    Tags,
    Icon,
    HighlightProfileId,
    CredentialProfileId,
    LegacyAlgorithms,
    LoginSequenceId,
}

impl SessionColumn {
    /// The unquoted SQL column name. Safe to interpolate into SQL via
    /// `format!`; each variant maps to a hardcoded literal.
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Hostname => "hostname",
            Self::Port => "port",
            Self::Protocol => "protocol",
            Self::Username => "username",
            Self::AuthMethod => "auth_method",
            Self::JumpHostId => "jump_host_id",
            Self::Tags => "tags",
            Self::Icon => "icon",
            Self::HighlightProfileId => "highlight_profile_id",
            Self::CredentialProfileId => "credential_profile_id",
            Self::LegacyAlgorithms => "legacy_algorithms",
            Self::LoginSequenceId => "login_sequence_id",
        }
    }
}

/// A typed bind value. Required because `PostgreSQL` does not implicitly
/// convert between `TEXT` and `INTEGER` the way `SQLite` does — a bare
/// `query.bind(string)` for a `port INT` column rejects with a type
/// error against Postgres while silently working on `SQLite`. Keeping the
/// type explicit on every bind makes the two providers behave the same.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindVal {
    Text(String),
    Int(i32),
    /// SQL `NULL`. Distinct from `Text(String::new())` — a literal empty
    /// string is a valid non-null value for nullable `TEXT` columns.
    Null,
}

/// Placeholder syntax for the target dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// `$1, $2, …` — `PostgreSQL` / sqlx Postgres.
    Postgres,
    /// `?` — `SQLite` / sqlx `SQLite`.
    Sqlite,
}

/// Builds the `SET col = …, col = …` fragment plus the bind list for a
/// dynamic `UPDATE`. The caller still owns the surrounding statement
/// (`UPDATE <table> SET <fragment> WHERE …`) so per-provider WHERE
/// clauses (e.g. `PostgreSQL` `RLS` `AND owner = current_user`) stay local.
pub struct UpdateBuilder {
    dialect: Dialect,
    sets: Vec<String>,
    values: Vec<BindVal>,
}

impl UpdateBuilder {
    pub fn new(dialect: Dialect) -> Self {
        Self {
            dialect,
            sets: Vec::new(),
            values: Vec::new(),
        }
    }

    /// Render the placeholder for the next value to be appended. For
    /// `PostgreSQL` the index is 1-based; for `SQLite` it's always `?`.
    fn next_placeholder(&self) -> String {
        match self.dialect {
            Dialect::Postgres => format!("${}", self.values.len() + 1),
            Dialect::Sqlite => "?".to_string(),
        }
    }

    /// Append `col = <placeholder>` and bind the given value.
    fn push(&mut self, col: SessionColumn, val: BindVal) {
        let placeholder = self.next_placeholder();
        self.sets.push(format!("{} = {placeholder}", col.as_sql()));
        self.values.push(val);
    }

    /// `Some(s)` → set column to `s`. `None` → skip the column entirely.
    pub fn set_text(&mut self, col: SessionColumn, val: Option<&String>) {
        if let Some(s) = val {
            self.push(col, BindVal::Text(s.clone()));
        }
    }

    /// `Some(n)` → set column to `n`. `None` → skip the column entirely.
    pub fn set_int(&mut self, col: SessionColumn, val: Option<i32>) {
        if let Some(n) = val {
            self.push(col, BindVal::Int(n));
        }
    }

    /// `Some(b)` → set column to `1`/`0`. `None` → skip the column.
    /// Used for boolean flags stored as INT in the schema.
    pub fn set_bool_as_int(&mut self, col: SessionColumn, val: Option<bool>) {
        if let Some(b) = val {
            self.push(col, BindVal::Int(i32::from(b)));
        }
    }

    /// Nullable UUID column with three-state semantics:
    /// - `None` → skip the column (don't touch it)
    /// - `Some(None)` → set the column to SQL `NULL`
    /// - `Some(Some(u))` → set the column to `u`
    ///
    /// The outer `Option` tells us whether the IPC payload mentioned
    /// this field at all; the inner `Option` tells us what to set it to.
    pub fn set_optional_uuid(&mut self, col: SessionColumn, val: Option<&Option<Uuid>>) {
        if let Some(inner) = val {
            let placeholder = self.next_placeholder();
            self.sets.push(format!("{} = {placeholder}", col.as_sql()));
            self.values.push(match inner {
                Some(u) => BindVal::Text(u.to_string()),
                None => BindVal::Null,
            });
        }
    }

    /// True when no `SET` columns have been collected. Callers should
    /// short-circuit on this — an empty `SET` clause is a SQL syntax
    /// error in both `PostgreSQL` and `SQLite`.
    pub fn is_empty(&self) -> bool {
        self.sets.is_empty()
    }

    /// `name = $1, port = $2` — ready to drop into `UPDATE … SET <here>`.
    pub fn set_clause(&self) -> String {
        self.sets.join(", ")
    }

    /// The placeholder the WHERE clause should use for its bind value
    /// (e.g. `WHERE id = $14`). Doesn't append anything — the WHERE
    /// value is bound after `values()` by the caller.
    pub fn where_placeholder(&self) -> String {
        self.next_placeholder()
    }

    /// The bind values in the order they appear in the SET clause.
    /// Bind these to the sqlx query, then bind any WHERE values.
    pub fn values(&self) -> &[BindVal] {
        &self.values
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_column_names_match_schema() {
        // Every variant must map to a hardcoded snake_case identifier.
        // Spot-check the column names so a future variant rename can't
        // silently desync from the database schema.
        assert_eq!(SessionColumn::Name.as_sql(), "name");
        assert_eq!(SessionColumn::JumpHostId.as_sql(), "jump_host_id");
        assert_eq!(
            SessionColumn::LegacyAlgorithms.as_sql(),
            "legacy_algorithms"
        );
        assert_eq!(
            SessionColumn::CredentialProfileId.as_sql(),
            "credential_profile_id"
        );
    }

    #[test]
    fn empty_builder() {
        let b = UpdateBuilder::new(Dialect::Postgres);
        assert!(b.is_empty());
        assert_eq!(b.set_clause(), "");
        assert_eq!(b.where_placeholder(), "$1");
        assert!(b.values().is_empty());
    }

    #[test]
    fn skips_none_fields() {
        let mut b = UpdateBuilder::new(Dialect::Sqlite);
        b.set_text(SessionColumn::Name, None);
        b.set_int(SessionColumn::Port, None);
        b.set_bool_as_int(SessionColumn::LegacyAlgorithms, None);
        b.set_optional_uuid(SessionColumn::JumpHostId, None);
        assert!(b.is_empty());
    }

    #[test]
    fn postgres_placeholders_are_numbered() {
        let mut b = UpdateBuilder::new(Dialect::Postgres);
        b.set_text(SessionColumn::Name, Some(&"web1".to_string()));
        b.set_int(SessionColumn::Port, Some(22));
        b.set_bool_as_int(SessionColumn::LegacyAlgorithms, Some(true));
        assert_eq!(
            b.set_clause(),
            "name = $1, port = $2, legacy_algorithms = $3"
        );
        assert_eq!(b.where_placeholder(), "$4");
    }

    #[test]
    fn sqlite_placeholders_are_question_marks() {
        let mut b = UpdateBuilder::new(Dialect::Sqlite);
        b.set_text(SessionColumn::Name, Some(&"web1".to_string()));
        b.set_int(SessionColumn::Port, Some(22));
        assert_eq!(b.set_clause(), "name = ?, port = ?");
        assert_eq!(b.where_placeholder(), "?");
    }

    #[test]
    fn typed_bind_values_preserve_type() {
        let mut b = UpdateBuilder::new(Dialect::Postgres);
        b.set_text(SessionColumn::Name, Some(&"web1".to_string()));
        b.set_int(SessionColumn::Port, Some(22));
        b.set_bool_as_int(SessionColumn::LegacyAlgorithms, Some(true));
        // Reuse a column purely to assert that false binds as 0.
        b.set_bool_as_int(SessionColumn::LegacyAlgorithms, Some(false));
        assert_eq!(
            b.values(),
            &[
                BindVal::Text("web1".to_string()),
                BindVal::Int(22),
                BindVal::Int(1),
                BindVal::Int(0),
            ]
        );
    }

    #[test]
    fn optional_uuid_three_state() {
        // Skip
        let mut b = UpdateBuilder::new(Dialect::Postgres);
        b.set_optional_uuid(SessionColumn::JumpHostId, None);
        assert!(b.is_empty());

        // Set to NULL
        let mut b = UpdateBuilder::new(Dialect::Postgres);
        b.set_optional_uuid(SessionColumn::JumpHostId, Some(&None));
        assert_eq!(b.set_clause(), "jump_host_id = $1");
        assert_eq!(b.values(), &[BindVal::Null]);

        // Set to a real UUID
        let mut b = UpdateBuilder::new(Dialect::Postgres);
        let u = Uuid::nil();
        b.set_optional_uuid(SessionColumn::JumpHostId, Some(&Some(u)));
        assert_eq!(b.set_clause(), "jump_host_id = $1");
        assert_eq!(b.values(), &[BindVal::Text(u.to_string())]);
    }

    #[test]
    fn full_session_update_layout() {
        // Exercise the full happy path: every column type in one query.
        let mut b = UpdateBuilder::new(Dialect::Postgres);
        b.set_text(SessionColumn::Name, Some(&"web1".to_string()));
        b.set_int(SessionColumn::Port, Some(22));
        b.set_optional_uuid(SessionColumn::JumpHostId, Some(&None));
        b.set_bool_as_int(SessionColumn::LegacyAlgorithms, Some(true));

        assert_eq!(
            b.set_clause(),
            "name = $1, port = $2, jump_host_id = $3, legacy_algorithms = $4"
        );
        assert_eq!(b.where_placeholder(), "$5");
        assert_eq!(
            b.values(),
            &[
                BindVal::Text("web1".to_string()),
                BindVal::Int(22),
                BindVal::Null,
                BindVal::Int(1),
            ]
        );
    }
}
