# Releasing ShellStation

End-to-end checklist for cutting a new release.

## 0. One-time setup: updater signing keys

The updater plugin (added in 0.21.0) requires a minisign-style Ed25519
keypair. Generate it **once**, store the private key in CI secrets, and
embed the public key in `src-tauri/tauri.conf.json`.

```bash
# Generate the keypair. You'll be prompted for a password — keep it.
npx tauri signer generate -w ~/.tauri/shellstation_updater.key

# Read the PUBLIC key (small) — copy into tauri.conf.json:
cat ~/.tauri/shellstation_updater.key.pub
```

In `src-tauri/tauri.conf.json` replace `REPLACE_WITH_BASE64_PUBLIC_KEY`
under `plugins.updater.pubkey` with the value you just printed.

In your Forgejo Actions runner secrets (`Settings → Secrets and Variables`):

| Secret name                          | Value                                           |
| ------------------------------------ | ----------------------------------------------- |
| `TAURI_SIGNING_PRIVATE_KEY`          | Contents of `~/.tauri/shellstation_updater.key` |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | The password you chose at generation time       |

**Never commit the private key.** Treat the password the same — both end up
on a CI runner only.

If you ever need to rotate: generate a new keypair, update `pubkey` in
`tauri.conf.json` AND ship that new app version first. Then update the CI
secrets. Old installs will only accept updates signed by the embedded
public key, so a key rotation needs at least one transition release.

## 1. Pre-flight

- [ ] All target features merged to `main`
- [ ] `cargo clippy -- -D warnings` clean (in `src-tauri/`)
- [ ] `cargo fmt -- --check` clean
- [ ] `npx eslint src/ --ext .ts,.tsx` clean
- [ ] `npx prettier --check "src/**/*.{ts,tsx,css,json}"` clean
- [ ] `npx tsc --noEmit` clean
- [ ] `cargo test` passes
- [ ] Manual smoke test on Linux (your dev box)

## 2. Bump the version

Update the version in **all four** locations to the new `X.Y.Z`:

| File                        | Field                  |
| --------------------------- | ---------------------- |
| `src-tauri/Cargo.toml`      | `[package].version`    |
| `src-tauri/tauri.conf.json` | `version`              |
| `package.json`              | `version`              |
| `package-lock.json`         | both top-level entries |

Refresh the lock files so they pick up the new version:

```bash
cd src-tauri && cargo update -p shellstation --offline && cd ..
npm install --package-lock-only --ignore-scripts
```

Commit:

```bash
git add src-tauri/Cargo.toml src-tauri/tauri.conf.json src-tauri/Cargo.lock \
        package.json package-lock.json
git commit -m "Bump version to X.Y.Z"
git push origin main
```

## 3. Tag and trigger the pipeline

```bash
git tag vX.Y.Z
git push origin vX.Y.Z
```

This fires `.forgejo/workflows/release.yml`, which:

1. Builds Linux `.deb` + `.AppImage` (+ `.AppImage.sig`) on the Linux runner
2. Builds Windows `.msi` + `.exe` (NSIS) + `.exe.sig` on the Windows runner
3. Builds macOS `.dmg` + `.app.tar.gz` + `.app.tar.gz.sig` on the macOS runners (x64 + arm64)
4. Generates `latest.json` — the updater manifest — from the collected `.sig` files
5. Creates a Forgejo release at `vX.Y.Z` and uploads all artifacts
6. Creates / updates the GitHub mirror release with the same artifacts

The updater plugin in the app fetches `latest.json` from the GitHub
release; if the build was unsigned (missing `TAURI_SIGNING_PRIVATE_KEY`
secret) the manifest will have an empty `platforms` object and the
plugin will treat the release as having no compatible update.

Watch progress at <https://git.fiedler.live/tux/shellstation/actions>.

## 4. (Historical) Build macOS manually

The macOS build is now part of the pipeline (see `build-macos-x64` and
`build-macos-arm64` jobs). The manual fallback below is kept for the case
where the macOS runner is offline.

```bash
git fetch --tags
git checkout vX.Y.Z
npm ci
# `app` bundle is needed in addition to `dmg` so the updater can produce
# the .app.tar.gz artifact. Signing env vars must be exported for the
# updater plugin to embed the signature.
export TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/shellstation_updater.key)"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="<your password>"
npm run tauri build -- --bundles dmg,app
```

Output lands in `src-tauri/target/release/bundle/dmg/` (the `.dmg` is the
first-install artifact) and `src-tauri/target/release/bundle/macos/`
(the `.app.tar.gz` + `.sig` is the updater artifact). Upload both to the
GitHub release manually if the pipeline didn't.

> **Note:** Unsigned macOS builds trigger Gatekeeper. End users must
> right-click the `.app` and choose **Open** the first time, or run
> `xattr -d com.apple.quarantine /Applications/ShellStation.app`.

## 5. Post-release

- [ ] Edit the Forgejo release notes (changelog, breaking changes, install instructions)
- [ ] Verify each artifact downloads and launches on a clean VM
- [ ] Announce / update README install instructions if the artifact names changed

## Hotfix releases

Same flow, but branch from the tag:

```bash
git checkout -b hotfix/X.Y.Z+1 vX.Y.Z
# ... fix ...
# bump version (step 2), commit, tag vX.Y.Z+1, push tag
```

Merge the hotfix branch back to `main` afterwards.

## Troubleshooting

**Linux build fails on `libwebkit2gtk-4.1-dev`** — Debian/Ubuntu < 24.04 may
only have the `4.0` package. Update the runner OS or pin the workflow to
`libwebkit2gtk-4.0-dev` and adjust `tauri.conf.json` accordingly.

**Windows build fails on `link.exe not found`** — MSVC Build Tools missing
the C++ workload on the runner. Re-run the `winget install` command from
the runner setup with `--add Microsoft.VisualStudio.Workload.VCTools`.

**Forgejo release upload fails with 401** — `RELEASE_TOKEN` secret expired
or lacks `write:repository` scope. Regenerate at
<https://git.fiedler.live/user/settings/applications>.

**Pipeline triggers but no runner picks up the job** — runner labels in
`runs-on:` don't match the registered runner labels. Check
<https://git.fiedler.live/-/admin/actions/runners>.
