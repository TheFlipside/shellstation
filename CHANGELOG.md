# Changelog

All notable changes to this project are documented in this file.

## Unreleased

### Added

### Changed

- Refreshed Rust dependency lockfile: `russh` 0.61.1 → 0.61.2 (broadens `russh::keys::load_secret_key` to accept SEC1 EC private keys that carry full explicit domain parameters in addition to named-curve OIDs — purely additive at the existing `ssh/mod.rs:1199` call site — plus an `AgentClient` `Send` fix that does not affect this codebase since no SSH-agent forwarding is implemented), `uuid` 1.23.1 → 1.23.2, `chrono` 0.4.44 → 0.4.45. All three are pure patch bumps with no source changes; the existing `^constraint` versions in `Cargo.toml` already permitted them.
- Refreshed npm dependency lockfile: `i18next` 26.0.8 → 26.3.1 (includes the 26.0.6 security fix for interpolation/log-forging), `react` + `react-dom` 19.2.5 → 19.2.7, `react-i18next` 17.0.4 → 17.0.8, `zustand` 5.0.12 → 5.0.14, `@tauri-apps/cli` 2.11.1 → 2.11.2 (realigns JS CLI with the Rust `tauri` 2.11.2 crate), `@types/node` 25.6.0 → 25.9.2, `@types/react` + `@types/react-dom` 19.2.14 → 19.2.17, `@vitejs/plugin-react` 6.0.1 → 6.0.2, `eslint-plugin-prettier` 5.5.5 → 5.5.6, `typescript-eslint` 8.59.0 → 8.60.1, `vite` 8.0.10 → 8.0.16 (Rolldown 1.0.0-rc.17 → 1.0.3 stable, launch-editor-middleware path-traversal hardening), `vitest` 4.1.5 → 4.1.8. All within existing `^constraint` ranges; no `package.json` edits required.
- `eslint.config.js`: migrated from the deprecated `tseslint.config()` helper to ESLint core's `defineConfig()` (typescript-eslint 8.x deprecation). Added a targeted `eslint-disable-next-line` for the `securityPlugin.configs.recommended` line because `eslint-plugin-security` 4.0.0 ships no `.d.ts` and is therefore `any`-typed upstream, which typescript-eslint's `no-unsafe-argument` / `no-unsafe-member-access` rules legitimately cannot verify.
- `tsconfig.node.json`: now also includes `eslint.config.js` (with `allowJs: true`) so typescript-eslint's projectService type-checks it under a real tsconfig where `@types/node` is in scope. Without this, `import.meta.dirname` resolved as `error`-typed under the inferred default project and triggered `no-unsafe-assignment` after the typescript-eslint bump.

### Fixed

- Terminal zoom (Ctrl+/Ctrl-/Ctrl+0) no longer leaves the remote PTY thinking the terminal still has its old cols/rows. Symptom was vim drawing its bottom-right line/column position indicator (and other status content) at the now-out-of-bounds old coordinates after zooming in, with resetting the zoom only moving the artifacts rather than clearing them. Root cause: `effectiveFontSize` changes called `FitAddon.fit()` to recompute the xterm.js grid, but only the container `ResizeObserver` path invoked `ssh_resize`/`telnet_resize`/`pty_resize` — and the container's box dimensions don't change when only the font size changes. Fix: attach a `term.onResize()` listener as the single canonical signal for cols/rows propagation, so any source of `fit()` (container resize, zoom, visibility flip) automatically reaches the backend. Removes a dead `handleResize` `useCallback` that was clearly intended for this wiring but never connected.

### Security

### Changed

### Removed

### 0.20.0 - 2026-05-23

### Added

