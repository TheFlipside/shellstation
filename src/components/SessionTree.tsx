import React from "react";
import { SortableContext, useSortable, verticalListSortingStrategy } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import type { Folder, Session } from "../stores/sessionStore";
import { useSessionStore } from "../stores/sessionStore";
import { FolderIcon, SharedFolderIcon, SessionIconComponent } from "./SessionIcons";

/**
 * The folders and sessions of the tree by the folder they are in, each list
 * in the order the tree shows it. Made once for all of them: a tree that
 * went through every folder and session for each folder it lists took as
 * long as the two multiplied, and a search can list many folders at once.
 */
export interface TreeIndex {
  /** The folders in a folder; under `null`, those at the top. */
  folders: ReadonlyMap<string | null, readonly Folder[]>;
  /** The sessions in a folder. */
  sessions: ReadonlyMap<string, readonly Session[]>;
}

export function indexTree(folders: readonly Folder[], sessions: readonly Session[]): TreeIndex {
  const foldersIn = new Map<string | null, Folder[]>();
  for (const folder of folders) {
    const siblings = foldersIn.get(folder.parent_id);
    if (siblings) siblings.push(folder);
    else foldersIn.set(folder.parent_id, [folder]);
  }
  const sessionsIn = new Map<string, Session[]>();
  for (const session of sessions) {
    const siblings = sessionsIn.get(session.folder_id);
    if (siblings) siblings.push(session);
    else sessionsIn.set(session.folder_id, [session]);
  }
  return { folders: foldersIn, sessions: sessionsIn };
}

const NO_FOLDERS: readonly Folder[] = [];
const NO_SESSIONS: readonly Session[] = [];

/**
 * What a tree is told that lists the content of a folder a search found.
 *
 * An item can be on the page more than once there: a session under its
 * folder and as a result of its own, and a folder that was found under
 * another that was. The items of such a tree are therefore not sortable, and
 * their drag-and-drop IDs get the ID of the folder that was found in front
 * (`of`), so that no two are alike.
 *
 * A folder in it that was found as well (`found`) is listed by its name and
 * is not opened there: it lists its content as a result of its own. Were it
 * opened in both places, a chain of folders that are all found, each open in
 * the tree, would be listed once for every folder of it, the chain's length
 * squared.
 */
export interface TreeResult {
  of: string;
  found: ReadonlySet<string>;
}

interface SessionTreeProps {
  parentId: string | null;
  depth: number;
  index: TreeIndex;
  onContextMenu: (e: React.MouseEvent, id: string, type: "folder" | "session") => void;
  onSessionDoubleClick: (id: string) => void;
  /** Set for a tree that lists what is in a folder a search found. */
  result?: TreeResult;
}

/** Item entry in the flattened visible tree, used for keyboard navigation. */
export interface FlatTreeItem {
  id: string;
  type: "folder" | "session";
}

/**
 * Walk the tree in render order and return visible items as a flat list.
 * Only recurses into expanded folders. Used by the sidebar keyboard
 * navigation handler to map arrow-key presses to the correct next item.
 */
export function flattenVisibleItems(
  folders: Folder[],
  sessions: Session[],
  expandedFolderIds: Set<string>,
  parentId: string | null = null,
): FlatTreeItem[] {
  const result: FlatTreeItem[] = [];
  const childFolders = folders.filter((f) => f.parent_id === parentId);
  const childSessions = sessions.filter((s) => s.folder_id === parentId);

  for (const folder of childFolders) {
    result.push({ id: folder.id, type: "folder" });
    if (expandedFolderIds.has(folder.id)) {
      result.push(...flattenVisibleItems(folders, sessions, expandedFolderIds, folder.id));
    }
  }
  for (const session of childSessions) {
    result.push({ id: session.id, type: "session" });
  }
  return result;
}

/** Prefix helpers to create globally-unique DnD IDs. */
export function folderDndId(id: string): string {
  return `folder-${id}`;
}
export function sessionDndId(id: string): string {
  return `session-${id}`;
}
export function parseDndId(dndId: string): { type: "folder" | "session"; id: string } | null {
  if (dndId.startsWith("folder-")) return { type: "folder", id: dndId.slice(7) };
  if (dndId.startsWith("session-")) return { type: "session", id: dndId.slice(8) };
  return null;
}

/**
 * The drag-and-drop ID of an item in the content of a folder a search found
 * (`resultOf`, that folder's ID), or the item's own in the tree. `parseDndId` reads only the
 * second kind: an item of the first is not dragged.
 */
export function dndIdIn(resultOf: string | undefined, dndId: string): string {
  return resultOf === undefined ? dndId : `result-${resultOf}:${dndId}`;
}

