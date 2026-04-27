# Changelog

All notable changes to this project are documented in this file.

## Unreleased

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

### Removed

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
  