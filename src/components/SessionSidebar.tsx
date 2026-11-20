import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import {
  DndContext,
  PointerSensor,
  useSensor,
  useSensors,
  type DragStartEvent,
} from "@dnd-kit/core";
import { isJumpHostCandidate, jumpHostForCopy, useSessionStore } from "../stores/sessionStore";
import type { Folder, Session } from "../stores/sessionStore";
import { useToastStore } from "../stores/toastStore";
import {
  SessionTree,
  TreeDragContext,
  indexTree,
  parseDndId,
  flattenVisibleItems,
  type TreeDrag,
} from "./SessionTree";
import {
  canDrag,
  dropAt,
  dropPointAt,
  dropTree,
  sameDrop,
  type DragItem,
  type Drop,
} from "../treeDrop";
import { ConfirmDialog } from "./ConfirmDialog";
import { ContextMenu, type ContextMenuItem } from "./ContextMenu";
import { dialogOpen, useOnDialogOpen } from "./DialogKeys";
import { FolderDialog } from "./FolderDialog";
import { MoveDialog } from "./MoveDialog";
import { SearchField } from "./SearchField";
import { SearchResults } from "./SearchResults";
import { FolderCredentialDialog } from "./FolderCredentialDialog";
import { BulkEditDialog } from "./BulkEditDialog";
import { SessionDialog, type SessionFormData } from "./SessionDialog";
import { SessionLoginDialog } from "./SessionLoginDialog";
import { SessionHighlightDialog } from "./SessionHighlightDialog";
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

/**
 * How long a dragged item has to be held over a closed folder before the
 * folder opens, in milliseconds, so that a folder inside it can be reached
 * in the same drag.
 */
export const DRAG_OPEN_MS = 700;

/**
 * Where the dragged item's preview is, from the pointer, in pixels: to the
 * right of it and below, so that it covers neither the row the pointer is on
 * nor the line that says where a drop goes.
 */
