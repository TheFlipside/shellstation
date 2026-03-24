import React, { useCallback, useEffect, useState } from "react";
import { useSessionStore } from "../stores/sessionStore";
import { SessionTree } from "./SessionTree";
import { ContextMenu, type ContextMenuItem } from "./ContextMenu";
import { FolderDialog } from "./FolderDialog";
import { SessionDialog, type SessionFormData } from "./SessionDialog";

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
  const store = useSessionStore();
  const {
    folders,
    sessions,
    searchQuery,
    searchResults,
    loadAll,
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
  const [moveTarget, setMoveTarget] = useState<MoveTarget | null>(null);

  useEffect(() => {
    loadAll().catch(noop);
  }, [loadAll]);

  const handleContextMenu = useCallback(
    (e: React.MouseEvent, id: string, type: "folder" | "session") => {
      setCtx({ x: e.clientX, y: e.clientY, id, type });
    },
    [],
  );

  const handleSessionDoubleClick = useCallback(
    (id: string) => {
      connectSession(id).catch((err: unknown) => {
        const msg = err instanceof Error ? err.message : String(err);
        alert(`Connection failed: ${msg}`);
      });
    },
    [connectSession],
  );

  const getContextItems = (): ContextMenuItem[] => {
    if (!ctx) return [];

    if (ctx.type === "folder") {
      const folder = folders.find((f) => f.id === ctx.id);
      return [
        {
          label: "New Session",
          onClick: () => {
            setSessionDialog({ mode: "create", folderId: ctx.id });
          },
        },
        {
          label: "New Subfolder",
          onClick: () => {
            setFolderDialog({ mode: "create", parentId: ctx.id });
          },
        },
        {
          label: "Rename",
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
          label: "Move To\u2026",
          onClick: () => {
            setMoveTarget({ id: ctx.id, type: "folder" });
          },
        },
        {
          label: "Delete",
          danger: true,
          onClick: () => {
            const childCount = sessions.filter((s) => s.folder_id === ctx.id).length;
            const msg =
              childCount > 0
                ? `Delete folder and its ${String(childCount)} session(s)?`
                : "Delete this folder?";
            if (window.confirm(msg)) {
              deleteFolder(ctx.id).catch(noop);
            }
          },
        },
      ];
    }

    // Session context menu
    const session = sessions.find((s) => s.id === ctx.id);
    return [
      {
        label: "Connect",
        onClick: () => {
          handleSessionDoubleClick(ctx.id);
        },
      },
      {
        label: "Edit",
        onClick: () => {
          if (session) {
            setSessionDialog({
              mode: "edit",
              folderId: session.folder_id,
              sessionId: session.id,
              initial: {
                folderId: session.folder_id,
                name: session.name,
                hostname: session.hostname,
                port: session.port,
                username: session.username,
                authMethod: session.auth_method,
                tags: tagsToDisplay(session.tags),
                jumpHostId: session.jump_host_id,
              },
            });
          }
        },
      },
      {
        label: "Move To\u2026",
        onClick: () => {
          setMoveTarget({ id: ctx.id, type: "session" });
        },
      },
      {
        label: "Delete",
        danger: true,
        onClick: () => {
          if (window.confirm(`Delete session "${session?.name ?? ""}"?`)) {
            deleteSession(ctx.id).catch(noop);
          }
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
            .map((t) => t.trim())
            .filter(Boolean),
        )
      : "[]";

    if (sessionDialog.mode === "create") {
      createSession({
        folderId: data.folderId,
        name: data.name,
        hostname: data.hostname,
        port: data.port,
        username: data.username,
        authMethod: data.authMethod,
        tags: tagsJson,
        jumpHostId: data.jumpHostId ?? undefined,
        password: data.password || undefined,
        keyPath: data.keyPath || undefined,
      }).catch(noop);
    } else if (sessionDialog.sessionId) {
      const sid = sessionDialog.sessionId;
      const originalFolderId = sessionDialog.folderId;
      const doUpdate = async (): Promise<void> => {
        await updateSession(sid, {
          name: data.name,
          hostname: data.hostname,
          port: data.port,
          username: data.username,
          authMethod: data.authMethod,
          tags: tagsJson,
          jumpHostId: data.jumpHostId,
          password: data.password || undefined,
          keyPath: data.keyPath || undefined,
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
        <span className="sidebar-title">Sessions</span>
        <div className="sidebar-actions">
          <button
            type="button"
            className="sidebar-btn"
            title="New Folder"
            onClick={() => {
              setFolderDialog({ mode: "create", parentId: null });
            }}
          >
            +F
          </button>
          <button
            type="button"
            className="sidebar-btn"
            title="New Session"
            onClick={() => {
              if (folders.length === 0) {
                alert("Create a folder first.");
                return;
              }
              setSessionDialog({ mode: "create", folderId: folders[0].id });
            }}
          >
            +S
          </button>
        </div>
      </div>
      <div className="sidebar-search">
        <input
          type="text"
          placeholder="Search sessions\u2026"
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
      <div className="sidebar-tree" role="tree">
        {searchResults ? (
          displaySessions.map((s) => (
            <div
              key={s.id}
              className="tree-item tree-session"
              style={{ paddingLeft: "8px" }}
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
              <span className="tree-icon">{"\uD83D\uDDA5\uFE0F"}</span>
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
          <div className="sidebar-empty">No folders yet. Click +F to create one.</div>
        )}
      </div>

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
          title={folderDialog.mode === "create" ? "New Folder" : "Rename Folder"}
          initialName={folderDialog.initialName}
          onSubmit={handleFolderSubmit}
          onCancel={() => {
            setFolderDialog(null);
          }}
        />
      )}

      {sessionDialog && (
        <SessionDialog
          title={sessionDialog.mode === "create" ? "New Session" : "Edit Session"}
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

      {moveTarget && (
        <div
          className="dialog-overlay"
          onClick={() => {
            setMoveTarget(null);
          }}
          role="presentation"
        >
          <div
            className="dialog"
            onClick={(e) => {
              e.stopPropagation();
            }}
            role="dialog"
            aria-modal="true"
            aria-labelledby="mv-title"
          >
            <h3 className="dialog-title" id="mv-title">
              Move To&hellip;
            </h3>
            <div className="dialog-field">
              <label htmlFor="mv-folder">Target Folder</label>
              <select id="mv-folder" defaultValue="">
                {moveTarget.type === "folder" && <option value="__root__">(Root)</option>}
                {folders
                  .filter((f) => f.id !== moveTarget.id)
                  .map((f) => (
                    <option key={f.id} value={f.id}>
                      {f.name}
                    </option>
                  ))}
              </select>
            </div>
            <div className="dialog-actions">
              <button
                type="button"
                className="dialog-btn dialog-btn-cancel"
                onClick={() => {
                  setMoveTarget(null);
                }}
              >
                Cancel
              </button>
              <button
                type="button"
                className="dialog-btn dialog-btn-primary"
                onClick={() => {
                  const sel = document.getElementById("mv-folder") as HTMLSelectElement;
                  if (sel.value) handleMoveSubmit(sel.value);
                }}
              >
                Move
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
