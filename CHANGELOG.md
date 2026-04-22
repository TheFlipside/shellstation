# Changelog

All notable changes to this project are documented in this file.

## Unreleased

### Added

### Fixed

### Changed

### Removed

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
  