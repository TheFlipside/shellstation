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

Only the main window may invoke these commands. They are listed in the app manifest, `src-tauri/permissions/app-commands.toml`, as one permission, `app-commands`, and `src-tauri/capabilities/default.json` grants it to the window `main` alone. Tauri checks an app's own commands against the capabilities only when the app has a manifest; without one, every webview the app opens can call all of them. With it, any other window or webview is refused, and so is a page that is not the app's own. A new command goes into both `generate_handler!` in `lib.rs` and the manifest, or the main window is refused it; a test in `lib.rs` fails when the two differ.

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

The per-user mapping tables are protected too, by a rule of their own (see **Per-user mappings** below). Every `folders` and `sessions` row has two columns added by the RLS migration:

| Column       | Type | Purpose                                                         |
| ------------ | ---- | --------------------------------------------------------------- |
| `owner`      | TEXT | The PostgreSQL role name (`current_user`) that created the row. |
| `visibility` | TEXT | `'personal'` (default) or `'shared'`.                           |

**RLS policies enforced per table:**

| Policy            | Operation | Rule                                             |
| ----------------- | --------- | ------------------------------------------------ |
| `*_owner_all`     | ALL       | Owner can read, insert, update, delete own rows. |
| `*_shared_read`   | SELECT    | Any user can read shared rows.                   |
| `*_shared_delete` | DELETE    | Only the owner can delete shared rows.           |

`FORCE ROW LEVEL SECURITY` is enabled, meaning policies apply even to the table owner — no role bypasses RLS through ownership alone.

A shared row is everyone's to read and its owner's alone to change. No policy lets another role update it, whatever the update sets: the host, port, user name and jump host of a shared session decide where everyone who opens it sends their credentials, and its switch for legacy algorithms decides how well that is protected on the way. Nor can another role lock it, since `SELECT … FOR UPDATE` and its weaker forms go by the policies for `UPDATE`. Versions before this rule had a policy `*_shared_update` that let any role update a shared row. The app never used it, since its commands change only the user's own rows, but a role with its own statements could. `setup_postgres_rls` drops it, in the same statement as the policies it creates.

What the policies cannot say is left to four trigger functions:

- **`shellstation_keep_owner`** runs as `folders_keep_owner` and `sessions_keep_owner` and refuses any change of `owner`, so that an owner cannot hand a row to someone else, which the policies refuse a regular user as well. Only a superuser or the table owner (the admin role, or a member of it) gets past it. `FORCE ROW LEVEL SECURITY` still limits the admin to changing its own rows, so reassigning a removed user's rows takes a superuser ([ADMIN_GUIDE.md §4](ADMIN_GUIDE.md#4-removing-a-user)). The one other change allowed is that a user may claim a placeholder `'local'` row for themselves, which the startup fix-up does at the first setup, before RLS is on. A role named `local`, though, would own every such row, so no role should be named that.
- **`shellstation_keep_place`** runs as `folders_keep_place` (on `parent_id`) and `sessions_keep_place` (on `folder_id`). It refuses a move by anyone other than the row's owner, the table owner or a superuser. The app's own move commands already require the user to own the row.
- **`shellstation_own_folder`** runs as `folders_own_folder` and `sessions_own_folder`, for new rows and for moves. It refuses to put a row into a folder that is not the role's own; the top level has no folder to check. A new row has only its `owner` checked by the policies, and the foreign key to its folder is checked without RLS, so anyone could otherwise put a session of their own into another user's shared folder, where it shows among that user's. Every folder above must be the role's own as well, up to the top, and one the role cannot see is not: a folder of the role's that already sits in another user's, from before this rule, would otherwise stay a way into that user's tree. Such a folder can still be changed where it is, and moved out. The same look up the tree refuses to put a folder into itself or below itself. The table owner, its members and superusers get past the function. The app's own commands already require the folder to be the user's.
- **`shellstation_own_jump_host`** runs as `sessions_own_jump_host`, for new rows and for a change of `jump_host_id`. It refuses to name a session as a jump host that is not the role's own. The policies check a row's `owner` and nothing the row refers to, and the foreign key is checked without RLS, so the owner of a shared session could otherwise name any session by its ID, another user's personal one included. Whoever then opened the shared session and had a profile for that jump host, as its owner has, went through it: their own bastion opened a connection to the host and port that the other user's session names. The owner of a jump host could also delete it, and the foreign key then left the session that named it without one, to connect directly. And the foreign key's answer alone told whether a session with a given ID exists; the function answers first, and the same for a session that is not there as for one that is not the role's. (The primary key still tells: a new row under an ID that is taken is refused by it, whoever can see the row of that ID. So does the foreign key of the central `credentials` table, which has no row-level security.) A chain of jump hosts named under this rule therefore has one owner, who alone changes any part of it. An update that leaves the jump host as it was is let through, so a session that named another user's before this rule can still be changed, and taking a jump host away is always allowed. The table owner, its members and superusers get past the function. The app's own commands refuse such a jump host themselves. Its dialogs offer only the user's own sessions; a jump host that is set stays shown, and a copy of a session starts without one that is another user's. The data import leaves a new session without a jump host that it would be refused, and says so in its summary; the other imports report it as a warning.

