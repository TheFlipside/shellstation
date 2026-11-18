import React, { useMemo } from "react";
import type { Folder, Session } from "../stores/sessionStore";
import { useSessionStore } from "../stores/sessionStore";
import { PATH_SEPARATOR_CHAR, pathNameAsWritten } from "../displayText";
import { SessionTree, type TreeIndex } from "./SessionTree";
import { FolderIcon, SharedFolderIcon, SessionIconComponent } from "./SessionIcons";

/** What stands between the names of a folder's path. */
const PATH_SEPARATOR = ` ${PATH_SEPARATOR_CHAR} `;
/** What stands for the folders of a path that are not shown. */
const PATH_ELLIPSIS = String.fromCodePoint(0x2026);
/**
 * First strong isolate and pop directional isolate. A name between the two
 * keeps its own direction and gives none to what is around it.
 */
const FSI = String.fromCodePoint(0x2068);
const PDI = String.fromCodePoint(0x2069);

/** As many of the folders above a folder as a path has, the nearest ones. */
export const MAX_PATH_FOLDERS = 8;
/** As many of them as are shown beside the folder's name. */
const PATH_FOLDERS_BESIDE = 2;
/** As many characters of a folder's name as a path shows in a tooltip. */
const MAX_PATH_NAME_CHARS = 64;
/** And beside a folder's name, where there is less room. */
const MAX_PATH_NAME_CHARS_BESIDE = 24;

/** The folders above a folder, as far as a result shows them. */
export interface FolderPath {
  /** Their names, the top one first. */
  names: string[];
  /** Whether there are folders above these. */
  more: boolean;
}

/**
 * The names of the folders above `folder`, the top one first. A folder that
 * is found by its name says nothing of where it is, and two can have the same
 * name, so a result shows this beside it.
 *
 * It has the [`MAX_PATH_FOLDERS`] nearest folders and no more. Folders can be
 * nested to any depth, and in PostgreSQL mode another user's shared ones are
 * among them: a search that finds every folder of a long chain would
 * otherwise put together a path as long as the chain for each of them.
 *
 * It ends at a folder that is not among `byId`, and at one it has been
 * through: a loop of folders can be there from before the database refused
 * one.
 */
export function folderPath(folder: Folder, byId: ReadonlyMap<string, Folder>): FolderPath {
  const nearestFirst: string[] = [];
  const seen = new Set<string>([folder.id]);
  let more = false;
  let parentId = folder.parent_id;
  while (parentId !== null && !seen.has(parentId)) {
    const parent = byId.get(parentId);
    if (!parent) break;
    if (nearestFirst.length === MAX_PATH_FOLDERS) {
      more = true;
      break;
    }
    seen.add(parent.id);
    nearestFirst.push(parent.name);
    parentId = parent.parent_id;
  }
  return { names: nearestFirst.reverse(), more };
}

/**
 * Names as the text of a path, after an ellipsis where folders above them are
 * left out.
 *
 * In PostgreSQL mode a name can be another user's, and a path is read for
 * where a folder is. Each name is therefore written as [`pathNameAsWritten`]
 * has it, and stands between isolates: a name in a right-to-left script
 * would otherwise take the separator beside it into its own direction, and
 * two such names would show in the other order, the folder before the one
 * it is in.
 */
function written(names: readonly string[], more: boolean, maxChars: number): string {
  const parts = names.map((name) => FSI + pathNameAsWritten(name, maxChars) + PDI);
  return (more ? [PATH_ELLIPSIS, ...parts] : parts).join(PATH_SEPARATOR);
}

/**
 * A path as a tooltip has it: every folder of `path`, and after them `last`,
 * the folder the path is of, where it is given.
 */
export function pathWritten(path: FolderPath, last?: string): string {
  const names = last === undefined ? path.names : [...path.names, last];
  return written(names, path.more, MAX_PATH_NAME_CHARS);
}

/**
 * A path as it is shown beside a folder that was found: the two folders
 * nearest to it, which are what tells two folders of one name apart most
 * often, with names cut shorter than in a tooltip.
 */
export function pathBeside(path: FolderPath): string {
  const more = path.more || path.names.length > PATH_FOLDERS_BESIDE;
  return written(path.names.slice(-PATH_FOLDERS_BESIDE), more, MAX_PATH_NAME_CHARS_BESIDE);
}