- Unit tests covering every branch of the security-relevant pure functions in the Rust backend: `is_restricted_ip` (SSRF guard against private/CGNAT/link-local/ULA ranges on IPv4 and IPv6), `validate_import_data` (count caps and per-field length/port validation on import payloads), `sanitize_pg_error` / `sanitize_ssh_error` / `sanitize_telnet_error` (verify error messages collapse to user-facing strings without leaking paths, FDs, or usernames). All five functions went from 0% → 100% (97.4% for `validate_import_data`) line coverage; CRAP scores dropped ~90%, moving them out of the "complex and untested" bucket flagged by `cargo crap`.
- `db::update_builder::UpdateBuilder`: a typed dynamic-SQL helper that owns the placeholder syntax (PostgreSQL `$N` vs SQLite `?`), the `BindVal` enum, and the bool-as-int / optional-UUID three-state semantics that previously lived as ~90 lines of `if let Some(...)` boilerplate in each provider. The builder is unit-tested without a live database. `PostgresProvider::update_session` and `SqliteProvider::update_session` now consist of 13 builder calls plus the per-dialect WHERE clause; both dropped from CC 29 (CRAP 870) to CC 8 (CRAP 72).
- `db::update_builder::SessionColumn`: an enum of the thirteen `sessions`-table columns that the session-edit IPC may touch, replacing the untyped `col: &str` parameter on `UpdateBuilder`. Column names now go through `SessionColumn::as_sql() -> &'static str`, so the SQL embedded by `format!()` is provably one of thirteen hardcoded strings — SQL injection via column name is impossible by construction rather than by call-site discipline. Adding a new column requires a deliberate enum extension that fails to compile until the consumers are wired up. Closes the medium-severity design fragility flagged in the security audit.
- Pull-parser test fixtures for the mRemoteNG (`confCons.xml`) and SecureCRT XML importers, covering the branch table of each: empty input, single SSH/Telnet session, default-port fallback, missing-hostname warning, unsupported-protocol skip, nested folders with parent tracking, jump-host references, built-in `Default` session skip (SecureCRT), oversized dword handling, and `MAX_NESTING_DEPTH` rejection. mRemoteNG's `parse` went from CRAP 650 → 34.8 (75% line coverage); SecureCRT's `parse_key_block` went from CRAP 930 → 49.3 (72% line coverage).
- Doc comments at the top of `session_connect`, `db_save_config`, `db_import`, `process_iac_byte`, and both XML parsers explaining why their complexity is essential (IPC orchestration, RFC 854 state machine, ambiguous-grammar look-ahead) and where the right testing point lies (integration harness for the IPC commands, fixtures for the parsers).
- Overall: functions exceeding `cargo crap`'s threshold of 30 dropped from 107 to 96; test count rose from 15 to 85.
- Justfile, for easier access to common executed steps.

### Changed

