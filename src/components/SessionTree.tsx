import React from "react";
import type { Folder, Session } from "../stores/sessionStore";
import { useSessionStore } from "../stores/sessionStore";

interface SessionTreeProps {
  parentId: string | null;
  depth: number;
  folders: Folder[];
  sessions: Session[];
  onContextMenu: (e: React.MouseEvent, id: string, type: "folder" | "session") => void;
  onSessionDoubleClick: (id: string) => void;
}

export function SessionTree({
  parentId,
  depth,
  folders,
  sessions,
  onContextMenu,
  onSessionDoubleClick,
}: SessionTreeProps): React.JSX.Element {
  const { expandedFolderIds, toggleFolder, selectedItemId, selectItem } = useSessionStore();

  const childFolders = folders.filter((f) => f.parent_id === parentId);
  const childSessions = sessions.filter((s) => s.folder_id === parentId);

  return (
    <>
      {childFolders.map((folder) => {
        const isExpanded = expandedFolderIds.has(folder.id);
        const isSelected = selectedItemId === folder.id;

        return (
          <div key={folder.id}>
            <div
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
              <span className="tree-icon">{"\uD83D\uDCC1"}</span>
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
      })}
      {childSessions.map((session) => {
        const isSelected = selectedItemId === session.id;

        return (
          <div
            key={session.id}
            className={`tree-item tree-session ${isSelected ? "tree-item-selected" : ""}`}
            style={{ paddingLeft: `${String(depth * 16 + 8)}px` }}
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
            <span className="tree-icon">{"\uD83D\uDDA5\uFE0F"}</span>
            <span className="tree-label">{session.name}</span>
            <span className="tree-meta">{session.hostname}</span>
          </div>
        );
      })}
    </>
  );
}
