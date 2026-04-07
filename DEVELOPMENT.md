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
- [Code Signing](#23-code-signing)
- [CI/CD Release Pipeline](#24-cicd-release-pipeline)

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

Once all dependencies are installed, initialize the ShellStation project:

### Create the Tauri project

```bash
cargo tauri init
```

Or, to scaffold with the React + TypeScript + Vite template in one step:

```bash
npm create tauri-app@latest shellstation -- \
    --template react-ts \
    --manager npm
cd shellstation
```

### Install frontend dependencies

```bash
npm install
```

### Install core frontend libraries

```bash
npm install @tauri-apps/api@latest
npm install zustand                   # state management
npm install @xterm/xterm              # terminal emulator
npm install @xterm/addon-webgl        # GPU-accelerated renderer
npm install @xterm/addon-fit          # auto-resize terminal to container
npm install @xterm/addon-search       # search within scrollback
npm install @xterm/addon-ligatures    # font ligature support
```

### Install dev dependencies

```bash
npm install -D \
    @types/react \
    @types/react-dom \
    eslint \
    @eslint/js \
    typescript-eslint \
    eslint-plugin-react \
    eslint-plugin-react-hooks \
    eslint-plugin-security \
    prettier \
    vitest \
    @testing-library/react \
    @testing-library/jest-dom
```

### Add Rust crate dependencies

Add the following to `src-tauri/Cargo.toml` under `[dependencies]`:

```toml
[dependencies]
tauri = { version = "2", features = [] }
russh = "0.45"
russh-keys = "0.45"
sqlx = { version = "0.8", features = ["runtime-tokio", "sqlite", "postgres", "uuid", "chrono"] }
tokio = { version = "1", features = ["full"] }
keyring = "3"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
uuid = { version = "1", features = ["v4", "serde"] }
portable-pty = "0.8"
tracing = "0.1"
tracing-subscriber = "0.3"
```

### Run the initial database migration

```bash
sqlx database create
sqlx migrate run
```

(This requires the `DATABASE_URL` environment variable to be set and the `migrations/` directory to contain migration files.)

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

The `tauri.conf.json` `bundle.targets` field controls which formats are built. The current setting `"all"` builds every format available on the host OS. To build only specific formats:

```json
{
  "bundle": {
    "targets": ["msi", "nsis"]
  }
}
```

Valid targets: `msi`, `nsis`, `dmg`, `app`, `deb`, `appimage`, `rpm`.

### Version bumping

Update the version in three places before a release:

1. `tauri.conf.json` — `"version"` field (drives installer version metadata).
2. `src-tauri/Cargo.toml` — `version` under `[package]`.
3. `package.json` — `"version"` field.

All three must match. The Tauri bundler reads the version from `tauri.conf.json` for the installer filename and metadata.

### Release profile optimization

The default Cargo release profile is sufficient for most cases. For maximum binary size reduction, add to `src-tauri/Cargo.toml`:

```toml
[profile.release]
strip = true        # Strip debug symbols
lto = true          # Link-time optimization (slower build, smaller binary)
codegen-units = 1   # Single codegen unit (slower build, better optimization)
opt-level = "s"     # Optimize for size over speed
```

Trade-off: `lto = true` with `codegen-units = 1` increases release build time significantly (10-20 min) but produces binaries 20-40% smaller.

---

## 23. Code Signing

Unsigned binaries trigger OS warnings (Windows SmartScreen, macOS Gatekeeper). Code signing is required for a professional release.

### Windows (Authenticode)

You need a code signing certificate from a Certificate Authority (e.g., DigiCert, Sectigo, SSL.com) or an EV certificate for immediate SmartScreen trust.

Configure in `tauri.conf.json`:

```json
{
  "bundle": {
    "windows": {
      "certificateThumbprint": "<YOUR_CERT_THUMBPRINT>",
      "digestAlgorithm": "sha256",
      "timestampUrl": "http://timestamp.digicert.com"
    }
  }
}
```

The certificate must be installed in the Windows Certificate Store. For CI, use `signtool.exe` with a PFX file or Azure Trusted Signing.

Environment variables for CI signing:

```text
TAURI_SIGNING_PRIVATE_KEY              Base64-encoded private key for Tauri updater
TAURI_SIGNING_PRIVATE_KEY_PASSWORD     Password for the private key
```

### macOS (codesign + notarization)

Requires an Apple Developer account ($99/year) and a "Developer ID Application" certificate.

Configure environment variables:

```bash
export APPLE_CERTIFICATE="<base64-encoded .p12>"
export APPLE_CERTIFICATE_PASSWORD="<p12 password>"
export APPLE_SIGNING_IDENTITY="Developer ID Application: Your Name (TEAMID)"
export APPLE_ID="your@email.com"
export APPLE_PASSWORD="<app-specific-password>"
export APPLE_TEAM_ID="TEAMID"
```

Tauri handles `codesign` and `notarytool submit` automatically when these variables are set during `cargo tauri build`.

### Linux (GPG signing, optional)

Linux packages (`.deb`, `.AppImage`) do not require code signing for distribution. Optional GPG signing of `.deb` packages can be done post-build with `dpkg-sig`.

---

## 24. CI/CD Release Pipeline

A GitHub Actions workflow that builds, signs, and publishes releases for all three platforms.

### Workflow file: `.github/workflows/release.yml`

```yaml
name: Release

on:
  push:
    tags:
      - "v*"

jobs:
  build:
    strategy:
      matrix:
        include:
          - platform: ubuntu-22.04
            target: linux
          - platform: windows-latest
            target: windows
          - platform: macos-latest
            target: macos

    runs-on: ${{ matrix.platform }}

    steps:
      - uses: actions/checkout@v4

      - name: Install Linux dependencies
        if: matrix.target == 'linux'
        run: |
          sudo apt update
          sudo apt install -y \
            libwebkit2gtk-4.1-dev \
            libgtk-3-dev \
            libayatana-appindicator3-dev \
            librsvg2-dev \
            libssl-dev \
            libsoup-3.0-dev \
            libjavascriptcoregtk-4.1-dev

      - name: Install Rust stable
        uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt

      - name: Install Node.js
        uses: actions/setup-node@v4
        with:
          node-version: 20
          cache: npm

      - name: Install frontend dependencies
        run: npm ci

      - name: Lint (Rust)
        working-directory: src-tauri
        run: |
          cargo clippy -- -D warnings
          cargo fmt -- --check

      - name: Lint (Frontend)
        run: |
          npx eslint src/ --ext .ts,.tsx
          npx prettier --check "src/**/*.{ts,tsx,css,json}"
          npx tsc --noEmit

      - name: Run tests
        run: |
          cd src-tauri && cargo test && cd ..
          npx vitest run

      - name: Build Tauri release
        uses: tauri-apps/tauri-action@v0
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          # Windows signing
          TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}
          TAURI_SIGNING_PRIVATE_KEY_PASSWORD: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD }}
          # macOS signing
          APPLE_CERTIFICATE: ${{ secrets.APPLE_CERTIFICATE }}
          APPLE_CERTIFICATE_PASSWORD: ${{ secrets.APPLE_CERTIFICATE_PASSWORD }}
          APPLE_SIGNING_IDENTITY: ${{ secrets.APPLE_SIGNING_IDENTITY }}
          APPLE_ID: ${{ secrets.APPLE_ID }}
          APPLE_PASSWORD: ${{ secrets.APPLE_PASSWORD }}
          APPLE_TEAM_ID: ${{ secrets.APPLE_TEAM_ID }}
        with:
          tagName: ${{ github.ref_name }}
          releaseName: "ShellStation ${{ github.ref_name }}"
          releaseBody: "See the changelog for details."
          releaseDraft: true
          prerelease: false

      - name: Upload artifacts
        uses: actions/upload-artifact@v4
        with:
          name: shellstation-${{ matrix.target }}
          path: src-tauri/target/release/bundle/**/*
```

### Creating a release

```bash
# Bump versions in tauri.conf.json, Cargo.toml, and package.json
# Commit the version bump
git add -A && git commit -m "Bump version to 0.2.0"

# Tag and push
git tag v0.2.0
git push origin main --tags
```

The workflow triggers on the tag push, builds all three platforms in parallel, runs the full lint and test suite, produces signed installers, and creates a draft GitHub Release with the artifacts attached. Review the draft and publish when ready.

### Manual release build (no CI)

If building locally without CI:

```bash
# Build for the current platform
cargo tauri build

# Artifacts are in src-tauri/target/release/bundle/
ls src-tauri/target/release/bundle/
```

Cross-compilation is not supported by Tauri — each platform must be built on its native OS. For a multi-platform release without CI, build on each target machine and collect the artifacts manually.

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
