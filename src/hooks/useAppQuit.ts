import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useSettingsStore } from "../stores/settingsStore";
import { useTerminalStore } from "../stores/terminalStore";

/**
 * How long after a quit began the window is closed by the frontend itself,
 * should the app still be running. The backend exits within 4 s.
 */
export const QUIT_WATCHDOG_MS = 10_000;

export interface AppQuit {
  /** The quit confirmation is showing. */
  confirming: boolean;
  /** The backend is closing SSH or Telnet sessions before it exits. */
  quitting: boolean;
  /** Confirm the quit the confirmation asks about. */
  confirmQuit: () => void;
  /** Keep the app open. */
  cancelQuit: () => void;
}

/** Close the window, and with it the app; true once that has gone through. */
async function closeWindow(): Promise<boolean> {
  try {
    await getCurrentWindow().destroy();
    return true;
  } catch (e) {
    // eslint-disable-next-line no-console -- diagnostic detail goes to devtools, not the UI
    console.warn("closing the window failed:", e);
    return false;
  }
}

/**
 * The ways of quitting the app that reach the webview: Ctrl+Q and the
 * window's close button. Each asks to confirm when sessions are open and the
 * setting is on, then quits through the backend's `app_quit`, which closes
 * every session first, so that each SSH server is told the session has
 * ended, and then exits. The native menu's quit item goes to the backend
 * directly, and on macOS so does Cmd+Q, its shortcut, which the menu takes
 * before the webview sees it; the handler here takes Cmd+Q as well, for when
 * it does arrive. Whichever started it, the backend sends `app:quitting`
 * when the closing can take a moment, and `quitting` turns true.
 */
export function useAppQuit(): AppQuit {
  const [confirming, setConfirming] = useState(false);
  const [quitting, setQuitting] = useState(false);
  const quitStarted = useRef(false);
  const watchdog = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  // Should the app still be running some time after the quit began, close
  // the window, so that the user is never left with an app that ignores
  // every further quit.
  const startWatchdog = useCallback(() => {
    if (watchdog.current !== undefined) return;
    watchdog.current = setTimeout(() => {
      void closeWindow();
    }, QUIT_WATCHDOG_MS);
  }, []);

  useEffect(
    () => () => {
      clearTimeout(watchdog.current);
      watchdog.current = undefined;
    },
    [],
  );

  // Should the call fail, close the window, as the app did before it closed
  // its sessions itself; should that fail too, let the user try again.
  const quit = useCallback(() => {
    if (quitStarted.current) return;
    quitStarted.current = true;
    invoke("app_quit").then(startWatchdog, async (e: unknown) => {
      // eslint-disable-next-line no-console -- diagnostic detail goes to devtools, not the UI
      console.warn("app_quit failed:", e);
      if (!(await closeWindow())) quitStarted.current = false;
    });
  }, [startWatchdog]);

  const requestQuit = useCallback(() => {
    if (quitStarted.current) return;
    const aliveTabs = useTerminalStore.getState().tabs.filter((t) => !t.exited);
    if (aliveTabs.length > 0 && useSettingsStore.getState().confirmOnQuit) {
      setConfirming(true);
    } else {
      quit();
    }
  }, [quit]);

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent): void => {
      const modKey = e.metaKey || e.ctrlKey;
      if (modKey && e.key.toLowerCase() === "q") {
        e.preventDefault();
        requestQuit();
      }
    };
    document.addEventListener("keydown", handleKeyDown, true);
    return () => {
      document.removeEventListener("keydown", handleKeyDown, true);
    };
  }, [requestQuit]);

  // Closing the window quits the app, so it goes the same way as Ctrl+Q:
  // left to itself, the window would close with the sessions still open.
  // The handler is registered asynchronously; a close in the moment before
  // that, while the page is still starting, closes the window directly.
  useEffect(() => {
    const unlisten = getCurrentWindow().onCloseRequested((event) => {
      event.preventDefault();
      requestQuit();
    });
    return () => {
      void unlisten.then((fn) => {
        fn();
      });
    };
  }, [requestQuit]);

  // The quit may have begun in the backend (the menu's quit item), so this
  // also stops any further request here.
  useEffect(() => {
    const unlisten = listen("app:quitting", () => {
      quitStarted.current = true;
      startWatchdog();
      setConfirming(false);
      setQuitting(true);
    });
    return () => {
      void unlisten.then((fn) => {
        fn();
      });
    };
  }, [startWatchdog]);

  const confirmQuit = useCallback(() => {
    setConfirming(false);
    quit();
  }, [quit]);

  const cancelQuit = useCallback(() => {
    setConfirming(false);
  }, []);

  return { confirming, quitting, confirmQuit, cancelQuit };
}
