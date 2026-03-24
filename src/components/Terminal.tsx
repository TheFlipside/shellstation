import React, { useEffect, useRef, useCallback } from "react";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { SearchAddon } from "@xterm/addon-search";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { SessionType } from "../stores/terminalStore";

interface TerminalProps {
  sessionId: string;
  sessionType: SessionType;
  visible: boolean;
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

// eslint-disable-next-line @typescript-eslint/no-empty-function
const noop = (): void => {};

export function Terminal({ sessionId, sessionType, visible }: TerminalProps): React.JSX.Element {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<XTerm | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);

  const writeCmd = sessionType === "ssh" ? "ssh_write" : "pty_write";
  const resizeCmd = sessionType === "ssh" ? "ssh_resize" : "pty_resize";

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
      fontSize: 14,
      fontFamily: '"JetBrains Mono", "Fira Code", "Cascadia Code", monospace',
      theme: {
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

    // Listen for terminal output.
    let outputUnlisten: UnlistenFn | null = null;
    let exitUnlisten: UnlistenFn | null = null;

    const setupListeners = async (): Promise<void> => {
      outputUnlisten = await listen<string>(`terminal-output-${sessionId}`, (event) => {
        const bytes = base64Decode(event.payload);
        const text = new TextDecoder().decode(bytes);
        term.write(text);
      });

      exitUnlisten = await listen(`terminal-exit-${sessionId}`, () => {
        term.write("\r\n[Process exited]\r\n");
      });
    };

    setupListeners().catch(noop);

    // Handle window resize.
    const resizeObserver = new ResizeObserver(() => {
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
      if (outputUnlisten) outputUnlisten();
      if (exitUnlisten) exitUnlisten();
      term.dispose();
      termRef.current = null;
      fitAddonRef.current = null;
    };
  }, [sessionId, sessionType, writeCmd, resizeCmd, handleResize]);

  // Re-fit when visibility changes.
  useEffect(() => {
    if (visible && fitAddonRef.current) {
      const timer = setTimeout(() => {
        fitAddonRef.current?.fit();
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
      style={{
        width: "100%",
        height: "100%",
        display: visible ? "block" : "none",
      }}
    />
  );
}
