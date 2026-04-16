import React, { useEffect, useRef, useState, useCallback } from "react";
import i18n from "../i18n";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { SearchAddon } from "@xterm/addon-search";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { SessionType } from "../stores/terminalStore";
import { useSettingsStore, ALLOWED_TERMINAL_FONTS } from "../stores/settingsStore";
import { useSessionStore } from "../stores/sessionStore";
import { useHighlightStore } from "../stores/highlightStore";
import { HighlightEngine } from "../highlightEngine";
import { useTheme, type ResolvedTheme } from "../hooks/useTheme";

interface TerminalProps {
  sessionId: string;
  sessionType: SessionType;
  sessionDbId?: string;
  visible: boolean;
  exited?: boolean;
  onExit?: () => void;
  onReconnect?: () => void;
}

/** Decode a base64 string to bytes using the built-in atob. */
function base64Decode(encoded: string): Uint8Array {
  const binary = atob(encoded);
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

/** xterm.js color themes keyed by resolved theme name. */
const XTERM_THEMES: Record<ResolvedTheme, Record<string, string>> = {
  dark: {
    background: "#1e1e2e",
    foreground: "#cdd6f4",
    cursor: "#f5e0dc",
    selectionBackground: "#585b7066",
    black: "#45475a",
    red: "#f38ba8",
    green: "#a6e3a1",
    yellow: "#f9e2af",
    blue: "#89b4fa",
    magenta: "#f5c2e7",
    cyan: "#94e2d5",
    white: "#bac2de",
    brightBlack: "#585b70",
    brightRed: "#f38ba8",
    brightGreen: "#a6e3a1",
    brightYellow: "#f9e2af",
    brightBlue: "#89b4fa",
    brightMagenta: "#f5c2e7",
    brightCyan: "#94e2d5",
    brightWhite: "#a6adc8",
  },
  light: {
    background: "#eff1f5",
    foreground: "#4c4f69",
    cursor: "#dc8a78",
    selectionBackground: "#acb0be66",
    black: "#5c5f77",
    red: "#d20f39",
    green: "#40a02b",
    yellow: "#df8e1d",
    blue: "#1e66f5",
    magenta: "#ea76cb",
    cyan: "#179299",
    white: "#acb0be",
    brightBlack: "#6c6f85",
    brightRed: "#d20f39",
    brightGreen: "#40a02b",
    brightYellow: "#df8e1d",
    brightBlue: "#1e66f5",
    brightMagenta: "#ea76cb",
    brightCyan: "#179299",
    brightWhite: "#bcc0cc",
  },
};

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

  const { terminalFontFamily, terminalFontSize, copyOnSelect, pasteOnRightClick } =
    useSettingsStore();
  const resolvedTheme = useTheme();

  // Per-tab zoom offset — not persisted, local to this terminal instance.
  const [localZoomOffset, setLocalZoomOffset] = useState(0);
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

  const handleResize = useCallback(() => {
    const fit = fitAddonRef.current;
    const term = termRef.current;
    if (!fit || !term) return;

    fit.fit();
    invoke(resizeCmd, {
      id: sessionId,
      cols: term.cols,
      rows: term.rows,
    }).catch(noop);
  }, [sessionId, resizeCmd]);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const term = new XTerm({
      cursorBlink: true,
      fontSize: terminalFontSize,
      fontFamily: buildFontFamily(terminalFontFamily),
      theme: XTERM_THEMES[resolvedTheme],
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

    fitAddon.fit();

    termRef.current = term;
    fitAddonRef.current = fitAddon;

    // Forward user input to the backend.
    const onDataDisposable = term.onData((data: string) => {
      invoke(writeCmd, { id: sessionId, data }).catch(noop);
    });

    // Copy-on-select: when user finishes a selection, copy to clipboard.
    const onSelectionDisposable = term.onSelectionChange(() => {
      if (copyOnSelectRef.current) {
        const selection = term.getSelection();
        if (selection) {
          void navigator.clipboard.writeText(selection);
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
          void navigator.clipboard.writeText(selection);
        }
        return false;
      }

      // Ctrl+Shift+V — paste from clipboard
      if (event.ctrlKey && event.shiftKey && event.key === "V") {
        event.preventDefault();
        void navigator.clipboard.readText().then((text) => {
          invoke(writeCmd, { id: sessionId, data: text }).catch(noop);
        });
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
        void navigator.clipboard.readText().then((text) => {
          if (text) {
            invoke(writeCmd, { id: sessionId, data: text }).catch(noop);
          }
        });
      }
    };
    container.addEventListener("mousedown", handleRightClick, true);

    // Listen for terminal output.
    let outputUnlisten: UnlistenFn | null = null;
    let exitUnlisten: UnlistenFn | null = null;

    const setupListeners = async (): Promise<void> => {
      outputUnlisten = await listen<string>(`terminal-output-${sessionId}`, (event) => {
        const bytes = base64Decode(event.payload);
        const text = new TextDecoder().decode(bytes);
        const highlighted = highlightRef.current ? highlightRef.current.process(text) : text;
        term.write(highlighted);
      });

      exitUnlisten = await listen(`terminal-exit-${sessionId}`, () => {
        term.write(`\r\n${i18n.t("terminal.processExited")}\r\n`);
        if (sessionDbId) {
          term.write(`${i18n.t("terminal.reconnectHint")}\r\n`);
        }
        if (onExitRef.current) {
          onExitRef.current();
        }
      });
    };

    setupListeners()
      .then(() => invoke("terminal_ready", { id: sessionId }))
      .catch(noop);

    // Handle window resize.  Skip when the container is hidden
    // (display: none → 0×0) to prevent xterm.js from reflowing
    // content to a tiny column count and mangling the display.
    const resizeObserver = new ResizeObserver((entries) => {
      const { width, height } = entries[0].contentRect;
      if (width === 0 || height === 0) return;
      fitAddon.fit();
      invoke(resizeCmd, {
        id: sessionId,
        cols: term.cols,
        rows: term.rows,
      }).catch(noop);
    });
    resizeObserver.observe(container);

    return () => {
      resizeObserver.disconnect();
      onDataDisposable.dispose();
      onSelectionDisposable.dispose();
      container.removeEventListener("mousedown", handleRightClick, true);
      if (outputUnlisten) outputUnlisten();
      if (exitUnlisten) exitUnlisten();
      term.dispose();
      termRef.current = null;
      fitAddonRef.current = null;
    };
  }, [sessionId, sessionType, writeCmd, resizeCmd, handleResize]);

  // Apply font family, font size, and theme changes to a live terminal.
  useEffect(() => {
    const term = termRef.current;
    if (!term) return;
    term.options.fontFamily = buildFontFamily(terminalFontFamily);
    term.options.fontSize = effectiveFontSize;
    term.options.theme = XTERM_THEMES[resolvedTheme];
    fitAddonRef.current?.fit();
  }, [terminalFontFamily, effectiveFontSize, resolvedTheme]);

  // Re-fit and focus when visibility changes.
  useEffect(() => {
    if (visible && fitAddonRef.current) {
      const timer = setTimeout(() => {
        fitAddonRef.current?.fit();
        termRef.current?.focus();
      }, 50);
      return () => {
        clearTimeout(timer);
      };
    }
    return undefined;
  }, [visible]);

  return (
    <div
      ref={containerRef}
      className={`terminal-instance${visible ? "" : " terminal-instance-hidden"}`}
    />
  );
}
