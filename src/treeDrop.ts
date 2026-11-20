/**
 * Where an item of the session tree goes when it is dropped.
 *
 * A drag ends on a row of the tree or on the empty part below the rows.
 * `dropPointAt` says which, from where the pointer is, and `dropAt` says what
 * a drop there does, or `null` where it does nothing: into a folder, or
 * between two of its siblings. The rules are here and not in the sidebar so
 * that they can be tested without a layout.
 *
 * The backend decides in the end (`folder_move`, `session_move` and the two
 * reorder commands). What is refused here is what it would refuse, so that
 * the tree marks no place a drop cannot go to.
 */
import type { Folder, Session } from "./stores/sessionStore";
import type { TreeIndex } from "./components/SessionTree";

export interface DragItem {
  type: "folder" | "session";
  id: string;
}

/** What the pointer is over: a row, and how far down it, or no row. */
export type DropPoint =
  | { at: "row"; type: "folder" | "session"; id: string; ratio: number }
  | { at: "empty" };

/** What the tree shows while a drop would do something. */
export type DropMark =
  /** The folder the item goes into. */
  | { kind: "into"; folderId: string }
  /** A line at the edge of the row the item goes before or after. */
  | { kind: "line"; rowId: string; edge: "top" | "bottom" }
  /** The end of the top level. */
  | { kind: "end" };

export interface Drop {
  mark: DropMark;
  item: DragItem;
  /**
   * The folder the item is in afterwards. `null` is the top level, where
   * only folders are.
   */
  parentId: string | null;
  /** Whether that is another folder than the one it is in. */
  moves: boolean;
  /**
   * The items of its kind there, with it among them, in their new order.
   * `null` leaves it where a move puts it, at the end. In PostgreSQL mode
   * other users' items can be among them: the backend gives each of the
   * user's own its place in the list and leaves the others as they are.
   */
  order: string[] | null;
}

/** The tree as a drop is judged by. */
export interface DropTree {
  folders: ReadonlyMap<string, Folder>;
  sessions: ReadonlyMap<string, Session>;
  index: TreeIndex;
  /** The folders that are open. */
  expanded: ReadonlySet<string>;
  /** PostgreSQL mode, where a user moves only what is their own. */
  pgMode: boolean;
  /** The role the app connected as; `null` while it is not known. */
  pgUser: string | null;
}

export function dropTree(
  folders: readonly Folder[],
  sessions: readonly Session[],
  index: TreeIndex,
  expanded: ReadonlySet<string>,
  pgMode: boolean,
  pgUser: string | null,
): DropTree {
  return {
    folders: new Map(folders.map((f) => [f.id, f])),
    sessions: new Map(sessions.map((s) => [s.id, s])),
    index,
    expanded,
    pgMode,
    pgUser,
  };
}

/**
 * The upper and the lower part of a folder's row that put a dragged folder
 * before or after it. The part between puts it into the folder.
 */
const FOLDER_EDGE = 0.25;

/** While the role is not known, nothing is taken for the user's own. */
function owned(item: { owner: string }, tree: DropTree): boolean {
  return !tree.pgMode || item.owner === tree.pgUser;
}

/**
 * Whether something can be put into a folder: it is in the tree, up to the
 * top, and in PostgreSQL mode it and every folder above it are the user's
 * own, as the database asks (`shellstation_own_folder`). `dragged` is the
 * folder being dragged, which goes neither into itself nor below itself.
 */
function takes(folderId: string, tree: DropTree, dragged?: string): boolean {
  const seen = new Set<string>();
  let current: string | null = folderId;
  while (current !== null) {
    // A ring of folders has no top.
    if (current === dragged || seen.has(current)) return false;
    seen.add(current);
    const folder = tree.folders.get(current);
    if (!folder || !owned(folder, tree)) return false;
    current = folder.parent_id;
  }
  return true;
}

