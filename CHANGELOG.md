# Changelog

All notable changes to this project are documented in this file.

## Unreleased

### Added

- Version info in "About" help menu

### Fixed

### Changed

### Removed

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
  