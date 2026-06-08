#!/usr/bin/env python3
"""Generate the tauri-plugin-updater `latest.json` manifest.

Called from `.forgejo/workflows/release.yml` after all platform builds have
been downloaded into `release-files/`. Reads `RELEASE_VERSION` (e.g.
`0.21.0`) and `RELEASE_TAG` (e.g. `v0.21.0`) from the environment.

Only emits a platform block when its `.sig` file is present — that way a
release built without the signing key (CI secret missing, dev test, etc.)
still produces a syntactically-valid manifest that the updater plugin will
correctly interpret as "no compatible update."

Signature files are read and embedded verbatim; the JSON encoder handles
any embedded newlines in the minisign header. Do NOT pre-process the .sig
content — the plugin verifies the exact bytes minisign emitted.
"""
from __future__ import annotations

import datetime
import json
import os
import sys
from pathlib import Path


REPO_BASE = "https://github.com/TheFlipside/shellstation/releases/download"

# Map of updater target triple → (artifact basename template, sig basename
# template). `{v}` is replaced with the bare version (no leading `v`).
PLATFORMS: dict[str, tuple[str, str]] = {
    "linux-x86_64": (
        "ShellStation-linux-amd64-{v}.AppImage",
        "ShellStation-linux-amd64-{v}.AppImage.sig",
    ),
    "windows-x86_64": (
        "ShellStation-windows-x64-{v}-setup.exe",
        "ShellStation-windows-x64-{v}-setup.exe.sig",
    ),
    "darwin-x86_64": (
        "ShellStation-macos-x64-{v}.app.tar.gz",
        "ShellStation-macos-x64-{v}.app.tar.gz.sig",
    ),
    "darwin-aarch64": (
        "ShellStation-macos-arm64-{v}.app.tar.gz",
        "ShellStation-macos-arm64-{v}.app.tar.gz.sig",
    ),
}


def main() -> int:
    version = os.environ.get("RELEASE_VERSION") or sys.exit(
        "RELEASE_VERSION env var not set (export it before invoking this script)"
    )
    tag = os.environ.get("RELEASE_TAG") or sys.exit(
        "RELEASE_TAG env var not set (export it before invoking this script)"
    )
    base = f"{REPO_BASE}/{tag}"
    release_dir = Path("release-files")

    platforms: dict[str, dict[str, str]] = {}
    for target, (artifact_tpl, sig_tpl) in PLATFORMS.items():
        artifact = artifact_tpl.format(v=version)
        sig_path = release_dir / sig_tpl.format(v=version)
        if not sig_path.is_file():
            print(f"skip {target}: no signature at {sig_path}", file=sys.stderr)
            continue
        platforms[target] = {
            "signature": sig_path.read_text(encoding="utf-8"),
            "url": f"{base}/{artifact}",
        }

    manifest = {
        "version": version,
        "notes": f"See {base} for release notes",
        "pub_date": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "platforms": platforms,
    }
    (release_dir / "latest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