function ids(items: readonly { id: string }[] | undefined): string[] {
  return items ? items.map((item) => item.id) : [];
}

/** Whether an open folder lists anything under its row. */
function listsContent(folderId: string, tree: DropTree): boolean {
  if (!tree.expanded.has(folderId)) return false;
  return (
    (tree.index.folders.get(folderId)?.length ?? 0) > 0 ||
    (tree.index.sessions.get(folderId)?.length ?? 0) > 0
  );
}

/** A drop into a folder, at the end of what is in it. */
function into(
  item: DragItem,
  from: string | null,
  folderId: string,
  tree: DropTree,
  dragged?: string,
): Drop | null {
  // It is there already.
  if (from === folderId) return null;
  if (!takes(folderId, tree, dragged)) return null;
  return { mark: { kind: "into", folderId }, item, parentId: folderId, moves: true, order: null };
}

/**
 * A drop before or after `row`, a sibling-to-be of the item's own kind in
 * `parentId`. `siblings` are the items of that kind there, as listed.
 *
 * The row is one of the user's own (the callers see to it). A reorder
 * changes only the user's own rows, so the place beside another user's
 * cannot be promised: where that row is listed is its owner's.
 */
function beside(
  item: DragItem,
  from: string | null,
  parentId: string | null,
  siblings: string[],
  row: string,
  after: boolean,
): Drop | null {
  const others = siblings.filter((id) => id !== item.id);
  const at = others.indexOf(row);
  if (at === -1) return null;
  const place = after ? at + 1 : at;
  const moves = from !== parentId;
  // Where it is already.
  if (!moves && siblings.indexOf(item.id) === place) return null;
  const order = [...others.slice(0, place), item.id, ...others.slice(place)];
  return {
    mark: { kind: "line", rowId: row, edge: after ? "bottom" : "top" },
    item,
    parentId,
    moves,
    order,
  };
}

function dropFolder(dragged: Folder, point: DropPoint, tree: DropTree): Drop | null {
  const item: DragItem = { type: "folder", id: dragged.id };
  const from = dragged.parent_id;

  if (point.at === "empty") {
    // Below the rows: the end of the top level, which is everyone's.
    const siblings = ids(tree.index.folders.get(null));
    if (from !== null) {
      return { mark: { kind: "end" }, item, parentId: null, moves: true, order: null };
    }
    if (siblings[siblings.length - 1] === dragged.id) return null;
    const order = [...siblings.filter((id) => id !== dragged.id), dragged.id];
    return { mark: { kind: "end" }, item, parentId: null, moves: false, order };
  }

  if (point.type === "session") {
    // A session's row is part of what its folder lists.
    const session = tree.sessions.get(point.id);
    return session ? into(item, from, session.folder_id, tree, dragged.id) : null;
  }

  const row = tree.folders.get(point.id);
  if (!row || row.id === dragged.id) return null;
  const before = point.ratio < FOLDER_EDGE;
  // Under the row of an open folder is what it lists, not what follows it.
  const after = point.ratio > 1 - FOLDER_EDGE && !listsContent(row.id, tree);
  if (!before && !after) return into(item, from, row.id, tree, dragged.id);

  if (!owned(row, tree)) return null;
  const parentId = row.parent_id;
  if (parentId !== null && !takes(parentId, tree, dragged.id)) return null;
  return beside(item, from, parentId, ids(tree.index.folders.get(parentId)), row.id, after);
}

function dropSession(dragged: Session, point: DropPoint, tree: DropTree): Drop | null {
  const item: DragItem = { type: "session", id: dragged.id };
  const from = dragged.folder_id;
  // A session is in a folder: the top level takes none.
  if (point.at === "empty") return null;

  if (point.type === "folder") return into(item, from, point.id, tree);

  const row = tree.sessions.get(point.id);
  if (!row || row.id === dragged.id || !owned(row, tree)) return null;
  const folderId = row.folder_id;
  if (!takes(folderId, tree)) return null;
  const siblings = ids(tree.index.sessions.get(folderId));
  return beside(item, from, folderId, siblings, row.id, point.ratio >= 0.5);
}