Connecting holds a chain of jump hosts to the same rule, link by link: in PostgreSQL mode `session_connect` opens no connection through a jump host whose owner is not the owner of the session that names it, and says which session that is. The refusal comes before that jump host's profile is looked up and before anything is connected to. This is for what was named before the rule, and for a database where the trigger is not in place yet or does not hold a user (Option B). It goes by the `owner` of the two rows, and so holds as far as that does: a user who can write another owner into a row of their own, as under Option B, gets past it too. Rows that still have the placeholder `'local'` count as one owner's among themselves. One thing is left of such an earlier jump host: it can still be deleted, by its owner or by an admin who removes that user's sessions, and the session that named it, refused until then, connects directly from then on, without anyone being told. The setup has no step for that: refusing the deletion would let one user's session keep another user from deleting their own. The session's owner closes it by picking a jump host of their own. [ADMIN_GUIDE.md §5](ADMIN_GUIDE.md#5-applying-application-updates) has the query that lists these sessions.

The first two were all that kept a shared row from being taken over, or moved away and deleted with a folder of the mover's, while `*_shared_update` let anyone update it. They stay for a database whose owning role has not started this version yet, and as a second line behind the policies.

The functions run as the updating user. Their `search_path` is pinned to `pg_catalog, pg_temp`, with the temporary schema last, their catalog lookups are schema-qualified, and `folders` and `sessions` are named with the schema of the table the trigger is on, so a temporary table cannot stand in for `pg_class`, for `folders` or for `sessions`.

The functions live in a schema of their own, `shellstation_rls`, which `setup_postgres_rls` creates for the role that owns the tables. No regular role can create anything in it, or name what is in it: a new schema grants nothing to anyone, making one takes `CREATE` on the database, which regular users do not have, and whatever anyone else has been granted on the schema is revoked at each start of the owning role. Whoever fires a trigger needs no privilege on the schema or the function. Earlier versions created the functions without a schema, which put them into `public`, where a role that may create functions (every role before PostgreSQL 15, and the users of Option B) could create one under the setup's name before the owning role's first start. `CREATE OR REPLACE FUNCTION` keeps a function's owner, so the function then stayed that role's to change, with the triggers calling it, or the setup failed at it and created none of that function's triggers. Each function is made the owning role's in the statement that replaces it, also when a member of that role or a superuser runs the setup. A schema of the name that belongs to another role is not used: the setup reports it and leaves functions and triggers as they are. It is handed to the owning role by a start that is allowed to: a superuser's, or one of a role that has the privileges of the schema's owner.

A trigger of one of the setup's names is left alone only if it calls the setup's function, is enabled, fires on the events and for the column it should, and has the argument and a condition, or none, as it should. What the condition says is not compared. Naming a function takes `USAGE` on its schema, so a trigger that calls one of the setup's was made with the owning role's privileges. Any other trigger of the name is replaced: one of an earlier version, and one that someone else made under the name. The function in `public` that a replaced trigger called is dropped with the last trigger that called it, if it has the name of that trigger's function and is the starting role's to drop. One that no trigger of the setup's called at that start stays, as does one whose last trigger that start did not get to.

The setup's own statements run with the session's `search_path`, which has `public` in it. `pg_catalog` is searched first, but PostgreSQL prefers an operator elsewhere on the path whose argument types match exactly to one in `pg_catalog` that needs a cast. A role that may create things in `public` could thus define an `=` for an `oid` and a `regclass`, and a comparison such as `tgrelid = 'folders'::regclass` would run it, as the owning role, and take its answer. The setup therefore makes no comparison for which `pg_catalog` lacks an operator of exactly its two types: the name of a table, function or schema is cast on to `oid`, and a number compared with an `oid` is written as one. The functions it calls are named with `pg_catalog`, apart from aggregates that `pg_catalog` has for exactly their argument.

These guarantees hold under Option A (§4.5.2). Option B users hold `TRIGGER` and `TRUNCATE` on the tables, and both reach past row-level security anyway: such a user can add a trigger of their own that rewrites `owner`, or empty a table outright.

A session row also has columns of its own for a credential profile and a login sequence. In PostgreSQL mode the session dialog leaves them empty and keeps each user's choice in the mapping tables below, but Bulk Edit writes a login sequence into the rows of the user's own sessions, and a statement of someone's own can write either. They are IDs in the connecting user's local database, so in PostgreSQL mode they count only for the session's owner. Anyone else connects by their own mapping, and without one by no profile and no login sequence: otherwise the owner of a shared session, who also chooses its host, would choose which of another user's secrets is sent there. The same goes for a jump host. In another user's shared session it is that user's session too (`shellstation_own_jump_host` above), so it needs the connecting user's own mapping, since a jump host is not connected to without a profile.

