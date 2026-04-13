"""Tests for add_translations.py."""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent))

from add_translations import (  # noqa: E402  pylint: disable=wrong-import-position
    insert_after_key,
    main,
    navigate,
    replace_at_path,
)


def test_insert_after_key_preserves_order() -> None:
    """New items land directly after the anchor, other order preserved."""
    result = insert_after_key({"a": 1, "b": 2, "c": 3}, "b", {"x": 9, "y": 10})
    assert list(result.items()) == [
        ("a", 1),
        ("b", 2),
        ("x", 9),
        ("y", 10),
        ("c", 3),
    ]


def test_insert_after_key_missing_anchor_raises() -> None:
    """Unknown anchor key raises KeyError."""
    with pytest.raises(KeyError, match="anchor key 'nope' not found"):
        insert_after_key({"a": 1}, "nope", {"x": 2})


def test_navigate_nested() -> None:
    """navigate walks a nested dict path and returns the leaf dict."""
    root = {"settings": {"ui": {"theme": "dark"}}}
    assert navigate(root, ["settings", "ui"]) == {"theme": "dark"}


def test_navigate_root() -> None:
    """Empty path returns the root dict itself."""
    root = {"a": 1}
    assert navigate(root, []) is root


def test_navigate_missing_key() -> None:
    """Missing intermediate key raises KeyError."""
    with pytest.raises(KeyError):
        navigate({"a": {}}, ["a", "b"])


def test_replace_at_path_nested_is_immutable() -> None:
    """replace_at_path doesn't mutate the original nested dicts."""
    root = {"a": {"b": {"c": 1}}}
    new = replace_at_path(root, ["a", "b"], {"c": 2, "d": 3})
    assert new == {"a": {"b": {"c": 2, "d": 3}}}
    assert root == {"a": {"b": {"c": 1}}}


def test_replace_at_path_empty_returns_new_section() -> None:
    """Empty path just returns the replacement."""
    assert replace_at_path({"a": 1}, [], {"b": 2}) == {"b": 2}


def _make_repo(tmp_path: Path) -> Path:
    """Create a minimal i18next-style locales layout and return locales dir."""
    locales = tmp_path / "src" / "locales"
    for lang, saved in (("en", "Saved"), ("de", "Gespeichert")):
        (locales / lang).mkdir(parents=True)
        (locales / lang / "translation.json").write_text(
            json.dumps({"settings": {"loggingSaved": saved, "other": "X"}}),
            encoding="utf-8",
        )
    return locales


def test_main_happy_path(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    """End-to-end: spec applies to all locales, key lands after anchor."""
    locales = _make_repo(tmp_path)
    spec = tmp_path / "spec.json"
    spec.write_text(
        json.dumps(
            {
                "section_path": ["settings"],
                "anchor": "loggingSaved",
                "translations": {
                    "en": {"appLogging": "Application Logging"},
                    "de": {"appLogging": "Anwendungsprotokollierung"},
                },
            }
        ),
        encoding="utf-8",
    )

    rc = main(
        [
            "add_translations.py",
            "--locales-dir",
            str(locales),
            str(spec),
        ]
    )
    assert rc == 0
    out = capsys.readouterr().out
    assert "OK: en" in out
    assert "OK: de" in out

    de = json.loads((locales / "de" / "translation.json").read_text())
    assert list(de["settings"].keys()) == ["loggingSaved", "appLogging", "other"]
    assert de["settings"]["appLogging"] == "Anwendungsprotokollierung"


def test_main_idempotent_rerun(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    """Running twice leaves files unchanged on the second pass."""
    locales = _make_repo(tmp_path)
    spec = tmp_path / "spec.json"
    spec.write_text(
        json.dumps(
            {
                "section_path": ["settings"],
                "anchor": "loggingSaved",
                "translations": {"en": {"appLogging": "Application Logging"}},
            }
        ),
        encoding="utf-8",
    )
    argv = ["add_translations.py", "--locales-dir", str(locales), str(spec)]

    assert main(argv) == 0
    capsys.readouterr()
    before = (locales / "en" / "translation.json").read_bytes()

    assert main(argv) == 0
    out = capsys.readouterr().out
    assert "SKIP (already present): en" in out

    after = (locales / "en" / "translation.json").read_bytes()
    assert before == after


def test_main_missing_anchor_warns(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    """Missing anchor produces a WARN and non-zero exit code."""
    locales = _make_repo(tmp_path)
    spec = tmp_path / "spec.json"
    spec.write_text(
        json.dumps(
            {
                "section_path": ["settings"],
                "anchor": "doesNotExist",
                "translations": {"en": {"x": "y"}},
            }
        ),
        encoding="utf-8",
    )
    rc = main(["add_translations.py", "--locales-dir", str(locales), str(spec)])
    assert rc == 1
    assert "WARN" in capsys.readouterr().out


def test_main_no_ensure_ascii_round_trip(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    """--no-ensure-ascii writes non-ASCII literally."""
    locales = _make_repo(tmp_path)
    spec = tmp_path / "spec.json"
    spec.write_text(
        json.dumps(
            {
                "section_path": ["settings"],
                "anchor": "loggingSaved",
                "translations": {"de": {"appLogging": "Anwendungsprotokollierung"}},
            }
        ),
        encoding="utf-8",
    )
    rc = main(
        [
            "add_translations.py",
            "--locales-dir",
            str(locales),
            "--no-ensure-ascii",
            str(spec),
        ]
    )
    assert rc == 0
    capsys.readouterr()
    raw = (locales / "de" / "translation.json").read_text(encoding="utf-8")
    assert "Anwendungsprotokollierung" in raw
    assert "\\u" not in raw


def test_main_dry_run_does_not_write(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    """--dry-run reports OK but leaves files untouched."""
    locales = _make_repo(tmp_path)
    spec = tmp_path / "spec.json"
    spec.write_text(
        json.dumps(
            {
                "section_path": ["settings"],
                "anchor": "loggingSaved",
                "translations": {"en": {"appLogging": "X"}},
            }
        ),
        encoding="utf-8",
    )
    before = (locales / "en" / "translation.json").read_bytes()
    rc = main(
        [
            "add_translations.py",
            "--locales-dir",
            str(locales),
            "--dry-run",
            str(spec),
        ]
    )
    assert rc == 0
    assert "OK (dry-run)" in capsys.readouterr().out
    assert (locales / "en" / "translation.json").read_bytes() == before
