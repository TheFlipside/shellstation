import React, { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import {
  DndContext,
  DragOverlay,
  PointerSensor,
  KeyboardSensor,
  useSensor,
  useSensors,
  type DragStartEvent,
  type DragEndEvent,
} from "@dnd-kit/core";
import { useSessionStore } from "../stores/sessionStore";
import type { Folder, Session } from "../stores/sessionStore";
import { useToastStore } from "../stores/toastStore";
import { SessionTree, parseDndId, flattenVisibleItems } from "./SessionTree";
import { ConfirmDialog } from "./ConfirmDialog";
import { ContextMenu, type ContextMenuItem } from "./ContextMenu";
import { FolderDialog } from "./FolderDialog";
import { MoveDialog } from "./MoveDialog";
import { FolderCredentialDialog } from "./FolderCredentialDialog";
import { SessionDialog, type SessionFormData } from "./SessionDialog";
import { FolderIcon, SessionIconComponent } from "./SessionIcons";
import { SettingsDialog } from "./SettingsDialog";
import { useSettingsStore } from "../stores/settingsStore";

// eslint-disable-next-line @typescript-eslint/no-empty-function
const noop = (): void => {};

/** Convert a JSON tags array (e.g. '["prod","eu"]') to a display string ("prod, eu"). */
function tagsToDisplay(json: string): string {
  try {
    const arr: unknown = JSON.parse(json);
    if (Array.isArray(arr)) return (arr as string[]).join(", ");
  } catch {
    /* not valid JSON — return as-is */
  }
  return json;
}

interface ContextState {
  x: number;
  y: number;
  id: string;
  type: "folder" | "session";
}

interface MoveTarget {
  id: string;
  type: "folder" | "session";
}

export function SessionSidebar(): React.JSX.Element {
  const { t } = useTranslation();
  const store = useSessionStore();
  const {
    folders,
    sessions,
    searchQuery,
    searchResults,
    loadAll,
    checkForUpdates,
    createFolder,
    renameFolder,
    deleteFolder,
    createSession,
    updateSession,
    moveSession,
    moveFolder,
    deleteSession,
    searchSessions,
    clearSearch,
    connectSession,
    selectedItemId,
    selectedItemType,
    selectItem,
    expandedFolderIds,
    expandFolder,
    collapseFolder,
    toggleFolder,
  } = store;

  const [ctx, setCtx] = useState<ContextState | null>(null);
  const [folderDialog, setFolderDialog] = useState<{
    mode: "create" | "rename";
    parentId: string | null;
    folderId?: string;
    initialName?: string;
  } | null>(null);
  const [sessionDialog, setSessionDialog] = useState<{
    mode: "create" | "edit";
    folderId: string;
    sessionId?: string;
    initial?: Partial<SessionFormData>;
  } | null>(null);
  const prefillCredRef = useRef<{ password: string; keyPath: string } | null>(null);
  const [moveTarget, setMoveTarget] = useState<MoveTarget | null>(null);
  const [confirmDialog, setConfirmDialog] = useState<{
    message: string;
    onConfirm: () => void;
  } | null>(null);
  const [showSettings, setShowSettings] = useState(false);
  const [credentialFolder, setCredentialFolder] = useState<{
    id: string;
    name: string;
  } | null>(null);
  const [activeItem, setActiveItem] = useState<{
    type: "folder" | "session";
    id: string;
  } | null>(null);

  // Track whether a reorder is in progress to avoid double-firing.
  const reordering = useRef(false);

  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 5 } }),
    useSensor(KeyboardSensor),
  );

  const { autoRefreshInterval } = useSettingsStore();

  useEffect(() => {
    loadAll().catch(noop);
  }, [loadAll]);

  // Auto-refresh polling when interval is set (for multi-user PostgreSQL setups).
  // Polls a lightweight fingerprint; only fetches full data when it changes.
  useEffect(() => {
    if (autoRefreshInterval <= 0) return;
    const id = window.setInterval(() => {
      checkForUpdates().catch(noop);
    }, autoRefreshInterval * 1000);
    return () => {
      window.clearInterval(id);
    };
  }, [autoRefreshInterval, checkForUpdates]);

  /** Clone the given session by opening the SessionDialog in create mode with prefilled data. */
  const cloneSession = useCallback((session: Session) => {
    invoke<{ username: string; secret: string } | null>("credential_get", {
      sessionId: session.id,
    })
      .then((cred) => {
        const secret = cred?.secret ?? "";
        prefillCredRef.current = {
          password: session.auth_method === "password" ? secret : "",
          keyPath: session.auth_method === "publickey" ? secret : "",
        };
        setSessionDialog({
          mode: "create",
          folderId: session.folder_id,
          initial: {
            folderId: session.folder_id,
            name: session.name + "_copy",
            hostname: session.hostname,
            port: session.port,
            protocol: session.protocol,
            username: cred?.username ?? "",
            authMethod: session.auth_method,
            tags: tagsToDisplay(session.tags),
            icon: session.icon,
            jumpHostId: session.jump_host_id,
            password: session.auth_method === "password" ? secret : "",
            keyPath: session.auth_method === "publickey" ? secret : "",
          },
        });
      })
      .catch(noop);
  }, []);

  // Ctrl+D clones the currently selected session.
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent): void => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "d") {
        if (selectedItemType !== "session" || !selectedItemId) return;
        const session = sessions.find((s) => s.id === selectedItemId);
        if (!session) return;
        e.preventDefault();
        cloneSession(session);
      }
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [selectedItemId, selectedItemType, sessions, cloneSession]);

  const handleSessionDoubleClick = useCallback(
    (id: string) => {
      connectSession(id).catch((err: unknown) => {
        const msg = err instanceof Error ? err.message : String(err);
        useToastStore.getState().addToast(t("terminal.connectionFailed", { message: msg }));
      });
    },
    [connectSession, t],
  );

  /** Scroll the newly-selected tree item into view. */
  const scrollItemIntoView = useCallback((id: string): void => {
    const el = document.querySelector<HTMLElement>(`[data-item-id="${id}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }, []);

  // Arrow-key and Enter navigation within the session tree.
  const handleTreeKeyDown = useCallback(
    (e: React.KeyboardEvent): void => {
      const key = e.key;

      // Enter: toggle folder or connect session
      if (key === "Enter") {
        if (selectedItemType === "folder" && selectedItemId) {
          toggleFolder(selectedItemId);
        } else if (selectedItemType === "session" && selectedItemId) {
          handleSessionDoubleClick(selectedItemId);
        }
        return;
      }

      if (!["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"].includes(key)) return;
      e.preventDefault();

      const visible = flattenVisibleItems(folders, sessions, expandedFolderIds);
      if (visible.length === 0) return;

      const currentIdx = selectedItemId
        ? visible.findIndex((item) => item.id === selectedItemId)
        : -1;

      if (key === "ArrowDown") {
        const next = currentIdx < visible.length - 1 ? currentIdx + 1 : 0;
        selectItem(visible[next].id, visible[next].type);
        scrollItemIntoView(visible[next].id);
      } else if (key === "ArrowUp") {
        const next = currentIdx > 0 ? currentIdx - 1 : visible.length - 1;
        selectItem(visible[next].id, visible[next].type);
        scrollItemIntoView(visible[next].id);
      } else if (key === "ArrowRight") {
        if (selectedItemType === "folder" && selectedItemId) {
          expandFolder(selectedItemId);
        }
      } else if (key === "ArrowLeft") {
        if (selectedItemType === "folder" && selectedItemId) {
          if (expandedFolderIds.has(selectedItemId)) {
            collapseFolder(selectedItemId);
          } else {
            // Navigate to parent folder
            const folder = folders.find((f) => f.id === selectedItemId);
            if (folder?.parent_id) {
              selectItem(folder.parent_id, "folder");
              scrollItemIntoView(folder.parent_id);
            }
          }
        } else if (selectedItemType === "session" && selectedItemId) {
          // Navigate to the session's parent folder
          const session = sessions.find((s) => s.id === selectedItemId);
          if (session) {
            selectItem(session.folder_id, "folder");
            scrollItemIntoView(session.folder_id);
          }
        }
      }
    },
    [
      folders,
      sessions,
      expandedFolderIds,
      selectedItemId,
      selectedItemType,
      selectItem,
      expandFolder,
      collapseFolder,
      toggleFolder,
      scrollItemIntoView,
      handleSessionDoubleClick,
    ],
  );

  const handleContextMenu = useCallback(
    (e: React.MouseEvent, id: string, type: "folder" | "session") => {
      setCtx({ x: e.clientX, y: e.clientY, id, type });
    },
    [],
  );

  const handleDragStart = useCallback((event: DragStartEvent): void => {
    const parsed = parseDndId(String(event.active.id));
    if (parsed) setActiveItem(parsed);
  }, []);

  const handleDragEnd = useCallback(
    (event: DragEndEvent): void => {
      setActiveItem(null);
      const { active, over } = event;
      if (!over || active.id === over.id || reordering.current) return;

      const activeParsed = parseDndId(String(active.id));
      const overParsed = parseDndId(String(over.id));
      if (!activeParsed || !overParsed) return;

      // Only allow reordering among same type and same parent.
      if (activeParsed.type !== overParsed.type) return;

      reordering.current = true;

      if (activeParsed.type === "folder") {
        const activeFolder = folders.find((f) => f.id === activeParsed.id);
        const overFolder = folders.find((f) => f.id === overParsed.id);
        if (!activeFolder || !overFolder) {
          reordering.current = false;
          return;
        }
        // Only reorder within same parent.
        if (activeFolder.parent_id !== overFolder.parent_id) {
          reordering.current = false;
          return;
        }
        const parentId = activeFolder.parent_id;
        const siblings = folders.filter((f) => f.parent_id === parentId);
        const ids = siblings.map((f) => f.id);
        const fromIdx = ids.indexOf(activeParsed.id);
        const toIdx = ids.indexOf(overParsed.id);
        if (fromIdx === -1 || toIdx === -1) {
          reordering.current = false;
          return;
        }
        ids.splice(fromIdx, 1);
        ids.splice(toIdx, 0, activeParsed.id);
        store
          .reorderFolders(parentId, ids)
          .catch(noop)
          .finally(() => {
            reordering.current = false;
          });
      } else {
        const activeSession = sessions.find((s) => s.id === activeParsed.id);
        const overSession = sessions.find((s) => s.id === overParsed.id);
        if (!activeSession || !overSession) {
          reordering.current = false;
          return;
        }
        if (activeSession.folder_id !== overSession.folder_id) {
          reordering.current = false;
          return;
        }
        const folderId = activeSession.folder_id;
        const siblings = sessions.filter((s) => s.folder_id === folderId);
        const ids = siblings.map((s) => s.id);
        const fromIdx = ids.indexOf(activeParsed.id);
        const toIdx = ids.indexOf(overParsed.id);
        if (fromIdx === -1 || toIdx === -1) {
          reordering.current = false;
          return;
        }
        ids.splice(fromIdx, 1);
        ids.splice(toIdx, 0, activeParsed.id);
        store
          .reorderSessions(folderId, ids)
          .catch(noop)
          .finally(() => {
            reordering.current = false;
          });
      }
    },
    [folders, sessions, store],
  );

  const handleSortRootAlphabetically = useCallback((): void => {
    store.sortFolderAlphabetically(null).catch(noop);
  }, [store]);

  const getActiveItemData = (): { folder?: Folder; session?: Session } => {
    if (!activeItem) return {};
    if (activeItem.type === "folder") {
      return { folder: folders.find((f) => f.id === activeItem.id) };
    }
    return { session: sessions.find((s) => s.id === activeItem.id) };
  };

  const getContextItems = (): ContextMenuItem[] => {
    if (!ctx) return [];

    if (ctx.type === "folder") {
      const folder = folders.find((f) => f.id === ctx.id);
      return [
        {
          label: t("contextMenu.newSession"),
          onClick: () => {
            setSessionDialog({ mode: "create", folderId: ctx.id });
          },
        },
        {
          label: t("contextMenu.newSubfolder"),
          onClick: () => {
            setFolderDialog({ mode: "create", parentId: ctx.id });
          },
        },
        {
          label: t("contextMenu.rename"),
          onClick: () => {
            setFolderDialog({
              mode: "rename",
              parentId: null,
              folderId: ctx.id,
              initialName: folder?.name,
            });
          },
        },
        {
          label: t("contextMenu.sortAlphabetically"),
          onClick: () => {
            store.sortFolderAlphabetically(ctx.id, true).catch(noop);
          },
        },
        {
          label: t("contextMenu.setCredentials"),
          onClick: () => {
            setCredentialFolder({ id: ctx.id, name: folder?.name ?? "" });
          },
        },
        {
          label: t("contextMenu.moveTo"),
          onClick: () => {
            setMoveTarget({ id: ctx.id, type: "folder" });
          },
        },
        {
          label: t("contextMenu.delete"),
          danger: true,
          onClick: () => {
            const folderId = ctx.id;
            const childCount = sessions.filter((s) => s.folder_id === folderId).length;
            const msg =
              childCount > 0
                ? t("folder.deleteWithSessions", { count: String(childCount) })
                : t("folder.deleteEmpty");
            setConfirmDialog({
              message: msg,
              onConfirm: () => {
                deleteFolder(folderId).catch(noop);
                setConfirmDialog(null);
              },
            });
          },
        },
      ];
    }

    // Session context menu
    const session = sessions.find((s) => s.id === ctx.id);
    return [
      {
        label: t("contextMenu.connect"),
        onClick: () => {
          handleSessionDoubleClick(ctx.id);
        },
      },
      {
        label: t("contextMenu.edit"),
        onClick: () => {
          if (!session) return;
          invoke<{ username: string; secret: string } | null>("credential_get", {
            sessionId: session.id,
          })
            .then((cred) => {
              const secret = cred?.secret ?? "";
              prefillCredRef.current = {
                password: session.auth_method === "password" ? secret : "",
                keyPath: session.auth_method === "publickey" ? secret : "",
              };
              setSessionDialog({
                mode: "edit",
                folderId: session.folder_id,
                sessionId: session.id,
                initial: {
                  folderId: session.folder_id,
                  name: session.name,
                  hostname: session.hostname,
                  port: session.port,
                  protocol: session.protocol,
                  username: cred?.username ?? "",
                  authMethod: session.auth_method,
                  tags: tagsToDisplay(session.tags),
                  icon: session.icon,
                  jumpHostId: session.jump_host_id,
                  password: session.auth_method === "password" ? secret : "",
                  keyPath: session.auth_method === "publickey" ? secret : "",
                },
              });
            })
            .catch(noop);
        },
      },
      {
        label: t("contextMenu.clone"),
        onClick: () => {
          if (session) cloneSession(session);
        },
      },
      {
        label: t("contextMenu.moveTo"),
        onClick: () => {
          setMoveTarget({ id: ctx.id, type: "session" });
        },
      },
      {
        label: t("contextMenu.delete"),
        danger: true,
        onClick: () => {
          const sessionId = ctx.id;
          setConfirmDialog({
            message: t("session.deleteConfirm", { name: session?.name ?? "" }),
            onConfirm: () => {
              deleteSession(sessionId).catch(noop);
              setConfirmDialog(null);
            },
          });
        },
      },
    ];
  };

  const handleFolderSubmit = (name: string): void => {
    if (!folderDialog) return;
    if (folderDialog.mode === "create") {
      createFolder(name, folderDialog.parentId).catch(noop);
    } else if (folderDialog.folderId) {
      renameFolder(folderDialog.folderId, name).catch(noop);
    }
    setFolderDialog(null);
  };

  const handleSessionSubmit = (data: SessionFormData): void => {
    if (!sessionDialog) return;
    const tagsJson = data.tags
      ? JSON.stringify(
          data.tags
            .split(",")
            .map((tg) => tg.trim())
            .filter(Boolean),
        )
      : "[]";

    // Use prefilled credential as fallback when the dialog didn't
    // preserve the password/keyPath (e.g. webview clearing the field).
    const prefill = prefillCredRef.current;
    const effectivePassword =
      (data.password !== "" ? data.password : prefill?.password) ?? undefined;
    const effectiveKeyPath = (data.keyPath !== "" ? data.keyPath : prefill?.keyPath) ?? undefined;
    prefillCredRef.current = null;

    if (sessionDialog.mode === "create") {
      createSession({
        folderId: data.folderId,
        name: data.name,
        hostname: data.hostname,
        port: data.port,
        protocol: data.protocol,
        username: data.username,
        authMethod: data.authMethod,
        tags: tagsJson,
        icon: data.icon,
        jumpHostId: data.jumpHostId ?? undefined,
        password: effectivePassword,
        keyPath: effectiveKeyPath,
      }).catch(noop);
    } else if (sessionDialog.sessionId) {
      const sid = sessionDialog.sessionId;
      const originalFolderId = sessionDialog.folderId;
      const doUpdate = async (): Promise<void> => {
        await updateSession(sid, {
          name: data.name,
          hostname: data.hostname,
          port: data.port,
          protocol: data.protocol,
          username: data.username,
          authMethod: data.authMethod,
          tags: tagsJson,
          icon: data.icon,
          jumpHostId: data.jumpHostId,
          password: effectivePassword,
          keyPath: effectiveKeyPath,
        });
        if (data.folderId !== originalFolderId) {
          await moveSession(sid, data.folderId);
        }
      };
      doUpdate().catch(noop);
    }
    setSessionDialog(null);
  };

  const handleMoveSubmit = (targetFolderId: string): void => {
    if (!moveTarget) return;
    if (moveTarget.type === "session") {
      moveSession(moveTarget.id, targetFolderId).catch(noop);
    } else {
      const newParent = targetFolderId === "__root__" ? null : targetFolderId;
      moveFolder(moveTarget.id, newParent).catch(noop);
    }
    setMoveTarget(null);
  };

  const displaySessions = searchResults ?? sessions;

  return (
    <div className="session-sidebar">
      <div className="sidebar-header">
        <span className="sidebar-title">{t("sidebar.title")}</span>
        <div className="sidebar-actions">
          <button
            type="button"
            className="sidebar-btn"
            title={t("sidebar.newFolder")}
            onClick={() => {
              setFolderDialog({ mode: "create", parentId: null });
            }}
          >
            +F
          </button>
          <button
            type="button"
            className="sidebar-btn"
            title={t("sidebar.newSession")}
            onClick={() => {
              if (folders.length === 0) {
                alert(t("sidebar.createFolderFirst"));
                return;
              }
              let targetFolderId = folders[0].id;
              if (selectedItemId) {
                if (selectedItemType === "folder") {
                  targetFolderId = selectedItemId;
                } else if (selectedItemType === "session") {
                  const sel = sessions.find((s) => s.id === selectedItemId);
                  if (sel) targetFolderId = sel.folder_id;
                }
              }
              setSessionDialog({ mode: "create", folderId: targetFolderId });
            }}
          >
            +S
          </button>
          <button
            type="button"
            className="sidebar-btn"
            title={t("sidebar.sortAlphabetically")}
            onClick={handleSortRootAlphabetically}
          >
            A&#x2193;
          </button>
          <button
            type="button"
            className="sidebar-btn"
            title={t("sidebar.refresh")}
            onClick={() => {
              loadAll().catch(noop);
            }}
          >
            {"\u21BB"}
          </button>
        </div>
      </div>
      <div className="sidebar-search">
        <input
          type="text"
          placeholder={t("sidebar.searchPlaceholder")}
          value={searchQuery}
          onChange={(e) => {
            if (e.target.value) {
              searchSessions(e.target.value).catch(noop);
            } else {
              clearSearch();
            }
          }}
        />
      </div>
      <DndContext sensors={sensors} onDragStart={handleDragStart} onDragEnd={handleDragEnd}>
        <div className="sidebar-tree" role="tree" tabIndex={0} onKeyDown={handleTreeKeyDown}>
          {searchResults ? (
            displaySessions.map((s) => (
              <div
                key={s.id}
                className="tree-item tree-session"
                onDoubleClick={() => {
                  handleSessionDoubleClick(s.id);
                }}
                onContextMenu={(e) => {
                  e.preventDefault();
                  handleContextMenu(e, s.id, "session");
                }}
                role="treeitem"
                tabIndex={0}
                onKeyDown={(e) => {
                  if (e.key === "Enter") handleSessionDoubleClick(s.id);
                }}
              >
                <span className="tree-icon">
                  <SessionIconComponent iconKey={s.icon} />
                </span>
                <span className="tree-label">{s.name}</span>
                <span className="tree-meta">{s.hostname}</span>
              </div>
            ))
          ) : (
            <SessionTree
              parentId={null}
              depth={0}
              folders={folders}
              sessions={sessions}
              onContextMenu={handleContextMenu}
              onSessionDoubleClick={handleSessionDoubleClick}
            />
          )}
          {!searchResults && folders.length === 0 && (
            <div className="sidebar-empty">{t("sidebar.empty")}</div>
          )}
        </div>
        <DragOverlay>
          {(() => {
            const { folder, session } = getActiveItemData();
            if (folder) {
              return (
                <div className="tree-item-drag-overlay">
                  <span className="tree-icon">
                    <FolderIcon />
                  </span>
                  <span className="tree-label">{folder.name}</span>
                </div>
              );
            }
            if (session) {
              return (
                <div className="tree-item-drag-overlay">
                  <span className="tree-icon">
                    <SessionIconComponent iconKey={session.icon} />
                  </span>
                  <span className="tree-label">{session.name}</span>
                </div>
              );
            }
            return null;
          })()}
        </DragOverlay>
      </DndContext>

      {ctx && (
        <ContextMenu
          x={ctx.x}
          y={ctx.y}
          items={getContextItems()}
          onClose={() => {
            setCtx(null);
          }}
        />
      )}

      {folderDialog && (
        <FolderDialog
          title={
            folderDialog.mode === "create" ? t("folder.dialogCreate") : t("folder.dialogRename")
          }
          initialName={folderDialog.initialName}
          onSubmit={handleFolderSubmit}
          onCancel={() => {
            setFolderDialog(null);
          }}
        />
      )}

      {sessionDialog && (
        <SessionDialog
          title={
            sessionDialog.mode === "create" ? t("session.dialogCreate") : t("session.dialogEdit")
          }
          folders={folders}
          sessions={sessions}
          defaultFolderId={sessionDialog.folderId}
          initial={sessionDialog.initial}
          onSubmit={handleSessionSubmit}
          onCancel={() => {
            setSessionDialog(null);
          }}
        />
      )}

      {credentialFolder && (
        <FolderCredentialDialog
          folderId={credentialFolder.id}
          folderName={credentialFolder.name}
          sessions={sessions}
          onSubmit={(username, authMethod, credential, jumpHostId) => {
            store
              .folderApplyCredentials(
                credentialFolder.id,
                username,
                authMethod,
                credential,
                jumpHostId,
              )
              .then((count) => {
                useToastStore
                  .getState()
                  .addToast(t("folderCredential.success", { count: String(count) }), "info");
              })
              .catch((err: unknown) => {
                const msg = err instanceof Error ? err.message : String(err);
                useToastStore.getState().addToast(msg);
              });
            setCredentialFolder(null);
          }}
          onCancel={() => {
            setCredentialFolder(null);
          }}
        />
      )}

      <div className="sidebar-footer">
        <button
          type="button"
          className="sidebar-btn sidebar-btn-settings"
          title={t("settings.title")}
          onClick={() => {
            setShowSettings(true);
          }}
        >
          {"\u2699"}
        </button>
      </div>

      {showSettings && (
        <SettingsDialog
          onClose={() => {
            setShowSettings(false);
          }}
        />
      )}

      {moveTarget && (
        <MoveDialog
          folders={folders}
          excludeId={moveTarget.id}
          showRoot={moveTarget.type === "folder"}
          onSubmit={handleMoveSubmit}
          onCancel={() => {
            setMoveTarget(null);
          }}
        />
      )}

      {confirmDialog && (
        <ConfirmDialog
          message={confirmDialog.message}
          onConfirm={confirmDialog.onConfirm}
          onCancel={() => {
            setConfirmDialog(null);
          }}
        />
      )}
    </div>
  );
}
