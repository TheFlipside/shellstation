import React, { useCallback, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTerminalStore } from "../stores/terminalStore";
import { Terminal } from "./Terminal";

// eslint-disable-next-line @typescript-eslint/no-empty-function
const noop = (): void => {};

export function TerminalTabs(): React.JSX.Element {
  const { tabs, activeTabId, addTab, removeTab, setActiveTab } = useTerminalStore();

  const createTab = useCallback(async () => {
    const id = await invoke<string>("pty_spawn", { cols: 80, rows: 24 });
    addTab(id, `Terminal ${String(tabs.length + 1)}`);
  }, [addTab, tabs.length]);

  const closeTab = useCallback(
    async (id: string, e: React.MouseEvent) => {
      e.stopPropagation();
      await invoke("pty_kill", { id }).catch(noop);
      removeTab(id);
    },
    [removeTab],
  );

  // Open a terminal on first mount.
  useEffect(() => {
    if (tabs.length === 0) {
      createTab().catch(noop);
    }
  }, []);

  return (
    <div className="terminal-container">
      <div className="tab-bar">
        {tabs.map((tab) => (
          <button
            key={tab.id}
            className={`tab ${tab.id === activeTabId ? "tab-active" : ""}`}
            onClick={() => {
              setActiveTab(tab.id);
            }}
            type="button"
          >
            <span className="tab-title">{tab.title}</span>
            <span
              className="tab-close"
              onClick={(e) => {
                closeTab(tab.id, e).catch(noop);
              }}
              role="button"
              tabIndex={0}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  closeTab(tab.id, e as unknown as React.MouseEvent).catch(noop);
                }
              }}
            >
              &times;
            </span>
          </button>
        ))}
        <button
          className="tab tab-new"
          onClick={() => {
            createTab().catch(noop);
          }}
          type="button"
          title="New terminal"
        >
          +
        </button>
      </div>
      <div className="terminal-pane">
        {tabs.map((tab) => (
          <Terminal key={tab.id} sessionId={tab.id} visible={tab.id === activeTabId} />
        ))}
      </div>
    </div>
  );
}
