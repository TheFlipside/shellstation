# Project: ShellStation

## What This Project Does

ShellStation is a cross-platform, open-source terminal manager and SSH/Telnet client designed to replace tools like mRemoteNG and SecureCRT.

## Stack

- **Language/Frameworks:** Tauri 2.x, Rust, React, TypeScript
- **Build:** Cargo + Vite
- **Key deps:** tauri, xterm.js, russh, russh-keys, sqlx, tokio, keyring, serde, uuid, portable-pty, tracing

## Directory Layout

```
src-tauri/src/main.rs        → Tauri application entry point and command registration.
src-tauri/src/ssh/      → SSH connection manager, jump host logic, channel handling (russh).
src-tauri/src/telnet/   → Telnet connection manager (RFC 854, NAWS).
src-tauri/src/db/       → Database abstraction trait, SQLite and PostgreSQL implementations, migrations.
src-tauri/src/commands/       → Tauri IPC command handlers (session CRUD, connect, broadcast, etc.).
dsrc-tauri/src/credentials/       → Keychain integration via keyring-rs.
src-tauri/src/import/       → Parsers for mRemoteNG XML, SecureCRT XML, and CSV import.
src/components/       → React UI components: SessionTree, TerminalTabs, CommandPalette, Settings.
src/hooks/       → Custom React hooks for Tauri IPC, terminal lifecycle, and state management. 
src/stores/       → Zustand state stores for sessions, connections, UI state.
migrations/       → sqlx migration files (shared between SQLite and PostgreSQL).
```

## Essential Commands

```bash
# Build
cargo build

# Lint (must pass before committing)
cd src-tauri; cargo clippy -- -D warnings
cd src-tauri; cargo fmt -- --check
npx eslint src/ --ext .ts,.tsx
npx prettier --check "src/**/*.{ts,tsx,css,json}"
npx tsc --noEmit
```

## Skills Available

- `codebase-navigator` — use when first exploring this repo
- `code-quality` — use before committing any changes

## See Also

@README.md
