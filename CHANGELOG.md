# Changelog

All notable changes to this project are documented in this file.

## Unreleased

### Fixed

- File imports in **Settings** (SecureCRT, mRemoteNG, database JSON, highlight profiles) silently did nothing on Windows: no import, no toast, no log line. The shared file-picker helper treated "window regained focus and no file is present one animation frame later" as a cancel, but in WebView2 the window regains focus as the native dialog closes while the chosen file reaches the page over a separate IPC hop, so `change` routinely arrived after the helper had already resolved `null` and removed its input. Introduced in 0.12.0 with the helper itself. The helper moved to `src/pickFile.ts` and now settles only on the input's own `change` or `cancel` events. For webviews that never dispatch `cancel` (WebKit before Safari 16.4), a pick still pending when the next one starts is settled as cancelled then — the native picker is modal, so it can only be a cancel that went unreported — which bounds the leftover to one hidden element. Covered by `src/pickFile.test.ts`, whose focus-before-change test fails against the old helper with the file resolving as `null`.
- SecureCRT import dropped entity and character references inside element text: a username stored as `dom&amp;user` imported as `domuser`, `&#x2D;` vanished from a hostname. quick-xml reports references as separate `GeneralRef` events between the surrounding text chunks, and `read_text` ignored them. Character references and the five predefined XML entities are now resolved; anything else (undeclared entities, invalid character references) is kept verbatim instead of being lost. Session and folder names were unaffected, since they come from attributes, which quick-xml resolves itself; the mRemoteNG importer reads attributes only and was likewise unaffected.
- Imported usernames now go through the same control-character stripping and 255-character cap as imported names and hostnames. With character references resolved, a reference such as `&#x1b;` could otherwise place a raw control character into a username, which flows into SSH authentication and the `user@host` session log name.
- The import sanitizer (`sanitize_imported_name`) now also drops characters that change how a name _displays_ rather than what it contains, which `char::is_control` does not catch: the bidi embedding, override and isolate controls (U+202A–U+202E, U+2066–U+2069) that can make a crafted hostname or username render as a different one, the line and paragraph separators U+2028/U+2029, and invisible characters with no place in a host, user or session name (U+200B, U+FEFF, soft hyphen U+00AD, U+2060–U+2064, U+FFF9–U+FFFB, and the standalone Hangul fillers U+3164 / U+FFA0). Directional marks (U+200E, U+200F, U+061C) and joiners (U+200C, U+200D) are kept because right-to-left and Indic names and emoji sequences depend on them, and marks cannot reorder text the way overrides can. Applies to imported folder names, session names, hostnames, usernames and jump-host references.
- Import warnings and parse errors are now sanitized before they reach the UI. Parser warnings quote raw file content (`Skipped "<name>" — no hostname`, `unsupported protocol: <value>`, `In "<key>": …`) and quick-xml errors can quote XML fragments, so a crafted export could push control or bidi-override characters into the toasts that display them. Both now go through the same character filter as names, capped at 500 characters each. The number of warnings returned is also capped at 20 plus a final "…and N more warnings" entry: each warning becomes its own toast, and a file full of bad entries previously produced one toast per entry with no limit. The app-log summary line still reports the full warning count.
- The one import error that bypassed this — failing to create the top-level "SecureCRT Import" / "mRemoteNG Import" folder — now goes through the same message sanitizer, and is logged at `warn`. Previously the import aborted with a log that showed it starting and nothing after. The text is a database error rather than file content, so the log line carries it whole.
- **Settings → Highlighting → Import Profile** read the chosen `.ini` completely into webview memory and sent it over IPC before the backend's 10 MB limit could reject it; the other three imports already checked the size first. It now checks `file.size` against the same 10 MB up front, with a new `highlighting.importTooLarge` string in all 14 locales.
- `cargo clippy -- -D warnings` failed with 42 `double_must_use` errors under clippy 1.99: `async-trait` 0.1.91 added a bare `#[must_use]` to every generated method, whose boxed-future return type is already must-use. `async-trait` 0.1.92 stops emitting it; lockfile-only bump, no source change.

### Changed

- The SecureCRT and mRemoteNG import commands now log at `info` when they start (with the payload size) and finish (folder, session, skipped and warning counts), and at `warn` when parsing fails — as a fixed message, because quick-xml errors can quote fragments of the file (tag names, an unrecognized entity name inside an attribute value); the full error still reaches the UI. Previously an import left no trace in the app log at any level, so an import that never reached the backend was indistinguishable from one that did. No hostnames, usernames or session names are logged.
- The SecureCRT and mRemoteNG imports now reject a file with more than 10,000 folders or 50,000 sessions, the limits the JSON database import already enforced. The 100 MB file cap alone allowed hundreds of thousands of entries, each a database write. The check runs after parsing and before anything is written, so a rejected file leaves no partial import. The two constants in `commands/database.rs` are now `pub(crate)` and shared rather than duplicated.

## 0.30.0 - 2026-07-27

### Added

