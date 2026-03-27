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
        style={{ paddingLeft: `${String(depth * 16 + 8)}px` }}
        onClick={() => {
          toggleFolder(folder.id);
          selectItem(folder.id, "folder");
        }}
        onContextMenu={(e) => {
          e.preventDefault();
          selectItem(folder.id, "folder");
          onContextMenu(e, folder.id, "folder");
        }}
        role="treeitem"
        aria-expanded={isExpanded}
        tabIndex={0}
        onKeyDown={(e) => {
          if (e.key === "Enter") toggleFolder(folder.id);
        }}
      >
        <span className="tree-chevron">{isExpanded ? "\u25BE" : "\u25B8"}</span>
        <span className="tree-icon">
          <FolderIcon />
        </span>
        <span className="tree-label">{folder.name}</span>
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
      style={{ ...style, paddingLeft: `${String(depth * 16 + 8)}px` }}
      className={`tree-item tree-session ${isSelected ? "tree-item-selected" : ""}`}
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
      tabIndex={0}
      onKeyDown={(e) => {
        if (e.key === "Enter") onSessionDoubleClick(session.id);
      }}
    >
      <span className="tree-icon">
        <SessionIconComponent iconKey={session.icon} />
      </span>
      <span className="tree-label">{session.name}</span>
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
    <>
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
    </>
  );
}
