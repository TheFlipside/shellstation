# Shellstation — dev task runner
#
# Install just:        cargo install just   (or: apt install just / pacman -S just)
# List all recipes:    just                 (the default recipe)
# Run a recipe:        just <name>

set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

# Default: list all recipes.
default:
    @just --list

# Format check + clippy with warnings as errors.
lint:
    cd src-tauri && cargo fmt --all -- --check
    cd src-tauri &&  cargo clippy --all-targets --all-features -- -D clippy::pedantic -D warnings

# Apply rustfmt in place.
fmt:
    cd src-tauri && cargo fmt --all

# Run all tests across the workspace.
test:
    cd src-tauri && cargo test --all

# Lint + test. Run before pushing.
check: lint test

# Crap
crap:
    cd src-tauri && cargo llvm-cov --lcov --output-path lcov.info && cargo crap --lcov lcov.info

# Run the app.
run:
    cargo tauri dev

# Start the containerized testlab
testlab:
    cd ssh-testlab && ./setup.sh

# Build a release.
package:
    npm ci
    npm run tauri build -- --bundles

# Remove build artifacts.
clean:
    cd src-tauri && cargo clean
    rm -rf src-tauri/target
