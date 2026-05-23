# ShellStation

## Cross-Platform Terminal Manager & SSH/Telnet Client

**Project Design Document**
**Version 1.1 — March 2026**

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [Problem Analysis](#2-problem-analysis)
3. [Technology Stack](#3-technology-stack)
4. [Application Architecture](#4-application-architecture)
   - [4.5 PostgreSQL Multi-User Setup (RLS)](#45-postgresql-multi-user-setup-row-level-security)
   - [4.6 PostgreSQL Administration Guide](ADMIN_GUIDE.md)
5. [Feature Specifications](#5-feature-specifications)
6. [Project Structure](#6-project-structure)
7. [Development Roadmap](#7-development-roadmap)
8. [Key Rust Crate Dependencies](#8-key-rust-crate-dependencies)
9. [Quality Standards](#9-quality-standards)
10. [Future Considerations](#10-future-considerations)

---

## 1. Executive Summary

ShellStation is a cross-platform, open-source terminal manager and SSH/Telnet client designed to replace tools like mRemoteNG and SecureCRT. It addresses the critical shortcomings of both: mRemoteNG is Windows-only and relies on XML storage that degrades with large session datasets; SecureCRT is proprietary, expensive, and shares the same XML scaling limitation.

ShellStation provides a native terminal emulator with built-in SSH and Telnet support (no external PuTTY dependency), a scalable database backend supporting both local SQLite and centralized PostgreSQL deployments, full SSH jump host (ProxyJump) chaining, and a command broadcast system for multi-session automation.

---

## 2. Problem Analysis

### 2.1 mRemoteNG Limitations

- Windows-only with no cross-platform support.
- XML-based session storage causes severe performance degradation beyond a few hundred sessions (full DOM parsing on every load, no indexing, file corruption risk).
- Depends on PuTTY as an external embedded process for SSH, limiting integration depth and creating a fragile coupling.
- No multi-user or shared session database capability.

### 2.2 SecureCRT Limitations

- Requires a commercial license (approximately $100+ per seat).
- XML-based session storage shares the same scaling issues as mRemoteNG.
- Closed-source with no extensibility or community-driven development.
- Jump host configuration is cumbersome and limited to its own UI abstractions.

### 2.3 Core Requirements Derived

| Requirement                  | Rationale                                                            |
| ---------------------------- | -------------------------------------------------------------------- |
| Cross-platform               | Eliminates Windows lock-in; supports Linux, macOS, Windows equally.  |
| Scalable storage             | SQLite for single-user, PostgreSQL for teams; replaces XML entirely. |
| Built-in terminal/SSH/Telnet | No external PuTTY or OpenSSH process dependency.                     |
| SSH jump host chaining       | First-class support for ProxyJump with arbitrary hop depth.          |
| Command broadcast            | Send predefined command sets to one or multiple active sessions.     |
| Multi-user shared DB         | Central PostgreSQL instance allows team-wide session sharing.        |

---

## 3. Technology Stack

### 3.1 Stack Overview

| Layer              | Technology               | Purpose                                                                                                                     |
| ------------------ | ------------------------ | --------------------------------------------------------------------------------------------------------------------------- |
| Application Shell  | Tauri 2.x                | Cross-platform native app container; Rust backend, webview frontend. Produces small binaries without bundling Chromium.     |
| Backend Language   | Rust                     | Memory-safe, high-performance systems language. Handles SSH, DB, and IPC.                                                   |
| Frontend Framework | React + TypeScript       | Mature ecosystem for building complex UI with strong type safety.                                                           |
| Terminal Emulator  | xterm.js                 | Battle-tested terminal emulator used by VS Code, Theia, and others. Supports ANSI, mouse events, ligatures, WebGL renderer. |
| SSH Library        | russh                    | Pure Rust SSH2 implementation with native channel forwarding for jump hosts. Replaces Paramiko.                             |
| Local Database     | SQLite (via rusqlite)    | Zero-configuration embedded DB for single-user deployments.                                                                 |
| Central Database   | PostgreSQL (via sqlx)    | Scalable relational DB for multi-user/team shared session management.                                                       |
| ORM / Query Layer  | sqlx (compile-time)      | Async, compile-time verified SQL queries. Same crate supports both SQLite and PostgreSQL.                                   |
| Build System       | Cargo + Vite             | Cargo for Rust backend; Vite for fast frontend bundling with HMR.                                                           |
| Linting / Quality  | clippy, eslint, prettier | Zero-warning policy across both backend and frontend code.                                                                  |

### 3.2 Why Tauri + Rust over Alternatives

Several alternative stacks were considered and rejected for specific reasons:

**Python + PyQt6:** While Python is familiar and PyQt is cross-platform, terminal emulation in Python/Qt is limited. Paramiko's jump host implementation requires manual channel forwarding with fragile socket plumbing. Python's GIL also complicates concurrent session handling.

**Electron + Node.js:** Electron bundles Chromium, producing 150+ MB binaries and consuming significant RAM per window. While xterm.js and ssh2 (Node) would work, the resource overhead is excessive for a tool that may run dozens of sessions.

**Tauri 2.x + Rust:** Uses the system's native webview (WebKit on macOS/Linux, WebView2 on Windows), producing binaries under 10 MB. The Rust backend provides memory safety, true concurrency via async/await (Tokio), and access to russh, which implements SSH channel forwarding natively — eliminating the jump host issue entirely.

### 3.3 SSH Jump Host Architecture (Solving the Paramiko Problem)

Jump host support with Paramiko requires opening a direct-tcpip channel through the bastion host and then layering a new SSH session over that channel. Paramiko exposes this capability, but requires manual socket management, error handling for each hop, and careful lifecycle coordination — all of which prove brittle.

The russh crate solves this architecturally. Its connection model natively supports opening forwarded channels, and because Rust's ownership model enforces correct lifetime management, the channel-over-channel layering is both safe and ergonomic. The implementation pattern is:

1. Establish an SSH session to the bastion (jump host) using `russh::client::connect()`.
2. Open a direct-tcpip channel through the bastion to the target host's SSH port.
3. Layer a new russh SSH session over the forwarded channel to authenticate with the target.
4. For multi-hop chains (A → B → C → D), repeat the pattern recursively. Each hop produces a channel that becomes the transport for the next session.

This model supports arbitrary hop depth, and because each session object owns its channel, Rust's drop semantics ensure clean teardown in reverse order when a connection is closed.

---

## 4. Application Architecture

### 4.1 High-Level Component Diagram

The application follows a clean separation between the Tauri/Rust backend and the React/TypeScript frontend, communicating via Tauri's IPC command system.

| Frontend (WebView)                                                | Backend (Rust / Tauri)                                       |
| ----------------------------------------------------------------- | ------------------------------------------------------------ |
| React UI shell with tab management, session tree, settings panels | Tauri app lifecycle, window management, IPC command handlers |
| xterm.js terminal instances (one per session tab)                 | SSH connection manager (russh sessions, channel pool)        |
| Command palette and broadcast UI                                  | Database abstraction layer (sqlx → SQLite or PostgreSQL)     |
| Session import/export wizards                                     | Credential store (OS keychain via keyring-rs)                |

Communication flow: The frontend invokes Tauri commands (e.g., `ssh_connect`, `session_create`, `broadcast_command`) via the `@tauri-apps/api`. The Rust backend processes these asynchronously and streams terminal output back to xterm.js instances via Tauri events.

### 4.2 Database Schema (Core Tables)

The schema is designed to be identical across SQLite and PostgreSQL, using only standard SQL types. Migrations are managed via sqlx-cli.

| Table          | Key Columns                                                                              | Types                                                        | Notes                                                                                                                    |
| -------------- | ---------------------------------------------------------------------------------------- | ------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------ |
| `folders`      | id, name, parent_id                                                                      | UUID, TEXT, UUID?                                            | Hierarchical folder tree for organizing sessions. Self-referencing parent_id for nesting.                                |
| `sessions`     | id, folder_id, name, hostname, port, protocol, username, auth_method, jump_host_id, tags | UUID, UUID, TEXT, TEXT, INT, ENUM, TEXT, ENUM, UUID?, TEXT[] | Core session definitions. jump_host_id references another session to use as bastion. Tags enable flexible search/filter. |
| `credentials`  | id, session_id, auth_type, keychain_ref                                                  | UUID, UUID, ENUM, TEXT                                       | Credential metadata. Actual secrets stored in OS keychain (via keyring-rs), not in DB.                                   |
| `command_sets` | id, name, description                                                                    | UUID, TEXT, TEXT                                             | Named groups of predefined commands for broadcast.                                                                       |
| `commands`     | id, set_id, label, command_text, sort_order                                              | UUID, UUID, TEXT, TEXT, INT                                  | Individual commands within a set. sort_order controls execution sequence.                                                |
| `session_logs` | id, session_id, started_at, ended_at, log_path                                           | UUID, UUID, TIMESTAMP, TIMESTAMP?, TEXT                      | Optional session logging metadata. Actual log content written to files, not DB.                                          |

**Indexing strategy:** `sessions.hostname`, `sessions.tags` (GIN index on PostgreSQL, JSON index on SQLite), and `folders.parent_id` are indexed. With proper indexing, this schema handles tens of thousands of sessions without perceptible latency on either backend.

### 4.3 Database Backend Switching

The sqlx crate supports compile-time query verification against both SQLite and PostgreSQL. The application uses a Rust trait (`DatabaseProvider`) to abstract the connection pool, with concrete implementations for each backend. At startup, a configuration file determines which backend to use:

- **Local mode (default):** SQLite database file stored alongside the application config directory (`~/.config/shellstation/sessions.db` or platform equivalent).
- **Server mode:** PostgreSQL connection string configured in settings. Enables multi-user access with row-level locking for concurrent session edits.

Switching between modes is a settings change with a one-time migration/import step. An export/import command handles data transfer between backends.

### 4.4 Credential Security

Passwords and private key passphrases are never stored in the database. Instead, the application uses keyring-rs to interface with the operating system's native credential store: Keychain on macOS, Secret Service (GNOME Keyring / KWallet) on Linux, and Windows Credential Manager on Windows. The database stores only a reference identifier that maps to the keychain entry. SSH private keys are referenced by file path, with optional passphrase storage in the keychain.

### 4.5 PostgreSQL Multi-User Setup (Row-Level Security)

When using PostgreSQL as the backend, ShellStation enforces **Row-Level Security (RLS)** so that multiple users can share a single database while maintaining data isolation. Each user connects with their own PostgreSQL role, and RLS policies control what they can see and modify.

#### 4.5.1 How RLS Works

Every `folders` and `sessions` row has two columns added by the RLS migration:

| Column       | Type | Purpose                                                         |
| ------------ | ---- | --------------------------------------------------------------- |
| `owner`      | TEXT | The PostgreSQL role name (`current_user`) that created the row. |
| `visibility` | TEXT | `'personal'` (default) or `'shared'`.                           |

**RLS policies enforced per table:**

| Policy            | Operation | Rule                                             |
| ----------------- | --------- | ------------------------------------------------ |
| `*_owner_all`     | ALL       | Owner can read, insert, update, delete own rows. |
| `*_shared_read`   | SELECT    | Any user can read shared rows.                   |
| `*_shared_update` | UPDATE    | Any user can update shared rows.                 |
| `*_shared_delete` | DELETE    | Only the owner can delete shared rows.           |

`FORCE ROW LEVEL SECURITY` is enabled, meaning policies apply even to the table owner — no role bypasses RLS through ownership alone.

**Per-user credential mapping:** The `session_credentials` table maps each user's credential profile to shared sessions. When user A shares a session with user B, each user connects with their own credentials — the session definition is shared, the authentication is not.

#### 4.5.2 Permission Model

ShellStation's startup runs `sqlx::migrate!()` and `setup_postgres_rls()` on every launch. These operations are idempotent, but they require different privilege levels depending on whether the schema already exists. There are two valid approaches to structuring permissions:

**Option A — Separated roles (recommended for larger teams):**

An admin role owns the schema; regular users get DML only. This follows the principle of least privilege.

- **Admin role (table owner):** Required for the **first startup** and whenever a **new app version adds database migrations**. Needs full schema control: `CREATE TABLE`, `ALTER TABLE`, `CREATE POLICY`, plus DML on all tables.
- **Regular user roles:** Sufficient for **everyday use** after the admin has initialized the schema. Needs only `SELECT`, `INSERT`, `UPDATE`, `DELETE` on all application tables, plus `SELECT` on `_sqlx_migrations` (so `sqlx::migrate!()` can verify all migrations are applied and return without error).

When a regular user starts the app, `sqlx::migrate!()` sees all migrations are applied and does nothing. `setup_postgres_rls()` may fail on the `ALTER TABLE` statements (insufficient privileges), but this failure is **logged and non-fatal** — the policies already exist from the admin's initial run.

The trade-off: when a new ShellStation version introduces schema changes, the admin must connect first so that new migrations apply. After that, regular users can connect normally. See section 4.6.5 for the update procedure.

**Option B — All users can migrate (simpler for small teams):**

Every user gets schema modification rights. Any user who launches a new app version automatically applies pending migrations — no admin coordination needed.

- Each user role gets: `CREATE ON SCHEMA public`, `ALTER TABLE`, `CREATE POLICY`, plus DML on all tables.
- The first user to start the new version runs the migration; subsequent users see it's already applied.

The trade-off: every user has the privileges to modify or drop tables, not just read and write data. This is acceptable for small, trusted teams but inappropriate in environments where you want to limit who can alter the schema.

### 4.6 PostgreSQL Administration Guide

For step-by-step instructions on deploying ShellStation with a shared PostgreSQL backend — including database setup, user management, updates, and backups — see the dedicated **[PostgreSQL Administration Guide](ADMIN_GUIDE.md)**.

---

## 5. Feature Specifications

### 5.1 Terminal Emulator

- **Rendering:** xterm.js with WebGL addon for GPU-accelerated rendering. Falls back to canvas renderer if WebGL is unavailable.
- **Tabs:** Each session opens in its own tab within a tabbed pane. Tabs can be reordered, pinned, and split horizontally or vertically.
- **Theming:** Ships with a default dark theme. Supports custom color schemes via a JSON theme file (compatible with popular terminal theme formats).
- **Search:** Built-in text search within terminal scrollback buffer (xterm.js search addon).
- **Copy/Paste:** Standard Ctrl+Shift+C / Ctrl+Shift+V with optional auto-copy on selection.
- **Font:** Configurable font family and size with ligature support (xterm.js ligature addon).

### 5.2 SSH Connection Management

- **Protocols:** SSH2 (via russh) and Telnet (RFC 854, with NAWS window size negotiation). Future consideration for serial as a separate protocol handler.
- **Authentication methods:** Password, public key (RSA, Ed25519, ECDSA), keyboard-interactive, and agent forwarding.
- **Jump host chaining:** Any saved session can be designated as a jump host for another session. Chains of arbitrary depth are supported (A → B → C → target).
- **Connection pooling:** Optionally keep bastion connections alive for reuse when opening multiple sessions through the same jump host.
- **Known hosts:** Maintains its own known_hosts file with TOFU (Trust On First Use) prompting, or can use the system's `~/.ssh/known_hosts`.
- **Keep-alive:** Configurable SSH keepalive interval to prevent idle disconnects.

### 5.3 Session Management

- **Folder hierarchy:** Unlimited nesting depth for organizing sessions into groups (e.g., Production → EU → Web Servers).
- **Search and filter:** Full-text search across session name, hostname, tags, and folder path. Tag-based filtering for quick access.
- **Quick connect:** A shortcut bar for connecting without saving a session (hostname + optional user/port).
- **Import:** Bulk import from mRemoteNG XML (`confCons.xml`) and SecureCRT XML session files. CSV import for generic datasets.
- **Export:** Export session data (without credentials) as JSON or CSV for backup and sharing.

### 5.4 Command Broadcast System

The command broadcast feature allows sending predefined commands to one or more active sessions simultaneously. This is the equivalent of SecureCRT's "Chat Window" and "Send Commands to All Sessions" functionality.

- **Command sets:** Named collections of commands, each with a label, the command text, and an execution order.
- **Target selection:** Choose target sessions from a list of currently connected sessions, or select an entire folder of sessions.
- **Execution modes:** Sequential (wait for prompt before sending next command) or parallel (fire-and-forget to all targets).
- **Ad-hoc broadcast:** Type a one-off command in the broadcast bar and send it to all selected sessions without saving it as a command set.
- **Variable substitution:** Commands can contain placeholders (e.g., `{hostname}`, `{username}`) that are resolved per-session at execution time.

### 5.5 Session Logging

- Optional per-session logging to local files with configurable log directory.
- **Formats:** Raw terminal output or stripped plain text (ANSI escape codes removed).
- **Rotation:** Configurable max log size with automatic rotation.

---

## 6. Project Structure

The repository follows standard Tauri 2.x conventions with a clear separation of concerns:

| Path                         | Contents                                                                       |
| ---------------------------- | ------------------------------------------------------------------------------ |
| `src-tauri/src/main.rs`      | Tauri application entry point and command registration.                        |
| `src-tauri/src/ssh/`         | SSH connection manager, jump host logic, channel handling (russh).             |
| `src-tauri/src/db/`          | Database abstraction trait, SQLite and PostgreSQL implementations, migrations. |
| `src-tauri/src/commands/`    | Tauri IPC command handlers (session CRUD, connect, broadcast, etc.).           |
| `src-tauri/src/credentials/` | Keychain integration via keyring-rs.                                           |
| `src-tauri/src/import/`      | Parsers for mRemoteNG XML, SecureCRT XML, and CSV import.                      |
| `src/components/`            | React UI components: SessionTree, TerminalTabs, CommandPalette, Settings.      |
| `src/hooks/`                 | Custom React hooks for Tauri IPC, terminal lifecycle, and state management.    |
| `src/stores/`                | Zustand state stores for sessions, connections, UI state.                      |
| `migrations/`                | sqlx migration files (shared between SQLite and PostgreSQL).                   |

---

## 7. Development Roadmap

### 7.1 Phase 1 — Foundation (Weeks 1–4)

**Goal:** A working Tauri app that can open a local terminal tab with xterm.js.

1. Initialize Tauri 2.x project with React + TypeScript + Vite frontend.
2. Integrate xterm.js with WebGL addon; verify terminal I/O via Tauri commands.
3. Implement local PTY (pseudo-terminal) spawning on the Rust side for local shell sessions.
4. Set up CI pipeline: `cargo clippy` (zero warnings), eslint + prettier, `cargo test`, `npm test`.

### 7.2 Phase 2 — SSH Core (Weeks 5–8)

**Goal:** Connect to a remote host via SSH and interact through xterm.js.

1. Integrate russh for SSH2 connections (password and public key auth).
2. Implement the SSH session manager: connect, disconnect, reconnect, keepalive.
3. Wire SSH session I/O to xterm.js via Tauri event streaming.
4. Implement jump host chaining (direct-tcpip channel forwarding through bastion).
5. Test multi-hop scenarios (2-hop and 3-hop chains) with real infrastructure.

### 7.3 Phase 3 — Database & Session Management (Weeks 9–12)

**Goal:** Persistent session storage with a full CRUD UI.

1. Design and implement the database schema with sqlx migrations.
2. Implement `DatabaseProvider` trait with SQLite backend.
3. Build the session tree UI component (folders, drag-and-drop reordering, context menus).
4. Implement session CRUD operations via Tauri commands.
5. Add credential storage via keyring-rs with OS keychain integration.
6. Build quick-connect bar for ad-hoc connections.

### 7.4 Phase 4 — PostgreSQL & Multi-User (Weeks 13–16)

**Goal:** Optional PostgreSQL backend for team use.

1. Implement PostgreSQL backend for `DatabaseProvider` trait.
2. Add database backend selection in settings with connection string configuration.
3. Implement data migration tool (SQLite ↔ PostgreSQL and reverse).
4. Add row-level locking for concurrent session edits in PostgreSQL mode.
5. Test with multiple simultaneous client instances against a shared database.

### 7.5 Phase 5 — Command Broadcast & Polish (Weeks 17–20)

**Goal:** Command broadcast, import/export, and UI polish.

1. Implement command set CRUD (create, edit, delete named command groups).
2. Build broadcast UI: target selection, execution mode toggle, variable substitution.
3. Implement mRemoteNG and SecureCRT XML importers.
4. Add JSON import/export for generic session data.
5. Session logging with configurable format and rotation.

### 7.6 Phase 6 — Packaging & Release (Weeks 21–24)

**Goal:** Production-ready installers for all platforms.

1. Tauri bundler configuration for Windows (.msi/.exe), macOS (.dmg), and Linux (.deb/.AppImage).
2. Code signing for Windows and macOS.
3. Automated release pipeline via GitHub Actions (build, test, package, publish).
4. Write user documentation and README.
5. Beta testing with real-world session datasets (1000+ sessions).

---

## 8. Key Rust Crate Dependencies

| Crate                  | Version | Purpose                                                       |
| ---------------------- | ------- | ------------------------------------------------------------- |
| `tauri`                | 2.x     | Application shell, IPC, window management, bundling.          |
| `russh`                | 0.60+   | SSH2 client with native channel forwarding and key parsing.   |
| `sqlx`                 | 0.8+    | Async database driver with compile-time query checks.         |
| `tokio`                | 1.x     | Async runtime for concurrent SSH sessions and DB operations.  |
| `keyring`              | 3.x     | Cross-platform OS keychain access for credential storage.     |
| `serde` / `serde_json` | 1.x     | Serialization for IPC, config files, and import/export.       |
| `uuid`                 | 1.x     | UUID v4 generation for all entity primary keys.               |
| `portable-pty`         | 0.8+    | Cross-platform pseudo-terminal spawning for local shell tabs. |
| `tracing`              | 0.1+    | Structured logging and diagnostics.                           |

---

## 9. Quality Standards

ShellStation enforces a zero-warning, zero-lint-error policy across the entire codebase:

- **Rust:** `cargo clippy -- -D warnings` (all warnings treated as errors). No `unsafe` blocks without documented justification and a safety comment.
- **TypeScript:** Strict mode enabled, eslint with recommended + React hooks rules, prettier for formatting. Zero eslint errors or warnings.
- **SQL:** All queries compile-time verified by sqlx. No raw string SQL.
- **Testing:** Unit tests for all backend modules (`cargo test`), component tests for React UI (vitest + testing-library), integration tests for SSH connection scenarios.
- **CI enforcement:** All checks run on every pull request. Merge is blocked on any failure.

---

## 10. Future Considerations

The following features are explicitly out of scope for the initial release but are architecturally accounted for in the design:

- Serial port protocol support (additional protocol handler behind the same trait interface).
- SFTP/SCP file transfer panel integrated into session tabs.
- Plugin system for custom protocol handlers and UI extensions.
- Session recording and playback (asciinema-compatible format).
- LDAP/SSO authentication for PostgreSQL multi-user deployments.
- Mobile companion app for emergency access to saved sessions.
