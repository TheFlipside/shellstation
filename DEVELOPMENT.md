# ShellStation — Development Environment Setup

This guide walks through every step needed to set up a Linux workstation for ShellStation development, from system dependencies to a fully working build. It assumes a Debian/Ubuntu-based distribution and VS Code already installed.

> **Tested on:** Ubuntu 22.04+ and Debian 12+. If you are on Fedora, Arch, or another distribution, equivalent packages are noted where applicable.

---

## Table of Contents

1. [System Dependencies](#1-system-dependencies)
2. [Rust Toolchain](#2-rust-toolchain)
3. [Node.js and npm](#3-nodejs-and-npm)
4. [Tauri CLI](#4-tauri-cli)
5. [Database Tooling](#5-database-tooling)
6. [VS Code Extensions](#6-vs-code-extensions)
7. [Project Bootstrap](#7-project-bootstrap)
8. [Verify the Environment](#8-verify-the-environment)
9. [Common Issues](#9-common-issues)

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

## 9. Common Issues

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
