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
   - [4.7 App Updater](#47-app-updater)
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
| `*_shared_update` | UPDATE    | Any user can update shared rows (but see below). |
| `*_shared_delete` | DELETE    | Only the owner can delete shared rows.           |

`FORCE ROW LEVEL SECURITY` is enabled, meaning policies apply even to the table owner — no role bypasses RLS through ownership alone.

Apart from its owner and its place in the folder tree, a shared row stays editable by every user at the database level, including the hostname, port, username and jump host. The app's own edit commands require ownership, but a user with direct SQL access is not bound by them. A shared session is therefore only as trustworthy as the least trusted member of the team.

Two trigger functions keep a shared row with its owner, and where its owner put it. PostgreSQL accepts an updated row that passes any permissive policy. Without the triggers, any user could get around the owner-only delete rule in two ways:

- **Taking ownership.** Setting yourself as the owner of a shared row passes `*_owner_all`. A policy's `WITH CHECK` cannot prevent this, because it sees only the new row.
- **Moving the row.** Moving a shared row under a folder of your own takes it out of everyone else's folder tree. Deleting that folder then deletes the row too, because the foreign keys cascade and a cascade is not subject to RLS.

The two functions:

- **`shellstation_keep_owner`** runs as `folders_keep_owner` and `sessions_keep_owner` and refuses any change of `owner`. Only a superuser or the table owner (the admin role, or a member of it) gets past it. `FORCE ROW LEVEL SECURITY` still limits the admin to its own and shared rows, so reassigning a removed user's personal rows takes a superuser ([ADMIN_GUIDE.md §4](ADMIN_GUIDE.md#4-removing-a-user)). The one other change allowed is that a user may claim a placeholder `'local'` row for themselves, which the startup fix-up does. RLS limits a user to the `'local'` rows that are shared. A role named `local`, though, would own all of them, so no role should be named that.
- **`shellstation_keep_place`** runs as `folders_keep_place` (on `parent_id`) and `sessions_keep_place` (on `folder_id`). It refuses a move by anyone other than the row's owner, the table owner or a superuser. The app's own move commands already require the user to own the row.

Both functions run as the updating user. Their `search_path` is pinned to `pg_catalog, pg_temp`, with the temporary schema last, and their catalog lookups are schema-qualified, so a temporary table cannot stand in for `pg_class`. These guarantees hold under Option A (§4.5.2). Option B users hold `TRIGGER` and `TRUNCATE` on the tables, and both reach past row-level security anyway: such a user can add a trigger of their own that rewrites `owner`, or empty a table outright.

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

The trade-off: every user has the privileges to modify or drop tables, not just read and write data. Their table privileges also include `TRIGGER` and `TRUNCATE`, which reach past row-level security (§4.5.1). This is acceptable for small, trusted teams but inappropriate in environments where you want to limit who can alter the schema.

### 4.6 PostgreSQL Administration Guide

For step-by-step instructions on deploying ShellStation with a shared PostgreSQL backend — including database setup, user management, updates, and backups — see the dedicated **[PostgreSQL Administration Guide](ADMIN_GUIDE.md)**.

### 4.7 App Updater

ShellStation ships its own in-app updater built on `tauri-plugin-updater`. The goal is to make the upgrade path a single click for end users without sacrificing transport-level or supply-chain safety. The model has three trust anchors and a clear failure isolation policy.

#### 4.7.1 Trust model

| Anchor                          | Location                                                                       | Owner                                                 |
| ------------------------------- | ------------------------------------------------------------------------------ | ----------------------------------------------------- |
| Ed25519 **public key**          | Compiled into the app binary via `tauri.conf.json` → `plugins.updater.pubkey`. | Embedded at build time; cannot be changed at runtime. |
| Ed25519 **private key**         | `TAURI_SIGNING_PRIVATE_KEY` secret on the CI runner.                           | Project maintainer only.                              |
| Update manifest (`latest.json`) | Published on the GitHub Releases page next to each signed artifact.            | Anyone with push access to the release.               |

Every artifact the updater installs must validate against the embedded public key. A signed manifest is **not** required — the manifest is just a directory; what matters is that the artifact URL it points at carries a valid `.sig` produced by the held private key. An attacker who can replace `latest.json` (e.g. by compromising the GitHub release) cannot make the updater install anything unless they also hold the private key.

#### 4.7.2 Endpoint and manifest

The plugin is configured with a single endpoint:

```
https://github.com/TheFlipside/shellstation/releases/latest/download/latest.json
```

GitHub's `/releases/latest/download/<file>` URL is a 302 to whichever release is tagged "Latest". Re-tagging a previous release as "Latest" is therefore the rollback procedure.

`latest.json` has the format defined by `tauri-plugin-updater` 2.x:

```json
{
  "version": "0.21.0",
  "notes": "See https://github.com/.../releases/tag/v0.21.0 for release notes",
  "pub_date": "2026-06-15T12:00:00Z",
  "platforms": {
    "linux-x86_64": { "signature": "<minisign blob>", "url": "<artifact url>" },
    "windows-x86_64": { "signature": "<minisign blob>", "url": "<artifact url>" },
    "darwin-x86_64": { "signature": "<minisign blob>", "url": "<artifact url>" },
    "darwin-aarch64": { "signature": "<minisign blob>", "url": "<artifact url>" }
  }
}
```

A platform entry is **omitted entirely** when its build was unsigned (signing secret missing on a dev push). The plugin treats a missing platform as "no compatible update" rather than an error.

#### 4.7.3 Per-platform artifacts and install strategy

| Platform               | First-install bundle | Updater bundle                                      | Install mode                                                                                                   |
| ---------------------- | -------------------- | --------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| Linux x86_64           | `.deb`               | `.AppImage` (the AppImage replaces itself in place) | Direct file replacement, then `relaunch()`.                                                                    |
| Windows x86_64         | `.msi` / NSIS `.exe` | NSIS `.exe`                                         | `installMode: passive` — installer runs without user interaction, then exits and the new binary is relaunched. |
| macOS x86_64 / aarch64 | `.dmg`               | `.app.tar.gz`                                       | The `.app` bundle at the running path is replaced atomically; `relaunch()` swaps the running process.          |

The DMG is only the first-install path on macOS — the updater never downloads a DMG. Conversely, AppImage and NSIS `.exe` serve double duty as both first-install and updater artifacts because they are self-contained.

#### 4.7.4 Auto-check and manual triggers

The frontend mounts `UpdateDialog.tsx` once at the app root. On mount it runs a background check if the `autoCheckForUpdates` setting is on (default `true`); a single `useRef` guard ensures the check fires at most once per app session, including under React StrictMode's double-mount. The native Help menu's "Check for Updates…" item fires a Tauri event (`updater:check-requested`) that the dialog listens for to run a manual check; the difference between the two flows is purely UX:

- **Auto-check** that fails (offline, transient DNS, unsigned dev release) logs to the devtools console and goes silent.
- **Manual check** that fails shows a toast and a generic error dialog; the raw plugin error is never rendered to the user because it can leak the endpoint URL or resolver hostnames.

#### 4.7.5 IPC surface

The webview is granted only two `updater:*` permissions: `allow-check` and `allow-download-and-install`. The standalone `allow-download` and `allow-install` are intentionally excluded — they expose a download-without-immediate-install flow that has no UX in this app and would increase the IPC surface area for a hypothetical future webview-side attacker. The `process:*` grant is similarly narrowed to `allow-restart` only; `allow-exit` is not needed and is not granted. The opener plugin gets no grant at all: the Help menu opens its links from Rust, and the plugin's injected link-click handler is turned off.

The `check` IPC accepts an optional `proxy` parameter from the webview. The shipping JS never passes one. A future XSS in the webview could theoretically use this to redirect update checks through an attacker-controlled proxy (SSRF / fingerprinting). The mitigation today is that ShellStation has no HTML-rendering surface: terminal output goes through xterm.js's canvas/WebGL renderer (not HTML), the CSP is tight (`script-src 'self'`), and translation strings are rendered as React text nodes. If a future feature ever introduces an HTML sink for untrusted content, the proxy SSRF should be closed by wrapping `check()` in a Rust-side IPC command that doesn't forward the proxy parameter.

#### 4.7.6 CI integration

The release pipeline (`.forgejo/workflows/release.yml`) carries the practical infrastructure:

- A `preflight` job grep-checks `tauri.conf.json` for the placeholder pubkey string and aborts the entire release if the developer forgot to replace it. This prevents a release shipping with a pubkey that can't verify any signature, which would surface as a confusing "Update failed" error on every check.
- Each build job (`build-linux`, `build-windows`, `build-macos-x64`, `build-macos-arm64`) receives the signing secrets as environment variables. Tauri's bundler emits `.sig` files alongside each platform's updater bundle.
- The `publish-release` job runs `.forgejo/scripts/generate_latest_json.py`, which composes `latest.json` from the collected `.sig` files: missing signatures skip their platform block instead of failing the whole step.
- The manifest is uploaded to both the Forgejo and the GitHub release; only the GitHub one is what the running app sees.

#### 4.7.7 Failure isolation

| Failure                                   | What happens                                                                                            |
| ----------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| Manifest URL unreachable                  | Auto-check: silent log. Manual: error toast.                                                            |
| Manifest fetched but no platform match    | Plugin returns `Err(TargetNotFound)`. Auto-check: silent log. Manual: error toast.                      |
| Artifact downloaded but signature invalid | Plugin returns `Err`. Install is aborted; running binary is untouched.                                  |
| Download interrupted mid-stream           | Plugin returns `Err` from `downloadAndInstall`. Same outcome as above; partial files are discarded.     |
| Plugin error contains URL / hostname      | Never surfaced in UI; user sees the generic `updates.errorUnknown` string. Detail goes to console only. |
| Placeholder pubkey shipped (regression)   | `preflight` CI job rejects the tag push; no release artifacts are built or uploaded.                    |

For one-time setup, key generation, and rotation procedures, see **[DEVELOPMENT.md — App updater signing keys](DEVELOPMENT.md#app-updater-signing-keys)**.

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
- **Known hosts:** Uses the system's `~/.ssh/known_hosts`, which it shares with OpenSSH, with TOFU (Trust On First Use) prompting. ShellStation reads the file itself, as OpenSSH does: a line it cannot read costs only that line. The prompt tells a first connection from a changed key, a host on record only with another key type, and a file it could not check. Anything but a first connection is shown as a warning and takes a click to accept. A key listed under `@revoked` is refused without asking.
- **Keep-alive:** Configurable SSH keepalive interval to prevent idle disconnects.

### 5.3 Session Management

- **Folder hierarchy:** Unlimited nesting depth for organizing sessions into groups (e.g., Production → EU → Web Servers).
- **Search and filter:** Full-text search across session name, hostname, tags, and folder path. Tag-based filtering for quick access.
- **Quick connect:** A shortcut bar for connecting without saving a session (hostname + optional user/port).
- **Import:** Bulk import from mRemoteNG XML (`confCons.xml`) and SecureCRT XML session files. CSV import for generic datasets.
- **Export:** Export session data (without credentials) as JSON or CSV for backup and sharing.
- **Data export / import:** A single JSON file can carry any mix of the database (without secrets), the client settings and the command bar buttons; the user picks the sections on export, and import offers only the sections present in the file. Settings and buttons live in the webview's localStorage, so this is how they move to another machine together with a copied database. The exception is the connection settings (the private-address check, the connect timeout and the keepalive), which the backend keeps in its own config so that the webview cannot pass its own values when it connects; switching the private-address check off asks the user in a native dialog the backend opens. Since a file may come from someone else, import never weakens safety settings (it can switch them on but not off, and paste on right-click, copy on selection and closing a tab on disconnect off but not on), and it lists the buttons it would add with their commands before anything runs. The backend opens the save dialog itself, so an export can only write where the user chose.

### 5.4 Command Broadcast System

The command broadcast feature allows sending predefined commands to one or more active sessions simultaneously. This is the equivalent of SecureCRT's "Chat Window" and "Send Commands to All Sessions" functionality.

- **Command sets:** Named collections of commands, each with a label, the command text, and an execution order.
- **Target selection:** Choose target sessions from a list of currently connected sessions, or select an entire folder of sessions.
- **Execution modes:** Sequential (wait for prompt before sending next command) or parallel (fire-and-forget to all targets).
- **Ad-hoc broadcast:** Type a one-off command in the broadcast bar and send it to all selected sessions without saving it as a command set.
- **Variable substitution:** Commands can contain placeholders (e.g., `{hostname}`, `{username}`) that are resolved per-session at execution time.

### 5.5 Session Logging

- Optional per-session logging to local files with configurable log directory.
- **Where logs go:** A log holds what the remote host sends, so the webview cannot choose where it is written. A new log directory, like the application log's directory and the SQLite database file, is chosen in a dialog the backend opens. The backend words that dialog itself, in the operating system's language, and opens it in the current location. Saving accepts only the default, the path already configured, or a choice made in the last ten minutes. The filename format allows letters, digits, spaces, `- _ . ( )` and the placeholders, and must end in `.log` or `.txt`. A format in the config file that breaks these rules is replaced by the default at startup. A log whose name would be a Windows device name (`CON`, `COM1`, …) gets a `_` in front.
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