interface SearchResultsProps {
  /** The folders the search found, as far as the tree has them. */
  foundFolders: Folder[];
  /** The sessions the search found. */
  foundSessions: Session[];
  /** Every folder of the tree, for the folders above one that was found. */
  folders: Folder[];
  /** Everything in the tree by its folder: what a folder that was found lists. */
  index: TreeIndex;
  /** The folders that were found and do not list their content just now. */
  closedFolderIds: ReadonlySet<string>;
  onToggleFolder: (id: string) => void;
  onContextMenu: (e: React.MouseEvent, id: string, type: "folder" | "session") => void;
  onSessionDoubleClick: (id: string) => void;
}

/**
 * What the sidebar shows in place of the tree while a search is on: first the
 * folders whose name has the query in it, each with what is in it, and then
 * the sessions that have the query in a field of their own.
 *
 * A folder lists its content as the tree does, with the folders in it open
 * or closed as they are in the tree, and is itself open until it is closed
 * here. A folder in it that was found as well is listed by its name only,
 * since it lists its content as a result of its own. A session can be listed
 * twice, under its folder and as a result of its own.
 */
export function SearchResults({
  foundFolders,
  foundSessions,
  folders,
  index,
  closedFolderIds,
  onToggleFolder,
  onContextMenu,
  onSessionDoubleClick,
}: SearchResultsProps): React.JSX.Element {
  const { selectedItemId, selectItem } = useSessionStore();
  const byId = useMemo(() => new Map(folders.map((f) => [f.id, f])), [folders]);
  const foundIds = useMemo(() => new Set(foundFolders.map((f) => f.id)), [foundFolders]);

  return (
    <>
      {foundFolders.map((folder) => {
        const open = !closedFolderIds.has(folder.id);
        const path = folderPath(folder, byId);
        return (
          <div key={folder.id}>
            <div
              className={`tree-item tree-folder${selectedItemId === folder.id ? " tree-item-selected" : ""}`}
              style={{ "--tree-depth": 0 } as React.CSSProperties}
              data-item-id={folder.id}
              onClick={() => {
                selectItem(folder.id, "folder");
              }}
              onMouseDown={(e) => {
                if (e.detail > 1) e.preventDefault();
              }}
              onDoubleClick={() => {
                onToggleFolder(folder.id);
              }}
              onContextMenu={(e) => {
                e.preventDefault();
                selectItem(folder.id, "folder");
                onContextMenu(e, folder.id, "folder");
              }}
              role="treeitem"
              aria-expanded={open ? "true" : "false"}
              tabIndex={-1}
            >
              <span
                className="tree-chevron"
                onClick={(e) => {
                  e.stopPropagation();
                  onToggleFolder(folder.id);
                }}
                onMouseDown={(e) => {
                  e.stopPropagation();
                }}
                role="button"
                aria-label={open ? "Collapse" : "Expand"}
              >
                {String.fromCodePoint(open ? 0x25be : 0x25b8)}
              </span>
              <span
                className={`tree-icon${folder.visibility === "shared" ? " tree-icon-shared" : ""}`}
              >
                {folder.visibility === "shared" ? <SharedFolderIcon /> : <FolderIcon />}
              </span>
              <span className="tree-label" title={pathWritten(path, folder.name)}>
                {folder.name}
              </span>
              {path.names.length > 0 && (
                <span className="tree-meta tree-path" dir="ltr" title={pathWritten(path)}>
                  {pathBeside(path)}
                </span>
              )}
            </div>
            {open && (
              <SessionTree
                parentId={folder.id}
                depth={1}
                index={index}
                onContextMenu={onContextMenu}
                onSessionDoubleClick={onSessionDoubleClick}
                result={{ of: folder.id, found: foundIds }}
              />
            )}
          </div>
        );
      })}
      {foundSessions.map((s) => (
        <div
          key={s.id}
          data-item-id={s.id}
          className={`tree-item tree-session${selectedItemId === s.id ? " tree-item-selected" : ""}`}
          onClick={() => {
            selectItem(s.id, "session");
          }}
          onDoubleClick={() => {
            onSessionDoubleClick(s.id);
          }}
          onContextMenu={(e) => {
            e.preventDefault();
            selectItem(s.id, "session");
            onContextMenu(e, s.id, "session");
          }}
          role="treeitem"
          tabIndex={0}
          onKeyDown={(e) => {
            if (e.key !== "Enter") return;
            // Not on to the tree as well, which connects to the selected
            // session on Enter: that is this one after a click on it, and
            // the Enter would open two connections.
            e.stopPropagation();
            onSessionDoubleClick(s.id);
          }}
        >
          <span className="tree-icon">
            <SessionIconComponent iconKey={s.icon} />
          </span>
          <span className={`tree-label${s.visibility === "shared" ? " tree-label-shared" : ""}`}>
            {s.name}
          </span>
          <span className="tree-meta">{s.hostname}</span>
        </div>
      ))}
    </>
  );
}
