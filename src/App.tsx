import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";
import { ConfirmDialog } from "./components/ConfirmDialog";
import { DatabaseStatusBanner } from "./components/DatabaseStatusBanner";
import { QuittingOverlay } from "./components/QuittingOverlay";
import { SessionSidebar } from "./components/SessionSidebar";
import { SettingsDialog } from "./components/SettingsDialog";
import { TerminalTabs } from "./components/TerminalTabs";
import { ToastContainer } from "./components/ToastContainer";
import { UpdateDialog } from "./components/UpdateDialog";
import { UserIdentDialog } from "./components/UserIdentDialog";
import { useAppQuit } from "./hooks/useAppQuit";
import { useTheme } from "./hooks/useTheme";
import { showPrivateIpCheckOffNotice } from "./privateIpCheckNotice";
import { useAppStore } from "./stores/appStore";
import { startUpConnectionSettings, useSettingsStore } from "./stores/settingsStore";
import { useTerminalStore } from "./stores/terminalStore";
import { useToastStore } from "./stores/toastStore";

interface DbStatus {
  backend: string;
  healthy: boolean;
  error: string | null;
  pgUser: string | null;
}

function App(): React.JSX.Element {
  const { t } = useTranslation();
  const uiScale = useSettingsStore((s) => s.uiScale);
  const sidebarWidth = useSettingsStore((s) => s.sidebarWidth);
  const setSidebarWidth = useSettingsStore((s) => s.setSidebarWidth);
  useTheme();
  const { confirming, quitting, confirmQuit, cancelQuit } = useAppQuit();
  const aliveTabCount = useTerminalStore((s) => s.tabs.filter((tb) => !tb.exited).length);
  const [dbStatus, setDbStatus] = useState<DbStatus | null>(null);
  const [showSettingsFromBanner, setShowSettingsFromBanner] = useState(false);
  const dragging = useRef(false);

  // The connection settings live in the backend. Hand over any that an older
  // version kept in localStorage, then show the backend's. Failures are
  // shown, and an older "on" for the private-address check that did not
  // reach the backend gets a notice of its own.
  useEffect(() => {
    // StrictMode runs this twice in development; only the second run reports.
    let active = true;
    let dismissNotice: (() => void) | undefined;
    void startUpConnectionSettings().then(({ errors, privateIpCheckOff }) => {
      if (!active) return;
      const { addToast } = useToastStore.getState();
      for (const err of errors) addToast(String(err), "error");
      if (privateIpCheckOff) dismissNotice = showPrivateIpCheckOffNotice();
    });
    return () => {
      active = false;
      // A remount shows it again if it still applies.
      dismissNotice?.();
    };
  }, []);

  // Check DB status on mount and populate app store
  useEffect(() => {
    invoke<DbStatus>("db_get_status")
      .then((status) => {
        if (!status.healthy) {
          setDbStatus(status);
        }
        const { setDbBackend, setPgUser, loadUserIdent } = useAppStore.getState();
        const backend = status.backend === "postgres" ? "postgres" : "sqlite";
        setDbBackend(backend);
        setPgUser(status.pgUser ?? null);
        if (backend === "postgres" && status.healthy) {
          void loadUserIdent();
        }
      })
      .catch(() => {
        // Status check failed — ignore
      });
  }, []);

  // Show user identity prompt in PG mode when not yet configured
  const dbBackend = useAppStore((s) => s.dbBackend);
  const userIdent = useAppStore((s) => s.userIdent);
  const [showIdentPrompt, setShowIdentPrompt] = useState(false);

  useEffect(() => {
    // Wait until DB status has been fetched and userIdent loaded
    if (dbBackend !== "postgres" || dbStatus !== null) return;
    // PG mode, healthy DB — check if we need the prompt
    if (userIdent === null) {
      const timer = setTimeout(() => {
        const currentIdent = useAppStore.getState().userIdent;
        if (currentIdent === null || currentIdent === "") {
          setShowIdentPrompt(true);
        }
      }, 300);
      return () => {
        clearTimeout(timer);
      };
    }
  }, [dbBackend, userIdent, dbStatus]);

  const handleMouseDown = useCallback(() => {
    dragging.current = true;
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";

    const handleMouseMove = (e: MouseEvent): void => {
      if (!dragging.current) return;
      const zoomFactor = uiScale / 100;
      const newWidth = Math.max(160, Math.min(600, e.clientX / zoomFactor));
      setSidebarWidth(newWidth);
    };

    const handleMouseUp = (): void => {
      dragging.current = false;
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
      document.removeEventListener("mousemove", handleMouseMove);
      document.removeEventListener("mouseup", handleMouseUp);
    };

    document.addEventListener("mousemove", handleMouseMove);
    document.addEventListener("mouseup", handleMouseUp);
  }, [setSidebarWidth, uiScale]);

  return (
    <div className="app" style={{ "--ui-zoom": uiScale / 100 } as React.CSSProperties}>
      <ToastContainer />
      <UpdateDialog />
      {dbStatus !== null && !dbStatus.healthy && dbStatus.error !== null && (
        <DatabaseStatusBanner
          error={dbStatus.error}
          onOpenSettings={() => {
            setShowSettingsFromBanner(true);
          }}
        />
      )}
      <div className="app-layout">
        <div
          className="app-sidebar"
          style={{ width: `${String(sidebarWidth)}px`, zoom: uiScale / 100 }}
        >
          <SessionSidebar />
        </div>
        <div
          className="app-divider"
          onMouseDown={handleMouseDown}
          role="separator"
          aria-orientation="vertical"
          tabIndex={0}
        />
        <div className="app-main">
          <TerminalTabs uiScale={uiScale} />
        </div>
      </div>
      {confirming && (
        <ConfirmDialog
          message={t("settings.quitConfirmMessage", { count: String(aliveTabCount) })}
          confirmLabel={t("settings.quit")}
          onConfirm={confirmQuit}
          onCancel={cancelQuit}
        />
      )}
      {showSettingsFromBanner && (
        <SettingsDialog
          onClose={() => {
            setShowSettingsFromBanner(false);
          }}
        />
      )}
      {showIdentPrompt && (
        <UserIdentDialog
          onDone={() => {
            setShowIdentPrompt(false);
          }}
        />
      )}
      {quitting && <QuittingOverlay />}
    </div>
  );
}

export default App;