**Per-user mappings:** The `session_credentials` and `session_login_sequences` tables map a session to a user's own credential profile and login sequence. When user A shares a session with user B, each user connects with their own credentials — the session definition is shared, the authentication is not.

A user assigns the two in the session dialog for a session of their own. Another user's shared session is not theirs to edit, so its context menu has **My Login** in place of **Edit**. That dialog names the session's owner, shows its protocol, host, port and user name and, for SSH, its switch for legacy algorithms, takes a credential profile and a login sequence, and lists the session's jump hosts with their hosts and ports and a profile for each: they are the owner's sessions as well, and a jump host is not connected to without a profile. The user decides by what the owner wrote, so it is shown in fields of its own, apart from the dialog's text, and whole, in a box that does not scroll, since it is the end of a host that decides where a connection goes; a host that can be confirmed takes 255 characters at the most as it is shown. A host is shown in printable ASCII, with every other character written out as its code point, since a host in another script can look like a trusted one, and a user name likewise, with a backslash of its own doubled; in a name, which can be in any script, the characters are written out that change how text displays (control characters, bidi overrides and isolates, invisible characters). A name is shown up to 64 characters, and a host and a user name up to 255, the longest that can be confirmed. Where the chain goes on to a jump host the user cannot see, or to one of another owner than the session naming it, the dialog says that the session cannot be connected to. It takes no choice and saves nothing before it has read what is assigned, writes only what was changed, and neither takes input nor closes while it saves. The folder commands **Apply Credential Profile** and **Apply Login Sequence** write mappings as well, for the user's own sessions only: those in a folder of the user's own and in the user's own folders below it, which their statements hold to themselves. They cover what is there when they run, without showing it, so another user's session that is in such a folder, as one from before `shellstation_own_folder` can be, is left to **My Login**. The walk down the folders ends on a loop of them.

A mapping is for a session, and the owner of a shared session can change what the session names after another user has assigned a profile to it: its host, port, protocol, user name, jump host and switch for legacy algorithms. A password would then go to a host of the owner's choosing. A login sequence, which types what its steps hold as the host prints what each waits for, would go in clear text to whatever answers on Telnet, which checks no host key. What a user assigns to a session that is not theirs is therefore used only for what they confirmed the session names. A save in **My Login** confirms what the dialog shows: the backend reads the session and its jump hosts again, refuses the save if one is no longer what was shown, and remembers what each names, before anything is assigned. Where a profile or login sequence that the user assigned would be used for a session that is not the user's own, `session_connect` compares the row it connects by with what was confirmed, before that profile or sequence is read and before anything is connected to, and refuses with what has changed; it does so for the session connected to and for each jump host on the way. The dialog says where something assigned waits to be confirmed, marks what has changed, and shows what was confirmed before, crossed out, under what the session names now; of a jump host a session went on to before, that is the host and port the user confirmed of it.

A session is confirmed for the way the dialog showed it. Of the session it is for, that is everything a connection goes by: protocol, host, port, user name, switch and jump host. Of a jump host it is the host, the port and the jump host it goes on to, which is all the dialog shows of one and all that is used: a jump host is connected to over SSH, with the user's own profile and the switch of the session that goes through it. The two are kept apart, so that what was confirmed of a session as a jump host, without its protocol being shown, does not count when the same session is connected to by itself; a session confirmed as the one connected to passes as a jump host. And since it is that session's switch a jump host is connected to with, a profile assigned to a jump host is used only through a session the user has confirmed, also where nothing is assigned to that session itself. The switch and the jump host count for SSH only.

A session that names what the dialog could not show for what it is cannot be confirmed, and nothing assigned is used for it. That is a protocol that is neither SSH nor Telnet, which a connect takes for SSH, a port outside 1 to 65535, which a connect would cut down to another, and a host longer than 255 characters: the app's own commands refuse all three, but a row can be written past them. And it is what those commands take, though no session needs any of it: no host, or one with a backslash, a space, a control character or any other character in it that no host name has, as an SSH connect checks and a Telnet connect does not; a host that takes more than 255 characters written out; a user name longer than 255 characters, with a control character in it, or with a space at its start or end. A connect refuses a port outside the range for any session.

