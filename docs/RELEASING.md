# Releasing ShellStation

End-to-end checklist for cutting a new release.

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

| File                          | Field                  |
| ----------------------------- | ---------------------- |
| `src-tauri/Cargo.toml`        | `[package].version`    |
| `src-tauri/tauri.conf.json`   | `version`              |
| `package.json`                | `version`              |
| `package-lock.json`           | both top-level entries |

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

1. Builds Linux `.deb` + `.AppImage` on the Linux runner
2. Builds Windows `.msi` + `.exe` (NSIS) on the Windows runner
3. Creates a Forgejo release at `vX.Y.Z` and uploads all artifacts

Watch progress at <https://git.fiedler.live/tux/shellstation/actions>.

## 4. Build macOS manually

The pipeline does not build macOS. On the MacBook:

```bash
git fetch --tags
git checkout vX.Y.Z
npm ci
npm run tauri build -- --bundles dmg,app
```

Output lands in `src-tauri/target/release/bundle/dmg/`. Upload the `.dmg`
to the Forgejo release page manually:

<https://git.fiedler.live/tux/shellstation/releases/tag/vX.Y.Z>

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