- Built-in app update flow via the Tauri `tauri-plugin-updater` plugin. Documented residual concerns (neither exploitable today): (1) the plugin's `check` IPC accepts a `proxy` parameter that, in a hypothetical future XSS, could be set to point at internal hosts (SSRF). ShellStation has no current XSS surface — xterm.js renders bytes, not HTML, and the CSP is tight — and the IPC permission has been narrowed to `updater:allow-check` + `updater:allow-download-and-install` only (no standalone `allow-download` / `allow-install`). (2) The upstream macOS admin-elevation install path (when the app needs root to replace its `.app`) builds an embedded shell script with the install path unescaped; an install path containing a single quote (e.g. `My App's.app`) could break out. The realistic install path is `/Applications/ShellStation.app` which has no such character. Will be filed upstream against `tauri-apps/plugins-workspace`. The app now checks for newer releases against `https://github.com/TheFlipside/shellstation/releases/latest/download/latest.json` and prompts the user with a signed, in-place install when one is available. New **Help → Check for Updates…** menu item triggers a manual check. New **Settings → Check for updates on startup** toggle controls automatic checks (default on). On Windows the NSIS installer is invoked in `installMode: passive` for a click-free update experience; on macOS the `.app` bundle is replaced from a `.app.tar.gz` artifact and the app restarts (no DMG dance); on Linux the AppImage is replaced. All download artifacts are Ed25519-signed at build time; the app verifies signatures against the public key embedded in `tauri.conf.json` before running anything. CI extended: `.forgejo/workflows/release.yml` now requires `TAURI_SIGNING_PRIVATE_KEY` + `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` secrets, produces `.sig` files alongside each installer, and emits `latest.json` from a new `.forgejo/scripts/generate_latest_json.py` helper. Translated into all 14 locales. See `docs/RELEASING.md` for one-time signing-key setup before the next release.
- Two new settings under **Settings → Terminal**: **Background color** and **Text color**. Each is an HTML color picker that, when set, overrides the corresponding xterm.js theme field (`background` / `foreground`) for every terminal session. The remaining ANSI palette still tracks the active app theme; the cursor and selection colors are derived from whichever background/text colors end up in effect (see the Changed entries below). A **Reset** button next to each picker (visible only when an override is set) clears the override and reverts to the active theme's default. Stored in `useSettingsStore` as `terminalBackgroundColor` / `terminalForegroundColor` (`string | null`, hex `#RRGGBB` validated; `null` = use theme default), persisted across reloads, and sanitized on rehydrate against the hex regex. The Terminal font preview swatch now also reflects the chosen colors so the user can see the effect without opening a session. Translated into all 13 non-English locales.
- Frontend component test harness. `vitest` and `@testing-library/react` were already dependencies but had no configuration and no tests; the missing piece was a DOM environment, so `jsdom` (dev-only) is now installed and `vite.config.ts` carries a `test` block (`environment: "jsdom"`, `include: src/**/*.test.{ts,tsx}`, `clearMocks`). `globals` is deliberately left off — test files import `describe`/`it`/`expect` explicitly, which also means Testing Library's auto-cleanup has to be registered by hand in the new `src/test/setup.ts`, alongside stubs for the two browser APIs jsdom lacks and the components need (`ResizeObserver`, `matchMedia`). New `npm test` (single run) and `npm run test:watch` scripts. Both mocked constructors and the `ResizeObserver` stub use function expressions rather than arrow functions, because the call sites use `new` and arrows are not constructible.
- `src/components/DialogKeys.test.tsx`: 12 tests pinning the key-ownership contract — Escape passes through when nothing claims it, is taken while a claim is mounted, is released again on unmount, goes to the last dialog in document order, falls back to the dialog below when the top one closes, goes to the dialog painted on top rather than the one mounted last (tested in both mount orders), and is not taken by a `DialogKeys` given only an `onEnter`; Enter is claimed but left alone inside textareas and during IME composition; and a dedicated regression test that renders `UpdateDialog` in its idle (null-rendering) state and asserts Escape still reaches a terminal stub. Both fixes were verified by reintroducing the respective bug — `return <DialogKeys … />` in place of `return null`, and mount-order arbitration in place of document-order — and confirming that exactly the corresponding test fails.
- Test coverage for the command-bar scroll fix: `src/terminalRegistry.test.ts` (4 tests — lookup, unknown-session no-op, unregistration, and the guard that stops a stale teardown from evicting a newer terminal that reclaimed the same session id) and `src/components/CommandBar.test.tsx` (4 tests — command button, freeform send, send-to-all reaching every target, and no scroll for an exited session). `Terminal.test.tsx` gained two tests for the registration/unregistration lifecycle. Verified by removing the `scrollTerminalToBottom` call and confirming exactly the three scroll tests fail.
- First component test suite: `src/components/Terminal.test.tsx`, 8 tests covering the multiline-paste confirmation control and the focus handling around it — dialog opens for control-character content, single-line content pastes straight through, the dialog is skipped when `confirmMultilinePaste` is off, a second paste while the dialog is open is ignored (the pending payload cannot be swapped under the user), focus returns to the terminal on both Confirm and Cancel, focus is _not_ stolen when the terminal is in a background tab, and the visibility-change path focuses only once the tab becomes visible. xterm.js, the Tauri IPC layer, and i18n are mocked; the real Zustand settings store is used so the tests drive the same `confirmMultilinePaste` code path as the app. Verified the suite has teeth by removing the focus-restore call and confirming exactly the two focus tests fail.

### Changed