What was confirmed is kept in `session-logins.json` in the app's config directory, written for its owner alone, and by session alone. It is not in the shared database, where it would be within reach of another role's privileges and take a start of the owning role. A file that is not what the app writes counts as nothing confirmed and is written over by the next save, and each session is then confirmed once more; so it is on a machine or account without the file, and for what was assigned before sessions were confirmed. One that cannot be read for the moment counts as nothing confirmed until it can, and is not written over. While the role the app connected as is not known, the app takes no session for the user's own, so nothing a user has assigned is used for any, and the refusal says to restart. What is confirmed is a host as written: what answers under that name is for the host key check, and on Telnet for nothing, as with a session of the user's own. It rests on a row's `owner`, as the other per-user rules do, and so holds under Option A (§4.5.2). `CHANGELOG.md` has what remains under Known issues.

The profiles and sequences themselves stay in each user's local database, so a mapping holds only an ID. That ID still decides which of a user's secrets a connection sends. Someone able to write another user's mapping could have a session send that user's password for one host to another host. A mapping therefore belongs to the database role that wrote it, named in its `owner` column, and RLS, with `FORCE` as on the other tables, gives a role its own mappings and, of the others, only the placeholders described below. Each table has one policy for every command, `*_owner_all`. Its `WITH CHECK` accepts a row only if it is the role's own and for a session the role can see, and since it is the only policy, an updated row has no other to pass by. The session is part of the rule because the foreign key is checked without RLS: its answer alone would tell any role whether a session with a given ID exists. The app's own statements name `owner = current_user` as well. As on the other tables, this holds under Option A (§4.5.2): an Option B user holds `TRIGGER` and `TRUNCATE` here too, and a trigger of theirs on a mapping table would run when another user saves a mapping.

Under its role, a mapping is keyed by the **user identity** from Settings. That is a name the client chooses for itself, so it protects nothing. It tells apart people who share one database account, between whom there is no isolation in any case. The primary key is `(session_id, owner, user_ident)`, so a row of one role never collides with a row of another, whatever names the two use. The migrations are shared with SQLite, which cannot drop a primary key, so the migration adds `owner` and a unique index, and `setup_postgres_rls` makes that index the primary key in place of the old `(session_id, user_ident)`.

Mappings from before `owner` existed carry the placeholder `'local'`, and the app does not connect with a placeholder; only a version from before `owner` existed still does. Which role wrote such a row is not on record. Each client therefore claims the placeholders under its own user identity, at startup and when that identity is set, and deletes the placeholder of a session its user has mapped again since. The policy lets any role read, claim or delete a placeholder, but only for a session that role can see. The placeholders for a personal session are thus left to that session's owner. A placeholder cannot be changed and stay one, and none can be added. A placeholder is no role's own row: a role named `local` is held to the sessions it can see like any other, and can save no mapping of its own. A placeholder for a shared session is taken by its name alone, the name that protects nothing: before, any user could read and rewrite it, so whether the user it names wrote it cannot be known, and until that user's client has claimed it, another user can read, claim or delete it first. An admin who would rather not carry such rows over deletes them ([ADMIN_GUIDE.md §5](ADMIN_GUIDE.md#5-applying-application-updates)), and the users assign their profiles again.

#### 4.5.2 Permission Model

ShellStation's startup runs `sqlx::migrate!()` and `setup_postgres_rls()` on every launch. These operations are idempotent, but they require different privilege levels depending on whether the schema already exists. There are two valid approaches to structuring permissions:

**Option A — Separated roles (recommended for larger teams):**

An admin role owns the schema; regular users get DML only. This follows the principle of least privilege.

- **Admin role (table owner):** Required for the **first startup** and whenever a **new app version adds database migrations**. Needs full schema control: `CREATE TABLE`, `ALTER TABLE`, `CREATE POLICY`, plus DML on all tables, and `CREATE` on the database for the schema of the trigger functions (§4.5.1).
- **Regular user roles:** Sufficient for **everyday use** after the admin has initialized the schema. Needs only `SELECT`, `INSERT`, `UPDATE`, `DELETE` on all application tables, plus `SELECT` on `_sqlx_migrations` (so `sqlx::migrate!()` can verify all migrations are applied and return without error).

When a regular user starts the app, `sqlx::migrate!()` sees all migrations are applied and does nothing. `setup_postgres_rls()` ends at its check that the role owns the tables, but this failure is **logged and non-fatal** — the policies already exist from the admin's initial run.

The trade-off: when a new ShellStation version introduces schema changes, the admin must connect first so that new migrations apply. After that, regular users can connect normally. See section 4.6.5 for the update procedure.

**Option B — All users can migrate (simpler for small teams):**

Every user gets schema modification rights. Any user who launches a new app version automatically applies pending migrations — no admin coordination needed.

- Each user role gets: `CREATE ON SCHEMA public`, `ALTER TABLE`, `CREATE POLICY`, plus DML on all tables.
- The first user to start the new version runs the migration; subsequent users see it's already applied.

