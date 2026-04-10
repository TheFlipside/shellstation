#!/usr/bin/env python3
"""Insert new translation keys into every locale file under src/locales/.

Reads a spec file describing which keys to add, where to anchor them, and
the per-locale translations, then writes the keys into each matching
src/locales/<locale>/translation.json. Non-ASCII characters are written as
\\uXXXX escapes to match the existing file style; insertion order is
preserved (keys are placed directly after the anchor key).

Usage
-----
    python3 tools/add_translations.py <spec.json>
    python3 tools/add_translations.py -          # read spec from stdin

Spec format
-----------
    {
      "section_path": ["settings"],          // nested dict path; use [] for root
      "anchor": "loggingSaved",              // key to insert after
      "key_order": [                         // optional; defaults to dict order
        "appLogging",
        "appLoggingEnabledLabel"
      ],
      "translations": {
        "en": {
          "appLogging": "Application Logging",
          "appLoggingEnabledLabel": "Enable application logging"
        },
        "de": {
          "appLogging": "Anwendungsprotokollierung",
          "appLoggingEnabledLabel": "Anwendungsprotokollierung aktivieren"
        }
      }
    }

Locales listed in `translations` that do not exist on disk are skipped with
a warning. Locales on disk that are missing from `translations` are skipped
silently — fall back to the i18next default at runtime.
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent
LOCALES_DIR = REPO_ROOT / "src" / "locales"
LOCALE_NAME_RE = re.compile(r"^[a-zA-Z0-9_-]+$")


def insert_after_key(
    d: dict[str, Any], anchor: str, new_items: dict[str, Any]
) -> dict[str, Any]:
    """Return a new dict with `new_items` inserted directly after `anchor`."""
    out: dict[str, Any] = {}
    inserted = False
    for k, v in d.items():
        out[k] = v
        if k == anchor:
            for nk, nv in new_items.items():
                out[nk] = nv
            inserted = True
    if not inserted:
        raise KeyError(f"anchor key '{anchor}' not found")
    return out


def navigate(root: dict[str, Any], path: list[str]) -> dict[str, Any]:
    """Walk a nested dict by key path, returning the leaf dict."""
    node: Any = root
    for key in path:
        if not isinstance(node, dict) or key not in node:
            raise KeyError(f"section path {path} not found at '{key}'")
        node = node[key]
    if not isinstance(node, dict):
        raise TypeError(f"section path {path} does not point to a dict")
    return node


def replace_at_path(
    root: dict[str, Any],
    path: list[str],
    new_section: dict[str, Any],
) -> dict[str, Any]:
    """Return `root` with the dict at `path` replaced by `new_section`."""
    if not path:
        return new_section
    out = dict(root)
    node = out
    for key in path[:-1]:
        node[key] = dict(node[key])
        node = node[key]
    node[path[-1]] = new_section
    return out


def load_spec(arg: str) -> dict[str, Any]:
    """Load a spec from a file path, or from stdin when arg is '-'."""
    if arg == "-":
        return json.loads(sys.stdin.read())
    return json.loads(Path(arg).read_text(encoding="utf-8"))


def apply_to_locale(
    locale_path: Path,
    section_path: list[str],
    anchor: str,
    ordered_new: dict[str, Any],
) -> str:
    """Insert ``ordered_new`` after ``anchor`` inside ``locale_path``.

    Returns a short status string ("OK", "SKIP …", or "WARN …") describing
    the outcome for the given locale file.
    """
    with locale_path.open("r", encoding="utf-8") as f:
        data = json.load(f)

    try:
        section = navigate(data, section_path)
    except (KeyError, TypeError) as e:
        return f"WARN ({e})"

    # Idempotency: skip if every new key is already present.
    if all(k in section for k in ordered_new):
        return "SKIP (already present)"

    try:
        updated_section = insert_after_key(section, anchor, ordered_new)
    except KeyError as e:
        return f"WARN ({e})"

    new_data = replace_at_path(data, section_path, updated_section)

    with locale_path.open("w", encoding="utf-8") as f:
        json.dump(new_data, f, ensure_ascii=True, indent=2)
        f.write("\n")
    return "OK"


def main(argv: list[str]) -> int:
    """CLI entry point: load spec from argv[1] and apply it to all locales."""
    if len(argv) != 2 or argv[1] in ("-h", "--help"):
        print(__doc__)
        return 0 if argv[1:] == ["-h"] or argv[1:] == ["--help"] else 1

    spec = load_spec(argv[1])
    section_path: list[str] = spec.get("section_path", [])
    anchor: str = spec["anchor"]
    translations: dict[str, dict[str, str]] = spec["translations"]
    key_order: list[str] = spec.get("key_order") or list(
        next(iter(translations.values())).keys()
    )

    if not LOCALES_DIR.is_dir():
        print(f"ERROR: locales directory not found at {LOCALES_DIR}", file=sys.stderr)
        return 2

    exit_code = 0
    for locale, locale_translations in sorted(translations.items()):
        if not LOCALE_NAME_RE.match(locale):
            print(f"WARN: invalid locale name {locale!r} — skipped")
            exit_code = 1
            continue
        locale_path = LOCALES_DIR / locale / "translation.json"
        if not locale_path.is_file():
            print(f"WARN: {locale} not found at {locale_path} — skipped")
            continue

        missing = [k for k in key_order if k not in locale_translations]
        if missing:
            print(f"WARN: {locale} is missing keys {missing} — skipped")
            exit_code = 1
            continue

        ordered_new = {k: locale_translations[k] for k in key_order}
        status = apply_to_locale(locale_path, section_path, anchor, ordered_new)
        print(f"{status}: {locale}")
        if status.startswith("WARN"):
            exit_code = 1

    return exit_code


if __name__ == "__main__":
    sys.exit(main(sys.argv))
