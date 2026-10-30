import React, { useCallback, useEffect, useRef, useState } from "react";
import i18n from "../i18n";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { SearchAddon } from "@xterm/addon-search";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import type { SessionType } from "../stores/terminalStore";
import { useSettingsStore, ALLOWED_TERMINAL_FONTS } from "../stores/settingsStore";
import { useSessionStore } from "../stores/sessionStore";
import { useHighlightStore } from "../stores/highlightStore";
import { HighlightEngine } from "../highlightEngine";
import { useTheme } from "../hooks/useTheme";
import { PasteConfirmDialog } from "./PasteConfirmDialog";
import { isSessionClosedError, registerTerminal, unregisterTerminal } from "../terminalRegistry";
import { resolveTerminalTheme } from "../terminalTheme";

interface TerminalProps {
  sessionId: string;
  sessionType: SessionType;
  sessionDbId?: string;
  visible: boolean;
  exited?: boolean;
  onExit?: () => void;
  onReconnect?: () => void;
  onClose?: () => void;
}

/**
 * Decode a base64 string to bytes using the built-in atob. Returns null for
 * a string that is not valid base64, on which atob throws.
 */
function base64Decode(encoded: string): Uint8Array | null {
  let binary: string;
  try {
    binary = atob(encoded);
  } catch {
    return null;
  }
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

const MIN_FONT_SIZE = 6;
const MAX_FONT_SIZE = 72;

const ALLOWED_FONTS: ReadonlySet<string> = new Set(ALLOWED_TERMINAL_FONTS);

/** Build a CSS font-family string: selected font + monospace fallback. */
function buildFontFamily(font: string): string {
  const safe = ALLOWED_FONTS.has(font) ? font : "monospace";
  return `"${safe}", monospace`;
}

// eslint-disable-next-line @typescript-eslint/no-empty-function
const noop = (): void => {};

export function Terminal({
  sessionId,
  sessionType,
  sessionDbId,
  visible,
  exited,
  onExit,
  onReconnect,
  onClose,
}: TerminalProps): React.JSX.Element {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<XTerm | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const highlightRef = useRef<HighlightEngine | null>(null);
  const onExitRef = useRef(onExit);
  onExitRef.current = onExit;
  const exitedRef = useRef(exited);
  exitedRef.current = exited;
  const onReconnectRef = useRef(onReconnect);
  onReconnectRef.current = onReconnect;
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  const {
    terminalFontFamily,
    terminalFontSize,
    terminalBackgroundColor,
    terminalForegroundColor,
    copyOnSelect,
    pasteOnRightClick,
    confirmMultilinePaste,
  } = useSettingsStore();
  const resolvedTheme = useTheme();

  // Per-tab zoom offset — not persisted, local to this terminal instance.
  const [localZoomOffset, setLocalZoomOffset] = useState(0);
  // Pending paste content awaiting user confirmation (null = no dialog open).
  const [pendingPaste, setPendingPaste] = useState<string | null>(null);
  const effectiveFontSize = Math.max(
    MIN_FONT_SIZE,
    Math.min(MAX_FONT_SIZE, terminalFontSize + localZoomOffset),
  );

  // Keep mutable refs for values accessed inside event handlers that
  // are registered once (not re-created on every settings change).
  const copyOnSelectRef = useRef(copyOnSelect);
  copyOnSelectRef.current = copyOnSelect;
  const pasteOnRightClickRef = useRef(pasteOnRightClick);
  pasteOnRightClickRef.current = pasteOnRightClick;
  const confirmMultilinePasteRef = useRef(confirmMultilinePaste);
  confirmMultilinePasteRef.current = confirmMultilinePaste;
  const pendingPasteRef = useRef<string | null>(pendingPaste);
  pendingPasteRef.current = pendingPaste;
  const localZoomOffsetRef = useRef(localZoomOffset);
  localZoomOffsetRef.current = localZoomOffset;
  const baseFontSizeRef = useRef(terminalFontSize);
  baseFontSizeRef.current = terminalFontSize;

  // Build highlight engine from the session's profile assignment.
  const session = useSessionStore((s) =>
    sessionDbId ? s.sessions.find((sess) => sess.id === sessionDbId) : undefined,
  );
  const highlightProfileId = session?.highlight_profile_id ?? null;
  const getProfileById = useHighlightStore((s) => s.getProfileById);

  useEffect(() => {
    if (highlightProfileId) {
      const profile = getProfileById(highlightProfileId);
      highlightRef.current = profile ? new HighlightEngine(profile.rules) : null;
    } else {
      highlightRef.current = null;
    }
  }, [highlightProfileId, getProfileById]);

  const writeCmd =
    sessionType === "ssh" ? "ssh_write" : sessionType === "telnet" ? "telnet_write" : "pty_write";
  const resizeCmd =
    sessionType === "ssh"
      ? "ssh_resize"
      : sessionType === "telnet"
        ? "telnet_resize"
        : "pty_resize";

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const term = new XTerm({
      cursorBlink: true,
      fontSize: terminalFontSize,
      fontFamily: buildFontFamily(terminalFontFamily),
      theme: resolveTerminalTheme(resolvedTheme, terminalBackgroundColor, terminalForegroundColor),
    });

    const fitAddon = new FitAddon();
    const searchAddon = new SearchAddon();

    term.loadAddon(fitAddon);
    term.loadAddon(searchAddon);
    term.open(container);

    try {
      const webglAddon = new WebglAddon();
      webglAddon.onContextLoss(() => {
        webglAddon.dispose();
      });
      term.loadAddon(webglAddon);
    } catch {
      // WebGL not available — canvas fallback is automatic.
    }

    // Push every cols/rows change to the backend, regardless of source —
    // container resize, font-size change (zoom), or visibility flip all
    // route through fit() and surface here. Registered before the first
    // fit() so the initial sizing also propagates.
    const onResizeDisposable = term.onResize(({ cols, rows }) => {
      invoke(resizeCmd, { id: sessionId, cols, rows }).catch(noop);
    });

    fitAddon.fit();

    termRef.current = term;
    fitAddonRef.current = fitAddon;

    // Exit handling runs once, on whichever signal arrives first: the
    // backend's exit event, or a write rejected because the session has
    // already closed. The second path covers a late or lost event — without
    // it the tab sits on a dead session, swallowing input, and Ctrl+W/Enter
    // never switch to close/reconnect. Set on unmount too, so a write that
    // rejects after teardown cannot touch the disposed terminal.
    let sessionEnded = false;
    const handleExit = (): void => {
      if (sessionEnded) return;
      sessionEnded = true;
      term.write(`\r\n${i18n.t("terminal.processExited")}\r\n`);
      if (sessionDbId) {
        term.write(`${i18n.t("terminal.reconnectHint")}\r\n`);
      }
      onExitRef.current?.();
    };

    // Let senders outside this component (command buttons, command bar)
    // reach this instance — see terminalRegistry.
    registerTerminal(sessionId, term, handleExit);

    // Forward user input to the backend.
    const onDataDisposable = term.onData((data: string) => {
      invoke(writeCmd, { id: sessionId, data }).catch((err: unknown) => {
        if (isSessionClosedError(err)) handleExit();
      });
    });

    // Route clipboard text into the terminal. If the "warn before multi-line
    // paste" setting is on and the content contains any C0/C1 control
    // character (newline, ESC, BEL, BS, NUL, DEL, …; tab is allowed),
    // open the confirmation dialog instead of pasting immediately.
    // Subsequent paste attempts while a dialog is already open are dropped
    // so the pending content can't be silently swapped under the user;
    // pendingPasteRef is set synchronously to close the check-then-set
    // race for paste events arriving in the same event-loop turn.
    // eslint-disable-next-line no-control-regex
    const CONTROL_CHAR_RE = /[\x00-\x08\x0a-\x1f\x7f-\x9f]/;
    const attemptPaste = (text: string): void => {
      if (confirmMultilinePasteRef.current && CONTROL_CHAR_RE.test(text)) {
        if (pendingPasteRef.current !== null) return;
        pendingPasteRef.current = text;
        setPendingPaste(text);
      } else {
        term.paste(text);
      }
    };

    // Copy-on-select: when user finishes a selection, copy to clipboard.
    const onSelectionDisposable = term.onSelectionChange(() => {
      if (copyOnSelectRef.current) {
        const selection = term.getSelection();
        if (selection) {
          void writeText(selection).catch(noop);
        }
      }
    });

    // Keyboard handler for Ctrl+Shift+C/V (copy/paste) and Ctrl+Plus/Minus (zoom).
    // Return true to let xterm handle the key normally, false to suppress it.
    term.attachCustomKeyEventHandler((event: KeyboardEvent): boolean => {
      // Only act on keydown events.
      if (event.type !== "keydown") return true;

      // Ctrl+Shift+C — copy selection
      if (event.ctrlKey && event.shiftKey && event.key === "C") {
        event.preventDefault();
        const selection = term.getSelection();
        if (selection) {
          void writeText(selection).catch(noop);
        }
        return false;
      }

      // Ctrl+Shift+V — paste from clipboard
      if (event.ctrlKey && event.shiftKey && event.key === "V") {
        event.preventDefault();
        void readText()
          .then((text) => {
            if (text) {
              attemptPaste(text);
            }
          })
          .catch(noop);
        return false;
      }

      // Ctrl+= or Ctrl++ — zoom in (per-tab only)
      if (event.ctrlKey && !event.shiftKey && (event.key === "=" || event.key === "+")) {
        event.preventDefault();
        const current = baseFontSizeRef.current + localZoomOffsetRef.current;
        if (current < MAX_FONT_SIZE) {
          setLocalZoomOffset((prev) => prev + 1);
        }
        return false;
      }

      // Ctrl+- — zoom out (per-tab only)
      if (event.ctrlKey && !event.shiftKey && event.key === "-") {
        event.preventDefault();
        const current = baseFontSizeRef.current + localZoomOffsetRef.current;
        if (current > MIN_FONT_SIZE) {
          setLocalZoomOffset((prev) => prev - 1);
        }
        return false;
      }

      // Ctrl+0 — reset zoom to global default (clear per-tab offset)
      if (event.ctrlKey && !event.shiftKey && event.key === "0") {
        event.preventDefault();
        setLocalZoomOffset(0);
        return false;
      }

      // Ctrl+W on a disconnected session — close the tab.
      if (event.ctrlKey && event.key === "w" && exitedRef.current) {
        event.preventDefault();
        onCloseRef.current?.();
        return false;
      }

      // Enter on a disconnected session — trigger reconnect if available,
      // otherwise just block the input (the backend session is gone).
      if (event.key === "Enter" && exitedRef.current) {
        event.preventDefault();
        onReconnectRef.current?.();
        return false;
      }

      return true;
    });

    // Right-click paste: listen on mousedown (button 2) instead of
    // contextmenu — the contextmenu event is globally suppressed and
    // may not provide sufficient user activation for clipboard access.
    const handleRightClick = (event: MouseEvent): void => {
      if (event.button === 2 && pasteOnRightClickRef.current) {
        event.preventDefault();
        event.stopPropagation();
        void readText()
          .then((text) => {
            if (text) {
              attemptPaste(text);
            }
          })
          .catch(noop);
      }
    };
    container.addEventListener("mousedown", handleRightClick, true);

    // Native browser paste (Ctrl+V, menu paste) — xterm.js attaches its own
    // paste listener to its internal textarea and would otherwise call
    // term.paste() directly, bypassing attemptPaste and defeating the
    // confirmMultilinePaste setting. Intercept in the capture phase before
    // xterm sees the event and route the content through attemptPaste.
    const handleNativePaste = (event: ClipboardEvent): void => {
      // Always consume the paste event so xterm's bubble-phase handler
      // never sees it, even if clipboardData turns out to be empty.
      event.preventDefault();
      event.stopPropagation();
      const text = event.clipboardData?.getData("text/plain");
      if (!text) return;
      attemptPaste(text);
    };
    container.addEventListener("paste", handleNativePaste, true);

    // One decoder for the whole session, in streaming mode. The backend sends
    // whatever each read returned, so a chunk can end partway through a
    // multi-byte UTF-8 character; a streaming decoder holds those bytes back
    // until the next chunk completes the character, where decoding each chunk
    // on its own would turn both halves into U+FFFD.
    const decoder = new TextDecoder();

    // Listen for terminal output and exit. Registering a listener is async,
    // so the effect can be cleaned up before it finishes: StrictMode mounts,
    // unmounts and remounts every component in development, and a tab can be
    // closed straight away. Cleanup therefore removes each listener whenever
    // its registration completes, and only a still-mounted instance tells the
    // backend it is ready. That signal is one-shot and releases the output
    // buffered so far, so from a discarded instance it would hand that output
    // to a terminal nobody sees.
    let disposed = false;
    const listeners: Promise<UnlistenFn>[] = [
      listen<string>(`terminal-output-${sessionId}`, (event) => {
        // A listener still registering at cleanup can fire before removal.
        if (disposed) return;
        const bytes = base64Decode(event.payload);
        if (!bytes) {
          // The backend always sends base64, so this is a bug there. Drop the
          // chunk rather than throw out of the event handler, and keep the
          // payload out of the log: it is terminal output.
          // eslint-disable-next-line no-console -- diagnostic detail goes to devtools, not the UI
          console.warn("terminal output event was not valid base64; dropped it");
          return;
        }
        const text = decoder.decode(bytes, { stream: true });
        const highlighted = highlightRef.current ? highlightRef.current.process(text) : text;
        term.write(highlighted);
      }),
      // handleExit needs no such check: cleanup sets sessionEnded.
      listen(`terminal-exit-${sessionId}`, handleExit),
    ];

    Promise.all(listeners)
      .then(() => (disposed ? undefined : invoke("terminal_ready", { id: sessionId })))
      .catch(noop);

    // Handle container resize.  Skip when the container is hidden
    // (display: none → 0×0) to prevent xterm.js from reflowing
    // content to a tiny column count and mangling the display. The
    // backend IPC is pushed by the term.onResize listener above.
    const resizeObserver = new ResizeObserver((entries) => {
      const { width, height } = entries[0].contentRect;
      if (width === 0 || height === 0) return;
      fitAddon.fit();
    });
    resizeObserver.observe(container);

    return () => {
      sessionEnded = true;
      disposed = true;
      resizeObserver.disconnect();
      onDataDisposable.dispose();
      onSelectionDisposable.dispose();
      onResizeDisposable.dispose();
      container.removeEventListener("mousedown", handleRightClick, true);
      container.removeEventListener("paste", handleNativePaste, true);
      for (const listener of listeners) {
        listener
          .then((unlisten) => {
            unlisten();
          })
          .catch(noop);
      }
      setPendingPaste(null);
      unregisterTerminal(sessionId, term);
      term.dispose();
      termRef.current = null;
      fitAddonRef.current = null;
    };
  }, [sessionId, sessionType, writeCmd, resizeCmd]);

  // Apply font family, font size, and theme changes to a live terminal.
  useEffect(() => {
    const term = termRef.current;
    if (!term) return;
    term.options.fontFamily = buildFontFamily(terminalFontFamily);
    term.options.fontSize = effectiveFontSize;
    term.options.theme = resolveTerminalTheme(
      resolvedTheme,
      terminalBackgroundColor,
      terminalForegroundColor,
    );
    fitAddonRef.current?.fit();
  }, [
    terminalFontFamily,
    effectiveFontSize,
    resolvedTheme,
    terminalBackgroundColor,
    terminalForegroundColor,
  ]);

  // Single path for "give the terminal keyboard focus". Every caller must go
  // through here so the visibility rule can never drift between call sites: a
  // terminal in a background tab must not steal focus from the active one.
  // Reads `visible` through a ref because callers may fire on a delay (see the
  // 50 ms timer below), by which point the captured prop could be stale. The
  // ref is written during render like the other prop mirrors above — writes to
  // a ref are not a render side effect, and every effect below sees the value
  // from the render it was scheduled by.
  const visibleRef = useRef(visible);
  visibleRef.current = visible;
  const focusIfVisible = useCallback((): void => {
    if (visibleRef.current) {
      termRef.current?.focus();
    }
  }, []);

  // Re-fit and focus when visibility changes.
  useEffect(() => {
    if (visible && fitAddonRef.current) {
      const timer = setTimeout(() => {
        fitAddonRef.current?.fit();
        focusIfVisible();
      }, 50);
      return () => {
        clearTimeout(timer);
      };
    }
    return undefined;
  }, [visible, focusIfVisible]);

  // Restore focus to the terminal once the paste confirmation dialog closes.
  // The dialog focuses its Cancel button on mount; when it unmounts, focus
  // falls back to <body> and the user would have to click into the terminal
  // before typing again. Runs on both Confirm and Cancel.
  const pasteDialogWasOpen = useRef(false);
  useEffect(() => {
    if (pendingPaste !== null) {
      pasteDialogWasOpen.current = true;
    } else if (pasteDialogWasOpen.current) {
      pasteDialogWasOpen.current = false;
      focusIfVisible();
    }
    // `visible` is deliberately not a dependency: the visibility check lives
    // inside focusIfVisible (via a ref), and re-running this effect on a bare
    // visibility flip would do nothing anyway — the one-shot flag is already
    // consumed by then. Do not "fix" this with exhaustive-deps.
  }, [pendingPaste, focusIfVisible]);

  return (
    <>
      <div
        ref={containerRef}
        className={`terminal-instance${visible ? "" : " terminal-instance-hidden"}`}
      />
      {pendingPaste !== null ? (
        <PasteConfirmDialog
          content={pendingPaste}
          onConfirm={() => {
            // Functional updater reads the latest pending content, even if
            // a re-render happened between the user clicking Confirm and
            // this handler firing.
            setPendingPaste((current) => {
              if (current !== null) {
                termRef.current?.paste(current);
              }
              return null;
            });
          }}
          onCancel={() => {
            setPendingPaste(null);
          }}
        />
      ) : null}
    </>
  );
}