- The terminal cursor no longer disappears against a custom background. Each theme's cursor is an accent color (`#f5e0dc` on dark, `#dc8a78` on light) chosen against that theme's own background, but it was applied verbatim regardless of the **Settings → Terminal → Background color** override — set a light background on the dark theme and the near-white cursor became invisible. `resolveTerminalTheme()` now checks the WCAG contrast ratio between the theme's cursor and the background actually in effect, and substitutes the effective foreground when it falls below 3:1 (the WCAG minimum for a non-text UI element). The cursor then ends up exactly as readable as the text around it. The check only runs when the effective background actually differs from the theme's own, so the accent is left alone whenever it is sitting on the background it was designed for — including the light theme's cursor, which is at 2.3:1 by design and would otherwise have been silently replaced. That comparison is on the resolved color rather than on "is an override set", because the color picker opens pre-filled with the theme default: confirming it unchanged stores an override identical to the default, and that has to behave exactly like no override. `contrastRatio()` returns a non-finite value for a malformed color, and the call site treats that as "no contrast" so bad input degrades to the readable branch instead of throwing.
- Second Rust dependency refresh of this release, superseding the lockfile state described further down. Two manifest bumps, both security-motivated and detailed under **Security**: `russh` 0.61.2 → 0.62.4 and `quick-xml` 0.40.1 → 0.41.0. Everything else came from a plain `cargo update` within the existing `^` constraints: `tauri` 2.11.2 → 2.11.5, `tauri-build` 2.6.2 → 2.6.3, `tauri-plugin-dialog` 2.7.1 → 2.7.2, `tokio` 1.52.3 → 1.53.1, `serde` 1.0.228 → 1.0.229, `serde_json` 1.0.150 → 1.0.151, `uuid` 1.23.2 → 1.24.0, `zeroize` 1.8.2 → 1.9.0, `async-trait` 0.1.89 → 0.1.91, `regex` 1.12.3 → 1.13.1, plus 181 transitive packages. No source change was required for any of them; the highest MSRV introduced is 1.85 (`russh`), well under the 1.96 toolchain. Verified with `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo fmt --check`, and a `cargo audit` re-run. The `tokio` bump is a small win for the telnet writer task at `telnet/mod.rs:242`: 1.53.0 clears an `mpsc::Receiver`'s waker on drop, which is exactly the shape there — the receiver is dropped when the select loop breaks while its sender stays alive in the manager.
- Two available upgrades were deliberately **not** taken, recorded here so the analysis is not repeated. `base64` 0.22 → 0.23 is pure cost: `hyper-util`, `reqwest`, `sqlx-core`, `sqlx-postgres`, `plist`, `tauri-codegen` and `wry` all still require 0.22.x, so the bump would ship a second copy of the crate to serve three `BASE64_STANDARD.encode` calls and no `decode` at all, and 0.23 additionally enables its `simd-unsafe` feature by default. `keyring` 3.6.3 → 4.1.5 compiles with no source change and is credential-compatible in both directions (verified by a write/read round trip between the two versions against a live Secret Service), but it carries two behavioral regressions that deserve their own change: the `linux-native-sync-persistent` feature no longer exists, so Linux loses the keyutils cache that today serves reads when the Secret Service is locked or not yet up and that links entries into the kernel's persistent keyring; and `Entry::new` flips its init latch with `compare_exchange` _before_ calling `set_credential_store()`, so a single failure — plausible at startup, where `migrate_legacy_credentials` touches the keychain from the Tauri setup hook at `lib.rs:829` — leaves every later call returning `NoDefaultStore` for the rest of the process lifetime, where keyring 3 would simply retry.
- The **Settings → Terminal** preview swatch now shows a selection and a block cursor, not just the background and text colors, so all four colors that respond to the overrides are visible while tuning them. The swatch renders its last word inverted and appends a cursor block, reusing its own preview string rather than introducing a new translation key across the 14 locales.
- The terminal palette and its derivation moved out of `Terminal.tsx` into a new `src/terminalTheme.ts`, and the settings preview now renders through the same `resolveTerminalTheme()` a live session uses. Previously the preview re-implemented the override fallback inline (`terminalBackgroundColor ?? themeDefaultBg`), so it could only ever show two of the colors and would have drifted from the terminal as more were derived. `XTERM_THEMES` is now imported from there rather than from a sibling component.
- Selecting text in a terminal now reverses the colors instead of tinting them. The old selection was a translucent overlay (`#585b7066` on dark, `#acb0be66` on light) drawn behind text that kept its own color, so against bright output — white text, or anything on a light background — it was barely visible. Selection colors are no longer per-theme constants: `resolveTerminalTheme()` derives them from the palette it just resolved, so the effective foreground becomes `selectionBackground` and the effective background becomes `selectionForeground`. Selected text therefore has exactly the contrast of unselected text, and because the derivation happens after the **Settings → Terminal → Background/Text color** overrides are applied, it follows custom colors instead of being pinned to the app theme. `selectionInactiveBackground` is set to the same pair rather than a dimmed variant: xterm.js applies `selectionForeground` whether or not the terminal has focus, so dimming only the background would push the forced text color toward the background and bring back the washed-out case. The colors are deliberately opaque — xterm.js clamps an opaque selection to 30% alpha, but only for the `*Transparent` variants its DOM renderer uses, and this app installs the WebGL renderer, which reads the opaque variants computed before that clamp. One consequence, inherent to xterm.js having no per-cell inversion: `selectionForeground` paints every selected cell in one color, so ANSI-colored output (`ls`, `grep --color`, colored log levels) renders in a single uniform color while it is selected, and returns to normal on deselect. Covered by 5 tests in `Terminal.test.tsx` (component level) plus the new `src/terminalTheme.test.ts` (11 tests over the derivation itself: contrast-ratio bounds and symmetry, selection inversion for every override combination, and both cursor branches), verified by restoring the old tint and by mutating each cursor branch, confirming exactly the corresponding tests fail.
- `Terminal.tsx`: the two places that focus the terminal — the visibility-change effect and the paste-dialog-close effect — now share one `focusIfVisible()` helper instead of each re-implementing "focus if this tab is visible". The helper reads `visible` through a ref because the visibility path fires on a 50 ms delay, by which point a captured prop could be stale. Prevents the two call sites from drifting apart if only one is edited later.
- Refreshed Rust dependency lockfile: `russh` 0.61.1 → 0.61.2 (broadens `russh::keys::load_secret_key` to accept SEC1 EC private keys that carry full explicit domain parameters in addition to named-curve OIDs — purely additive at the existing `ssh/mod.rs:1199` call site — plus an `AgentClient` `Send` fix that does not affect this codebase since no SSH-agent forwarding is implemented), `uuid` 1.23.1 → 1.23.2, `chrono` 0.4.44 → 0.4.45. All three are pure patch bumps with no source changes; the existing `^constraint` versions in `Cargo.toml` already permitted them.
- Refreshed npm dependency lockfile: `i18next` 26.0.8 → 26.3.1 (includes the 26.0.6 security fix for interpolation/log-forging), `react` + `react-dom` 19.2.5 → 19.2.7, `react-i18next` 17.0.4 → 17.0.8, `zustand` 5.0.12 → 5.0.14, `@tauri-apps/cli` 2.11.1 → 2.11.2 (realigns JS CLI with the Rust `tauri` 2.11.2 crate), `@types/node` 25.6.0 → 25.9.2, `@types/react` + `@types/react-dom` 19.2.14 → 19.2.17, `@vitejs/plugin-react` 6.0.1 → 6.0.2, `eslint-plugin-prettier` 5.5.5 → 5.5.6, `typescript-eslint` 8.59.0 → 8.60.1, `vite` 8.0.10 → 8.0.16 (Rolldown 1.0.0-rc.17 → 1.0.3 stable, launch-editor-middleware path-traversal hardening), `vitest` 4.1.5 → 4.1.8. All within existing `^constraint` ranges; no `package.json` edits required.
- `eslint.config.js`: migrated from the deprecated `tseslint.config()` helper to ESLint core's `defineConfig()` (typescript-eslint 8.x deprecation). Added a targeted `eslint-disable-next-line` for the `securityPlugin.configs.recommended` line because `eslint-plugin-security` 4.0.0 ships no `.d.ts` and is therefore `any`-typed upstream, which typescript-eslint's `no-unsafe-argument` / `no-unsafe-member-access` rules legitimately cannot verify.
- `tsconfig.node.json`: now also includes `eslint.config.js` (with `allowJs: true`) so typescript-eslint's projectService type-checks it under a real tsconfig where `@types/node` is in scope. Without this, `import.meta.dirname` resolved as `error`-typed under the inferred default project and triggered `no-unsafe-assignment` after the typescript-eslint bump.
- Moved `MAX_INFLIGHT_WRITES` out of `src-tauri/src/commands/mod.rs` and into a new `src-tauri/src/limits.rs` module. The constant is read from `ssh/mod.rs` and `telnet/mod.rs`, which previously created an upward dependency on `commands::` — `crate::limits::` is the natural home for crate-wide numeric bounds shared across siblings.

### Fixed