function SortableFolder({
  folder,
  depth,
  index,
  onContextMenu,
  onSessionDoubleClick,
  result,
}: {
  folder: Folder;
  depth: number;
  index: TreeIndex;
  onContextMenu: SessionTreeProps["onContextMenu"];
  onSessionDoubleClick: SessionTreeProps["onSessionDoubleClick"];
  result: SessionTreeProps["result"];
}): React.JSX.Element {
  const { expandedFolderIds, toggleFolder, selectedItemId, selectItem } = useSessionStore();
  // A folder that was found itself has its content as a result of its own.
  const listedApart = result?.found.has(folder.id) === true;
  const isExpanded = !listedApart && expandedFolderIds.has(folder.id);
  const isSelected = selectedItemId === folder.id;

  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id: dndIdIn(result?.of, folderDndId(folder.id)),
    data: { type: "folder" as const, id: folder.id, parentId: folder.parent_id },
    disabled: result !== undefined,
  });

  const style: React.CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition: transition ?? undefined,
    opacity: isDragging ? 0.4 : undefined,
  };

  return (
    <div ref={setNodeRef} style={style}>
      <div
        {...(result === undefined ? attributes : {})}
        {...listeners}
        className={`tree-item tree-folder ${isSelected ? "tree-item-selected" : ""}`}
        style={{ "--tree-depth": depth } as React.CSSProperties}
        data-item-id={folder.id}
        onClick={() => {
          selectItem(folder.id, "folder");
        }}
        onMouseDown={(e) => {
          if (e.detail > 1) e.preventDefault();
        }}
        onDoubleClick={() => {
          if (!listedApart) toggleFolder(folder.id);
        }}
        onContextMenu={(e) => {
          e.preventDefault();
          selectItem(folder.id, "folder");
          onContextMenu(e, folder.id, "folder");
        }}
        role="treeitem"
        aria-expanded={listedApart ? undefined : isExpanded ? "true" : "false"}
        tabIndex={-1}
      >
        {listedApart ? (
          <span className="tree-chevron" aria-hidden="true" />
        ) : (
          <span
            className="tree-chevron"
            onClick={(e) => {
              e.stopPropagation();
              toggleFolder(folder.id);
            }}
            onMouseDown={(e) => {
              e.stopPropagation();
            }}
            onPointerDown={(e) => {
              e.stopPropagation();
            }}
            role="button"
            aria-label={isExpanded ? "Collapse" : "Expand"}
          >
            {isExpanded ? "\u25BE" : "\u25B8"}
          </span>
        )}
        <span className={`tree-icon${folder.visibility === "shared" ? " tree-icon-shared" : ""}`}>
          {folder.visibility === "shared" ? <SharedFolderIcon /> : <FolderIcon />}
        </span>
        <span className="tree-label" title={folder.name}>
          {folder.name}
        </span>
      </div>
      {isExpanded && (
        <SessionTree
          parentId={folder.id}
          depth={depth + 1}
          index={index}
          onContextMenu={onContextMenu}
          onSessionDoubleClick={onSessionDoubleClick}
          result={result}
        />
      )}
    </div>
  );
}

function SortableSession({
  session,
  depth,
  onContextMenu,
  onSessionDoubleClick,
  result,
}: {
  session: Session;
  depth: number;
  onContextMenu: SessionTreeProps["onContextMenu"];
  onSessionDoubleClick: SessionTreeProps["onSessionDoubleClick"];
  result: SessionTreeProps["result"];
}): React.JSX.Element {
  const { selectedItemId, selectItem } = useSessionStore();
  const isSelected = selectedItemId === session.id;

  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id: dndIdIn(result?.of, sessionDndId(session.id)),
    data: { type: "session" as const, id: session.id, folderId: session.folder_id },
    disabled: result !== undefined,
  });

  const style: React.CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition: transition ?? undefined,
    opacity: isDragging ? 0.4 : undefined,
  };

  return (
    <div
      ref={setNodeRef}
      {...(result === undefined ? attributes : {})}
      {...listeners}
      style={{ ...style, "--tree-depth": depth } as React.CSSProperties}
      className={`tree-item tree-session ${isSelected ? "tree-item-selected" : ""}`}
      data-item-id={session.id}
      onClick={() => {
        selectItem(session.id, "session");
      }}
      onMouseDown={(e) => {
        if (e.detail > 1) e.preventDefault();
      }}
      onDoubleClick={() => {
        onSessionDoubleClick(session.id);
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        selectItem(session.id, "session");
        onContextMenu(e, session.id, "session");
      }}
      role="treeitem"
      tabIndex={-1}
    >
      <span className="tree-icon">
        <SessionIconComponent iconKey={session.icon} />
      </span>
      <span
        className={`tree-label${session.visibility === "shared" ? " tree-label-shared" : ""}`}
        title={`${session.name} (${session.hostname})`}
      >
        {session.name}
      </span>
      <span className="tree-meta">{session.hostname}</span>
    </div>
  );
}

export function SessionTree({
  parentId,
  depth,
  index,
  onContextMenu,
  onSessionDoubleClick,
  result,
}: SessionTreeProps): React.JSX.Element {
  const childFolders = index.folders.get(parentId) ?? NO_FOLDERS;
  const childSessions =
    parentId === null ? NO_SESSIONS : (index.sessions.get(parentId) ?? NO_SESSIONS);

  const folderDndIds = childFolders.map((f) => dndIdIn(result?.of, folderDndId(f.id)));
  const sessionDndIds = childSessions.map((s) => dndIdIn(result?.of, sessionDndId(s.id)));

  return (
    <div role="group">
      <SortableContext items={folderDndIds} strategy={verticalListSortingStrategy}>
        {childFolders.map((folder) => (
          <SortableFolder
            key={folder.id}
            folder={folder}
            depth={depth}
            index={index}
            onContextMenu={onContextMenu}
            onSessionDoubleClick={onSessionDoubleClick}
            result={result}
          />
        ))}
      </SortableContext>
      <SortableContext items={sessionDndIds} strategy={verticalListSortingStrategy}>
        {childSessions.map((session) => (
          <SortableSession
            key={session.id}
            session={session}
            depth={depth}
            onContextMenu={onContextMenu}
            onSessionDoubleClick={onSessionDoubleClick}
            result={result}
          />
        ))}
      </SortableContext>
    </div>
  );
}
