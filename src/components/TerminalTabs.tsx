import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useTerminalStore } from "../stores/terminalStore";
import { useSessionStore } from "../stores/sessionStore";
import { Terminal } from "./Terminal";
import { QuickConnect, type QuickConnectParams } from "./QuickConnect";
import { HostVerifyDialog, type HostVerifyRequest } from "./HostVerifyDialog";
import { ConfirmDialog } from "./ConfirmDialog";
import { ContextMenu, type ContextMenuItem } from "./ContextMenu";
import { useSettingsStore } from "../stores/settingsStore";

// eslint-disable-next-line @typescript-eslint/no-empty-function
const noop = (): void => {};

interface TerminalTabsProps {
  uiScale: number;
}

export function TerminalTabs({ uiScale }: TerminalTabsProps): React.JSX.Element {
  const { t } = useTranslation();
  const {
    tabs,
    activeTabId,
    addTab,
    removeTab,
    setActiveTab,
    reorderTabs,
    connectionError,
    setConnectionError,
  } = useTerminalStore();
  const dragIndexRef = useRef<number | null>(null);
  const dragStartXRef = useRef(0);
  const isDraggingRef = useRef(false);
  const tabBarRef = useRef<HTMLDivElement>(null);
  const [dropTargetIndex, setDropTargetIndex] = useState<number | null>(null);

  // Stable insertion-order list of tab IDs for rendering Terminal components.
  // Reordering tabs in the store must NOT reorder DOM nodes, because xterm.js
  // loses its WebGL context when its container is detached/reattached.
  const stableTabIdsRef = useRef<string[]>([]);
  const currentIds = new Set(tabs.map((t) => t.id));
  // Append any newly added tabs.
  for (const tab of tabs) {
    if (!stableTabIdsRef.current.includes(tab.id)) {
      stableTabIdsRef.current.push(tab.id);
    }
  }
  // Remove closed tabs.
  stableTabIdsRef.current = stableTabIdsRef.current.filter((id) => currentIds.has(id));
  const tabById = new Map(tabs.map((t) => [t.id, t]));

  const getTabIndexAtX = useCallback(
    (clientX: number): number => {
      if (!tabBarRef.current) return 0;
      const tabElements = tabBarRef.current.querySelectorAll<HTMLElement>(".tab:not(.tab-new)");
      for (let i = 0; i < tabElements.length; i++) {
        const rect = tabElements[i].getBoundingClientRect();
        if (clientX < rect.left + rect.width / 2) return i;
      }
      return tabs.length - 1;
    },
    [tabs.length],
  );

  const DRAG_THRESHOLD = 8;

  const handlePointerDown = useCallback((e: React.PointerEvent, index: number) => {
    if (e.button !== 0) return;
    if ((e.target as HTMLElement).closest(".tab-close")) return;
    dragIndexRef.current = index;
    dragStartXRef.current = e.clientX;
    isDraggingRef.current = false;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }, []);

  const handlePointerMove = useCallback(
    (e: React.PointerEvent) => {
      if (dragIndexRef.current === null) return;
      if (!isDraggingRef.current) {
        if (Math.abs(e.clientX - dragStartXRef.current) < DRAG_THRESHOLD) return;
        isDraggingRef.current = true;
      }
      setDropTargetIndex(getTabIndexAtX(e.clientX));
    },
    [getTabIndexAtX],
  );

  const handlePointerUp = useCallback(
    (e: React.PointerEvent) => {
      if (dragIndexRef.current === null) return;
      if (isDraggingRef.current) {
        const toIndex = getTabIndexAtX(e.clientX);
        reorderTabs(dragIndexRef.current, toIndex);
      }
      dragIndexRef.current = null;
      isDraggingRef.current = false;
      setDropTargetIndex(null);
    },
    [getTabIndexAtX, reorderTabs],
  );
  const { closeOnDisconnect, openLocalOnStartup, restrictPrivateIps, confirmOnCloseTab } =
    useSettingsStore();
  const [showQuickConnect, setShowQuickConnect] = useState(false);
  const [tabCtx, setTabCtx] = useState<{ x: number; y: number; tabId: string } | null>(null);
  const [confirmClose, setConfirmClose] = useState<{
    message: string;
    confirmLabel: string;
    onConfirm: () => void;
  } | null>(null);
  const [hostVerifyRequest, setHostVerifyRequest] = useState<HostVerifyRequest | null>(null);
  const verifyQueueRef = useRef<HostVerifyRequest[]>([]);

  const createLocalTab = useCallback(async () => {
    const id = await invoke<string>("pty_spawn", { cols: 80, rows: 24 });
    addTab(id, t("terminal.newTab", { count: String(tabs.length + 1) }), "local");
  }, [addTab, tabs.length, t]);

  const handleQuickConnect = useCallback(
    async (params: QuickConnectParams) => {
      setShowQuickConnect(false);
      try {
        if (params.protocol === "telnet") {
          const id = await invoke<string>("telnet_connect", {
            host: params.host,
            port: params.port,
            cols: 80,
            rows: 24,
            restrictPrivateIps: restrictPrivateIps,
          });
          addTab(id, `${params.host}:${String(params.port)}`, "telnet", {
            host: params.host,
            port: params.port,
          });
        } else {
          const id = await invoke<string>("ssh_connect", {
            host: params.host,
            port: params.port,
            username: params.username,
            authMethod: params.authMethod,
            authCredential: params.authCredential,
            cols: 80,
            rows: 24,
            restrictPrivateIps: restrictPrivateIps,
          });
          addTab(id, `${params.username}@${params.host}`, "ssh", {
            host: params.host,
            username: params.username,
          });
        }
      } catch (err: unknown) {
        const message = err instanceof Error ? err.message : String(err);
        setConnectionError(t("terminal.connectionFailed", { message }));
      }
    },
    [addTab, t, restrictPrivateIps, setConnectionError],
  );

  const showNextVerifyRequest = useCallback(() => {
    const next = verifyQueueRef.current.shift();
    setHostVerifyRequest(next ?? null);
  }, []);

  const handleHostVerifyResponse = useCallback(
    (sessionId: string, accept: boolean) => {
      // Close the dialog immediately — don't wait for the backend round-trip.
      showNextVerifyRequest();
      invoke("ssh_host_verify_response", { id: sessionId, accept }).catch(noop);
    },
    [showNextVerifyRequest],
  );

  const destroyTab = useCallback(
    async (id: string) => {
      const tab = tabs.find((tb) => tb.id === id);
      if (tab?.type === "ssh") {
        await invoke("ssh_disconnect", { id }).catch(noop);
      } else if (tab?.type === "telnet") {
        await invoke("telnet_disconnect", { id }).catch(noop);
      } else {
        await invoke("pty_kill", { id }).catch(noop);
      }
      removeTab(id);
    },
    [removeTab, tabs],
  );

  const requestCloseTab = useCallback(
    (id: string) => {
      if (confirmOnCloseTab) {
        setConfirmClose({
          message: t("terminal.closeTabConfirm"),
          confirmLabel: t("terminal.tabContextClose"),
          onConfirm: () => {
            destroyTab(id).catch(noop);
            setConfirmClose(null);
          },
        });
      } else {
        destroyTab(id).catch(noop);
      }
    },
    [confirmOnCloseTab, destroyTab, t],
  );

  const destroyMultipleTabs = useCallback(
    async (ids: string[]) => {
      for (const id of ids) {
        await destroyTab(id);
      }
    },
    [destroyTab],
  );

  const requestCloseMultipleTabs = useCallback(
    (ids: string[]) => {
      if (ids.length === 0) return;
      if (confirmOnCloseTab) {
        setConfirmClose({
          message: t("terminal.closeAllTabsConfirm", { count: String(ids.length) }),
          confirmLabel: t("terminal.tabContextClose"),
          onConfirm: () => {
            destroyMultipleTabs(ids).catch(noop);
            setConfirmClose(null);
          },
        });
      } else {
        destroyMultipleTabs(ids).catch(noop);
      }
    },
    [confirmOnCloseTab, destroyMultipleTabs, t],
  );

  const cloneTab = useCallback(
    (id: string) => {
      const tab = tabs.find((tb) => tb.id === id);
      if (!tab) return;
      if (tab.sessionDbId) {
        useSessionStore
          .getState()
          .connectSession(tab.sessionDbId)
          .catch((err: unknown) => {
            const msg = err instanceof Error ? err.message : String(err);
            setConnectionError(t("terminal.connectionFailed", { message: msg }));
          });
      } else if (tab.type === "local") {
        createLocalTab().catch(noop);
      }
    },
    [tabs, createLocalTab, setConnectionError, t],
  );

  const getTabContextItems = useCallback(
    (tabId: string): ContextMenuItem[] => {
      const index = tabs.findIndex((tb) => tb.id === tabId);
      const tab = tabs[index];
      const items: ContextMenuItem[] = [
        {
          label: t("terminal.tabContextClose"),
          onClick: () => {
            requestCloseTab(tabId);
          },
        },
        {
          label: t("terminal.tabContextCloseOthers"),
          onClick: () => {
            requestCloseMultipleTabs(tabs.filter((tb) => tb.id !== tabId).map((tb) => tb.id));
          },
        },
        {
          label: t("terminal.tabContextCloseRight"),
          onClick: () => {
            requestCloseMultipleTabs(tabs.slice(index + 1).map((tb) => tb.id));
          },
        },
        {
          label: t("terminal.tabContextCloseAll"),
          danger: true,
          onClick: () => {
            requestCloseMultipleTabs(tabs.map((tb) => tb.id));
          },
        },
      ];
      if (tab.sessionDbId || tab.type === "local") {
        items.push({
          label: t("terminal.tabContextClone"),
          onClick: () => {
            cloneTab(tabId);
          },
        });
      }
      return items;
    },
    [tabs, t, requestCloseTab, requestCloseMultipleTabs, cloneTab],
  );

  // Listen for host key verification events from any SSH session.
  // The `cancelled` flag ensures that if React strict-mode unmounts before the
  // async `listen()` resolves, the stale listener is cleaned up immediately
  // instead of leaking (which would cause duplicate events and multi-click bugs).
  useEffect(() => {
    let cancelled = false;
    let unlisten: UnlistenFn | null = null;

    listen<HostVerifyRequest>("ssh-host-verify", (event) => {
      if (cancelled) return;
      setHostVerifyRequest((current) => {
        if (current !== null) {
          // A dialog is already showing — queue this request.
          verifyQueueRef.current.push(event.payload);
          return current;
        }
        return event.payload;
      });
    })
      .then((fn) => {
        if (cancelled) {
          fn(); // Cleanup already ran — unlisten immediately.
        } else {
          unlisten = fn;
        }
      })
      .catch(noop);

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // Open a local terminal on first mount if the setting is enabled.
  const didSpawnRef = useRef(false);
  useEffect(() => {
    if (!didSpawnRef.current && openLocalOnStartup && tabs.length === 0) {
      didSpawnRef.current = true;
      createLocalTab().catch(noop);
    }
  }, []);

  return (
    <div className="terminal-container">
      <div className="tab-bar" ref={tabBarRef} style={{ zoom: uiScale / 100 }}>
        {tabs.map((tab, index) => (
          <button
            key={tab.id}
            className={`tab ${tab.id === activeTabId ? "tab-active" : ""}${dropTargetIndex === index && dragIndexRef.current !== null && dragIndexRef.current !== index ? " tab-drop-target" : ""}`}
            onClick={() => {
              setActiveTab(tab.id);
            }}
            type="button"
            title={tab.title}
            onPointerDown={(e) => {
              handlePointerDown(e, index);
            }}
            onPointerMove={handlePointerMove}
            onPointerUp={handlePointerUp}
            onContextMenu={(e) => {
              e.preventDefault();
              setTabCtx({ x: e.clientX, y: e.clientY, tabId: tab.id });
            }}
          >
            <span className="tab-title">
              {tab.type === "ssh" ? "\u{1F310} " : tab.type === "telnet" ? "\u{1F4E1} " : ""}
              {tab.title}
            </span>
            <span
              className="tab-close"
              onClick={(e) => {
                e.stopPropagation();
                requestCloseTab(tab.id);
              }}
              role="button"
              tabIndex={0}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.stopPropagation();
                  requestCloseTab(tab.id);
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
            createLocalTab().catch(noop);
          }}
          type="button"
          title={t("terminal.newLocalTerminal")}
        >
          +
        </button>
        <button
          className="tab tab-new tab-ssh"
          onClick={() => {
            setShowQuickConnect(true);
          }}
          type="button"
          title={t("terminal.quickConnect")}
        >
          {t("terminal.connect")}
        </button>
      </div>
      {connectionError !== null && (
        <div className="connection-error-banner">
          <span>{connectionError}</span>
          <button
            type="button"
            onClick={() => {
              setConnectionError(null);
            }}
          >
            &times;
          </button>
        </div>
      )}
      <div className="terminal-pane">
        {stableTabIdsRef.current.map((id) => {
          const tab = tabById.get(id);
          if (!tab) return null;
          return (
            <Terminal
              key={tab.id}
              sessionId={tab.id}
              sessionType={tab.type}
              visible={tab.id === activeTabId}
              onExit={
                closeOnDisconnect
                  ? () => {
                      destroyTab(tab.id).catch(noop);
                    }
                  : undefined
              }
            />
          );
        })}
      </div>
      {showQuickConnect && (
        <QuickConnect
          onConnect={(params) => {
            handleQuickConnect(params).catch(noop);
          }}
          onCancel={() => {
            setShowQuickConnect(false);
          }}
        />
      )}
      {hostVerifyRequest && (
        <HostVerifyDialog
          request={hostVerifyRequest}
          onRespond={handleHostVerifyResponse}
        />
      )}
      {tabCtx && (
        <ContextMenu
          x={tabCtx.x}
          y={tabCtx.y}
          items={getTabContextItems(tabCtx.tabId)}
          onClose={() => {
            setTabCtx(null);
          }}
        />
      )}
      {confirmClose && (
        <ConfirmDialog
          message={confirmClose.message}
          confirmLabel={confirmClose.confirmLabel}
          onConfirm={confirmClose.onConfirm}
          onCancel={() => {
            setConfirmClose(null);
          }}
        />
      )}
    </div>
  );
}
