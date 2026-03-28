import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import "./App.css";
import { ConfirmDialog } from "./components/ConfirmDialog";
import { DatabaseStatusBanner } from "./components/DatabaseStatusBanner";
import { SessionSidebar } from "./components/SessionSidebar";
import { SettingsDialog } from "./components/SettingsDialog";
import { TerminalTabs } from "./components/TerminalTabs";
import { ToastContainer } from "./components/ToastContainer";
import { useTheme } from "./hooks/useTheme";
import { useSettingsStore } from "./stores/settingsStore";
import { useTerminalStore } from "./stores/terminalStore";

interface DbStatus {
  backend: string;
  healthy: boolean;
  error: string | null;
}

function App(): React.JSX.Element {
  const { t } = useTranslation();
  const uiScale = useSettingsStore((s) => s.uiScale);
  useTheme();
  const [sidebarWidth, setSidebarWidth] = useState(260);
  const [showQuitConfirm, setShowQuitConfirm] = useState(false);
  const [dbStatus, setDbStatus] = useState<DbStatus | null>(null);
  const [showSettingsFromBanner, setShowSettingsFromBanner] = useState(false);
  const dragging = useRef(false);

  // Check DB status on mount
  useEffect(() => {
    invoke<DbStatus>("db_get_status")
      .then((status) => {
        if (!status.healthy) {
          setDbStatus(status);
        }
      })
      .catch(() => {
        // Status check failed — ignore
      });
  }, []);

  const handleMouseDown = useCallback(() => {
    dragging.current = true;
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";

    const handleMouseMove = (e: MouseEvent): void => {
      if (!dragging.current) return;
      const newWidth = Math.max(160, Math.min(600, e.clientX));
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
  }, []);

  const requestQuit = useCallback(() => {
    const tabs = useTerminalStore.getState().tabs;
    const aliveTabs = tabs.filter((t) => !t.exited);
    if (aliveTabs.length === 0) {
      void getCurrentWindow().destroy();
      return;
    }
    const { confirmOnQuit } = useSettingsStore.getState();
    if (confirmOnQuit) {
      setShowQuitConfirm(true);
    } else {
      void getCurrentWindow().destroy();
    }
  }, []);

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

  useEffect(() => {
    const unlisten = getCurrentWindow().onCloseRequested((event) => {
      const tabs = useTerminalStore.getState().tabs;
      const aliveTabs = tabs.filter((t) => !t.exited);
      const { confirmOnQuit } = useSettingsStore.getState();
      if (aliveTabs.length > 0 && confirmOnQuit) {
        event.preventDefault();
        setShowQuitConfirm(true);
      }
    });
    return () => {
      void unlisten.then((fn) => {
        fn();
      });
    };
  }, []);

  const handleQuitConfirm = useCallback(() => {
    setShowQuitConfirm(false);
    void getCurrentWindow().destroy();
  }, []);

  return (
    <div className="app" style={{ "--ui-zoom": uiScale / 100 } as React.CSSProperties}>
      <ToastContainer />
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
      {showQuitConfirm && (
        <ConfirmDialog
          message={t("settings.quitConfirmMessage", {
            count: String(useTerminalStore.getState().tabs.filter((tb) => !tb.exited).length),
          })}
          confirmLabel={t("settings.quit")}
          onConfirm={handleQuitConfirm}
          onCancel={() => {
            setShowQuitConfirm(false);
          }}
        />
      )}
      {showSettingsFromBanner && (
        <SettingsDialog
          onClose={() => {
            setShowSettingsFromBanner(false);
          }}
        />
      )}
    </div>
  );
}

export default App;