The trade-off: every user has the privileges to modify or drop tables, not just read and write data. Their table privileges also include `TRIGGER` and `TRUNCATE`, which reach past row-level security (§4.5.1), and with `CREATE` on the schema they can put operators and functions into it that other users' statements may run in place of the built-in ones. This is acceptable for small, trusted teams but inappropriate in environments where you want to limit who can alter the schema.

#### 4.5.3 Locks

PostgreSQL gives a lock to whoever asks for it and keeps it until that transaction ends, and a statement that needs the lock waits for it without end unless it is told otherwise. Every user of a shared database can take locks that others need: `LOCK TABLE` takes no more than the `UPDATE` and `DELETE` the app needs anyway. A row lock is only for rows a role may update, which are its own (§4.5.1). The one lock on another user's row is the one a foreign key takes: a row of a user's own that refers to another user's session keeps that session from being deleted, though not from being changed, while the transaction that wrote it is open. Such a row is a mapping for it, or a row in the central `credentials` table, which the app does not use and which has no row-level security. A session that names it as its jump host is such a row only where it named it before the trigger of §4.5.1, or where that trigger is not in place yet or does not hold the user (Option B): elsewhere the trigger refuses it before the foreign key is checked. It does not take intent either: a transaction left open in another tool holds its locks just the same, as does a client that stopped in the middle of one. The app does three things about it.

- **Its statements give up.** Every connection the app keeps its data through sets three limits for itself before it is used, the ones that run the migrations and the RLS setup at startup included: `lock_timeout` 5 s, so that a statement that cannot get a lock fails and the edit can be tried again; `statement_timeout` 60 s, for a statement that takes that long for any other reason; and `idle_in_transaction_session_timeout` 30 s, so that the server ends a session of the app that has left a transaction open, with the locks it holds. The app's transactions never wait for the user, so only a client that has stopped gets there. A value the server already sets, for the database or the role, is kept, unless it is 0, which reads the same as not set. The limits are settings of the session, set by a statement after connecting; they do not hold through a connection pooler that hands each transaction to another session. A migration is bound by them like anything else: one that waits more than 5 s for a lock, or has a statement that takes more than 60 s, fails and is tried again at the next start, and the database error at startup says which it was. Without the limits an edit that met a held lock never returned and kept its connection, of which there are ten, and a start waited in the setup before there was a window.
- **It waits for fewer locks.** To keep two of a user's own changes from giving two items the same place, creating or moving a folder or session locks the siblings it joins. It locks only the user's own. At the top level the siblings include every other user's shared folders, and waiting for one of those would let whoever holds it keep a user from making a folder of their own. Two users who end up with the same place there are told apart by name.
- **The owning role's start locks nothing it does not change.** The RLS setup runs at every start. `ALTER TABLE`, `CREATE POLICY` and `CREATE TRIGGER` lock the whole table before they find that there is nothing to do: the first two against every other use of it, the third against writers. A lock against every other use waits for every open transaction that has touched the table, and while it waits, everything else that wants the table waits behind it, plain reads included: one transaction left open would hold up every user for as long as that start waits. Each step therefore looks in the catalog first and changes only what is missing. Since no `ALTER TABLE` is left to stop a role that does not own the tables, the setup asks that outright, after its two fix-up updates and before any step that changes the schema, and ends there for anyone else. A step that has something to change can fail at a held lock, which anyone can time to the owning role's start. The setup therefore goes a table at a time, after replacing the trigger functions, which locks no table. A step that gives up on a lock, or as a statement that took too long, ends the steps of its table, which would wait as long, and the setup goes on with the next table; the next start of the owning role goes on from there. So that this leaves no table half protected, each table's policies and the switching on of RLS are one statement, and so one transaction: a table has both or is as it was, where RLS without a policy would refuse everything to everyone. They are the table's last step, after its constraint or primary key and its triggers, so that a table's protection is complete when it appears. A step that fails for another reason, such as a constraint that existing rows break, does not stop the steps after it, or it would keep them from that database at every start. The one exception is a function's triggers, which are left alone when that function could not be replaced, since a new trigger would then call a function that is not the owning role's. A step that fails at the connection, and not at the database, ends the setup, since every further step would meet the same; so does a failure of one of the two fix-up updates, which a lock on the whole of `folders` or `sessions` causes. What failed is reported together, as one error in the application log.

