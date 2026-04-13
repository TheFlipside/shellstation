#!/usr/bin/env python3
"""Insert new translation keys into locale files in one pass.

Reads a spec describing which keys to add, where to anchor them, and the
per-locale translations, then writes the keys into each locale file that
matches the configured layout. Insertion order is preserved (new keys go
directly after the anchor key).

Usage
-----
    python3 add_translations.py [options] <spec.json>
    python3 add_translations.py [options] -          # read spec from stdin

Layout options
--------------
    --locales-dir PATH       Base directory (default: src/locales)
    --file-template STR      Path template below locales-dir, must contain
                             '{locale}' (default: '{locale}/translation.json')
    --indent N               JSON indent (default: 2)
    --ensure-ascii           Escape non-ASCII as \\uXXXX (default)
    --no-ensure-ascii        Write non-ASCII literally (Flutter .arb etc.)
    --dry-run                Report changes without writing files

Spec format
-----------
    {
      "section_path": ["settings"],
      "anchor": "loggingSaved",
      "key_order": ["appLogging", "appLoggingEnabledLabel"],
      "translations": {
        "en": {"appLogging": "Application Logging", ...},
        "de": {"appLogging": "Anwendungsprotokollierung", ...}
      }
    }

`section_path` walks a nested dict; use `[]` for root. `key_order` is
optional and defaults to the dict order of the first locale's keys.
Locales listed in `translations` that don't exist on disk are skipped with
a warning; locales on disk that aren't in the spec are left untouched.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any

LOCALE_NAME_RE = re.compile(r"^[a-zA-Z0-9_-]+$")


def insert_after_key(
    d: dict[str, Any], anchor: str, new_items: dict[str, Any]
) -> dict[str, Any]:
    """Return a new dict with ``new_items`` inserted directly after ``anchor``."""
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
    """Return ``root`` with the dict at ``path`` replaced by ``new_section``."""
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


def apply_to_locale(  # pylint: disable=too-many-arguments
    locale_path: Path,
    section_path: list[str],
    anchor: str,
    ordered_new: dict[str, Any],
    indent: int,
    ensure_ascii: bool,
    dry_run: bool,
) -> str:
    """Insert ``ordered_new`` after ``anchor`` inside ``locale_path``.

    Returns a short status string ("OK", "SKIP ...", or "WARN ...").
    """
    with locale_path.open("r", encoding="utf-8") as f:
        data = json.load(f)

    try:
        section = navigate(data, section_path)
    except (KeyError, TypeError) as e:
        return f"WARN ({e})"

    if all(k in section for k in ordered_new):
        return "SKIP (already present)"

    try:
        updated_section = insert_after_key(section, anchor, ordered_new)
    except KeyError as e:
        return f"WARN ({e})"

    new_data = replace_at_path(data, section_path, updated_section)

    if dry_run:
        return "OK (dry-run)"

    with locale_path.open("w", encoding="utf-8") as f:
        json.dump(new_data, f, ensure_ascii=ensure_ascii, indent=indent)
        f.write("\n")
    return "OK"


def build_parser() -> argparse.ArgumentParser:
    """Construct the CLI argument parser."""
    p = argparse.ArgumentParser(
        description="Insert translation keys into locale files in one pass.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    p.add_argument("spec", help="Path to spec JSON, or '-' to read from stdin")
    p.add_argument(
        "--locales-dir",
        default="src/locales",
        help="Base directory containing locale files (default: src/locales)",
    )
    p.add_argument(
        "--file-template",
        default="{locale}/translation.json",
        help=(
            "Path template relative to --locales-dir; must contain '{locale}' "
            "(default: '{locale}/translation.json')"
        ),
    )
    p.add_argument("--indent", type=int, default=2, help="JSON indent (default: 2)")
    ascii_group = p.add_mutually_exclusive_group()
    ascii_group.add_argument(
        "--ensure-ascii",
        dest="ensure_ascii",
        action="store_true",
        default=True,
        help="Escape non-ASCII as \\uXXXX (default)",
    )
    ascii_group.add_argument(
        "--no-ensure-ascii",
        dest="ensure_ascii",
        action="store_false",
        help="Write non-ASCII literally (Flutter .arb etc.)",
    )
    p.add_argument(
        "--dry-run",
        action="store_true",
        help="Report changes without writing files",
    )
    return p


def main(argv: list[str]) -> int:  # pylint: disable=too-many-locals
    """CLI entry point."""
    args = build_parser().parse_args(argv[1:])

    if "{locale}" not in args.file_template:
        print("ERROR: --file-template must contain '{locale}'", file=sys.stderr)
        return 2

    locales_dir = Path(args.locales_dir).resolve()
    if not locales_dir.is_dir():
        print(f"ERROR: locales directory not found at {locales_dir}", file=sys.stderr)
        return 2

    spec = load_spec(args.spec)
    section_path: list[str] = spec.get("section_path", [])
    anchor: str = spec["anchor"]
    translations: dict[str, dict[str, str]] = spec["translations"]
    key_order: list[str] = spec.get("key_order") or list(
        next(iter(translations.values())).keys()
    )

    exit_code = 0
    for locale, locale_translations in sorted(translations.items()):
        if not LOCALE_NAME_RE.match(locale):
            print(f"WARN: invalid locale name {locale!r} - skipped")
            exit_code = 1
            continue

        rel = args.file_template.format(locale=locale)
        locale_path = locales_dir / rel
        if not locale_path.is_file():
            print(f"WARN: {locale} not found at {locale_path} - skipped")
            continue

        missing = [k for k in key_order if k not in locale_translations]
        if missing:
            print(f"WARN: {locale} is missing keys {missing} - skipped")
            exit_code = 1
            continue

        ordered_new = {k: locale_translations[k] for k in key_order}
        status = apply_to_locale(
            locale_path,
            section_path,
            anchor,
            ordered_new,
            indent=args.indent,
            ensure_ascii=args.ensure_ascii,
            dry_run=args.dry_run,
        )
        print(f"{status}: {locale}")
        if status.startswith("WARN"):
            exit_code = 1

    return exit_code


if __name__ == "__main__":
    sys.exit(main(sys.argv))