const DRAG_PREVIEW_OFFSET = { x: 12, y: 16 };

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
  // Another user's shared session whose login the user is assigning.
  const [loginSession, setLoginSession] = useState<Session | null>(null);
  // Another user's shared session whose highlight profile the user is choosing.
  const [highlightSession, setHighlightSession] = useState<Session | null>(null);
  const [bulkEditFolder, setBulkEditFolder] = useState<{
    id: string;
    name: string;
  } | null>(null);
  // The item being dragged, and what dropping it where the pointer is would
  // do, which is what the tree marks. The ref has the drop that the state
  // was last set to, to tell whether a new one differs.
  const [activeItem, setActiveItem] = useState<DragItem | null>(null);
  const [drop, setDrop] = useState<Drop | null>(null);
  const dropRef = useRef<Drop | null>(null);
  // Where the pointer was last, in the window.
  const pointer = useRef<{ x: number; y: number } | null>(null);
  const previewRef = useRef<HTMLDivElement | null>(null);

  // Whether a drop is being carried out. Nothing is picked up until the tree
  // that comes of it is there: a drop would be judged by the tree before it.
  const [placing, setPlacing] = useState(false);
  const treeRef = useRef<HTMLDivElement>(null);

  // One-shot scroll target. Set by handlers that mutate the tree (delete,
  // create, search-clear) when the target element doesn't exist in the DOM
  // yet. The effect below consumes it on the *next* render — by then the
  // store has flushed and the new tree is committed, so scrollIntoView
  // resolves correctly. Replaces a less reliable `requestAnimationFrame`
  // pattern that could fire before React's render finished.
  const [pendingScrollId, setPendingScrollId] = useState<string | null>(null);
  // Initial capture is intentional. Refs survive StrictMode's mount/remount,
  // so the double-effect run is idempotent: the second run sees the same
  // `searchResults` and short-circuits the `wasFiltered && !isFiltered`
  // condition. Do NOT reset to null here.
  const prevSearchResultsRef = useRef(searchResults);
  // The folders a search found that the user has closed in its results. A
  // folder that was found lists what is in it until then, however it is in
  // the tree, and the tree's own open folders are not changed by it.
  const [closedResultFolders, setClosedResultFolders] = useState<ReadonlySet<string>>(
    () => new Set<string>(),
  );
  const toggleResultFolder = useCallback((id: string): void => {
    setClosedResultFolders((closed) => {
      const next = new Set(closed);
      if (!next.delete(id)) next.add(id);
      return next;
    });
  }, []);
  // The next search starts with every folder it finds open again.
  useEffect(() => {
    if (searchResults === null) {
      setClosedResultFolders((closed) => (closed.size === 0 ? closed : new Set<string>()));
    }
  }, [searchResults]);

  // An item is picked up with the pointer alone. The library's keyboard
  // sensor picks one up on Enter and Space, which are the tree's own keys:
  // Enter on a row that had been clicked connected or opened it and picked
  // it up as well, to be dropped by whatever Enter or Space came next.
  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 5 } }));

  const { autoRefreshInterval, uiScale } = useSettingsStore();
  const zoom = uiScale / 100;

  // The preview is outside the sidebar, which is scaled with CSS `zoom`, and
  // is placed by the pointer's own coordinates, so no scale comes between
  // the two. It is scaled to the sidebar's size by a transform of its own.
  const placePreview = useCallback((): void => {
    const preview = previewRef.current;
    const at = pointer.current;
    if (!preview || !at) return;
    const x = String(at.x + DRAG_PREVIEW_OFFSET.x);
    const y = String(at.y + DRAG_PREVIEW_OFFSET.y);
    preview.style.transform = `translate(${x}px, ${y}px) scale(${String(zoom)})`;
  }, [zoom]);
  const setPreview = useCallback(
    (preview: HTMLDivElement | null): void => {
      previewRef.current = preview;
      placePreview();
    },
    [placePreview],
  );

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
  const isPgMode = dbBackend === "postgres";
  const cloneSession = useCallback(
    (session: Session) => {
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
          jumpHostId: jumpHostForCopy(session, sessions, isPgMode, pgUser),
          highlightProfileId: session.highlight_profile_id,
          credentialProfileId: session.credential_profile_id,
          loginSequenceId: session.login_sequence_id,
          legacyAlgorithms: session.legacy_algorithms,
        },
      });
    },
    [sessions, isPgMode, pgUser],
  );

  // Ctrl+D clones the currently selected session.
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent): void => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "d") {
        // Typed into a dialog, it is not for the tree behind it.
        if (dialogOpen()) return;
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

  // Consume a pending scroll request after the tree has rendered. Used by
  // delete/create handlers and the search-clear restore: when those flows
  // set `pendingScrollId`, this effect fires on the resulting render and
  // scrolls the target into view once the DOM matches.
  // Dep array deliberately omits `folders`/`sessions`/`searchResults` —
  // setPendingScrollId is always called in the same microtask as the store
  // mutation it pairs with, so the underlying data is already committed by
  // the time this effect runs. Adding those deps would cause spurious
  // re-fires (cheap no-ops, but noise).
  useEffect(() => {
    if (!pendingScrollId) return;
    scrollItemIntoView(pendingScrollId);
    setPendingScrollId(null);
  }, [pendingScrollId, scrollItemIntoView]);

  // When the search input is cleared, the tree (which had been replaced by
  // the flat search-results list) remounts at scroll-top zero. Restore the
  // previously-selected item to its visible position so the user doesn't
  // lose their place.
  useEffect(() => {
    const wasFiltered = prevSearchResultsRef.current !== null;
    const isFiltered = searchResults !== null;
    prevSearchResultsRef.current = searchResults;
    if (wasFiltered && !isFiltered && selectedItemId) {
      setPendingScrollId(selectedItemId);
    }
  }, [searchResults, selectedItemId]);

  // Arrow-key and Enter navigation within the session tree.
  const handleTreeKeyDown = useCallback(
    (e: React.KeyboardEvent): void => {
      // Skip all tree key handling when a modal dialog is open — the dialog
      // captures Enter/Escape via its own hooks but cannot prevent the tree's
      // React onKeyDown from firing first.
      if (confirmDialog || folderDialog || sessionDialog || loginSession || highlightSession)
        return;

      const key = e.key;

      // Enter: toggle folder or connect session. A folder that a search
      // found is opened and closed in the results, not in the tree.
      if (key === "Enter") {
        if (selectedItemType === "folder" && selectedItemId) {
          if (searchResults?.folders.some((f) => f.id === selectedItemId)) {
            toggleResultFolder(selectedItemId);
          } else {
            toggleFolder(selectedItemId);
          }
        } else if (selectedItemType === "session" && selectedItemId) {
          handleSessionDoubleClick(selectedItemId);
        }
        return;
      }

      // F2: rename folder or edit session (owned items only in PG mode).
      // For another user's shared session it opens the user's own login.
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
          if (isPgMode && session.owner !== pgUser) {
            // While the role is not known, no session is taken for another
            // user's either.
            if (pgUser !== null) setLoginSession(session);
            return;
          }
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
                    setPendingScrollId(parentId);
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
                    setPendingScrollId(parentFolderId);
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
      searchResults,
      toggleResultFolder,
      scrollItemIntoView,
      handleSessionDoubleClick,
      confirmDialog,
      folderDialog,
      sessionDialog,
      loginSession,
      highlightSession,
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
                            setPendingScrollId(parentId);
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
      // Another user's shared session is not the user's to edit, but the
      // login the user connects to it with is. Not while the role is not
      // known: every session would count as another user's then.
      ...(isPg && pgUser !== null && !isSessionOwner && session
        ? [
            {
              label: t("contextMenu.myLogin"),
              onClick: () => {
                setLoginSession(session);
              },
            },
          ]
        : []),
      // Nor is how its output is highlighted in the user's own window the
      // owner's: highlight profiles are each user's own. For a session of
      // the user's own that is in the session dialog. While the role is not
      // known no session has that dialog, so all of them have this.
      ...(isPg && !isSessionOwner && session
        ? [
            {
              label: t("contextMenu.myHighlighting"),
              onClick: () => {
                setHighlightSession(session);
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
                          setPendingScrollId(parentFolderId);
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
              setPendingScrollId(parentId);
            }
          })
          .catch(noop);
      } else if (folderDialog.folderId) {
        renameFolder(folderDialog.folderId, name).catch(noop);
      }
      setFolderDialog(null);
      treeRef.current?.focus();
    },
    [folderDialog, createFolder, renameFolder, selectItem],
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
            setPendingScrollId(data.folderId);
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
    [sessionDialog, createSession, updateSession, moveSession, selectItem],
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
    return searchResults.sessions.filter((s) => reachableFolderIds.has(s.folder_id));
  }, [searchResults, sessions, reachableFolderIds]);

  // Everything by the folder it is in, for the tree and for what a folder
  // lists that a search found.
  const treeIndex = useMemo(() => indexTree(folders, sessions), [folders, sessions]);

  // ── Drag and drop ──────────────────────────────────────────────────

  const dropData = useMemo(
    () => dropTree(folders, sessions, treeIndex, expandedFolderIds, isPgMode, pgUser),
    [folders, sessions, treeIndex, expandedFolderIds, isPgMode, pgUser],
  );
  const searching = searchResults !== null;

  const endDrag = useCallback((): void => {
    pointer.current = null;
    dropRef.current = null;
    setDrop(null);
    setActiveItem(null);
  }, []);

  const handleDragStart = useCallback((event: DragStartEvent): void => {
    const parsed = parseDndId(String(event.active.id));
    // Nothing is picked up under a dialog, as one that opened between the
    // press and the first move.
    if (!parsed || dialogOpen()) return;
    const from = event.activatorEvent;
    pointer.current = from instanceof MouseEvent ? { x: from.clientX, y: from.clientY } : null;
    setActiveItem(parsed);
  }, []);

  // What a drop of the dragged item would do where the pointer is now. A
  // dialog covers the window, so nothing is under the pointer but the
  // dialog; the question is not left to that alone.
  const dropNow = useCallback((): Drop | null => {
    const at = pointer.current;
    const tree = treeRef.current;
    // The tree is not what a search lists, where nothing is dropped.
    if (!activeItem || !at || !tree || searching || dialogOpen()) return null;
    const point = dropPointAt(at.x, at.y, tree, zoom);
    return point ? dropAt(activeItem, point, dropData) : null;
  }, [activeItem, dropData, searching, zoom]);

  // A dialog that opens while an item is held ends the drag: the release
  // that follows, under the dialog, drops nothing.
  useOnDialogOpen(endDrag);

  const handleDragEnd = useCallback((): void => {
    // Asked again at the release, and not taken from the last move: the
    // tree may have been read again since, and what was under the pointer
    // then may not be now.
    const dropped = dropNow();
    endDrag();
    if (!dropped || placing) return;
    setPlacing(true);
    // Within the chain, so that whatever comes of it ends in the `finally`.
    Promise.resolve()
      .then(() => store.applyDrop(dropped))
      .then(() => {
        // Shown where it is now.
        if (dropped.parentId !== null) expandFolder(dropped.parentId);
        selectItem(dropped.item.id, dropped.item.type);
        setPendingScrollId(dropped.item.id);
      })
      .catch((err: unknown) => {
        const msg = err instanceof Error ? err.message : String(err);
        useToastStore.getState().addToast(msg);
      })
      .finally(() => {
        setPlacing(false);
      });
  }, [dropNow, endDrag, placing, store, expandFolder, selectItem]);

  // While an item is dragged: where the pointer is, and what a drop there
  // would do. The tree scrolls under a pointer that is held still, and it
  // changes, as when a folder opens, so both are a reason to look again.
  useEffect(() => {
    if (!activeItem) return;
    const tree = treeRef.current;
    const judge = (): void => {
      const next = dropNow();
      if (sameDrop(dropRef.current, next)) return;
      dropRef.current = next;
      setDrop(next);
    };
    const handleMove = (e: PointerEvent): void => {
      if (!e.isPrimary) return;
      pointer.current = { x: e.clientX, y: e.clientY };
      placePreview();
      judge();
    };
    document.addEventListener("pointermove", handleMove);
    tree?.addEventListener("scroll", judge, { passive: true });
    judge();
    return () => {
      document.removeEventListener("pointermove", handleMove);
      tree?.removeEventListener("scroll", judge);
    };
  }, [activeItem, dropNow, placePreview]);

  // A closed folder that an item is held over, to go into it, opens. Only
  // that folder's being closed counts, so that a folder opened or closed
  // elsewhere meanwhile does not start the time over.
  const intoFolder = drop?.mark.kind === "into" ? drop.mark.folderId : null;
  const heldOver = intoFolder !== null && !expandedFolderIds.has(intoFolder) ? intoFolder : null;
  useEffect(() => {
    if (heldOver === null) return;
    const timer = window.setTimeout(() => {
      expandFolder(heldOver);
    }, DRAG_OPEN_MS);
    return () => {
      window.clearTimeout(timer);
    };
  }, [heldOver, expandFolder]);

  const dropMark = drop?.mark ?? null;
  const treeDrag = useMemo<TreeDrag>(
    () => ({
      canDrag: (item) => !placing && canDrag(item, dropData),
      dragged: activeItem,
      mark: dropMark,
    }),
    [placing, dropData, activeItem, dropMark],
  );

  // The dragged item, for its preview. One that is gone from the tree while
  // it is held, as after a refresh, has none.
  const dragged = getActiveItemData();

  // The folders a search found, held to the same rule: one that the tree
  // does not show is not listed.
  const foundFolders = useMemo(
    () => (searchResults ? searchResults.folders.filter((f) => reachableFolderIds.has(f.id)) : []),
    [searchResults, reachableFolderIds],
  );

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
        <SearchField
          placeholder={t("sidebar.searchPlaceholder")}
          value={searchQuery}
          onChange={(value) => {
            if (value) {
              searchSessions(value);
            } else {
              clearSearch();
            }
          }}
          onEscape={() => {
            clearSearch();
            treeRef.current?.focus();
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
        onDragStart={handleDragStart}
        onDragEnd={handleDragEnd}
        onDragCancel={endDrag}
      >
        <div
          className={`sidebar-tree${activeItem ? " sidebar-tree-dragging" : ""}${
            dropMark?.kind === "end" ? " sidebar-tree-drop-end" : ""
          }`}
          role="tree"
          tabIndex={0}
          onKeyDown={handleTreeKeyDown}
          ref={treeRef}
        >
          {searchResults ? (
            <SearchResults
              foundFolders={foundFolders}
              foundSessions={displaySessions}
              folders={folders}
              index={treeIndex}
              closedFolderIds={closedResultFolders}
              onToggleFolder={toggleResultFolder}
              onContextMenu={handleContextMenu}
              onSessionDoubleClick={handleSessionDoubleClick}
            />
          ) : (
            <TreeDragContext.Provider value={treeDrag}>
              <SessionTree
                parentId={null}
                depth={0}
                index={treeIndex}
                onContextMenu={handleContextMenu}
                onSessionDoubleClick={handleSessionDoubleClick}
              />
            </TreeDragContext.Provider>
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
      </DndContext>

      {/* The dragged item's preview, beside the pointer. In the body, as the
          context menu is: the sidebar is scaled, and what is fixed inside it
          is placed in its units and not the window's. */}
      {(dragged.folder ?? dragged.session) &&
        createPortal(
          <div ref={setPreview} className="tree-item-drag-overlay" aria-hidden="true">
            <span className="tree-icon">
              {dragged.session ? (
                <SessionIconComponent iconKey={dragged.session.icon} />
              ) : (
                <FolderIcon />
              )}
            </span>
            <span className="tree-label">{(dragged.folder ?? dragged.session)?.name}</span>
          </div>,
          document.body,
        )}

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

      {loginSession && (
        <SessionLoginDialog
          session={loginSession}
          sessions={sessions}
          onClose={() => {
            setLoginSession(null);
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

      {highlightSession && (
        <SessionHighlightDialog
          session={highlightSession}
          onSubmit={(profileId) => {
            store
              .setSessionHighlightProfile(highlightSession.id, profileId)
              .catch((err: unknown) => {
                const msg = err instanceof Error ? err.message : String(err);
                useToastStore.getState().addToast(msg);
              });
            setHighlightSession(null);
            treeRef.current?.focus();
          }}
          onCancel={() => {
            setHighlightSession(null);
            treeRef.current?.focus();
          }}
        />
      )}

      {bulkEditFolder && (
        <BulkEditDialog
          folderName={bulkEditFolder.name}
          jumpHostCandidates={sessions.filter((s) => isJumpHostCandidate(s, isPgMode, pgUser))}
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
