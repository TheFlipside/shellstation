import React from "react";
import { SortableContext, useSortable, verticalListSortingStrategy } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import type { Folder, Session } from "../stores/sessionStore";
import { useSessionStore } from "../stores/sessionStore";
import { FolderIcon, SessionIconComponent } from "./SessionIcons";

interface SessionTreeProps {
  parentId: string | null;
  depth: number;
  folders: Folder[];
  sessions: Session[];
  onContextMenu: (e: React.MouseEvent, id: string, type: "folder" | "session") => void;
  onSessionDoubleClick: (id: string) => void;
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

function SortableFolder({
  folder,
  depth,
  folders,
  sessions,
  onContextMenu,
  onSessionDoubleClick,
}: {
  folder: Folder;
  depth: number;
  folders: Folder[];
  sessions: Session[];
  onContextMenu: SessionTreeProps["onContextMenu"];
  onSessionDoubleClick: SessionTreeProps["onSessionDoubleClick"];
}): React.JSX.Element {
  const { expandedFolderIds, toggleFolder, selectedItemId, selectItem } = useSessionStore();
  const isExpanded = expandedFolderIds.has(folder.id);
  const isSelected = selectedItemId === folder.id;

  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id: folderDndId(folder.id),
    data: { type: "folder" as const, id: folder.id, parentId: folder.parent_id },
  });

  const style: React.CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition: transition ?? undefined,
    opacity: isDragging ? 0.4 : undefined,
  };

  return (
    <div ref={setNodeRef} style={style}>
      <div
        {...attributes}
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
          toggleFolder(folder.id);
        }}
        onContextMenu={(e) => {
          e.preventDefault();
          selectItem(folder.id, "folder");
          onContextMenu(e, folder.id, "folder");
        }}
        role="treeitem"
        aria-expanded={isExpanded ? "true" : "false"}
        tabIndex={-1}
      >
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
        <span className="tree-icon">
          <FolderIcon />
        </span>
        <span className="tree-label" title={folder.name}>
          {folder.name}
        </span>
      </div>
      {isExpanded && (
        <SessionTree
          parentId={folder.id}
          depth={depth + 1}
          folders={folders}
          sessions={sessions}
          onContextMenu={onContextMenu}
          onSessionDoubleClick={onSessionDoubleClick}
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
}: {
  session: Session;
  depth: number;
  onContextMenu: SessionTreeProps["onContextMenu"];
  onSessionDoubleClick: SessionTreeProps["onSessionDoubleClick"];
}): React.JSX.Element {
  const { selectedItemId, selectItem } = useSessionStore();
  const isSelected = selectedItemId === session.id;

  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id: sessionDndId(session.id),
    data: { type: "session" as const, id: session.id, folderId: session.folder_id },
  });

  const style: React.CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition: transition ?? undefined,
    opacity: isDragging ? 0.4 : undefined,
  };

  return (
    <div
      ref={setNodeRef}
      {...attributes}
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
      <span className="tree-label" title={`${session.name} (${session.hostname})`}>
        {session.name}
      </span>
      <span className="tree-meta">{session.hostname}</span>
    </div>
  );
}

export function SessionTree({
  parentId,
  depth,
  folders,
  sessions,
  onContextMenu,
  onSessionDoubleClick,
}: SessionTreeProps): React.JSX.Element {
  const childFolders = folders.filter((f) => f.parent_id === parentId);
  const childSessions = sessions.filter((s) => s.folder_id === parentId);

  const folderDndIds = childFolders.map((f) => folderDndId(f.id));
  const sessionDndIds = childSessions.map((s) => sessionDndId(s.id));

  return (
    <div role="group">
      <SortableContext items={folderDndIds} strategy={verticalListSortingStrategy}>
        {childFolders.map((folder) => (
          <SortableFolder
            key={folder.id}
            folder={folder}
            depth={depth}
            folders={folders}
            sessions={sessions}
            onContextMenu={onContextMenu}
            onSessionDoubleClick={onSessionDoubleClick}
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
          />
        ))}
      </SortableContext>
    </div>
  );
}