- Upgraded `tauri` to 2.11.2 and `tauri-build` to 2.6.2.
- Upgraded `serde_json` to 1.0.150.
- Upgraded `tokio` to 1.52.3.
- Upgraded `sqlx` to 0.9. The two consumer-visible changes:
  - `sqlx::query()` (and family) now only accept `&'static str` or `sqlx::AssertSqlSafe<…>` (PR #3723). The twenty dynamic-SQL call sites that build statements with `format!()` — the `UpdateBuilder` consumers, the bulk-edit `IN (?,?,…)` builders, the BFS folder-walk queries, and the keychain-rotation `CREATE DATABASE` path — are now wrapped in `sqlx::AssertSqlSafe(sql.as_str())`. With `SessionColumn` already enforcing that the only non-placeholder dynamic content is a typed column name, each `AssertSqlSafe` is honest by inspection.
  - The Cargo feature `sqlite` is now a meta-feature that pulls in `sqlite-deserialize` / `sqlite-load-extension` / `sqlite-unlock-notify` (PR #3928, SQLite extension loading is now `unsafe`). ShellStation uses none of those APIs, so the feature was switched to `sqlite-bundled` to keep the binary surface minimal.
- Upgraded russh to 0.61. As of russh 0.60.2, SHA-1 MACs (`hmac-sha1`, `hmac-sha1-etm`) are no longer part of `Preferred::DEFAULT.mac`. ShellStation's default (non-legacy) connection path inherits that tightening — these MACs are no longer offered during negotiation when **Legacy algorithms** is off. The Legacy toggle in the session editor remains the escape hatch and is now the **only** way to re-enable SHA-1 MACs. Targets that previously connected on default settings via SHA-1 MAC negotiation (some older Cisco IOS releases, certain industrial switches) will now require enabling the toggle; the existing "no shared algorithm" hint already points users at that setting.
- Moved `ADMIN_GUIDE.md`, `DESIGN.md`, `DEVELOPMENT.md`, and `RELEASING.md` into a dedicated `docs/` folder. Internal cross-references between these documents continue to use plain filenames because they still share a directory; external references in `README.md`, `CLAUDE.md`, and a comment in `src-tauri/src/db/postgres.rs` were updated to point at `docs/`.
- Upgraded `quick-xml` to 0.40.1. `Attribute::unescape_value()` was deprecated in favour of `Attribute::normalized_value(XmlVersion)` (PR #371), and `BytesText::xml_content()` likewise gained a required `XmlVersion` parameter for correct EOL normalization (PR #938). Both importers pass `XmlVersion::Implicit1_0` because both mRemoteNG (`confCons.xml`) and SecureCRT XML exports declare `<?xml version="1.0"?>`. For the two `Attribute::normalized_value` call sites this is behaviour-preserving — `unescape_value()` in 0.39.2 already delegated to XML 1.0 normalization. For the `BytesText::xml_content()` call site in the SecureCRT parser it is a **deliberate semantic change**: the old `xml_content()` was internally aliased to the XML 1.1 path, which also collapses U+0085 (NEL) and U+2028 (LS) to `\n`. XML 1.0 keeps those code points as-is, matching the file's own version declaration. The call site carries a comment explaining the choice. None of the other 0.40 breaking changes apply to ShellStation (no `NsReader`, no `Reader::read_text()`, no namespace `resolve*()` calls).
- Rust backend now passes `clippy::pedantic` clean (`just check`). Most fixes were mechanical (use method references over closures, `map_or`/`let-else` over `match` for one-arm cases, `clone_from` over reassignment, inlined format args, redundant `continue`/`async` removed). Doc comments throughout the backend now backtick identifier-shaped tokens (snake_case, CamelCase, product names like `PostgreSQL`/`SQLite`/`SecureCRT`/`ShellStation`/`ConPTY`, and protocol abbreviations like `NAWS`/`IAC`/`SB`/`SE`). Crate-level allows are documented in `src-tauri/src/lib.rs` for patterns that are deliberate architectural choices (`option_option` for three-state nullable updates, `cast_*` for already-bounded integer ranges, `too_many_lines` for the documented orchestration entry points, `needless_pass_by_value` for Tauri command convention).

### 0.12.0

### Added

- Visible warning in settings when PostgreSQL SSL mode is set to `disable` (cleartext credentials over the wire) and a softer note for `prefer` (which falls back to cleartext when the server lacks TLS)
- Keyboard activation (Enter/Space) on the shared-folder chevron in the sidebar (a11y)

### Fixed

- Drag-and-drop overlay drifted away from the cursor at non-100% UI scale (regression after the Tauri 2.11 / wry 0.55 upgrade — webview now reports `getBoundingClientRect` and pointer-event coordinates in different units around CSS `zoom`, defeating the previous compensation modifier)
- Search-result rows in the sidebar were skipped by `scrollIntoView` on selection because they lacked the `data-item-id` attribute the lookup queries
- Shared-folder expansion state could persist after the folder disappeared, causing it to re-open expanded when shared sessions reappeared
- "Saved" indicators for session logging, application logging, and user identity stayed visible indefinitely (or could fire `setState` on an unmounted dialog); they now auto-clear after 3 s and cancel cleanly on dialog close
- Hidden `<input type="file">` elements leaked into `document.body` when the user dismissed the highlight, database, or mRemoteNG/SecureCRT import pickers without selecting a file
- `DndContext` was remounting on every folder/session add or delete (count-based key), aborting any in-flight drag
- `addCommandButton`, `updateCommandButton`, `reorderCommandButtons`, and `setToastDismissSeconds` accepted unvalidated input that could bypass the limits enforced on rehydration
- Confirming the SSH host-key verification dialog with Enter on a first-time connection opened the session twice: the dialog listener fired on the bubble phase after the sidebar's tree `onKeyDown` had already re-triggered the connect. `useEnterKey` now listens in the capture phase, uses a stack so only the topmost dialog reacts, and stops propagation before the event reaches background handlers.
- `useEnterKey` / `useEscapeKey` now register a stable ref per hook instance instead of pushing the raw callback onto the stack. The stack only mutates on mount/unmount, eliminating two latent issues: StrictMode's intentional mount → unmount → mount no longer leaves duplicate entries, and unstable callbacks (inline arrow functions in dialog props) no longer churn the stack on every render. The ref is updated in `useLayoutEffect` rather than during render to remain safe under React's concurrent renderer. `useEscapeKey` also moves to the capture phase to match `useEnterKey`, so background tree/list handlers cannot fire before the dialog consumes the keypress (the capture handler still returns early when no dialog is open, so background Escape handlers like `ContextMenu` and `CustomSelect` keep working when no modal is mounted).

### Security

- PostgreSQL password is no longer round-tripped from the OS keychain through Tauri IPC into webview state when opening Settings; the field loads empty and an empty submission preserves the existing keychain entry
- `autoRefreshInterval` requires a minimum of 5 s (or 0 to disable); prevents a tampered persisted value from driving sub-second `session_data_fingerprint` polling against the IPC bridge
- `setLanguage` validates against the bundled-locale allowlist before passing to i18next (defense against attacker-supplied keys reaching i18next's internal resource map via tampered localStorage)
- Command-button rehydration now requires UUID-format ids and validates field shape (length, color regex)

### Removed

- 150% from the UI scale options. A small (~50 px) residual drag-overlay offset remains at exactly that zoom ratio under the new wry webview; 125% is now the maximum. Users with 150% saved are migrated to 125% on load.

### 0.11.4

### Added

- Optional "Confirm before pasting multi-line content" setting (Security section): when enabled, a preview dialog appears whenever pasted clipboard content contains line breaks or escape sequences. Helps prevent paste-jacking attacks where hidden commands embedded in copied text would execute on paste. Off by default.

### Fixed

- Translation fixes and improvements
- Pasting multi-line text on Windows inserted an extra blank line after each line (CRLF clipboard content is now normalized; bracketed paste mode is also honored when the remote shell enables it)

### Changed

- Updated Tauri to 2.11.1 (Rust crate and `@tauri-apps/api`), plus matching plugin versions

### 0.11.3 - 2026-04-27

### Added

- "Sort by Hostname / IP" context menu action for folders, with numeric IP-address-aware ordering (e.g. 10.0.0.2 before 10.0.0.10)

### Fixed

- Any authenticated user could rename, sort, reorder, or bulk-edit shared folders and sessions they don't own in PostgreSQL mode (authorization bypass)
- Any authenticated user could update another user's shared sessions via `update_session` in PostgreSQL mode (authorization bypass)
- Drag-and-drop overlay offset from cursor when UI scale is not 100% (CSS zoom compensation)
- Sort and reorder operations in SQLite using non-atomic writes that could leave corrupted sort_order on crash
- Bulk operations (credential profile, login sequence, bulk edit) in SQLite using non-atomic writes that could leave inconsistent state on crash

### Changed

- All folder/session sort, reorder, rename, and bulk-edit operations enforce `owner = current_user` in PostgreSQL mode
- Folder context menu items (Rename, Sort, Apply Credential Profile, Apply Login Sequence, Bulk Edit) restricted to owned folders in PostgreSQL mode
- Session context menu items (Edit, Clone) restricted to owned sessions in PostgreSQL mode
- Keyboard shortcuts (F2 rename/edit, Ctrl+D clone) restricted to owned items in PostgreSQL mode
- Bulk operations (credential profile, login sequence, bulk edit) traverse only owned folder subtrees in PostgreSQL mode
- SQLite reorder, sort, and bulk operations now wrapped in transactions for atomicity
- `update_session` enforces `owner = current_user` in PostgreSQL mode

### 0.11.2 - 2026-04-27

### Fixed

- Search results not updating after editing a session name, requiring a new search to see the change
- Deleting a session or folder jumping the view to the top of the list instead of selecting the parent folder
- Dialog overlays clipping behind the title bar and menu bar when UI scale is above 100%
- Session dialog and move dialog showing orphaned folders with broken parent chains in PostgreSQL mode
- Search returning sessions in unreachable folders that cannot be navigated to in the sidebar tree
- Any authenticated user could toggle another user's shared folder/session visibility via `set_visibility` (authorization bypass)

### Changed

- CSS selector in scroll-into-view helper now uses `CSS.escape()` to prevent malformed queries from non-UUID IDs
- Expanded folder state loaded from localStorage is now validated at startup to prevent crashes from corrupted data
- Resolved clippy warnings: collapsible match arms in mRemoteNG/SecureCRT importers, explicit counter loop in legacy migration
- Folder picker in session and move dialogs now restricted to tree-reachable folders owned by the current user (PostgreSQL mode)
- `set_visibility` command enforces ownership check before updating folder or session visibility

### 0.11.1 - 2026-04-23

### Added

- Import size limits for highlight profiles and login sequences to prevent resource exhaustion

### Fixed

- PostgreSQL startup failing for DML-only users due to `sqlx::migrate!()` attempting `CREATE TABLE` without schema privileges
- Import of highlight profiles and login sequences failing with UNIQUE constraint error when entries with the same name already exist
- Sharing a folder not cascading to descendant folders and sessions, causing inconsistent visibility for remote users
- Dynamic "Shared" folder expanding on single click instead of double click like regular folders
- Login sequence translations using literal "carriage return" equivalents instead of technical abbreviation "CR" across all 13 non-English locales
- Users could move shared folders/sessions into personal folders, causing data loss for the original owner
- Keyboard shortcuts (F2, Delete) in search results acting on the previously selected item instead of the clicked search result
- Session search being case-sensitive on PostgreSQL
- Users could clone sessions or create items inside shared folders they don't own, risking data loss when the owner changes folder visibility

### Changed

- PostgreSQL Administration Guide extracted from DESIGN.md into dedicated ADMIN_GUIDE.md, rewritten to present group role setup first
- PostgreSQL RLS setup failure downgraded from ERROR to WARN log level for DML-only users
- Setting a folder to shared or personal now cascades to all contained sub-folders and sessions
- "Move To" context menu and folder picker restricted to owned items and folders in PostgreSQL mode
- "New Session", "New Subfolder", and session dialog folder picker restricted to owned folders in PostgreSQL mode
- "Delete" context menu option hidden for shared folders and sessions not owned by the current user in PostgreSQL mode

### 0.11.0 - 2026-04-22

### Added

- Login sequence profiles: automate post-connection device prompts (e.g. username/password on Cisco switches) with regex-based expect/send steps, assignable per session
- Login sequence manager accessible from the sidebar and session dialogs
- Escape sequences in login responses: `\s` (username), `\w` (password), `\r`, `\n`, `\t`, `\b`, `\e`, `\\`, `\p` (1s pause)
- Bulk-assign login sequences to folders via context menu and bulk edit dialog
- Per-user login sequence mapping in PostgreSQL shared mode
- Login sequence export/import support

### Fixed

- Settings status messages (save confirmations, errors) appearing indented due to stray left margin

### 0.10.2 - 2026-04-22

### Fixed

- SSH connections to Cisco Small Business switches (SG350, Catalyst 1300) failing with "Channel send error" through jump hosts
- Dropdown menus positioned incorrectly when UI scale is not 100% (cross-engine CSS zoom compensation)
- Highlight profile dialog buttons using unstyled default HTML appearance instead of themed dialog buttons

### Changed

- Credential manager profile list now shows dividers between name and username, and separator lines between entries
- Improved translations across all 14 languages: fixed awkward wording, translated missing keys, added hint texts for settings sections
- Session log and application log hint texts now use tooltip icons consistent with the rest of the settings UI

### 0.10.1 - 2026-04-21

### Fixed

- SSH connections to legacy Cisco devices failing with "early eof" through jump hosts
- Compiler warning for unused variable on Windows builds

### Changed

- Broader SSH algorithm support (ECDH NIST P curves, AES-128-GCM) for modern Cisco IOS XE devices
- Credential retrieval returns zeroized memory wrapper to prevent secrets lingering in RAM
- Keychain error messages sanitized to prevent leaking OS keychain backend details
- PostgreSQL password field excluded from serialization to prevent accidental leaks
- Import parsers enforce maximum folder nesting depth (100 levels)
- Session log file creation verifies path stays within configured directory after open
- Highlight engine rejects regex patterns prone to catastrophic backtracking (ReDoS)

### 0.10.0 - 2026-04-20

### Added

- Version info in "About" help menu
- Ability to quickly reconnect disconnected sessions
- Visual indication for shared objects

### Fixed

- Implement proper keyboard-interactive authentication
- Race condition between backend data emission and frontend listener registration
- Clipboard paste from external applications not working on Linux (WebKitGTK)
- Application freeze when closing local sessions on Windows (ConPTY reader thread not unblocked)
- Potential deadlock from nested mutex acquisition in PTY session kill path
- SSH to legacy Cisco devices through jump hosts
- Fleeting cmd windows on Windows when accepting SSH host key for the first time

### Changed

- Jump host selection filtered to sessions tagged "jumphost" for better UX at scale
- More verbose logging in debug mode
- Position of GUI separator is saved persistently
- Implementation of proper multi-user RLS system with shared databases
- Multiple minor UX improvements
- New icons for switch and router

### Removed

- Close dialogues by clicking outside

### 0.9.4 - 2026-04-14

### Added

- Archive with extra files for releases
- Ability to use legacy algorithms per ssh session

### Changed

- Credential workflow refactored with new credential manager
- Minor hardening for data handling

## 0.9.3 - 2026-04-14

### Added

- Display session name in list on mouse hover
- More colors for command buttons
- Ability to select jump hosts in dropdown list with typing names
- Show progress when importing databases
- New free form input field for sending commands to sessions

### Fixed

- Taskmanager icon not showing (MS Windows)
- Be able to start app maximized (MS Windows)
- Launch App again on action "Restart"
- Settings Menu sizing
- Folders in Session list expandable/retractable with click on arrow
- Be able to set jump-host on sessions to "None"
- Correct folder structure on database import

### Changed

- Don't display warnings about unsupported session settings during database import
- Different syntax for application log files

## 0.9.2 - 2026-04-13

### Changed

- Upgrade to russh 0.60

### Removed

- Obsolete dependency of russh-keys
