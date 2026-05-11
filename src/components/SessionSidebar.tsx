import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import {
  DndContext,
  DragOverlay,
  PointerSensor,
  KeyboardSensor,
  useSensor,
  useSensors,
  type DragStartEvent,
  type DragEndEvent,
  type Modifier,
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
import { BulkEditDialog } from "./BulkEditDialog";
import { SessionDialog, type SessionFormData } from "./SessionDialog";
import { FolderIcon, SessionIconComponent } from "./SessionIcons";
import { SettingsDialog } from "./SettingsDialog";
import { CredentialManager } from "./CredentialManager";
import { LoginSequenceManager } from "./LoginSequenceManager";
import { FolderLoginSequenceDialog } from "./FolderLoginSequenceDialog";
import { useAppStore } from "../stores/appStore";
import { useSettingsStore } from "../stores/settingsStore";
import { useHighlightStore } from "../stores/highlightStore";
import { useCredentialProfilesStore } from "../stores/credentialProfilesStore";
import { useLoginSequenceStore } from "../stores/loginSequenceStore";

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
    clearSelection,
  } = store;
  const { dbBackend, pgUser } = useAppStore();

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
  const [confirmDialog, setConfirmDialog] = useState<{
    message: string;
    onConfirm: () => void;
  } | null>(null);
  const [showSettings, setShowSettings] = useState(false);
  const [showCredentialManager, setShowCredentialManager] = useState(false);
  const [showLoginSequenceManager, setShowLoginSequenceManager] = useState(false);
  const [credentialFolder, setCredentialFolder] = useState<{
    id: string;
    name: string;
  } | null>(null);
  const [loginSequenceFolder, setLoginSequenceFolder] = useState<{
    id: string;
    name: string;
  } | null>(null);
  const [bulkEditFolder, setBulkEditFolder] = useState<{
    id: string;
    name: string;
  } | null>(null);
  const [activeItem, setActiveItem] = useState<{
    type: "folder" | "session";
    id: string;
  } | null>(null);

  // Track whether a reorder is in progress to avoid double-firing.
  const reordering = useRef(false);
  const treeRef = useRef<HTMLDivElement>(null);

  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 5 } }),
    useSensor(KeyboardSensor),
  );

  const { autoRefreshInterval, uiScale } = useSettingsStore();

  // Compensate for CSS zoom on the sidebar container so dnd-kit positions
  // the drag overlay correctly at non-100% UI scale. Two coupled effects:
  //   - dnd-kit reports draggingNodeRect in screen pixels (post-zoom), so
  //     the overlay is placed too far down/right by rect * (zoom - 1).
  //   - dnd-kit reports the cursor transform pre-multiplied by zoom, so the
  //     overlay translates further than the cursor each frame.
  // The combined correction divides (transform - rect * (zoom - 1)) by zoom,
  // cancelling both the initial-position excess and the movement
  // amplification at once.
  const zoomModifier = useCallback<Modifier>(
    ({ transform, draggingNodeRect }) => {
      const zoom = uiScale / 100;
      const dx = draggingNodeRect ? draggingNodeRect.left * (zoom - 1) : 0;
      const dy = draggingNodeRect ? draggingNodeRect.top * (zoom - 1) : 0;
      return {
        ...transform,
        x: (transform.x - dx) / zoom,
        y: (transform.y - dy) / zoom,
      };
    },
    [uiScale],
  );
  const dndModifiers = useMemo(() => [zoomModifier], [zoomModifier]);

  const loadHighlightProfiles = useHighlightStore((s) => s.loadProfiles);
  const loadCredentialProfiles = useCredentialProfilesStore((s) => s.loadAll);
  const loadLoginSequences = useLoginSequenceStore((s) => s.loadAll);

  useEffect(() => {
    loadAll().catch(noop);
    loadHighlightProfiles().catch(noop);
    loadCredentialProfiles().catch(noop);
    loadLoginSequences().catch(noop);
  }, [loadAll, loadHighlightProfiles, loadCredentialProfiles, loadLoginSequences]);

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
    setSessionDialog({
      mode: "create",
      folderId: session.folder_id,
      initial: {
        folderId: session.folder_id,
        name: session.name + "_copy",
        hostname: session.hostname,
        port: session.port,
        protocol: session.protocol,
        username: session.username,
        tags: tagsToDisplay(session.tags),
        icon: session.icon,
        jumpHostId: session.jump_host_id,
        highlightProfileId: session.highlight_profile_id,
        credentialProfileId: session.credential_profile_id,
        loginSequenceId: session.login_sequence_id,
        legacyAlgorithms: session.legacy_algorithms,
      },
    });
  }, []);

  // Ctrl+D clones the currently selected session.
  const isPgMode = dbBackend === "postgres";
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent): void => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "d") {
        if (selectedItemType !== "session" || !selectedItemId) return;
        const session = sessions.find((s) => s.id === selectedItemId);
        if (!session) return;
        if (isPgMode && session.owner !== pgUser) return;
        e.preventDefault();
        cloneSession(session);
      }
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [selectedItemId, selectedItemType, sessions, cloneSession, isPgMode, pgUser]);

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
    const el = document.querySelector<HTMLElement>(`[data-item-id="${CSS.escape(id)}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }, []);

  // Arrow-key and Enter navigation within the session tree.
  const handleTreeKeyDown = useCallback(
    (e: React.KeyboardEvent): void => {
      // Skip all tree key handling when a modal dialog is open — the dialog
      // captures Enter/Escape via its own hooks but cannot prevent the tree's
      // React onKeyDown from firing first.
      if (confirmDialog || folderDialog || sessionDialog) return;

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

      // F2: rename folder or edit session (owned items only in PG mode)
      if (key === "F2" && selectedItemId) {
        e.preventDefault();
        if (selectedItemType === "folder") {
          const folder = folders.find((f) => f.id === selectedItemId);
          if (isPgMode && folder?.owner !== pgUser) return;
          setFolderDialog({
            mode: "rename",
            parentId: null,
            folderId: selectedItemId,
            initialName: folder?.name,
          });
        } else if (selectedItemType === "session") {
          const session = sessions.find((s) => s.id === selectedItemId);
          if (!session) return;
          if (isPgMode && session.owner !== pgUser) return;
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
              username: session.username,
              tags: tagsToDisplay(session.tags),
              icon: session.icon,
              jumpHostId: session.jump_host_id,
              highlightProfileId: session.highlight_profile_id,
              credentialProfileId: session.credential_profile_id,
              loginSequenceId: session.login_sequence_id,
              legacyAlgorithms: session.legacy_algorithms,
            },
          });
        }
        return;
      }

      // Delete: trigger delete confirmation for selected item (owned items only in PG mode)
      if (key === "Delete" && selectedItemId) {
        e.preventDefault();
        if (selectedItemType === "folder") {
          const folderId = selectedItemId;
          const targetFolder = folders.find((f) => f.id === folderId);
          if (isPgMode && targetFolder?.owner !== pgUser) return;
          const parentId = targetFolder?.parent_id ?? null;
          const childCount = sessions.filter((s) => s.folder_id === folderId).length;
          const msg =
            childCount > 0
              ? t("folder.deleteWithSessions", { count: String(childCount) })
              : t("folder.deleteEmpty");
          setConfirmDialog({
            message: msg,
            onConfirm: () => {
              deleteFolder(folderId)
                .then(() => {
                  if (parentId) {
                    selectItem(parentId, "folder");
                    requestAnimationFrame(() => {
                      scrollItemIntoView(parentId);
                    });
                  } else {
                    clearSelection();
                  }
                  setConfirmDialog(null);
                })
                .catch(noop);
            },
          });
        } else if (selectedItemType === "session") {
          const session = sessions.find((s) => s.id === selectedItemId);
          if (isPgMode && session?.owner !== pgUser) return;
          setConfirmDialog({
            message: t("session.deleteConfirm", { name: session?.name ?? "" }),
            onConfirm: () => {
              const parentFolderId = session?.folder_id;
              deleteSession(selectedItemId)
                .then(() => {
                  if (parentFolderId) {
                    selectItem(parentFolderId, "folder");
                    requestAnimationFrame(() => {
                      scrollItemIntoView(parentFolderId);
                    });
                  } else {
                    clearSelection();
                  }
                  setConfirmDialog(null);
                })
                .catch(noop);
            },
          });
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
      confirmDialog,
      folderDialog,
      sessionDialog,
      deleteFolder,
      deleteSession,
      clearSelection,
      isPgMode,
      pgUser,
      t,
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

  const getActiveItemData = useCallback((): { folder?: Folder; session?: Session } => {
    if (!activeItem) return {};
    if (activeItem.type === "folder") {
      return { folder: folders.find((f) => f.id === activeItem.id) };
    }
    return { session: sessions.find((s) => s.id === activeItem.id) };
  }, [activeItem, folders, sessions]);

  const getContextItems = useCallback((): ContextMenuItem[] => {
    if (!ctx) return [];

    const isPg = dbBackend === "postgres";

    if (ctx.type === "folder") {
      const folder = folders.find((f) => f.id === ctx.id);
      const isOwner = isPg && folder?.owner === pgUser;
      // When isOwner is true, folder is guaranteed to be defined (owner equality
      // requires a non-undefined left side). Cast once for the visibility toggle.
      const ownerFolder = isOwner ? folder : null;
      return [
        ...(!isPg || isOwner
          ? [
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
            ]
          : []),
        ...(!isPg || isOwner
          ? [
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
                label: t("contextMenu.sortByHostname"),
                onClick: () => {
                  store.sortSessionsByHostname(ctx.id).catch(noop);
                },
              },
              {
                label: t("contextMenu.applyCredentialProfile"),
                onClick: () => {
                  setCredentialFolder({ id: ctx.id, name: folder?.name ?? "" });
                },
              },
              {
                label: t("contextMenu.applyLoginSequence"),
                onClick: () => {
                  setLoginSequenceFolder({ id: ctx.id, name: folder?.name ?? "" });
                },
              },
              {
                label: t("contextMenu.bulkEdit"),
                onClick: () => {
                  setBulkEditFolder({ id: ctx.id, name: folder?.name ?? "" });
                },
              },
            ]
          : []),
        ...(!isPg || isOwner
          ? [
              {
                label: t("contextMenu.moveTo"),
                onClick: () => {
                  setMoveTarget({ id: ctx.id, type: "folder" });
                },
              },
            ]
          : []),
        ...(ownerFolder
          ? [
              {
                label:
                  ownerFolder.visibility === "shared"
                    ? t("contextMenu.makePersonal")
                    : t("contextMenu.makeShared"),
                onClick: () => {
                  const toggled = ownerFolder.visibility === "shared" ? "personal" : "shared";
                  invoke("set_visibility", { id: ctx.id, itemType: "folder", visibility: toggled })
                    .then(() => loadAll().catch(noop))
                    .catch((err: unknown) => {
                      useToastStore.getState().addToast(String(err));
                    });
                },
              },
            ]
          : []),
        ...(!isPg || isOwner
          ? [
              {
                label: t("contextMenu.delete"),
                danger: true,
                onClick: () => {
                  const folderId = ctx.id;
                  const parentId = folder?.parent_id ?? null;
                  const childCount = sessions.filter((s) => s.folder_id === folderId).length;
                  const msg =
                    childCount > 0
                      ? t("folder.deleteWithSessions", { count: String(childCount) })
                      : t("folder.deleteEmpty");
                  setConfirmDialog({
                    message: msg,
                    onConfirm: () => {
                      deleteFolder(folderId)
                        .then(() => {
                          if (parentId) {
                            selectItem(parentId, "folder");
                            requestAnimationFrame(() => {
                              scrollItemIntoView(parentId);
                            });
                          } else {
                            clearSelection();
                          }
                          setConfirmDialog(null);
                        })
                        .catch(noop);
                    },
                  });
                },
              },
            ]
          : []),
      ];
    }

    // Session context menu
    const session = sessions.find((s) => s.id === ctx.id);
    const isSessionOwner = isPg && session?.owner === pgUser;
    const ownerSession = isSessionOwner ? session : null;
    return [
      {
        label: t("contextMenu.connect"),
        onClick: () => {
          handleSessionDoubleClick(ctx.id);
        },
      },
      ...(!isPg || isSessionOwner
        ? [
            {
              label: t("contextMenu.edit"),
              onClick: () => {
                if (!session) return;
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
                    username: session.username,
                    tags: tagsToDisplay(session.tags),
                    icon: session.icon,
                    jumpHostId: session.jump_host_id,
                    highlightProfileId: session.highlight_profile_id,
                    credentialProfileId: session.credential_profile_id,
                    loginSequenceId: session.login_sequence_id,
                    legacyAlgorithms: session.legacy_algorithms,
                  },
                });
              },
            },
            {
              label: t("contextMenu.clone"),
              onClick: () => {
                if (session) cloneSession(session);
              },
            },
          ]
        : []),
      ...(!isPg || isSessionOwner
        ? [
            {
              label: t("contextMenu.moveTo"),
              onClick: () => {
                setMoveTarget({ id: ctx.id, type: "session" });
              },
            },
          ]
        : []),
      ...(ownerSession
        ? [
            {
              label:
                ownerSession.visibility === "shared"
                  ? t("contextMenu.makePersonal")
                  : t("contextMenu.makeShared"),
              onClick: () => {
                const toggled = ownerSession.visibility === "shared" ? "personal" : "shared";
                invoke("set_visibility", { id: ctx.id, itemType: "session", visibility: toggled })
                  .then(() => loadAll().catch(noop))
                  .catch((err: unknown) => {
                    useToastStore.getState().addToast(String(err));
                  });
              },
            },
          ]
        : []),
      ...(!isPg || isSessionOwner
        ? [
            {
              label: t("contextMenu.delete"),
              danger: true,
              onClick: () => {
                const sessionId = ctx.id;
                const parentFolderId = session?.folder_id;
                setConfirmDialog({
                  message: t("session.deleteConfirm", { name: session?.name ?? "" }),
                  onConfirm: () => {
                    deleteSession(sessionId)
                      .then(() => {
                        if (parentFolderId) {
                          selectItem(parentFolderId, "folder");
                          requestAnimationFrame(() => {
                            scrollItemIntoView(parentFolderId);
                          });
                        } else {
                          clearSelection();
                        }
                        setConfirmDialog(null);
                      })
                      .catch(noop);
                  },
                });
              },
            },
          ]
        : []),
    ];
  }, [
    ctx,
    dbBackend,
    pgUser,
    folders,
    sessions,
    t,
    store,
    loadAll,
    cloneSession,
    handleSessionDoubleClick,
    scrollItemIntoView,
    selectItem,
    clearSelection,
    deleteFolder,
    deleteSession,
  ]);

  const handleFolderSubmit = useCallback(
    (name: string): void => {
      if (!folderDialog) return;
      if (folderDialog.mode === "create") {
        const parentId = folderDialog.parentId;
        createFolder(name, parentId)
          .then(() => {
            if (parentId) {
              selectItem(parentId, "folder");
              requestAnimationFrame(() => {
                scrollItemIntoView(parentId);
              });
            }
          })
          .catch(noop);
      } else if (folderDialog.folderId) {
        renameFolder(folderDialog.folderId, name).catch(noop);
      }
      setFolderDialog(null);
      treeRef.current?.focus();
    },
    [folderDialog, createFolder, renameFolder, selectItem, scrollItemIntoView],
  );

  const handleSessionSubmit = useCallback(
    (data: SessionFormData): void => {
      if (!sessionDialog) return;
      const tagsJson = data.tags
        ? JSON.stringify(
            data.tags
              .split(",")
              .map((tg) => tg.trim())
              .filter(Boolean),
          )
        : "[]";

      if (sessionDialog.mode === "create") {
        createSession({
          folderId: data.folderId,
          name: data.name,
          hostname: data.hostname,
          port: data.port,
          protocol: data.protocol,
          username: data.username,
          tags: tagsJson,
          icon: data.icon,
          jumpHostId: data.jumpHostId ?? undefined,
          highlightProfileId: data.highlightProfileId ?? undefined,
          credentialProfileId: data.credentialProfileId ?? undefined,
          loginSequenceId: data.loginSequenceId ?? undefined,
          legacyAlgorithms: data.legacyAlgorithms,
        })
          .then(() => {
            selectItem(data.folderId, "folder");
            requestAnimationFrame(() => {
              scrollItemIntoView(data.folderId);
            });
          })
          .catch(noop);
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
            tags: tagsJson,
            icon: data.icon,
            jumpHostId: data.jumpHostId,
            highlightProfileId: data.highlightProfileId,
            credentialProfileId: data.credentialProfileId,
            loginSequenceId: data.loginSequenceId,
            legacyAlgorithms: data.legacyAlgorithms,
          });
          if (data.folderId !== originalFolderId) {
            await moveSession(sid, data.folderId);
          }
        };
        doUpdate().catch(noop);
      }
      setSessionDialog(null);
      treeRef.current?.focus();
    },
    [sessionDialog, createSession, updateSession, moveSession, selectItem, scrollItemIntoView],
  );

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

  // Compute which folders are reachable from root (parent chain resolves to null).
  // Folders whose parent is missing from the visible set are orphaned and should not
  // appear in folder pickers or contain searchable sessions.
  const folderIds = useMemo(() => new Set(folders.map((f) => f.id)), [folders]);
  const reachableFolderIds = useMemo(() => {
    const folderMap = new Map(folders.map((f) => [f.id, f]));
    const reachable = new Set<string>();
    const memo = new Map<string, boolean>();
    const isReachable = (id: string): boolean => {
      const cached = memo.get(id);
      if (cached !== undefined) return cached;
      const folder = folderMap.get(id);
      if (!folder) {
        memo.set(id, false);
        return false;
      }
      if (folder.parent_id === null) {
        memo.set(id, true);
        return true;
      }
      if (!folderMap.has(folder.parent_id)) {
        memo.set(id, false);
        return false;
      }
      // Guard against circular references
      memo.set(id, false);
      const result = isReachable(folder.parent_id);
      memo.set(id, result);
      return result;
    };
    for (const f of folders) {
      if (isReachable(f.id)) reachable.add(f.id);
    }
    return reachable;
  }, [folders]);

  // Filter search results to exclude sessions in unreachable folders (orphaned
  // due to broken parent chains after DB manipulation or multi-user RLS).
  const displaySessions = useMemo(() => {
    if (!searchResults) return sessions;
    return searchResults.filter((s) => reachableFolderIds.has(s.folder_id));
  }, [searchResults, sessions, reachableFolderIds]);

  // Folders eligible for session/move dialogs: owned by current user and tree-reachable.
  const ownedReachableFolders = useMemo(
    () =>
      dbBackend === "postgres"
        ? folders.filter((f) => f.owner === pgUser && reachableFolderIds.has(f.id))
        : folders,
    [folders, dbBackend, pgUser, reachableFolderIds],
  );

  // Orphan shared sessions: sessions whose folder_id doesn't match any loaded folder.
  // In PG mode with RLS, this happens when another user shares a session but its folder
  // is personal and therefore not visible.
  const orphanShared = useMemo(
    () => sessions.filter((s) => s.visibility === "shared" && !folderIds.has(s.folder_id)),
    [sessions, folderIds],
  );
  const showSharedFolder = !searchResults && orphanShared.length > 0;
  const [sharedExpanded, setSharedExpanded] = useState(false);
  // Collapse the shared folder whenever it disappears so that re-appearance
  // starts in the collapsed state rather than reusing stale expansion.
  useEffect(() => {
    if (!showSharedFolder) setSharedExpanded(false);
  }, [showSharedFolder]);

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
                useToastStore.getState().addToast(t("sidebar.createFolderFirst"));
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
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.preventDefault();
              e.stopPropagation();
              clearSearch();
              treeRef.current?.focus();
            }
          }}
        />
        {searchQuery && (
          <button
            type="button"
            className="sidebar-search-clear"
            onClick={() => {
              clearSearch();
            }}
            title={t("sidebar.clearSearch")}
          >
            &times;
          </button>
        )}
      </div>
      <DndContext
        sensors={sensors}
        modifiers={dndModifiers}
        onDragStart={handleDragStart}
        onDragEnd={handleDragEnd}
      >
        <div
          className="sidebar-tree"
          role="tree"
          tabIndex={0}
          onKeyDown={handleTreeKeyDown}
          ref={treeRef}
        >
          {searchResults ? (
            displaySessions.map((s) => (
              <div
                key={s.id}
                data-item-id={s.id}
                className={`tree-item tree-session${selectedItemId === s.id ? " tree-item-selected" : ""}`}
                onClick={() => {
                  selectItem(s.id, "session");
                }}
                onDoubleClick={() => {
                  handleSessionDoubleClick(s.id);
                }}
                onContextMenu={(e) => {
                  e.preventDefault();
                  selectItem(s.id, "session");
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
                <span
                  className={`tree-label${s.visibility === "shared" ? " tree-label-shared" : ""}`}
                >
                  {s.name}
                </span>
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
          {!searchResults && folders.length === 0 && orphanShared.length === 0 && (
            <div className="sidebar-empty">{t("sidebar.empty")}</div>
          )}
          {showSharedFolder && (
            <div role="group">
              <div
                className="tree-item tree-folder"
                style={{ "--tree-depth": 0 } as React.CSSProperties}
                onDoubleClick={() => {
                  setSharedExpanded((v) => !v);
                }}
                role="treeitem"
                aria-expanded={sharedExpanded ? "true" : "false"}
                tabIndex={-1}
              >
                <span
                  className="tree-chevron"
                  onClick={(e) => {
                    e.stopPropagation();
                    setSharedExpanded((v) => !v);
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      e.stopPropagation();
                      setSharedExpanded((v) => !v);
                    }
                  }}
                  role="button"
                  tabIndex={0}
                  aria-label={sharedExpanded ? "Collapse" : "Expand"}
                >
                  {sharedExpanded ? "\u25BE" : "\u25B8"}
                </span>
                <span className="tree-icon">{"\uD83C\uDF10"}</span>
                <span className="tree-label">{t("sidebar.sharedFolder")}</span>
              </div>
              {sharedExpanded &&
                orphanShared.map((s) => (
                  <div
                    key={s.id}
                    className={`tree-item tree-session ${selectedItemId === s.id ? "tree-item-selected" : ""}`}
                    style={{ "--tree-depth": 1 } as React.CSSProperties}
                    data-item-id={s.id}
                    onClick={() => {
                      selectItem(s.id, "session");
                    }}
                    onDoubleClick={() => {
                      handleSessionDoubleClick(s.id);
                    }}
                    onContextMenu={(e) => {
                      e.preventDefault();
                      selectItem(s.id, "session");
                      handleContextMenu(e, s.id, "session");
                    }}
                    role="treeitem"
                    tabIndex={-1}
                  >
                    <span className="tree-icon">
                      <SessionIconComponent iconKey={s.icon} />
                    </span>
                    <span
                      className="tree-label tree-label-shared"
                      title={`${s.name} (${s.hostname})`}
                    >
                      {s.name}
                    </span>
                    <span className="tree-meta">{s.hostname}</span>
                  </div>
                ))}
            </div>
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
            treeRef.current?.focus();
          }}
        />
      )}

      {sessionDialog && (
        <SessionDialog
          title={
            sessionDialog.mode === "create" ? t("session.dialogCreate") : t("session.dialogEdit")
          }
          folders={ownedReachableFolders}
          sessions={sessions}
          defaultFolderId={sessionDialog.folderId}
          sessionId={sessionDialog.sessionId}
          initial={sessionDialog.initial}
          onSubmit={handleSessionSubmit}
          onCancel={() => {
            setSessionDialog(null);
            treeRef.current?.focus();
          }}
          onManageCredentials={() => {
            setShowCredentialManager(true);
          }}
          onManageLoginSequences={() => {
            setShowLoginSequenceManager(true);
          }}
        />
      )}

      {bulkEditFolder && (
        <BulkEditDialog
          folderName={bulkEditFolder.name}
          jumpHostCandidates={sessions.filter((s) => s.protocol === "ssh")}
          onSubmit={(edit) => {
            store
              .folderBulkEditSessions(bulkEditFolder.id, edit)
              .then((count) => {
                useToastStore
                  .getState()
                  .addToast(t("bulkEdit.success", { count: String(count) }), "info");
              })
              .catch((err: unknown) => {
                const msg = err instanceof Error ? err.message : String(err);
                useToastStore.getState().addToast(msg);
              });
            setBulkEditFolder(null);
          }}
          onCancel={() => {
            setBulkEditFolder(null);
          }}
        />
      )}

      {credentialFolder && (
        <FolderCredentialDialog
          folderName={credentialFolder.name}
          onSubmit={(profileId) => {
            store
              .folderApplyCredentialProfile(credentialFolder.id, profileId)
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
          onManageCredentials={() => {
            setShowCredentialManager(true);
          }}
        />
      )}

      {loginSequenceFolder && (
        <FolderLoginSequenceDialog
          folderName={loginSequenceFolder.name}
          onSubmit={(sequenceId) => {
            store
              .folderApplyLoginSequence(loginSequenceFolder.id, sequenceId)
              .then((count) => {
                useToastStore
                  .getState()
                  .addToast(t("folderLoginSequence.success", { count: String(count) }), "info");
              })
              .catch((err: unknown) => {
                const msg = err instanceof Error ? err.message : String(err);
                useToastStore.getState().addToast(msg);
              });
            setLoginSequenceFolder(null);
          }}
          onCancel={() => {
            setLoginSequenceFolder(null);
          }}
          onManageSequences={() => {
            setShowLoginSequenceManager(true);
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
        <button
          type="button"
          className="sidebar-btn sidebar-btn-settings"
          title={t("credentialProfiles.open")}
          onClick={() => {
            setShowCredentialManager(true);
          }}
        >
          <svg
            width="20"
            height="20"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            <circle cx="8" cy="14" r="4" />
            <path d="M11 11l10-10" />
            <path d="M17 5l3 3" />
            <path d="M14 8l3 3" />
          </svg>
        </button>
        <button
          type="button"
          className="sidebar-btn sidebar-btn-settings"
          title={t("loginSequences.open")}
          onClick={() => {
            setShowLoginSequenceManager(true);
          }}
        >
          <svg
            width="20"
            height="20"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            <rect x="2" y="3" width="20" height="4" rx="1" />
            <rect x="2" y="10" width="20" height="4" rx="1" />
            <rect x="2" y="17" width="20" height="4" rx="1" />
            <circle cx="18" cy="5" r="3" fill="currentColor" />
          </svg>
        </button>
      </div>

      {showSettings && (
        <SettingsDialog
          onClose={() => {
            setShowSettings(false);
          }}
        />
      )}

      {showCredentialManager && (
        <CredentialManager
          onClose={() => {
            setShowCredentialManager(false);
          }}
        />
      )}

      {showLoginSequenceManager && (
        <LoginSequenceManager
          onClose={() => {
            setShowLoginSequenceManager(false);
          }}
        />
      )}

      {moveTarget && (
        <MoveDialog
          folders={ownedReachableFolders}
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
