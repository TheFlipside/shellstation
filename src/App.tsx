import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { getCurrentWindow } from "@tauri-apps/api/window";
import "./App.css";
import { ConfirmDialog } from "./components/ConfirmDialog";
import { SessionSidebar } from "./components/SessionSidebar";
import { TerminalTabs } from "./components/TerminalTabs";
import { useSettingsStore } from "./stores/settingsStore";
import { useTerminalStore } from "./stores/terminalStore";

function App(): React.JSX.Element {
  const { t } = useTranslation();
  const [sidebarWidth, setSidebarWidth] = useState(260);
  const [showQuitConfirm, setShowQuitConfirm] = useState(false);
  const dragging = useRef(false);

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

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent): void => {
      const modKey = e.metaKey || e.ctrlKey;
      if (modKey && e.key.toLowerCase() === "q") {
        e.preventDefault();
        const tabs = useTerminalStore.getState().tabs;
        if (tabs.length === 0) {
          void getCurrentWindow().destroy();
          return;
        }
        const { confirmOnQuit } = useSettingsStore.getState();
        if (confirmOnQuit) {
          setShowQuitConfirm(true);
        } else {
          void getCurrentWindow().destroy();
        }
      }
    };
    document.addEventListener("keydown", handleKeyDown, true);
    return () => {
      document.removeEventListener("keydown", handleKeyDown, true);
    };
  }, []);

  const handleQuitConfirm = useCallback(() => {
    setShowQuitConfirm(false);
    void getCurrentWindow().destroy();
  }, []);

  return (
    <div className="app">
      <div className="app-layout">
        <div className="app-sidebar" style={{ width: `${String(sidebarWidth)}px` }}>
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
          <TerminalTabs />
        </div>
      </div>
      {showQuitConfirm && (
        <ConfirmDialog
          message={t("settings.quitConfirmMessage", {
            count: String(useTerminalStore.getState().tabs.length),
          })}
          confirmLabel={t("settings.quit")}
          onConfirm={handleQuitConfirm}
          onCancel={() => {
            setShowQuitConfirm(false);
          }}
        />
      )}
    </div>
  );
}

export default App;
