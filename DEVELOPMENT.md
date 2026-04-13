# ShellStation — Development Environment Setup

This guide walks through every step needed to set up a development workstation for ShellStation, from system dependencies to a fully working build. It covers Linux, macOS, and Windows 11, plus instructions for producing release-ready production builds.

> **Tested on:** Ubuntu 22.04+, Debian 12+, Fedora 37+, Arch Linux, macOS 13 Ventura+, Windows 11 23H2+.

---

## Table of Contents

### Linux

- [System Dependencies](#1-system-dependencies)
- [Rust Toolchain](#2-rust-toolchain)
- [Node.js and npm](#3-nodejs-and-npm)
- [Tauri CLI](#4-tauri-cli)
- [Database Tooling](#5-database-tooling)
- [VS Code Extensions](#6-vs-code-extensions)
- [Project Bootstrap](#7-project-bootstrap)
- [Verify the Environment](#8-verify-the-environment)
- [Common Issues (Linux)](#9-common-issues-linux)

### macOS

- [Prerequisites (macOS)](#10-macos-prerequisites)
- [Rust Toolchain (macOS)](#11-macos-rust-toolchain)
- [Node.js and npm (macOS)](#12-macos-nodejs-and-npm)
- [Tauri CLI and Database Tooling (macOS)](#13-macos-tauri-cli-and-database-tooling)
- [Project Bootstrap and Verify (macOS)](#14-macos-project-bootstrap-and-verify)
- [Common Issues (macOS)](#15-macos-common-issues)

### Windows 11

- [Prerequisites (Windows)](#16-windows-11-prerequisites)
- [Rust Toolchain (Windows)](#17-windows-11-rust-toolchain)
- [Node.js and npm (Windows)](#18-windows-11-nodejs-and-npm)
- [Tauri CLI and Database Tooling (Windows)](#19-windows-11-tauri-cli-and-database-tooling)
- [Project Bootstrap and Verify (Windows)](#20-windows-11-project-bootstrap-and-verify)
- [Common Issues (Windows)](#21-windows-11-common-issues)

### Production Builds

- [Building for Release](#22-building-for-release)
- [CI/CD Release Pipeline (Forgejo Actions)](#23-cicd-release-pipeline-forgejo-actions)
- [Forgejo Runner Setup](#24-forgejo-runner-setup)

### Maintenance

- [Updating Dependencies](#25-updating-dependencies)

### Testlab

- [Docker SSH/Telnet Testlab](#26-docker-sshtelnet-testlab)

---

## 1. System Dependencies

Tauri 2.x uses WebKitGTK 4.1 on Linux for its webview. The following packages provide the build toolchain, WebKit rendering engine, GTK bindings, and supporting libraries.

### Debian / Ubuntu (22.04+)

```bash
sudo apt update
sudo apt install -y \
    build-essential \
    curl \
    wget \
    file \
    pkg-config \
    libssl-dev \
    libgtk-3-dev \
    libwebkit2gtk-4.1-dev \
    libayatana-appindicator3-dev \
    librsvg2-dev \
    libdbus-1-dev \
    libsoup-3.0-dev \
    libjavascriptcoregtk-4.1-dev
```

### Fedora (37+)

```bash
sudo dnf check-update
sudo dnf install -y \
    webkit2gtk4.1-devel \
    openssl-devel \
    curl \
    wget \
    file \
    libappindicator-gtk3-devel \
    librsvg2-devel
sudo dnf group install "C Development Tools and Libraries"
```

### Arch Linux

```bash
sudo pacman -Syu
sudo pacman -S --needed \
    webkit2gtk-4.1 \
    base-devel \
    curl \
    wget \
    file \
    openssl \
    appmenu-gtk-module \
    gtk3 \
    libappindicator-gtk3 \
    librsvg
```

---

## 2. Rust Toolchain

Install Rust via rustup (the official installer). Do not use distribution packages — they tend to lag behind and miss components like clippy and rustfmt.

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Follow the prompts and select the default installation. Then reload your shell:

```bash
source "$HOME/.cargo/env"
```

### Verify installation

```bash
rustc --version
cargo --version
rustup --version
```

### Install required components

```bash
rustup component add clippy rustfmt
```

- **clippy** — the Rust linter. ShellStation enforces zero clippy warnings.
- **rustfmt** — the standard Rust formatter.

### Install cargo utilities

```bash
cargo install cargo-watch    # auto-rebuild on file changes during development
cargo install sqlx-cli --no-default-features --features native-tls,sqlite,postgres
```

- **cargo-watch** — runs `cargo check` (or any command) on every save, useful for fast feedback during development.
- **sqlx-cli** — manages database migrations for both SQLite and PostgreSQL. The feature flags here enable both backends.

### Keep Rust up to date

```bash
rustup update
```

Run this periodically (or set up a cron job) to stay on the latest stable toolchain.

---

## 3. Node.js and npm

ShellStation's frontend is built with React + TypeScript + Vite, which requires Node.js. Use Node 20 LTS or later.

### Option A: Install via NodeSource (recommended)

```bash
curl -fsSL https://deb.nodesource.com/setup_20.x | sudo -E bash -
sudo apt install -y nodejs
```

### Option B: Install via nvm (Node Version Manager)

If you prefer to manage multiple Node versions:

```bash
curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.1/install.sh | bash
source ~/.bashrc
nvm install 20
nvm use 20
```

### Verify installed versions

```bash
node --version    # should be v20.x or later
npm --version     # should be 10.x or later
```

### Install global npm tools

```bash
sudo npm install -g typescript
```

---

## 4. Tauri CLI

The Tauri CLI handles project scaffolding, development server, and bundling. Install it via cargo:

```bash
cargo install tauri-cli --version "^2"
```

This installs the `cargo tauri` subcommand. Verify it with:

```bash
cargo tauri --version
```

Alternatively, you can use the npm-based CLI wrapper (useful if you prefer npx-style invocation):

```bash
sudo npm install -g @tauri-apps/cli@latest
```

### Run the environment check

Tauri ships a built-in diagnostic command that validates your system dependencies:

```bash
cargo tauri info
```

Review the output. Every line should show a checkmark. If anything is marked with ✘, install the missing dependency before continuing.

---

## 5. Database Tooling

ShellStation supports SQLite (local) and PostgreSQL (shared/team). You need the development libraries for both at build time.

### SQLite

SQLite is typically already available on most Linux systems. Ensure the development headers are present:

```bash
sudo apt install -y libsqlite3-dev sqlite3
```

Verify:

```bash
sqlite3 --version
```

### PostgreSQL (optional for local development)

If you want to develop and test against a real PostgreSQL instance locally:

```bash
sudo apt install -y postgresql postgresql-client libpq-dev
```

Start the service and create a development database:

```bash
sudo systemctl start postgresql
sudo systemctl enable postgresql

# Create a development user and database
sudo -u postgres createuser --interactive --pwprompt shellstation
sudo -u postgres createdb --owner=shellstation shellstation_dev
```

Set the database URL as an environment variable (add this to your `~/.bashrc` or `~/.zshrc`):

```bash
export DATABASE_URL="postgres://shellstation:<your-password>@localhost/shellstation_dev"
```

### Alternative: PostgreSQL via Docker

If you prefer not to install PostgreSQL system-wide:

```bash
docker run -d \
    --name shellstation-postgres \
    -e POSTGRES_USER=shellstation \
    -e POSTGRES_PASSWORD=devpassword \
    -e POSTGRES_DB=shellstation_dev \
    -p 5432:5432 \
    postgres:16-alpine
```

Then set:

```bash
export DATABASE_URL="postgres://shellstation:devpassword@localhost/shellstation_dev"
```

### SQLite for development (default)

For day-to-day development without PostgreSQL, sqlx can operate against a local SQLite file. Set:

```bash
export DATABASE_URL="sqlite:///home/$USER/.config/shellstation/dev.db"
```

Create the directory:

```bash
mkdir -pv /home/$USER/.config/shellstation
```

sqlx-cli will create the file automatically when you run migrations.

---

## 6. VS Code Extensions

The following extensions provide the best development experience for a Tauri + Rust + React/TypeScript project. Install them from the VS Code extensions panel or via the command line.

### Required

```bash
# Rust language support (rust-analyzer)
code --install-extension rust-lang.rust-analyzer

# Tauri integration
code --install-extension tauri-apps.tauri-vscode

# ESLint for TypeScript/React linting
code --install-extension dbaeumer.vscode-eslint

# Prettier for frontend formatting
code --install-extension esbenp.prettier-vscode
```

### Recommended

```bash
# TypeScript/JavaScript language support (ships with VS Code but ensure latest)
code --install-extension ms-vscode.vscode-typescript-next

# Better TOML support (for Cargo.toml, tauri.conf.json, etc.)
code --install-extension tamasfe.even-better-toml

# Crates helper (shows latest versions inline in Cargo.toml)
code --install-extension serayuzgur.crates

# Error Lens (inline compiler/lint errors)
code --install-extension usernamehw.errorlens

# SQLite viewer (inspect local DB from VS Code)
code --install-extension alexcvzz.vscode-sqlite

# GitLens (enhanced Git integration)
code --install-extension eamodio.gitlens
```

### VS Code settings (recommended)

Add these to your workspace `.vscode/settings.json` to align with ShellStation's quality standards:

```json
{
    "editor.formatOnSave": true,
    "editor.defaultFormatter": "esbenp.prettier-vscode",
    "[rust]": {
        "editor.defaultFormatter": "rust-lang.rust-analyzer",
        "editor.formatOnSave": true
    },
    "rust-analyzer.check.command": "clippy",
    "rust-analyzer.check.extraArgs": ["--", "-D", "warnings"],
    "eslint.validate": [
        "javascript",
        "javascriptreact",
        "typescript",
        "typescriptreact"
    ],
    "typescript.tsdk": "node_modules/typescript/lib",
    "editor.codeActionsOnSave": {
        "source.fixAll.eslint": "explicit"
    }
}
```

This configures rust-analyzer to run clippy instead of the default `cargo check` (catching more issues in real time) and sets up ESLint auto-fix on save.

---

## 7. Project Bootstrap

### Clone and install (Linux)

```bash
git clone https://git.fiedler.live/tux/shellstation.git
cd shellstation
npm install
```

`npm install` reads `package.json` and pulls all frontend dependencies including the Tauri CLI wrapper. Rust crates are resolved automatically by Cargo on the first build.

### Run the initial database migration

```bash
sqlx database create
sqlx migrate run
```

(Requires the `DATABASE_URL` environment variable to be set — see [Database Tooling](#5-database-tooling).)

### First build and run

```bash
cargo tauri dev
```

This compiles the Rust backend, starts the Vite dev server for the frontend, and opens the application window. The first build takes several minutes as it compiles all Rust dependencies.

---

## 8. Verify the Environment

Run the full quality check suite to confirm everything is wired up correctly:

### Rust checks

```bash
cd src-tauri

# Linter (zero warnings enforced)
cargo clippy -- -D warnings

# Formatter check (no changes = pass)
cargo fmt -- --check

# Unit tests
cargo test
```

### Frontend checks

```bash
# ESLint (zero errors/warnings)
npx eslint src/ --ext .ts,.tsx

# Prettier format check
npx prettier --check "src/**/*.{ts,tsx,css,json}"

# TypeScript type check
npx tsc --noEmit

# Unit tests
npx vitest run
```

### Tauri environment check

```bash
cargo tauri info
```

If all commands pass without errors, your development environment is ready.

---

## 9. Common Issues (Linux)

### `webkit2gtk-4.1` not found (Ubuntu < 22.04)

Tauri 2.x requires WebKitGTK 4.1, which is only available in Ubuntu 22.04 and later. If you are on an older release, you must upgrade your distribution or use a Docker-based build environment.

### `pkg-config` errors during `cargo build`

Usually means a `-dev` package is missing. Read the error message — it will name the `.pc` file it cannot find. Search for the package that provides it:

```bash
apt-file search missing-package.pc
```

Install `apt-file` first if needed: `sudo apt install apt-file && sudo apt-file update`.

### `sqlx` compile-time errors ("cannot find database")

sqlx verifies queries at compile time against a real database. If you don't have a running database, you can use sqlx's offline mode:

```bash
cargo sqlx prepare
```

This generates a `.sqlx/` directory with cached query metadata, allowing compilation without a live database connection.

### Slow first build

The initial `cargo build` downloads and compiles all Rust crate dependencies. This is normal and can take 5–10 minutes depending on your hardware. Subsequent builds are incremental and much faster. Using `cargo-watch` avoids full rebuilds during development.

### Permission denied on keychain (Linux)

The `keyring` crate requires a running D-Bus session and a secret service (GNOME Keyring or KWallet). If you are developing in a headless environment or minimal desktop:

```bash
sudo apt install -y gnome-keyring libsecret-1-dev
```

Then ensure a keyring daemon is running in your session.

### WebGL not available in Tauri dev mode

Some Linux environments (particularly VMs or Wayland sessions) may not expose WebGL to the webview. xterm.js will automatically fall back to the canvas renderer. This is not a bug — the application handles it gracefully.

---

## 10. macOS Prerequisites

Tauri 2.x on macOS uses the system WebKit framework (WKWebView) — no additional webview runtime is needed. The primary build dependency is the Xcode Command Line Tools.

### Install Xcode Command Line Tools

Open Terminal and run:

```bash
xcode-select --install
```

This installs `clang`, `make`, `git`, and the macOS SDK headers. You do **not** need the full Xcode IDE unless you plan to do code signing or work with Xcode projects directly.

Verify the installation:

```bash
xcode-select -p
# Should print: /Library/Developer/CommandLineTools or /Applications/Xcode.app/Contents/Developer
```

### Install Homebrew

Homebrew is the standard package manager for macOS development dependencies:

```bash
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
```

Follow the post-install instructions to add Homebrew to your `PATH`. Then verify:

```bash
brew --version
```

---

## 11. macOS Rust Toolchain

### Install Rust via rustup (macOS)

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Follow the prompts and select the default installation. Then reload your shell:

```bash
source "$HOME/.cargo/env"
```

### Verify Rust installation (macOS)

```bash
rustc --version
cargo --version
rustup --version
```

### Install required Rust components (macOS)

```bash
rustup component add clippy rustfmt
```

### Install cargo utilities (macOS)

```bash
cargo install cargo-watch
cargo install sqlx-cli --no-default-features --features native-tls,sqlite,postgres
```

### Keep Rust up to date (macOS)

```bash
rustup update
```

---

## 12. macOS Node.js and npm

### Install Node.js 20 LTS via Homebrew (recommended)

```bash
brew install node@20
```

Homebrew installs Node into a keg-only path. Follow the post-install output to add it to your `PATH`, or link it:

```bash
brew link --overwrite node@20
```

### Alternative: Install via nvm (macOS)

```bash
curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.1/install.sh | bash
source ~/.zshrc
nvm install 20
nvm use 20
```

### Verify installed versions (macOS)

```bash
node --version    # should be v20.x or later
npm --version     # should be 10.x or later
```

### Install global npm tools (macOS)

```bash
npm install -g typescript
```

---

## 13. macOS Tauri CLI and Database Tooling

### Tauri CLI (macOS)

```bash
cargo install tauri-cli --version "^2"
```

Verify:

```bash
cargo tauri --version
```

Run the environment diagnostic:

```bash
cargo tauri info
```

Review the output. Every line should show a checkmark. If anything is marked with a cross, install the missing dependency before continuing.

### SQLite (macOS)

macOS ships with SQLite pre-installed. Verify:

```bash
sqlite3 --version
```

If you need a newer version:

```bash
brew install sqlite
```

### PostgreSQL (optional, macOS)

For local PostgreSQL development:

```bash
brew install postgresql@16
brew services start postgresql@16
```

Create the development database:

```bash
createuser --interactive --pwprompt shellstation
createdb --owner=shellstation shellstation_dev
```

Set the database URL (add to your `~/.zshrc`):

```bash
export DATABASE_URL="postgres://shellstation:<your-password>@localhost/shellstation_dev"
```

### Alternative: PostgreSQL via Docker (macOS)

```bash
docker run -d \
    --name shellstation-postgres \
    -e POSTGRES_USER=shellstation \
    -e POSTGRES_PASSWORD=devpassword \
    -e POSTGRES_DB=shellstation_dev \
    -p 5432:5432 \
    postgres:16-alpine
```

Then set:

```bash
export DATABASE_URL="postgres://shellstation:devpassword@localhost/shellstation_dev"
```

### SQLite for development (default, macOS)

```bash
export DATABASE_URL="sqlite://$HOME/Library/Application Support/shellstation/dev.db"
mkdir -p "$HOME/Library/Application Support/shellstation"
```

---

## 14. macOS Project Bootstrap and Verify

### Clone and install (macOS)

```bash
git clone https://git.fiedler.live/tux/shellstation.git
cd shellstation
npm install
```

### Run database migrations (macOS)

```bash
sqlx database create
sqlx migrate run
```

### First build and run (macOS)

```bash
cargo tauri dev
```

The first build compiles all Rust dependencies and takes several minutes. Subsequent builds are incremental.

### Run the full quality check suite (macOS)

Rust checks:

```bash
cd src-tauri
cargo clippy -- -D warnings
cargo fmt -- --check
cargo test
cd ..
```

Frontend checks:

```bash
npx eslint src/ --ext .ts,.tsx
npx prettier --check "src/**/*.{ts,tsx,css,json}"
npx tsc --noEmit
npx vitest run
```

Tauri environment check:

```bash
cargo tauri info
```

If all commands pass without errors, your macOS development environment is ready.

---

## 15. macOS Common Issues

### `xcrun: error: invalid active developer path`

This means Xcode Command Line Tools are not installed or need updating:

```bash
xcode-select --install
```

If already installed but still failing after a macOS upgrade, reset the path:

```bash
sudo xcode-select --reset
```

### `pkg-config` not found (macOS)

Some Rust crates use `pkg-config` to locate system libraries. Install it via Homebrew:

```bash
brew install pkg-config
```

### `sqlx` compile-time errors (macOS)

Same as Linux — sqlx verifies queries at compile time. Use offline mode if no database is available:

```bash
cargo sqlx prepare
```

### Slow first build (macOS)

Normal on macOS as well. The initial `cargo build` can take 5-10 minutes. Subsequent builds are incremental. Using `cargo-watch` avoids full rebuilds during development.

### Permission denied on keychain (macOS)

The `keyring` crate uses macOS Keychain by default. If running in a CI environment or headless context, Keychain access may be restricted. For local development, Keychain access should work without additional configuration. If prompted, allow the `shellstation` binary to access the keychain.

### Apple Silicon (M1/M2/M3) considerations

Rust natively supports `aarch64-apple-darwin`. The default rustup installation on Apple Silicon installs the ARM toolchain. No Rosetta or special configuration is needed. All dependencies (including Homebrew, Node.js, and SQLite) run natively on ARM.

If you need to cross-compile for Intel Macs:

```bash
rustup target add x86_64-apple-darwin
cargo tauri build --target x86_64-apple-darwin
```

### WebGL not available in development

Some macOS configurations (particularly in VMs) may not expose WebGL to the webview. xterm.js falls back to the canvas renderer automatically. This is not a bug.

---

## 16. Windows 11 Prerequisites

Tauri 2.x on Windows uses Microsoft Edge WebView2 (pre-installed on Windows 11) for its webview. The primary build dependency is the Microsoft C++ Build Tools.

### Install Visual Studio Build Tools

Download **Visual Studio 2022 Build Tools** from <https://visualstudio.microsoft.com/visual-cpp-build-tools/>.

During installation, select the following workload:

- **Desktop development with C++**

Under "Individual components", ensure these are checked (they should be by default with the workload):

- MSVC v143 — VS 2022 C++ x64/x86 build tools (Latest)
- Windows 11 SDK (10.0.22621.0 or later)
- C++ CMake tools for Windows

> This provides `cl.exe`, `link.exe`, the Windows SDK headers, and the CRT libraries that Rust's MSVC toolchain requires. You do **not** need the full Visual Studio IDE — the Build Tools installer is sufficient.

### Verify WebView2

WebView2 ships with Windows 11. Confirm it is present:

```powershell
Get-ItemProperty -Path "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" -Name pv
```

If this returns a version string, WebView2 is installed. If not, download the Evergreen Bootstrapper from <https://developer.microsoft.com/en-us/microsoft-edge/webview2/>.

### Install Git for Windows

If not already installed:

```powershell
winget install Git.Git
```

After installation, open a new terminal and verify:

```powershell
git --version
```

### Optional: Windows Terminal

Windows Terminal provides a better experience than the default console. Install via:

```powershell
winget install Microsoft.WindowsTerminal
```

---

## 17. Windows 11 Rust Toolchain

### Install Rust via rustup (Windows)

Open PowerShell and run:

```powershell
winget install Rustlang.Rustup
```

Or download the installer from <https://rustup.rs/>.

Follow the prompts and select the default installation (which uses the `stable-x86_64-pc-windows-msvc` toolchain). Close and reopen your terminal after installation.

### Verify Rust installation (Windows)

```powershell
rustc --version
cargo --version
rustup --version
```

### Install required Rust components (Windows)

```powershell
rustup component add clippy rustfmt
```

### Install cargo utilities (Windows)

```powershell
cargo install cargo-watch
cargo install sqlx-cli --no-default-features --features native-tls,sqlite,postgres
```

### Keep Rust up to date (Windows)

```powershell
rustup update
```

---

## 18. Windows 11 Node.js and npm

### Install Node.js 20 LTS

```powershell
winget install OpenJS.NodeJS.LTS
```

Close and reopen your terminal, then verify:

```powershell
node --version
npm --version
```

### Install global npm tools (Windows)

```powershell
npm install -g typescript
```

---

## 19. Windows 11 Tauri CLI and Database Tooling

### Tauri CLI (Windows)

```powershell
cargo install tauri-cli --version "^2"
```

Verify:

```powershell
cargo tauri --version
```

Run the environment diagnostic:

```powershell
cargo tauri info
```

Review the output. Every line should show a checkmark. If anything is marked with a cross, install the missing dependency before continuing.

### SQLite (Windows)

SQLite is compiled from source by the `sqlx` crate via the bundled `sqlite3` feature — no separate installation is needed on Windows. However, if you want the `sqlite3` CLI for inspecting databases:

```powershell
winget install SQLite.SQLite
```

### PostgreSQL (optional, Windows)

For local PostgreSQL development:

```powershell
winget install PostgreSQL.PostgreSQL
```

Follow the installer prompts to set a superuser password and create the default cluster. Then open a new terminal and create the development database:

```powershell
createuser --interactive --pwprompt shellstation
createdb --owner=shellstation shellstation_dev
```

Set the environment variable (add to your PowerShell profile or system environment variables):

```powershell
$env:DATABASE_URL = "postgres://shellstation:<your-password>@localhost/shellstation_dev"
```

To make it permanent, use System Settings > Environment Variables, or add the line to your `$PROFILE` file.

### SQLite for development (default, Windows)

```powershell
$env:DATABASE_URL = "sqlite:///$env:APPDATA/shellstation/dev.db"
New-Item -ItemType Directory -Force -Path "$env:APPDATA\shellstation"
```

---

## 20. Windows 11 Project Bootstrap and Verify

### Clone and install (Windows)

```powershell
git clone https://git.fiedler.live/tux/shellstation.git
cd shellstation
npm install
```

### Run database migrations (Windows)

```powershell
sqlx database create
sqlx migrate run
```

### First build and run (Windows)

```powershell
cargo tauri dev
```

The first build compiles all Rust dependencies and takes several minutes. Subsequent builds are incremental.

### Run the full quality check suite (Windows)

Rust checks:

```powershell
cd src-tauri
cargo clippy -- -D warnings
cargo fmt -- --check
cargo test
cd ..
```

Frontend checks:

```powershell
npx eslint src/ --ext .ts,.tsx
npx prettier --check "src/**/*.{ts,tsx,css,json}"
npx tsc --noEmit
npx vitest run
```

Tauri environment check:

```powershell
cargo tauri info
```

If all commands pass without errors, your Windows development environment is ready.

---

## 21. Windows 11 Common Issues

### `link.exe` not found or MSVC errors

The Rust MSVC toolchain requires the Visual Studio Build Tools. If you see linker errors, ensure you installed the "Desktop development with C++" workload and are using a terminal that has the MSVC environment loaded. Opening **Developer PowerShell for VS 2022** from the Start menu guarantees the correct paths are set.

### `pkg-config` not found (Windows)

Some crates probe for system libraries using `pkg-config`, which is not natively available on Windows. Most ShellStation dependencies use vendored/bundled C libraries (e.g., SQLite via `sqlx`), so this should not be an issue. If a crate does require it, install via:

```powershell
winget install bloodrock.pkg-config-lite
```

### Long path errors

Enable long path support if you encounter `MAX_PATH` (260 character) errors:

```powershell
# Run PowerShell as Administrator
New-ItemProperty -Path "HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem" -Name "LongPathsEnabled" -Value 1 -PropertyType DWORD -Force
```

Also configure Git:

```powershell
git config --global core.longpaths true
```

### Windows Defender slowing builds

Windows Defender real-time scanning can significantly slow Rust compilation. Add exclusions for:

- Your project directory (e.g., `C:\Users\<you>\Projects\shellstation`)
- The Cargo registry and build cache (`%USERPROFILE%\.cargo`)
- The Rust toolchain directory (`%USERPROFILE%\.rustup`)

Open **Windows Security > Virus & threat protection > Manage settings > Exclusions > Add or remove exclusions** and add each path as a folder exclusion.

### WebView2 runtime issues

If the application window opens but shows a blank white screen, WebView2 may need reinstalling. Download the Evergreen Standalone Installer from the Microsoft Edge WebView2 page and run it.

---

## 22. Building for Release

This section covers producing optimized, distributable binaries for all platforms.

### Prerequisites

Ensure all quality checks pass before building a release:

```bash
# Rust
cd src-tauri && cargo clippy -- -D warnings && cargo fmt -- --check && cargo test && cd ..

# Frontend
npx eslint src/ --ext .ts,.tsx
npx prettier --check "src/**/*.{ts,tsx,css,json}"
npx tsc --noEmit
npx vitest run
```

### Build the release bundle

```bash
cargo tauri build
```

This command:

1. Runs `npm run build` (TypeScript type check + Vite production build with minification).
2. Compiles the Rust backend in release mode (`--release`) with full optimizations.
3. Produces platform-specific installers in `src-tauri/target/release/bundle/`.

### Output artifacts by platform

| Platform | Artifacts | Location |
| --- | --- | --- |
| Windows | `.msi` installer, `.exe` (NSIS installer) | `src-tauri/target/release/bundle/msi/` and `nsis/` |
| macOS | `.dmg` disk image, `.app` bundle | `src-tauri/target/release/bundle/dmg/` and `macos/` |
| Linux | `.deb` package, `.AppImage` | `src-tauri/target/release/bundle/deb/` and `appimage/` |

### Configuring bundle targets

The `tauri.conf.json` `bundle.targets` field controls which formats are built. The current setting `"all"` builds every format available on the host OS. To build only specific formats, pass `--bundles` on the command line:

```bash
cargo tauri build -- --bundles deb,appimage    # Linux
cargo tauri build -- --bundles msi,nsis        # Windows
cargo tauri build -- --bundles dmg,app         # macOS
```

Valid targets: `msi`, `nsis`, `dmg`, `app`, `deb`, `appimage`, `rpm`.

### Version bumping and release process

The version-bumping checklist and end-to-end release flow live in [RELEASING.md](RELEASING.md). The release profile (`strip`, `lto`, `codegen-units`, `opt-level`) is already configured in `src-tauri/Cargo.toml`.

---

## 23. CI/CD Release Pipeline (Forgejo Actions)

ShellStation is built and released via Forgejo Actions on a self-hosted runner setup. The pipeline lives at [`.forgejo/workflows/release.yml`](.forgejo/workflows/release.yml).

### Pipeline overview

| Job              | Runner label | Outputs                            |
| ---------------- | ------------ | ---------------------------------- |
| `build-linux`    | `linux`      | `.deb`, `.AppImage`                |
| `build-windows`  | `windows`    | `.msi`, `.exe` (NSIS)              |
| `publish-release`| `linux`      | Forgejo release with all artifacts |

macOS is **not** built by the pipeline. Apple's notarization toolchain is macOS-only and requires Xcode, so macOS artifacts are produced manually on a developer's MacBook and uploaded to the same release page after the pipeline completes. See [RELEASING.md](RELEASING.md) for the manual macOS build steps.

### Trigger

The pipeline fires on any tag matching `v*`:

```bash
git tag v0.9.0
git push origin v0.9.0
```

### Required repository secrets

| Secret name     | Purpose                                                                                                                            |
| --------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| `RELEASE_TOKEN` | Forgejo access token with `write:repository` scope. Used by the `publish-release` job to create the release and upload artifacts. |

Generate the token at <https://git.fiedler.live/user/settings/applications> and add it under repo **Settings → Actions → Secrets**.

### Code signing

Releases are currently **unsigned**. Windows users see a SmartScreen "Unknown publisher" warning on first launch; macOS users must right-click the `.app` and choose **Open** (or run `xattr -d com.apple.quarantine`). This is acceptable for early releases. If signing becomes a requirement later, the recommended path is [SignPath.io's free OSS program](https://signpath.org/foundation) — no hardware token, no business entity, integrates with CI via API.

---

## 24. Forgejo Runner Setup

The pipeline needs two self-hosted runners: one Linux, one Windows. Both run the `forgejo-runner` daemon and register against `https://git.fiedler.live`.

### 24.1. Linux runner

Any modern Debian/Ubuntu host works. Recommended: Ubuntu 22.04+ or Debian 12+.

**1. Install the build prerequisites** (matches what the pipeline expects to find):

```bash
sudo apt update
sudo apt install -y \
    build-essential curl wget file pkg-config \
    libssl-dev libgtk-3-dev libwebkit2gtk-4.1-dev \
    libayatana-appindicator3-dev librsvg2-dev \
    libdbus-1-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev
```

**2. Install Node.js 20 LTS and Rust stable** as a dedicated runner user:

```bash
sudo useradd -m -s /bin/bash forgejo-runner
sudo -iu forgejo-runner bash -c '
  curl -fsSL https://deb.nodesource.com/setup_20.x | sudo -E bash -
  curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
'
sudo apt install -y nodejs
```

**3. Download the Forgejo runner binary**:

```bash
sudo -iu forgejo-runner bash -c '
  cd ~
  curl -L -o forgejo-runner https://code.forgejo.org/forgejo/runner/releases/download/v6.2.2/forgejo-runner-6.2.2-linux-amd64
  chmod +x forgejo-runner
  ./forgejo-runner generate-config > config.yml
'
```

(Check <https://code.forgejo.org/forgejo/runner/releases> for the latest version.)

**4. Register against Forgejo**. Get a registration token from <https://git.fiedler.live/-/admin/actions/runners> (instance-wide) or your repo's **Settings → Actions → Runners** (repo-scoped):

```bash
sudo -iu forgejo-runner bash -c '
  cd ~
  ./forgejo-runner register --no-interactive \
    --instance https://git.fiedler.live \
    --token <REGISTRATION_TOKEN> \
    --name linux-builder \
    --labels linux:host
'
```

The `linux` label matches `runs-on: linux` in the workflow. The `:host` suffix is critical — it tells the runner to execute jobs directly on the host OS instead of inside a Docker container. Without it, the runner will fail to start with `daemon Docker Engine socket not found` unless Docker is installed. Host mode is what you want for a Tauri build runner: the host already has Rust, Node, and the GTK dev libs installed, so there's no reason to add a container layer.

**5. Run as a systemd service**:

```bash
sudo tee /etc/systemd/system/forgejo-runner.service >/dev/null <<'EOF'
[Unit]
Description=Forgejo Actions Runner
After=network.target

[Service]
Type=simple
User=forgejo-runner
WorkingDirectory=/home/forgejo-runner
ExecStart=/home/forgejo-runner/forgejo-runner daemon
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable --now forgejo-runner
sudo systemctl status forgejo-runner
```

Verify the runner is online at <https://git.fiedler.live/-/admin/actions/runners>.

### 24.2. Windows runner

**Recommended OS:** Windows 11 Pro 23H2 or 24H2 (x64). Reasons:

- Matches what end users have, so platform-specific bugs surface during the build
- WebView2 Runtime is preinstalled (Tauri requires it)
- Modern PowerShell, winget, and proper symlink support
- Pro (not Home) gives Remote Desktop, Group Policy, Hyper-V

Avoid Windows Server (different WebView2 story), Windows 10 (EOL October 2025), and Windows 11 LTSC (no WebView2 by default).

**1. Base OS prep**:

- Install Windows 11 Pro, fully patch it, set a static hostname
- Create a dedicated local user account (admin is simplest initially)
- Disable sleep and hibernate so the runner stays online: `powercfg /change standby-timeout-ac 0`

**2. Install build prerequisites** (elevated PowerShell):

```powershell
winget install --id Git.Git -e
winget install --id OpenJS.NodeJS.LTS -e
winget install --id Rustlang.Rustup -e
winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --add Microsoft.VisualStudio.Component.Windows11SDK.22621 --includeRecommended"
winget install --id Microsoft.EdgeWebView2Runtime -e
```

Open a fresh terminal and verify:

```powershell
git --version
node --version
rustup default stable
rustc --version
cargo --version
```

**3. Install the Forgejo runner binary**:

- Download `forgejo-runner-<ver>-windows-amd64.exe` from <https://code.forgejo.org/forgejo/runner/releases>
- Place it at `C:\forgejo-runner\forgejo-runner.exe`
- In PowerShell:

```powershell
cd C:\forgejo-runner
.\forgejo-runner.exe generate-config > config.yml
```

**4. Register against Forgejo** (same token source as the Linux runner):

```powershell
.\forgejo-runner.exe register --no-interactive `
  --instance https://git.fiedler.live `
  --token <REGISTRATION_TOKEN> `
  --name windows-builder `
  --labels windows:host
```

The `windows` label matches `runs-on: windows` in the workflow. The `:host` suffix is critical — it tells the runner to execute jobs directly on Windows instead of inside a Docker container. Without it, the service will fail to start with `daemon Docker Engine socket not found`. If you already registered without `:host` and hit that error, stop the service, delete `C:\forgejo-runner\.runner`, grab a fresh registration token from the Forgejo admin UI, and re-run the register command with the corrected label.

**5. Run as a Windows service** using NSSM so it survives reboots:

```powershell
winget install --id NSSM.NSSM -e
nssm install ForgejoRunner "C:\forgejo-runner\forgejo-runner.exe" "daemon"
nssm set ForgejoRunner AppDirectory "C:\forgejo-runner"
nssm set ForgejoRunner Start SERVICE_AUTO_START
nssm start ForgejoRunner
```

Verify the runner appears as `windows-builder` at <https://git.fiedler.live/-/admin/actions/runners>.

**6. Smoke test**: push a throwaway tag (`git tag v0.9.0-test1 && git push origin v0.9.0-test1`) and watch both runners pick up their jobs in the Actions tab. Delete the tag after the test runs.

## 25. Updating Dependencies

Keep dependencies up-to-date periodically (at minimum before each release) to pick up security patches and bug fixes.

### Rust crates

```bash
# Check for outdated crates (install cargo-outdated if not present)
cargo install cargo-outdated
cd src-tauri && cargo outdated

# Update all crates to the latest version allowed by Cargo.toml constraints
cd src-tauri && cargo update

# To upgrade past semver-pinned versions, edit src-tauri/Cargo.toml
# then run cargo update again. Review changelogs for breaking changes.

# Audit for known security vulnerabilities (install cargo-audit if not present)
cargo install cargo-audit
cd src-tauri && cargo audit
```

#### Handling cargo audit findings

`cargo audit` reports the vulnerable crate name, its version, a RUSTSEC advisory ID, and a link to the full advisory description.

**Step 1 — Find why the crate is in your tree:**

```bash
# Shows which of your dependencies pulls in the vulnerable crate
cd src-tauri && cargo tree -i <vulnerable-crate>
```

**Step 2 — Fix it:**

- **Direct dependency** (listed in `Cargo.toml`): bump the version in `Cargo.toml` to one that resolves the advisory, then run `cargo update`.
- **Transitive dependency** (pulled in by another crate): run `cargo update -p <vulnerable-crate>` to pull a patched version within the existing semver range. If no patched version exists, update the parent crate that depends on it.
- **No fix available yet**: check the advisory details. If the vulnerability does not apply to your usage (e.g., an unused feature or code path), you can temporarily suppress it with `cargo audit --ignore RUSTSEC-XXXX-XXXX` and track the upstream issue until a fix is released.

After updating, verify the build and lint pass:

```bash
cd src-tauri && cargo build && cargo clippy -- -D warnings && cargo test
```

### npm packages

```bash
# Check for outdated packages
npm outdated

# Update all packages within the version ranges in package.json
npm update

# To upgrade to new major versions beyond the pinned ranges, use:
npx npm-check-updates -u   # rewrites package.json to latest versions
npm install                 # install the updated versions

# Audit for known security vulnerabilities
npm audit

# Auto-fix vulnerabilities where possible
npm audit fix
```

After updating, verify the frontend build and lint pass:

```bash
npx tsc --noEmit && npx vite build
npx eslint src/ --ext .ts,.tsx
npx prettier --check "src/**/*.{ts,tsx,css,json}"
```

### Tauri CLI

The Tauri CLI is listed as a devDependency in `package.json` (`@tauri-apps/cli`) and is updated along with npm packages. Ensure the CLI version stays compatible with the `tauri` and `tauri-build` crate versions in `Cargo.toml` — they should track the same major.minor release line.

### Recommended workflow

1. Create a dedicated branch: `git checkout -b deps/update-YYYY-MM-DD`
2. Update Rust crates (`cargo update`) and npm packages (`npm update` or `npx npm-check-updates -u && npm install`).
3. Run `cargo audit` and `npm audit` to check for remaining vulnerabilities.
4. Run the full lint and test suite (see [Verify the Environment](#8-verify-the-environment)).
5. Test the application manually (connect via SSH/Telnet, open local terminal).
6. Commit and open a PR for review.

## 26. Docker SSH/Telnet Testlab

To test the functionality of connecting to a remote system via SSH or Telnet,
including with the use of a jumphost, see the Readme in the directory **ssh-testlab**.
The setup allows tests on a small footprint docker container construct, providing
an SSH jumphost, an SSH target, and a Telnet target.