None of this stops a user from holding a lock. A session sets its own limits, so neither the app's nor any the server sets bind a role that sends its own statements. What the other users get is an error after 5 s where they got no answer. A row lock reaches its holder's own rows and what refers to them, such as another user's mapping for a shared session of the holder's, and through a foreign key it keeps another user's session from being deleted; a lock on a whole table reaches everything in it. Finding and ending the session that holds a lock is the admin's ([ADMIN_GUIDE.md §7](ADMIN_GUIDE.md#7-locks-held-by-a-user)).

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
- **Jump host chaining:** Any saved session can be designated as a jump host for another session. Chains of arbitrary depth are supported (A → B → C → target). In PostgreSQL mode a session's jump host is a session of the same owner (§4.5.1).
- **Connection pooling:** Optionally keep bastion connections alive for reuse when opening multiple sessions through the same jump host.
- **Known hosts:** Uses the user's `~/.ssh/known_hosts`, which it shares with OpenSSH, with TOFU (Trust On First Use) prompting. It also reads the other files OpenSSH reads by default: `~/.ssh/known_hosts2`, and the system-wide `ssh_known_hosts` and `ssh_known_hosts2` in `/etc/ssh` (on Windows, `%ProgramData%\ssh`). A system-wide file is read only if no one but an administrator can change it or the directory it is listed in (on Windows, judged by their owners and access lists), checked through the handle it is then read from, and is never written to; a trusted key is saved to `~/.ssh/known_hosts`. On Windows anyone who can read a file can lock part of it, or hold it open without sharing it for reading. A system-wide file with a locked part is therefore read through a mapping. ShellStation also remembers the `@revoked` lines of each system-wide file in its local data directory, which does not roam, with the file's size and the times it was created and last written. So that no one else can set those times back, the check that only an administrator can change a system-wide file also counts the right to change its times. While another process holds that file, it is still checked, through a handle that reads only its owner and access list, which a hold does not stop. If it passes, the remembered revocations still count and nothing else in it does. If the file's size or times have changed since, the key is left unchecked, since the file might revoke it now: the prompt warns, and accepting takes a click. The same goes when what was remembered cannot be read. Protection starts with the first whole read: a file that ShellStation has never read whole is skipped while held, as OpenSSH skips it, so that a hold alone cannot put a warning on another user's connections. ShellStation reads the files itself, as OpenSSH does: a line it cannot read costs only that line. The prompt tells a first connection from a changed key, a host on record only with another key type, a new host whose address is on record with another key, and a file it could not check. Anything but a first connection is shown as a warning and takes a click to accept. A key listed under `@revoked` is refused without asking. For a direct connection, the entries under the address connected to are looked up as well, as OpenSSH's `CheckHostIP` does, but they never trust a key and never prompt about a host trusted by name: a revocation under the address refuses the key, and another key under it turns a new host's prompt into a warning. A host reached through a jump host or given as an address, and a loopback, link-local or unspecified address, are looked up by name only.
- **Keep-alive:** Configurable SSH keepalive interval to prevent idle disconnects.
- **Reload:** A page load in the main window starts the frontend over without its tabs, so the backend closes every session then, local shells included. To tell a connect of the old page from one of the new, each page makes up a token when it loads, announces it (`page_announce`) and passes it with every connect command. The backend opens a session only for the page announced last, and a new page's announcement closes whatever sessions are still there. A connect the old page sent in the instant of the reload is therefore refused whenever it arrives, before it reaches the remote host. The frontend calls the connect commands only through `invokeConnect` (`src/page.ts`), which announces the page and adds the token.
- **Disconnect:** Closing a tab sends `SSH_MSG_DISCONNECT` to the target and then to each jump host, and waits until they have closed, up to 3 s for the chain, so a device that allows one session at a time sees the session end. Quitting the app does the same for every session at once, before it exits within 4 s, and says so in the window while SSH or Telnet sessions close. The session logs are closed and written within the same 4 s, after the sessions have closed (§5.5). Every way of quitting the app itself offers goes through the backend's `quit_app` for this: Ctrl+Q and the window's close button through the `app_quit` command, after the quit confirmation where the settings ask for one, and the menu's quit item (with Cmd+Q, its shortcut on macOS) directly and without the confirmation, so that it works even when the webview is hung. A quit from outside the app (the macOS Dock, logout, shutdown) and an update install leave the connections to the operating system for now.

### 5.3 Session Management

- **Folder hierarchy:** Unlimited nesting depth for organizing sessions into groups (e.g., Production → EU → Web Servers).
- **Search and filter:** Full-text search across session name, hostname, tags, and folder path. Tag-based filtering for quick access.
- **Quick connect:** A shortcut bar for connecting without saving a session (hostname + optional user/port).
- **Import:** Bulk import from mRemoteNG XML (`confCons.xml`) and SecureCRT XML session files. CSV import for generic datasets.
- **SecureCRT buttons:** A SecureCRT export can also hold its button bars and Command Manager. The backend reads the file first, and the user picks among its sessions, button bars and Command Manager, as in the data import, with every button listed with its command. The buttons go into the command bar, which lives in the webview, so the webview adds them after the existing ones, skipping a button that is already there, and checks them as it checks buttons from a ShellStation export. Only Send String buttons carry over, with their commands as written: the command bar reads the same escapes, octal codes for control characters included (`\000`–`\037` and `\177`; `\003` is Ctrl+C). It does not read codes for printable characters, so that no command can show as digits while typing text; the import writes such a code out as its character instead. A button with another function, or one with an octal code above `\177`, which SecureCRT sends as a raw byte and a command bar button cannot, is skipped with a warning.
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
- **How a log is created:** On the session's first output, and never by opening what is already there. A log's name is predictable, and the log directory may be one that others can write to, so the file is created by a call that fails when anything holds the name, a symbolic link included, and as 0600 on Unix as part of that call; a missing log directory is created as 0700 likewise. On Windows that call gives the file an access list of its own instead: full control for the user the app runs as, and nothing inherited from the log directory. A missing log directory gets such a list too. A network share is the exception: a server reached with another account would not know the user the list names, so a log and a log directory on a share are created the plain way, and are as private as the share makes them. A log directory that is already there keeps its list, so others may see a log's name there, but cannot open it. A name that is taken is passed over for the next of `_1` to `_999` before the extension, so two sessions with the same name in the same minute get a file each. A log that cannot be created is given up, and the session goes on without one. The same goes for a log whose directory its writer could not create, and for one that could not be handed to a writer (see below). The session's tab then says so in a toast that stays until dismissed and names the session, in at most 64 characters and without the characters the import drops from a name (control characters, bidi overrides, invisible characters). The backend sends the tab an event for this (`terminal-log-failed-<id>`) with the session's first output, once, and without the reason, which is in the application log. A log that is given up later, because a write to it fails or it stops taking writes (see below), sends the same event then. A session name goes into the filename as letters, digits and `- _ .` only, with a dot only between other characters, cut at 120 bytes. The create protects the log's own name, not the log directory: someone able to replace that directory, or one above it, with a link decides where the logs go.
- **How a log is written:** By a thread of its own for each log, and never under the lock that the sessions share. The output of every session passes one lock on its way to the window, and a log directory can be slow, such as one on a network share that does not answer, so nothing is done to the file system under that lock. There a session's output is stripped (see below) and handed to the log's writer. The writer makes sure the log directory is there when the session connects, creates the file on the first output, and writes the outputs that are waiting, together as far as they fit in 64 KiB, in one write. A log directory that is slow therefore holds up neither a connect nor the output of other sessions. Text that waits for a writer is kept in memory, so a session whose own log is more than 1 MiB behind waits for its writer, without the shared lock, until the log is no more than that behind: a slow log directory slows a session that puts out megabytes, to the pace of its log and without a notice, and nothing is lost. While it waits, a session shows no output, and a Telnet session sends no keys. A log that takes nothing for 5 s while its session waits is given up, and the session goes on without one: its tab says so in the toast above, the writer still writes what was waiting, should the directory take it, and the log ends there. A log directory that stops answering thus holds up each session logged there once, for 5 s, and only a session with more than 1 MiB of output that the log has not taken. What can wait for one log is 1 MiB and one output of the session. A log is given up as well at the first write that fails, as on a full disk or a share that went away: the application log has one warning for it, the session's tab says so in the toast above, and the session goes on without a log. No write is tried again, also when the disk has room again later; a reconnect begins a new log. What the file took before stays, and the output that was in the failed write, up to 64 KiB or the one output, and whatever waited behind it are not in it. Closing a tab closes its log without waiting: the writer writes what is left and closes the file. Quitting closes every log and waits for the writers, after the sessions have closed and within the same 4 s (§5.2). What a log directory has not taken by then is lost. So is what the writers hold when the process ends without the app's own quit, up to 1 MiB and one output for a log: on a crash, an update install, or a quit from outside the app. A writer stuck in a write that does not return stays, with its text, until it does, so at most 512 writers run at a time. That is more than there can be sessions (300), and is reached only by writers that have outlived their sessions; a session that connects then gets no log, and its tab says so.
- **The log directory:** The session logs and the application log treat their directory alike, whether it is the default or one the user chose. A missing one is created for its owner alone, as above, as are missing directories above it, with the same exception for a network share on Windows. One that is already there is restricted to 0700 on Unix, each time a session connects with session logging on and at each start with application logging on, and keeps its access list on Windows. On Unix the directory is opened for this by a call that refuses a symbolic link at its name, and restricted through that handle, so that someone able to put a link there cannot have a directory elsewhere restricted; a directory that belongs to another user is refused as well. The config directory, where only the user can put a link, is the one directory restricted through a link. A directory that cannot be restricted is still used. With application logging on, the application log says so: as a warning for its own directory, and at `info` for each session's. The application log's file is created by the logging library and not for its owner alone: on Unix it gets the mode the umask leaves it, so its directory is what keeps others from reading it, and on Windows it gets the access list of its directory, which a directory that was already there keeps. A directory chosen for a log is therefore taken to be for the logs: one that others share should not be chosen, and the Settings hints say so.
- **Format:** Stripped plain text (escape sequences and control characters removed). A raw format, with the output as the host sent it, is not implemented.
- **What a log holds:** Text in UTF-8, and no control character but CR, LF and tab. A log is something a user shows in a terminal or opens in an editor, so what the remote host sent must not act on that terminal, and the characters that most often make text show as something else are taken out as well. The output is read as UTF-8, as the terminal reads it, and then stripped character by character, since the C1 control characters (U+0080–U+009F) cannot be told from text byte by byte. Escape sequences are removed: CSI sequences, control strings (OSC, DCS, SOS, PM, APC) and the other escapes, whether they start with ESC or, for CSI and the strings, with the C1 control that stands for ESC and the character after it (U+009B for `ESC [`). A sequence that does not end is given up after 4096 characters, so that one cut short does not take the rest of the session with it; this is no limit on what a host can keep out of a log by putting it inside sequences, which the terminal does not show either. Every other control character is dropped, DEL and the rest of C1 included, with two exceptions. Those the terminal moves to the next line for (VT, FF, IND, NEL) are written as a line feed; a control string takes VT and FF with it. A backspace takes the character before it out of the log, if the same output of the session put it there. A CR stays only before an LF. Before anything else it becomes a line feed, or is dropped at the start of a line, so that nothing is written over what a line held when the log is shown; output that redraws a line, such as a progress bar, takes a line per update. The characters the import drops from a name because they reorder or hide text (bidi embeddings, overrides and isolates, line and paragraph separators, zero-width space and the like) are dropped from a log too, and the tag characters with them. That list is fixed and not complete: directional marks, joiners, variation selectors and other invisible characters that scripts and emoji need stay, and a host can always write text that looks like a prompt or like other output. A log is close to what the terminal showed, not the same (a redrawn line takes a line per update, and the dropped characters are missing), and it is not proof of what happened: it is text from a remote host, and should not be handed to anything that reads markup or runs what it is given. Bytes that are not UTF-8 are written as U+FFFD, as the terminal shows them, so what is written is valid UTF-8 whatever encoding the device sends, and text a device sends in another encoding cannot be read back from it. A write that fails is the log's last (see above), and part of it may be in the file, which can then end in a CR without its LF or in part of a character. All of this is for a terminal that reads UTF-8: one set to another encoding, as a Windows console on a legacy code page is, takes some bytes of ordinary characters for C1 controls, and a host can choose such characters, so a log should not be shown there. Logs written by earlier versions were not stripped this way.
- **Rotation:** A log file has a largest size, and a session's log a largest number of files. Both are set in Settings: 10 MiB and 10 files by default, within 1–256 MiB and 1–50 files, so 12.5 GiB at the most. A log holds what the remote host sends and has no end of its own, so without a limit a host could fill the volume the log directory is on. A file that is full is closed, and the log goes on in a new one, named after the first with `.part0002`, `.part0003`, … before the extension: `router.log`, then `router.part0002.log`. The numbers only go up, so a name is not used again once its file is deleted, and the names sort in the order the files were written up to `.part9999`; the numbers after it are longer and sort before it. The new file is created as the first one is, by a call that fails when anything holds the name and for its owner alone. A name that is taken is passed over for the next number; with 1000 names in a row taken, the log is given up, and the session's tab says so in the toast above. A file is filled up to its limit and no further, and what is written to it ends neither inside a character nor between a CR and its LF; a write that fails part of the way can still leave a file ending there. Once more files are there than are kept, the oldest are deleted. One session's log therefore takes no more than the two limits multiplied, 100 MiB by default, or up to twice that with files that cannot be deleted (see below), and holds the session's latest output: of a long session, the start is gone. Only files that the log's own writer created are deleted, by the names it gave them. As with the create, that protects the name and not the log directory. A file that cannot be deleted, such as one another program holds open on Windows, stays and is tried again when the next file is full, with a warning in the application log the first time; one that is gone already counts as deleted. When more than twice the number kept would be left, the log is given up: a log whose oldest files do not go has no limit. Someone able to write to the log directory can end a log this way, as by taking the names its next files would get. Neither limit can be switched off. The webview saves these settings, so the backend accepts only values within the ranges; within them the webview can set either limit, as it can switch session logging off. Values outside the ranges in the config file are replaced by the defaults at startup, and one that is no whole number makes the file unreadable, as any other number in it does. A log keeps the limits that were set when its session connected. The limits are for each log by itself, not for the log directory: up to 300 sessions can be logged at a time, a reconnect, which only the user starts, begins a new log, and the logs of sessions that have ended stay until the user removes them.

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
| `src-tauri/permissions/`     | App manifest: the app's own commands, as a permission for the capabilities.    |
| `src-tauri/capabilities/`    | Which window may use which permission. Only the main window has any.           |
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
| `sqlx`                 | 0.9+    | Async database driver with compile-time query checks.         |
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