/**
 * Whether an item of the tree can be dragged: it is there, and in PostgreSQL
 * mode it is the user's own.
 */
export function canDrag(item: DragItem, tree: DropTree): boolean {
  const found = item.type === "folder" ? tree.folders.get(item.id) : tree.sessions.get(item.id);
  return found !== undefined && owned(found, tree);
}

/** What dropping `item` at `point` does, or `null` where it does nothing. */
export function dropAt(item: DragItem, point: DropPoint, tree: DropTree): Drop | null {
  if (!canDrag(item, tree)) return null;
  if (item.type === "folder") {
    const folder = tree.folders.get(item.id);
    return folder ? dropFolder(folder, point, tree) : null;
  }
  const session = tree.sessions.get(item.id);
  return session ? dropSession(session, point, tree) : null;
}

/** Whether two drops are the same, in what they show and in what they do. */
export function sameDrop(a: Drop | null, b: Drop | null): boolean {
  if (a === null || b === null) return a === b;
  if (a.item.id !== b.item.id || a.item.type !== b.item.type) return false;
  if (a.parentId !== b.parentId || a.moves !== b.moves) return false;
  if (a.mark.kind !== b.mark.kind) return false;
  if (a.mark.kind === "into" && b.mark.kind === "into" && a.mark.folderId !== b.mark.folderId) {
    return false;
  }
  if (
    a.mark.kind === "line" &&
    b.mark.kind === "line" &&
    (a.mark.rowId !== b.mark.rowId || a.mark.edge !== b.mark.edge)
  ) {
    return false;
  }
  const orderA = a.order;
  const orderB = b.order;
  if (orderA === null || orderB === null) return orderA === orderB;
  return orderA.length === orderB.length && orderA.every((id, i) => id === orderB[i]);
}

/**
 * How far down a row the pointer is, from 0 at its top to 1 at its bottom.
 *
 * The sidebar is scaled with CSS `zoom`, and engines disagree on what
 * `getBoundingClientRect` returns under it: the box as it is on the screen
 * (Chromium), which is what the pointer's coordinates are in, or the box as
 * laid out, before the zoom (WebKitGTK). Which of the two it is shows in the
 * rectangle's height against the laid-out one, and a laid-out rectangle is
 * scaled to the screen before the pointer is held against it.
 */
function ratioIn(row: HTMLElement, y: number, zoom: number): number {
  const rect = row.getBoundingClientRect();
  if (rect.height <= 0) return 0.5;
  const measured = row.offsetHeight > 0 ? rect.height / row.offsetHeight : zoom;
  const scale = Math.abs(measured - zoom) <= Math.abs(measured - 1) ? 1 : zoom;
  const ratio = (y - rect.top * scale) / (rect.height * scale);
  return Math.min(1, Math.max(0, ratio));
}

/**
 * What of the tree is at a point of the window, or `null` where that is not
 * the tree. The row is the one a click there would hit, so nothing that lies
 * over the tree, such as a dialog, lets a drop through. `zoom` is the
 * sidebar's.
 */
export function dropPointAt(
  x: number,
  y: number,
  tree: HTMLElement,
  zoom: number,
): DropPoint | null {
  const under = document.elementFromPoint(x, y);
  const row = under?.closest<HTMLElement>("[data-item-id]");
  // No row of the tree: only the tree's own box counts as its empty part.
  if (!row || !tree.contains(row)) return under === tree ? { at: "empty" } : null;
  const { itemId, itemType } = row.dataset;
  if (itemId === undefined || (itemType !== "folder" && itemType !== "session")) return null;
  return { at: "row", type: itemType, id: itemId, ratio: ratioIn(row, y, zoom) };
}
