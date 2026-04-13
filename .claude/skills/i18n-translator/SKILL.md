---
name: i18n-translator
description: |
  Load this skill when the user asks to add, extend, or sync translation keys
  across multiple locale files (i18next JSON, Flutter .arb, flat locale JSON,
  etc.). Avoids the token cost of opening every locale file individually by
  applying a single spec to all locales in one pass, preserving insertion
  order and file style.
---

# Skill: i18n Translator

## Goal
Add new translation keys to many locale files in one pass, without reading
each file into context. Inspect the repo layout once, build a spec, run the
script, verify with a diff.

## When to use
- User wants to add N new keys across M locale files.
- User wants to extend an existing section (e.g. `settings.*`) and needs the
  new keys placed next to a specific anchor key.
- User is porting translations they already have in a table/spreadsheet.

Do NOT use this for:
- Renaming keys across locales (use a codemod or `sed` — anchors don't help).
- Deleting keys (script is insert-only by design).
- Machine-translating missing strings (this is a mechanical-insert tool; the
  translations themselves must come from the user).

## Workflow

### Step 1 — Inspect the target repo
Before writing anything, discover the locale layout. Do NOT read every locale
file; one sample is enough.

```bash
# Common layouts:
#   src/locales/<lang>/translation.json   (i18next)
#   src/i18n/<lang>.json                  (flat)
#   lib/l10n/app_<lang>.arb               (Flutter)
#   locales/<lang>/LC_MESSAGES/*.po       (gettext — NOT supported)
find . -type d -name 'locales' -o -name 'l10n' -o -name 'i18n' | head
```

Read ONE locale file to learn:
- Indent (usually 2).
- Whether non-ASCII is escaped (`\uXXXX`) or written literally.
- Whether the file has a trailing newline.
- Nesting depth for the section the user wants to extend.

### Step 2 — Build the spec
Spec is a JSON document passed to the script on stdin or as a file:

```json
{
  "section_path": ["settings"],
  "anchor": "loggingSaved",
  "key_order": ["appLogging", "appLoggingEnabledLabel"],
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
```

- `section_path`: nested-dict path to the section. Use `[]` for the root.
- `anchor`: existing key to insert after. Preserves neighbor locality.
- `key_order`: optional. Defaults to dict order from the first locale.
- `translations`: map of `locale -> {key: value}`. Locales not on disk are
  skipped with a warning; locales on disk missing from the spec are left
  untouched (i18next falls back at runtime).

### Step 3 — Run the script
The script lives next to this SKILL.md as `add_translations.py`.

```bash
# Standard i18next layout (default)
python3 .claude/skills/i18n-translator/add_translations.py spec.json

# Custom layout — pass --locales-dir and --file-template
python3 .claude/skills/i18n-translator/add_translations.py \
    --locales-dir lib/l10n \
    --file-template 'app_{locale}.arb' \
    --no-ensure-ascii \
    spec.json

# Read spec from stdin
cat spec.json | python3 .claude/skills/i18n-translator/add_translations.py -
```

Flags:
- `--locales-dir PATH` — base directory (default: `src/locales`).
- `--file-template STR` — path template relative to locales-dir. Must contain
  `{locale}`. Default: `{locale}/translation.json`.
- `--indent N` — JSON indent (default: 2).
- `--ensure-ascii` / `--no-ensure-ascii` — escape non-ASCII? Default: on
  (matches i18next style). Turn off for Flutter `.arb` and most modern repos.
- `--dry-run` — print what would change without writing.

Note: `--file-template` is not a security boundary. A template like
`../{locale}/x.json` will happily escape `--locales-dir`. This is a local
dev tool — don't feed it templates from untrusted sources.

### Step 4 — Verify
```bash
git diff --stat <locales-dir>
git diff <locales-dir>/en/translation.json   # spot-check one file
```
Run the project's own lint/format on the locale files if it has one
(prettier, biome, etc.) so the diff matches house style.

## Idempotency
The script skips any locale where *all* new keys already exist. Re-running
on a partially-applied state is safe but will WARN if the anchor is missing
(e.g. because a previous run already inserted the new keys and moved the
anchor's neighbors).

## What NOT to do
- Do NOT paste translations into each locale file manually — that's the bug
  this skill exists to fix.
- Do NOT invent translations the user didn't provide. If a locale is missing
  from the spec, leave it out and let i18next fall back.
- Do NOT use this for `.po` / `.mo` gettext files — they're not JSON.
- Do NOT commit without running the project formatter; style drift causes
  noisy diffs on unrelated lines.