- **Escape now reaches the terminal again.** In 0.21.0, pressing Escape in any session did nothing — most visibly, vim would not leave insert mode and users had to fall back to Ctrl-C. It also affected `less`, `mc`, and readline vi-mode. Regression introduced by the in-app updater: `UpdateDialog` is mounted permanently at the app root and called `useEscapeKey()` unconditionally (as the rules of hooks require), even though it renders `null` until a check finds an update. That hook registered a document-level capture-phase listener which called `stopPropagation()` on every Escape, so the key never descended to xterm.js and no `\x1b` was ever sent to the remote host. Ctrl-C kept working because the handler only intercepted Escape. Not reproducible on 0.20.x, which predates the updater.
- Escape and Enter ownership is now structurally tied to what a dialog actually renders, which removes the whole class of bug above rather than just the one instance. `useEscapeKey` / `useEnterKey` are replaced by a `<DialogKeys onEscape={…} onEnter={…} />` element that dialogs render inside their own markup (17 call sites converted; both hooks deleted). A claim made in JSX cannot outlive what the component renders, so an invisible component can no longer hold a key hostage application-wide. The failure mode for a dialog that forgets to render it is local and immediately visible — that dialog does not dismiss on Escape — instead of a silent, global loss of a keystroke. `useEnterKey` had the same latent defect and is worse when triggered, since it also calls `preventDefault()`; its four callers happened to be conditionally mounted, so it never fired in practice. Enter still ignores textareas and IME composition.
- Commands sent from the command bar now scroll the target terminal back to the live prompt, matching what typing does. Typed input is handled by xterm.js, which scrolls the viewport to the prompt on keystroke (`scrollOnUserInput`); command buttons and the freeform command bar write straight to the backend over IPC and never pass through xterm, so a user who had scrolled up into the scrollback saw no visible reaction — the command ran, but they had to scroll down manually to see it. Fixed by adding a small `src/terminalRegistry.ts` module that maps session id → live xterm instance (`Terminal.tsx` registers on mount, unregisters on teardown), and calling `scrollTerminalToBottom()` once at the top of `executeCommand` in `CommandBar.tsx`. Putting it there rather than at the four call sites (command button, send-to-all, freeform-to-active, freeform-to-all) means every present and future sender inherits the behavior. Scrolling happens before the first write so the echoed output lands in view, and subsequent output keeps the viewport pinned because it is already at the bottom — including across `\p` pause segments. Send-to-all scrolls every targeted session, not just the visible one, so the other tabs are also at the prompt when the user switches to them. Deliberately not applied to arriving terminal output: yanking the viewport down mid-scrollback whenever the remote host prints something is the behavior scrollback exists to avoid.
- Session sidebar: the previously-selected item is now restored to its visible scroll position after the search input is cleared. Previously, clearing the search caused the tree (which had been replaced by a flat search-results list) to remount with scroll at the top, losing the user's place. A `useEffect` on `searchResults` now detects the filtered→unfiltered transition and queues a scroll request that runs after the tree is committed.
- Session sidebar: deleting a session or folder now reliably scrolls the new selection (the deleted item's parent folder) into view. The previous implementation used `requestAnimationFrame` inside a promise `.then`, which could fire before React had committed the post-`loadAll` render — leaving `scrollIntoView` to target stale DOM. Replaced with a `pendingScrollId` state slot that a `useEffect` consumes on the next render, so the scroll is guaranteed to run against the new tree. The same mechanism replaced four other deferred-scroll sites (folder create/rename and session create/edit), giving a single consistent path for "scroll into view after data settles".
- Highlight-profile rules with regex patterns like `\b(Twe|Hu|Te|Gi|Fa|Fo|Eth)\d+(?:/\d+)*\b` (network interface names) no longer silently fail to apply. The ReDoS safety guard in `src/highlightEngine.ts` was catching the substring `+)*` anywhere in the pattern, but `\d+(?:/\d+)*` is genuinely safe — the `/` literal at the start of the inner group anchors each outer iteration. The heuristic was rebuilt with two complementary checks: (1) single quantified atom inside a group followed by an outer quantifier (catches `(\d+)+`, `(.+)*`, `(a+)*`, `([abc]+)+`), and (2) multiple character-class atoms with no literal anchor inside a group followed by an outer quantifier (catches `(\d+\w+)+`, `(\w+\s+)*`, `(\d*\w*)+`). Groups containing any literal character — `(?:/\d+)*`, `(?:\w+,)+`, `(a+b+)+` — are allowed because the literal acts as an anchor that breaks the inter-iteration backtracking chain. Outer `{n}` (exact count) is no longer flagged as dangerous because a fixed repetition can't create iteration ambiguity. Verified against 18 known cases (10 dangerous, 8 safe); all classify correctly. Also corrected a misleading comment claiming the `try/catch` in `process()` defends against catastrophic backtracking — it doesn't, V8 hangs the thread rather than throwing, so `isSafePattern` is the actual defense. Known residual limitations of the heuristic (not exploitable in the current threat model, which is "user imports a third-party profile; worst case is a tab freeze recoverable by closing the tab"): nested-paren shapes like `((\d)+)+` and `((a|b)c)+` bypass both the nested-quantifier and alternation-quantifier checks; addressing those properly would need a parser-based analyzer or a switch to a linear-time regex engine such as RE2.
- Terminal zoom (Ctrl+/Ctrl-/Ctrl+0) no longer leaves the remote PTY thinking the terminal still has its old cols/rows. Symptom was vim drawing its bottom-right line/column position indicator (and other status content) at the now-out-of-bounds old coordinates after zooming in, with resetting the zoom only moving the artifacts rather than clearing them. Root cause: `effectiveFontSize` changes called `FitAddon.fit()` to recompute the xterm.js grid, but only the container `ResizeObserver` path invoked `ssh_resize`/`telnet_resize`/`pty_resize` — and the container's box dimensions don't change when only the font size changes. Fix: attach a `term.onResize()` listener as the single canonical signal for cols/rows propagation, so any source of `fit()` (container resize, zoom, visibility flip) automatically reaches the backend. Removes a dead `handleResize` `useCallback` that was clearly intended for this wiring but never connected.

- Confirming (or cancelling) the multiline-paste warning dialog now returns keyboard focus to the terminal. Previously the user had to click into the session before typing again: `PasteConfirmDialog` focuses its Cancel button on mount, and when the dialog unmounted that focus fell back to `<body>` with nothing restoring it. Fixed with a rising/falling-edge effect on `pendingPaste` in `src/components/Terminal.tsx` that calls `term.focus()` on the dialog's close transition. The focus call has to happen in an effect rather than inline in the `onConfirm`/`onCancel` handlers, because at the time those handlers run the dialog is still mounted and still holds focus — an inline `focus()` would be undone by the following commit that removes the button. A `visible` guard keeps a background tab from stealing focus, and the edge-tracking ref ensures an unrelated re-render with no dialog open never grabs focus. The paste-confirmation security control itself is untouched: the effect only moves DOM focus and never touches `pendingPaste`, `attemptPaste`, or `term.paste()`.

### Security

- `russh` 0.61.2 → 0.62.4 closes two advisories that are reachable from this app's client role. **GHSA-g9hv-x236-4qp3** (CVSS 5.3): a malicious or man-in-the-middle SSH server replies to key exchange with a short curve25519 ephemeral key, and the missing length check in `compute_shared_secret` panics the client. This fires during KEX — **before** host-key verification — so the known-hosts prompt that would otherwise flag a hostile server never gets the chance to run, and `curve25519-sha256` is in the default set `base_preferred()` keeps. **GHSA-5xvq-cp9x-6p6r** (CVSS 5.3): an all-zero peer public value causes an out-of-bounds panic in `encode_mpint` while computing the exchange hash; primarily a server-role issue, with the client secondarily affected after host-key verification. Both are denial-of-service, not code execution. The host-key path itself is untouched by the upgrade: `russh/src/keys/` is byte-identical between 0.61.2 and 0.62.4, so `check_known_hosts_path` still returns `Err(Error::KeyChanged { line })` and the explicit arm at `ssh/mod.rs:1493` keeps matching rather than falling into its catch-all. `rsa` remains pinned at `=0.10.0-rc.18`, so the RUSTSEC-2023-0071 reasoning documented in `Cargo.toml` is unchanged. Incidentally, `ed25519-dalek` and `curve25519-dalek` graduate from release candidates to 3.0.0 / 5.0.0 stable — these back Ed25519 host-key verification and X25519 key exchange.
- `quick-xml` 0.40.1 → 0.41.0 closes **RUSTSEC-2026-0194** (quadratic run time when checking a start tag for duplicate attribute names), which is genuinely reachable here: both `import/mremoteng.rs:163` and `import/securecrt.rs:433` call `BytesStart::attributes()` with duplicate-checking left on, over XML the user imports from an outside source, so a hostile or malformed `confCons.xml` could hang the import. 0.41.0 makes the check O(N) via a hash pre-filter. The same release covers **RUSTSEC-2026-0195** (unbounded namespace-declaration allocation), which was never reachable from this code — it requires `NsReader`, and both parsers use the plain `Reader`. The advisory-flagged transitive copies (0.38.4 via `plist`, 0.39.2 via `wayland-scanner`) collapse into the same 0.41.0, so the whole tree is on one patched version.
- Lockfile refresh clears the remaining `cargo audit` findings without any manifest change: `rustls-webpki` 0.103.11 → 0.103.13 covers **RUSTSEC-2026-0104** (reachable panic parsing a certificate revocation list), **RUSTSEC-2026-0099** and **RUSTSEC-2026-0098** (name constraints wrongly accepted for wildcard and URI names); `anyhow` 1.0.102 → 1.0.104 covers **RUSTSEC-2026-0190** (`downcast_mut` unsoundness); `rand` 0.8.5 → 0.8.7 covers **RUSTSEC-2026-0097**. None of these were reachable in practice — this app supplies no CRLs to either `reqwest` (updater HTTPS) or `sqlx` (PostgreSQL TLS), no crate in the graph calls `anyhow::Error::downcast_mut`, and `rand`'s `log` feature is off — but they are cleared rather than carried. The yanked `spin` 0.9.8 → 0.9.9, `aes` 0.9.0 → 0.9.2 and `crypto-bigint` 0.7.3 → 0.7.5 go with them; the `crypto-bigint` bump also fixes a real Karatsuba carry-truncation bug.
- A dialog can no longer answer a keystroke meant for the dialog on top of it. Escape/Enter ownership went to whichever modal mounted most recently, but every `.dialog-overlay` shares `z-index: 100`, so what the user actually sees on top is whichever overlay comes last in the document — and those two orders diverge, because dialogs portaled to `document.body` (`ConfirmDialog`, `PasteConfirmDialog`, `UpdateDialog`) always paint above dialogs rendered inline in the app tree. The exploitable case: `HostVerifyDialog` mounts on its own schedule when an SSH host key needs verification, so it could appear _behind_ an already-open modal (say the quit confirmation) and still own Enter — a user confirming the dialog they can see would silently TOFU-accept a host key they never saw, which is precisely the prompt that flags a changed key / possible MITM. Escape would likewise silently reject a connection. `KbdInteractiveDialog` (auth prompt) has the same asynchronous mount. `DialogKeys` now resolves ownership by document order instead: each instance renders a hidden anchor element, and the claim whose anchor comes last in the document — the one painted on top — receives the key. Claims with a detached anchor are skipped, and when nothing claims a key it passes through untouched, since letting a keystroke reach the terminal is always safer than swallowing it. Pre-existing behavior inherited from the `useEscapeKey` / `useEnterKey` stacks, not introduced by the rewrite, but fixed as part of it since the rewrite owns this model. Regression-tested with a portaled claim that mounts before an inline one.
- `ssh_resize` / `telnet_resize` now use a `tokio::sync::watch::Sender<Option<ResizeRequest>>` instead of the previous `mpsc(4)` + `try_send` arrangement. The watch primitive has the correct semantics for this signal: send_replace is infallible (no Tauri command worker can park, no resize can be silently dropped to "full"), the reader task picks up only the most recent value on its next iteration (intermediate values discarded by design), and the channel's `None` initial state means no redundant `window_change` / NAWS round-trip fires at session startup. Replaces the previous Changed-section entry that described the now-superseded `try_send` approach. `pty_resize` is unaffected because it never had a queue — it calls `portable_pty::MasterPty::resize` synchronously under a `std::sync::Mutex`.

  The auditor also flagged the `state.manager.lock().await` immediately preceding the resize send as a residual stall point that could be replaced with `try_lock`. This was rejected: the lock-hold duration is a single `HashMap::get` plus an `Arc::clone` (sub-microsecond), so the stall is theoretical, while `try_lock` would silently drop a single resize whenever any concurrent op (`ssh_write`, `ssh_disconnect`, …) happened to hold the lock at that instant. Since a single zoom action produces exactly one resize IPC with no frontend retry, a dropped resize would reintroduce the visible-UI desync we just fixed.

- `telnet_write` now mirrors `ssh_write` and dispatches the channel send via `tokio::spawn` instead of awaiting it on the Tauri command thread. The bounded capacity-64 write channel could otherwise park workers when the writer task is slow to drain (e.g. a stalled remote network). `try_send` was rejected here because dropping keystrokes silently would be far worse UX than dropping a resize event: a missing `\n` looks like a frozen shell. The detached task pattern lets the IPC return immediately while still preserving every byte the user typed.
- Per-session inflight-writes semaphore added to both `SshSession` and `TelnetSession`. Capped at `MAX_INFLIGHT_WRITES = 64` permits, acquired via `try_acquire_owned` before each spawned write task and released on task drop. This closes a heap-exhaustion DoS vector that the `tokio::spawn` write pattern would otherwise expose: under sustained adversarial load with a stalled writer, the spawn-and-park pattern allowed unbounded task accumulation, each task pinning up to `MAX_WRITE_SIZE` (64 KiB) of user payload. Worst-case held bytes per session are now bounded to two stacking 4 MiB windows (up to 64 spawned tasks each owning a 64 KiB payload, plus up to 64 already-buffered slots in the bounded write channel) ≈ 8 MiB. Across the 100-session cap, ≈ 800 MiB theoretical maximum. The same protection was applied to the pre-existing `ssh_write` path, which had inherited the same theoretical issue. Frontend sees a `"Too many pending writes for this session"` error when the cap is hit — the only path this can fire under realistic use is a runaway / malicious renderer.

### 0.20.0 - 2026-05-23

### Added

- Unit tests covering every branch of the security-relevant pure functions in the Rust backend: `is_restricted_ip` (SSRF guard against private/CGNAT/link-local/ULA ranges on IPv4 and IPv6), `validate_import_data` (count caps and per-field length/port validation on import payloads), `sanitize_pg_error` / `sanitize_ssh_error` / `sanitize_telnet_error` (verify error messages collapse to user-facing strings without leaking paths, FDs, or usernames). All five functions went from 0% → 100% (97.4% for `validate_import_data`) line coverage; CRAP scores dropped ~90%, moving them out of the "complex and untested" bucket flagged by `cargo crap`.
- `db::update_builder::UpdateBuilder`: a typed dynamic-SQL helper that owns the placeholder syntax (PostgreSQL `$N` vs SQLite `?`), the `BindVal` enum, and the bool-as-int / optional-UUID three-state semantics that previously lived as ~90 lines of `if let Some(...)` boilerplate in each provider. The builder is unit-tested without a live database. `PostgresProvider::update_session` and `SqliteProvider::update_session` now consist of 13 builder calls plus the per-dialect WHERE clause; both dropped from CC 29 (CRAP 870) to CC 8 (CRAP 72).
- `db::update_builder::SessionColumn`: an enum of the thirteen `sessions`-table columns that the session-edit IPC may touch, replacing the untyped `col: &str` parameter on `UpdateBuilder`. Column names now go through `SessionColumn::as_sql() -> &'static str`, so the SQL embedded by `format!()` is provably one of thirteen hardcoded strings — SQL injection via column name is impossible by construction rather than by call-site discipline. Adding a new column requires a deliberate enum extension that fails to compile until the consumers are wired up. Closes the medium-severity design fragility flagged in the security audit.
- Pull-parser test fixtures for the mRemoteNG (`confCons.xml`) and SecureCRT XML importers, covering the branch table of each: empty input, single SSH/Telnet session, default-port fallback, missing-hostname warning, unsupported-protocol skip, nested folders with parent tracking, jump-host references, built-in `Default` session skip (SecureCRT), oversized dword handling, and `MAX_NESTING_DEPTH` rejection. mRemoteNG's `parse` went from CRAP 650 → 34.8 (75% line coverage); SecureCRT's `parse_key_block` went from CRAP 930 → 49.3 (72% line coverage).
- Doc comments at the top of `session_connect`, `db_save_config`, `db_import`, `process_iac_byte`, and both XML parsers explaining why their complexity is essential (IPC orchestration, RFC 854 state machine, ambiguous-grammar look-ahead) and where the right testing point lies (integration harness for the IPC commands, fixtures for the parsers).
- Overall: functions exceeding `cargo crap`'s threshold of 30 dropped from 107 to 96; test count rose from 15 to 85.
- Justfile, for easier access to common executed steps.

### Changed

- Upgraded `tauri` to 2.11.2 and `tauri-build` to 2.6.2.
- Upgraded `serde_json` to 1.0.150.
- Upgraded `tokio` to 1.52.3.
- Upgraded `sqlx` to 0.9. The two consumer-visible changes:
  - `sqlx::query()` (and family) now only accept `&'static str` or `sqlx::AssertSqlSafe<…>` (PR #3723). The twenty dynamic-SQL call sites that build statements with `format!()` — the `UpdateBuilder` consumers, the bulk-edit `IN (?,?,…)` builders, the BFS folder-walk queries, and the keychain-rotation `CREATE DATABASE` path — are now wrapped in `sqlx::AssertSqlSafe(sql.as_str())`. With `SessionColumn` already enforcing that the only non-placeholder dynamic content is a typed column name, each `AssertSqlSafe` is honest by inspection.
  - The Cargo feature `sqlite` is now a meta-feature that pulls in `sqlite-deserialize` / `sqlite-load-extension` / `sqlite-unlock-notify` (PR #3928, SQLite extension loading is now `unsafe`). ShellStation uses none of those APIs, so the feature was switched to `sqlite-bundled` to keep the binary surface minimal.
- Upgraded russh to 0.61. As of russh 0.60.2, SHA-1 MACs (`hmac-sha1`, `hmac-sha1-etm`) are no longer part of `Preferred::DEFAULT.mac`. ShellStation's default (non-legacy) connection path inherits that tightening — these MACs are no longer offered during negotiation when **Legacy algorithms** is off. The Legacy toggle in the session editor remains the escape hatch and is now the **only** way to re-enable SHA-1 MACs. Targets that previously connected on default settings via SHA-1 MAC negotiation (some older Cisco IOS releases, certain industrial switches) will now require enabling the toggle; the existing "no shared algorithm" hint already points users at that setting.
- Moved `ADMIN_GUIDE.md`, `DESIGN.md`, `DEVELOPMENT.md`, and `RELEASING.md` into a dedicated `docs/` folder. Internal cross-references between these documents continue to use plain filenames because they still share a directory; external references in `README.md`, `CLAUDE.md`, and a comment in `src-tauri/src/db/postgres.rs` were updated to point at `docs/`.
- Upgraded `quick-xml` to 0.40.1. `Attribute::unescape_value()` was deprecated in favour of `Attribute::normalized_value(XmlVersion)` (PR #371), and `BytesText::xml_content()` likewise gained a required `XmlVersion` parameter for correct EOL normalization (PR #938). Both importers pass `XmlVersion::Implicit1_0` because both mRemoteNG (`confCons.xml`) and SecureCRT XML exports declare `<?xml version="1.0"?>`. For the two `Attribute::normalized_value` call sites this is behaviour-preserving — `unescape_value()` in 0.39.2 already delegated to XML 1.0 normalization. For the `BytesText::xml_content()` call site in the SecureCRT parser it is a **deliberate semantic change**: the old `xml_content()` was internally aliased to the XML 1.1 path, which also collapses U+0085 (NEL) and U+2028 (LS) to `\n`. XML 1.0 keeps those code points as-is, matching the file's own version declaration. The call site carries a comment explaining the choice. None of the other 0.40 breaking changes apply to ShellStation (no `NsReader`, no `Reader::read_text()`, no namespace `resolve*()` calls).
- Rust backend now passes `clippy::pedantic` clean (`just check`). Most fixes were mechanical (use method references over closures, `map_or`/`let-else` over `match` for one-arm cases, `clone_from` over reassignment, inlined format args, redundant `continue`/`async` removed). Doc comments throughout the backend now backtick identifier-shaped tokens (`snake_case`, `CamelCase`, product names like `PostgreSQL`/`SQLite`/`SecureCRT`/`ShellStation`/`ConPTY`, and protocol abbreviations like `NAWS`/`IAC`/`SB`/`SE`). Crate-level allows are documented in `src-tauri/src/lib.rs` for patterns that are deliberate architectural choices (`option_option` for three-state nullable updates, `cast_*` for already-bounded integer ranges, `too_many_lines` for the documented orchestration entry points, `needless_pass_by_value` for Tauri command convention).

### 0.12.0

### Added

- Visible warning in settings when PostgreSQL SSL mode is set to `disable` (cleartext credentials over the wire) and a softer note for `prefer` (which falls back to cleartext when the server lacks TLS)
- Keyboard activation (Enter/Space) on the shared-folder chevron in the sidebar (a11y)

### Fixed

- Drag-and-drop overlay drifted away from the cursor at non-100% UI scale (regression after the Tauri 2.11 / wry 0.55 upgrade — webview now reports `getBoundingClientRect` and pointer-event coordinates in different units around CSS `zoom`, defeating the previous compensation modifier)
- Search-result rows in the sidebar were skipped by `scrollIntoView` on selection because they lacked the `data-item-id` attribute the lookup queries
- Shared-folder expansion state could persist after the folder disappeared, causing it to re-open expanded when shared sessions reappeared
- "Saved" indicators for session logging, application logging, and user identity stayed visible indefinitely (or could fire `setState` on an unmounted dialog); they now auto-clear after 3 s and cancel cleanly on dialog close
- Hidden `<input type="file">` elements leaked into `document.body` when the user dismissed the highlight, database, or mRemoteNG/SecureCRT import pickers without selecting a file
- `DndContext` was remounting on every folder/session add or delete (count-based key), aborting any in-flight drag
- `addCommandButton`, `updateCommandButton`, `reorderCommandButtons`, and `setToastDismissSeconds` accepted unvalidated input that could bypass the limits enforced on rehydration
- Confirming the SSH host-key verification dialog with Enter on a first-time connection opened the session twice: the dialog listener fired on the bubble phase after the sidebar's tree `onKeyDown` had already re-triggered the connect. `useEnterKey` now listens in the capture phase, uses a stack so only the topmost dialog reacts, and stops propagation before the event reaches background handlers.
- `useEnterKey` / `useEscapeKey` now register a stable ref per hook instance instead of pushing the raw callback onto the stack. The stack only mutates on mount/unmount, eliminating two latent issues: StrictMode's intentional mount → unmount → mount no longer leaves duplicate entries, and unstable callbacks (inline arrow functions in dialog props) no longer churn the stack on every render. The ref is updated in `useLayoutEffect` rather than during render to remain safe under React's concurrent renderer. `useEscapeKey` also moves to the capture phase to match `useEnterKey`, so background tree/list handlers cannot fire before the dialog consumes the keypress (the capture handler still returns early when no dialog is open, so background Escape handlers like `ContextMenu` and `CustomSelect` keep working when no modal is mounted).

### Security

- PostgreSQL password is no longer round-tripped from the OS keychain through Tauri IPC into webview state when opening Settings; the field loads empty and an empty submission preserves the existing keychain entry
- `autoRefreshInterval` requires a minimum of 5 s (or 0 to disable); prevents a tampered persisted value from driving sub-second `session_data_fingerprint` polling against the IPC bridge
- `setLanguage` validates against the bundled-locale allowlist before passing to i18next (defense against attacker-supplied keys reaching i18next's internal resource map via tampered localStorage)
- Command-button rehydration now requires UUID-format ids and validates field shape (length, color regex)

### Removed

- 150% from the UI scale options. A small (~50 px) residual drag-overlay offset remains at exactly that zoom ratio under the new wry webview; 125% is now the maximum. Users with 150% saved are migrated to 125% on load.

### 0.11.4

### Added

- Optional "Confirm before pasting multi-line content" setting (Security section): when enabled, a preview dialog appears whenever pasted clipboard content contains line breaks or escape sequences. Helps prevent paste-jacking attacks where hidden commands embedded in copied text would execute on paste. Off by default.

### Fixed

- Translation fixes and improvements
- Pasting multi-line text on Windows inserted an extra blank line after each line (CRLF clipboard content is now normalized; bracketed paste mode is also honored when the remote shell enables it)

### Changed

- Updated Tauri to 2.11.1 (Rust crate and `@tauri-apps/api`), plus matching plugin versions

### 0.11.3 - 2026-04-27

### Added

- "Sort by Hostname / IP" context menu action for folders, with numeric IP-address-aware ordering (e.g. 10.0.0.2 before 10.0.0.10)

### Fixed

- Any authenticated user could rename, sort, reorder, or bulk-edit shared folders and sessions they don't own in PostgreSQL mode (authorization bypass)
- Any authenticated user could update another user's shared sessions via `update_session` in PostgreSQL mode (authorization bypass)
- Drag-and-drop overlay offset from cursor when UI scale is not 100% (CSS zoom compensation)
- Sort and reorder operations in SQLite using non-atomic writes that could leave corrupted sort_order on crash
- Bulk operations (credential profile, login sequence, bulk edit) in SQLite using non-atomic writes that could leave inconsistent state on crash

### Changed

- All folder/session sort, reorder, rename, and bulk-edit operations enforce `owner = current_user` in PostgreSQL mode
- Folder context menu items (Rename, Sort, Apply Credential Profile, Apply Login Sequence, Bulk Edit) restricted to owned folders in PostgreSQL mode
- Session context menu items (Edit, Clone) restricted to owned sessions in PostgreSQL mode
- Keyboard shortcuts (F2 rename/edit, Ctrl+D clone) restricted to owned items in PostgreSQL mode
- Bulk operations (credential profile, login sequence, bulk edit) traverse only owned folder subtrees in PostgreSQL mode
- SQLite reorder, sort, and bulk operations now wrapped in transactions for atomicity
- `update_session` enforces `owner = current_user` in PostgreSQL mode

### 0.11.2 - 2026-04-27

### Fixed

- Search results not updating after editing a session name, requiring a new search to see the change
- Deleting a session or folder jumping the view to the top of the list instead of selecting the parent folder
- Dialog overlays clipping behind the title bar and menu bar when UI scale is above 100%
- Session dialog and move dialog showing orphaned folders with broken parent chains in PostgreSQL mode
- Search returning sessions in unreachable folders that cannot be navigated to in the sidebar tree
- Any authenticated user could toggle another user's shared folder/session visibility via `set_visibility` (authorization bypass)

### Changed

- CSS selector in scroll-into-view helper now uses `CSS.escape()` to prevent malformed queries from non-UUID IDs
- Expanded folder state loaded from localStorage is now validated at startup to prevent crashes from corrupted data
- Resolved clippy warnings: collapsible match arms in mRemoteNG/SecureCRT importers, explicit counter loop in legacy migration
- Folder picker in session and move dialogs now restricted to tree-reachable folders owned by the current user (PostgreSQL mode)
- `set_visibility` command enforces ownership check before updating folder or session visibility

### 0.11.1 - 2026-04-23

### Added

- Import size limits for highlight profiles and login sequences to prevent resource exhaustion

### Fixed

- PostgreSQL startup failing for DML-only users due to `sqlx::migrate!()` attempting `CREATE TABLE` without schema privileges
- Import of highlight profiles and login sequences failing with UNIQUE constraint error when entries with the same name already exist
- Sharing a folder not cascading to descendant folders and sessions, causing inconsistent visibility for remote users
- Dynamic "Shared" folder expanding on single click instead of double click like regular folders
- Login sequence translations using literal "carriage return" equivalents instead of technical abbreviation "CR" across all 13 non-English locales
- Users could move shared folders/sessions into personal folders, causing data loss for the original owner
- Keyboard shortcuts (F2, Delete) in search results acting on the previously selected item instead of the clicked search result
- Session search being case-sensitive on PostgreSQL
- Users could clone sessions or create items inside shared folders they don't own, risking data loss when the owner changes folder visibility

### Changed

- PostgreSQL Administration Guide extracted from DESIGN.md into dedicated ADMIN_GUIDE.md, rewritten to present group role setup first
- PostgreSQL RLS setup failure downgraded from ERROR to WARN log level for DML-only users
- Setting a folder to shared or personal now cascades to all contained sub-folders and sessions
- "Move To" context menu and folder picker restricted to owned items and folders in PostgreSQL mode
- "New Session", "New Subfolder", and session dialog folder picker restricted to owned folders in PostgreSQL mode
- "Delete" context menu option hidden for shared folders and sessions not owned by the current user in PostgreSQL mode

### 0.11.0 - 2026-04-22

### Added

- Login sequence profiles: automate post-connection device prompts (e.g. username/password on Cisco switches) with regex-based expect/send steps, assignable per session
- Login sequence manager accessible from the sidebar and session dialogs
- Escape sequences in login responses: `\s` (username), `\w` (password), `\r`, `\n`, `\t`, `\b`, `\e`, `\\`, `\p` (1s pause)
- Bulk-assign login sequences to folders via context menu and bulk edit dialog
- Per-user login sequence mapping in PostgreSQL shared mode
- Login sequence export/import support

### Fixed

- Settings status messages (save confirmations, errors) appearing indented due to stray left margin

### 0.10.2 - 2026-04-22

### Fixed

- SSH connections to Cisco Small Business switches (SG350, Catalyst 1300) failing with "Channel send error" through jump hosts
- Dropdown menus positioned incorrectly when UI scale is not 100% (cross-engine CSS zoom compensation)
- Highlight profile dialog buttons using unstyled default HTML appearance instead of themed dialog buttons

### Changed

- Credential manager profile list now shows dividers between name and username, and separator lines between entries
- Improved translations across all 14 languages: fixed awkward wording, translated missing keys, added hint texts for settings sections
- Session log and application log hint texts now use tooltip icons consistent with the rest of the settings UI

### 0.10.1 - 2026-04-21

### Fixed

- SSH connections to legacy Cisco devices failing with "early eof" through jump hosts
- Compiler warning for unused variable on Windows builds

### Changed

- Broader SSH algorithm support (ECDH NIST P curves, AES-128-GCM) for modern Cisco IOS XE devices
- Credential retrieval returns zeroized memory wrapper to prevent secrets lingering in RAM
- Keychain error messages sanitized to prevent leaking OS keychain backend details
- PostgreSQL password field excluded from serialization to prevent accidental leaks
- Import parsers enforce maximum folder nesting depth (100 levels)
- Session log file creation verifies path stays within configured directory after open
- Highlight engine rejects regex patterns prone to catastrophic backtracking (ReDoS)

### 0.10.0 - 2026-04-20

### Added

- Version info in "About" help menu
- Ability to quickly reconnect disconnected sessions
- Visual indication for shared objects

### Fixed

- Implement proper keyboard-interactive authentication
- Race condition between backend data emission and frontend listener registration
- Clipboard paste from external applications not working on Linux (WebKitGTK)
- Application freeze when closing local sessions on Windows (ConPTY reader thread not unblocked)
- Potential deadlock from nested mutex acquisition in PTY session kill path
- SSH to legacy Cisco devices through jump hosts
- Fleeting cmd windows on Windows when accepting SSH host key for the first time

### Changed

- Jump host selection filtered to sessions tagged "jumphost" for better UX at scale
- More verbose logging in debug mode
- Position of GUI separator is saved persistently
- Implementation of proper multi-user RLS system with shared databases
- Multiple minor UX improvements
- New icons for switch and router

### Removed

- Close dialogues by clicking outside

### 0.9.4 - 2026-04-14

### Added

- Archive with extra files for releases
- Ability to use legacy algorithms per ssh session

### Changed

- Credential workflow refactored with new credential manager
- Minor hardening for data handling

## 0.9.3 - 2026-04-14

### Added

- Display session name in list on mouse hover
- More colors for command buttons
- Ability to select jump hosts in dropdown list with typing names
- Show progress when importing databases
- New free form input field for sending commands to sessions

### Fixed

- Taskmanager icon not showing (MS Windows)
- Be able to start app maximized (MS Windows)
- Launch App again on action "Restart"
- Settings Menu sizing
- Folders in Session list expandable/retractable with click on arrow
- Be able to set jump-host on sessions to "None"
- Correct folder structure on database import

### Changed

- Don't display warnings about unsupported session settings during database import
- Different syntax for application log files

## 0.9.2 - 2026-04-13

### Changed

- Upgrade to russh 0.60

### Removed

- Obsolete dependency of russh-keys
