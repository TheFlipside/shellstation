/**
 * What the sidebar lists while a search is on.
 *
 * The search found sessions only, by their own name, host, user name and
 * tags, so a search for a folder's name found nothing of that folder. A
 * folder whose name has the query in it is a result now, and lists what is
 * in it. The contract pinned here: the folders come first, each open with its
 * content as the tree has it and with the folders above it beside its name,
 * the sessions that were found follow, a folder is closed and opened here
 * without a change to the tree's own open folders, and nothing in the results
 * can be dragged, since an item can be listed more than once.
 *
 * In PostgreSQL mode the folders can be another user's, as many and as deep
 * as that user likes. So a path has a few folders and no more, shows a name
 * in a way that cannot be read as another path, and a search lists no more
 * than what it finds and what is in that.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { DndContext } from "@dnd-kit/core";
import { useSessionStore, type Folder, type Session } from "../stores/sessionStore";
import {
  MAX_PATH_FOLDERS,
  SearchResults,
  folderPath,
  pathBeside,
  pathWritten,
} from "./SearchResults";
import {
  SessionTree,
  dndIdIn,
  folderDndId,
  indexTree,
  parseDndId,
  sessionDndId,
} from "./SessionTree";

// The session store imports invoke; nothing here may reach a backend.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const SEP = ` ${String.fromCodePoint(0x203a)} `;
const ELLIPSIS = String.fromCodePoint(0x2026);
/** A name as a path has it: between isolates. */
function iso(name: string): string {
  return String.fromCodePoint(0x2068) + name + String.fromCodePoint(0x2069);
}

function folder(id: string, name: string, parentId: string | null = null): Folder {
  return { id, name, parent_id: parentId, sort_order: 0, owner: "local", visibility: "personal" };
}

function session(id: string, name: string, folderId: string): Session {
  return {
    id,
    folder_id: folderId,
    name,
    hostname: `${name}.example`,
    port: 22,
    protocol: "ssh",
    username: "",
    auth_method: "profile",
    jump_host_id: null,
    tags: "[]",
    icon: "desktop",
    sort_order: 0,
    highlight_profile_id: null,
    credential_profile_id: null,
    login_sequence_id: null,
    legacy_algorithms: false,
    owner: "local",
    visibility: "personal",
  };
}

// Customers, Production in it and EU prod in that, with sessions in the
// lower two.
const customers = folder("f-customers", "Customers");
const production = folder("f-production", "Production", customers.id);
const eu = folder("f-eu", "EU prod", production.id);
const lab = folder("f-lab", "Lab");
const folders = [customers, production, eu, lab];

const web = session("s-web", "web", production.id);
const prodDb = session("s-prod-db", "prod-db", production.id);
const euGw = session("s-eu-gw", "eu-gw", eu.id);
const labProd = session("s-lab-prod", "lab-prod", lab.id);
const sessions = [web, prodDb, euGw, labProd];

function byId(all: Folder[]): Map<string, Folder> {
  return new Map(all.map((f) => [f.id, f]));
}

/** Folders one in the other, the first at the top, each named `name`. */
function chain(length: number, name: string): Folder[] {
  const links: Folder[] = [];
  for (let n = 0; n < length; n++) {
    links.push(folder(`f-chain-${String(n)}`, name, n === 0 ? null : links[n - 1].id));
  }
  return links;
}

const onToggleFolder = vi.fn();
const onContextMenu = vi.fn();
const onSessionDoubleClick = vi.fn();
const onTreeKeyDown = vi.fn();
const onDragStart = vi.fn();

function renderResults(
  foundFolders: Folder[],
  foundSessions: Session[],
  closed: string[] = [],
  allFolders: Folder[] = folders,
): void {
  render(
    <DndContext onDragStart={onDragStart}>
      {/* The sidebar's tree, which has a key handler of its own. */}
      <div role="tree" onKeyDown={onTreeKeyDown}>
        <SearchResults
          foundFolders={foundFolders}
          foundSessions={foundSessions}
          folders={allFolders}
          index={indexTree(allFolders, sessions)}
          closedFolderIds={new Set(closed)}
          onToggleFolder={onToggleFolder}
          onContextMenu={onContextMenu}
          onSessionDoubleClick={onSessionDoubleClick}
        />
      </div>
    </DndContext>,
  );
}

function renderTree(): void {
  render(
    <DndContext onDragStart={onDragStart}>
      <SessionTree
        parentId={null}
        depth={0}
        index={indexTree(folders, sessions)}
        onContextMenu={onContextMenu}
        onSessionDoubleClick={onSessionDoubleClick}
      />
    </DndContext>,
  );
}

/** The rows as listed, each as its indentation and its text. */
function rows(): string[] {
  return screen.getAllByRole("treeitem").map((row) => {
    const depth = row.style.getPropertyValue("--tree-depth") || "0";
    const label = row.querySelector(".tree-label")?.textContent ?? "";
    return `${depth}:${label}`;
  });
}

function copies(id: string): HTMLElement[] {
  return Array.from(document.querySelectorAll<HTMLElement>(`[data-item-id="${id}"]`));
}

function row(id: string): HTMLElement {
  const found = copies(id);
  expect(found).toHaveLength(1);
  return found[0];
}

/** What begins a drag of a sortable item: the press of the primary pointer. */
function press(target: HTMLElement): void {
  fireEvent.pointerDown(target, { isPrimary: true, button: 0, clientX: 5, clientY: 5 });
}

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  useSessionStore.setState({
    folders,
    sessions,
    expandedFolderIds: new Set<string>(),
    selectedItemId: null,
    selectedItemType: null,
  });
});

describe("folderPath", () => {
  it("names the folders above a folder, the top one first", () => {
    expect(folderPath(customers, byId(folders))).toEqual({ names: [], more: false });
    expect(folderPath(production, byId(folders))).toEqual({ names: ["Customers"], more: false });
    expect(folderPath(eu, byId(folders))).toEqual({
      names: ["Customers", "Production"],
      more: false,
    });
  });

  it("ends at a folder that is not there", () => {
    const orphan = folder("f-orphan", "Orphan", "f-gone");
    expect(folderPath(orphan, byId([...folders, orphan])).names).toEqual([]);
    const below = folder("f-below", "Below", orphan.id);
    expect(folderPath(below, byId([...folders, orphan, below]))).toEqual({
      names: ["Orphan"],
      more: false,
    });
  });

  it("ends on a loop of folders", () => {
    const a = folder("f-a", "A", "f-b");
    const b = folder("f-b", "B", "f-a");
    expect(folderPath(a, byId([a, b]))).toEqual({ names: ["B"], more: false });
    const self = folder("f-self", "Self", "f-self");
    expect(folderPath(self, byId([self]))).toEqual({ names: [], more: false });
  });

  it("has the nearest folders and no more, however deep a folder is", () => {
    const links = chain(5000, "a").map((f, n) => ({ ...f, name: `a${String(n)}` }));
    const all = byId(links);

    // As many as it takes, and all of them.
    expect(folderPath(links[MAX_PATH_FOLDERS], all)).toEqual({
      names: links.slice(0, MAX_PATH_FOLDERS).map((f) => f.name),
      more: false,
    });
    // One more above: the nearest ones, and that there are more.
    const deeper = folderPath(links[MAX_PATH_FOLDERS + 1], all);
    expect(deeper.more).toBe(true);
    expect(deeper.names).toEqual(links.slice(1, MAX_PATH_FOLDERS + 1).map((f) => f.name));
    // And at the end of the chain no more than that.
    const deepest = folderPath(links[4999], all);
    expect(deepest.more).toBe(true);
    expect(deepest.names).toEqual(links.slice(4999 - MAX_PATH_FOLDERS, 4999).map((f) => f.name));
  });
});

describe("a path as it is shown", () => {
  const path = (names: string[], more = false): { names: string[]; more: boolean } => ({
    names,
    more,
  });

  it("is the names with the separator between them, each between isolates", () => {
    expect(pathWritten(path([]))).toBe("");
    expect(pathWritten(path(["Customers"]))).toBe(iso("Customers"));
    expect(pathWritten(path(["Customers", "Production"]))).toBe(
      iso("Customers") + SEP + iso("Production"),
    );
    // With the folder the path is of at its end, for that folder's tooltip.
    expect(pathWritten(path(["Customers"]), "Production")).toBe(
      iso("Customers") + SEP + iso("Production"),
    );
    expect(pathWritten(path([]), "Lab")).toBe(iso("Lab"));
    // What a name can well have in it stays as it is.
    expect(pathWritten(path(["10.0.0.0/24", "a > b"]))).toBe(
      iso("10.0.0.0/24") + SEP + iso("a > b"),
    );
  });

  it("begins with an ellipsis where folders above are left out", () => {
    expect(pathWritten(path(["B", "C"], true))).toBe(ELLIPSIS + SEP + iso("B") + SEP + iso("C"));
    expect(pathWritten(path(["B"], true), "C")).toBe(ELLIPSIS + SEP + iso("B") + SEP + iso("C"));
  });

  it("does not let one folder read as two, or as folders left out", () => {
    // The separator and the ellipsis, written by whoever named the folder.
    const spoof = `Customers${SEP}Production`;
    expect(pathWritten(path([spoof]))).toBe(iso("Customers \\u{203A} Production"));
    expect(pathBeside(path([spoof]))).toBe(iso("Customers \\u{203A} Production"));
    expect(pathWritten(path([ELLIPSIS, "X"]))).toBe(iso("\\u{2026}") + SEP + iso("X"));
    // And the text of a character written out is told from that character.
    expect(pathWritten(path(["a\\u{203A}b"]))).toBe(iso("a\\\\u{203A}b"));
  });

  it("keeps a right-to-left name from turning the path around", () => {
    // Two names in Hebrew letters. Without the isolates the separator
    // between them would take their direction, and the two would show in
    // the other order: the folder before the one it is in.
    const alef = String.fromCodePoint(0x5d0, 0x5d1, 0x5d2);
    const dalet = String.fromCodePoint(0x5d3, 0x5d4, 0x5d5);
    const shown = pathWritten(path([alef, dalet]));
    expect(shown).toBe(iso(alef) + SEP + iso(dalet));
    // An override or an isolate of the name's own is written out, so the
    // isolates around a name are the only ones there.
    const override = String.fromCodePoint(0x202e);
    const popIsolate = String.fromCodePoint(0x2069);
    expect(pathWritten(path([`a${override}b${popIsolate}c`]))).toBe(iso("a\\u{202E}b\\u{2069}c"));
  });

  it("cuts a name, shorter beside the folder than in a tooltip", () => {
    const long = "x".repeat(65);
    expect(pathWritten(path([long]))).toBe(iso(`${"x".repeat(64)}${ELLIPSIS}`));
    expect(pathBeside(path([long]))).toBe(iso(`${"x".repeat(24)}${ELLIPSIS}`));
    expect(pathBeside(path(["y".repeat(24)]))).toBe(iso("y".repeat(24)));
  });

  it("shows the two folders nearest to the folder beside it", () => {
    expect(pathBeside(path([]))).toBe("");
    expect(pathBeside(path(["A"]))).toBe(iso("A"));
    expect(pathBeside(path(["A", "B"]))).toBe(iso("A") + SEP + iso("B"));
    expect(pathBeside(path(["A", "B", "C"]))).toBe(ELLIPSIS + SEP + iso("B") + SEP + iso("C"));
    // Also where the path itself has left folders out.
    expect(pathBeside(path(["A", "B"], true))).toBe(ELLIPSIS + SEP + iso("A") + SEP + iso("B"));
    expect(pathBeside(path(["A"], true))).toBe(ELLIPSIS + SEP + iso("A"));
  });
});

describe("indexTree", () => {
  it("has the folders and sessions of each folder in the order they came in", () => {
    const index = indexTree(folders, sessions);
    expect(index.folders.get(null)).toEqual([customers, lab]);
    expect(index.folders.get(customers.id)).toEqual([production]);
    expect(index.folders.get(lab.id)).toBeUndefined();
    expect(index.sessions.get(production.id)).toEqual([web, prodDb]);
    expect(index.sessions.get(customers.id)).toBeUndefined();
  });

  it("lists a tree as it was listed before", () => {
    useSessionStore.setState({ expandedFolderIds: new Set([customers.id, production.id]) });
    renderTree();
    expect(rows()).toEqual([
      "0:Customers",
      "1:Production",
      "2:EU prod",
      "2:web",
      "2:prod-db",
      "0:Lab",
    ]);
  });
});

describe("dndIdIn", () => {
  it("leaves an item of the tree its own ID", () => {
    expect(dndIdIn(undefined, folderDndId(eu.id))).toBe(folderDndId(eu.id));
    expect(parseDndId(dndIdIn(undefined, sessionDndId(web.id)))).toEqual({
      type: "session",
      id: web.id,
    });
  });

  it("tells the copies of an item in the results apart, and none is read as the tree's", () => {
    const underProduction = dndIdIn(production.id, sessionDndId(euGw.id));
    const underEu = dndIdIn(eu.id, sessionDndId(euGw.id));
    expect(underProduction).not.toBe(underEu);
    expect(underProduction).not.toBe(sessionDndId(euGw.id));
    expect(parseDndId(underProduction)).toBeNull();
    expect(parseDndId(underEu)).toBeNull();
    expect(parseDndId(dndIdIn(production.id, folderDndId(eu.id)))).toBeNull();
  });
});

describe("SearchResults", () => {
  it("lists a folder that was found with what is in it, and then the sessions", () => {
    renderResults([production], [labProd]);

    // The folder, its folders and sessions one level in, and the session
    // that was found by its own name.
    expect(rows()).toEqual(["0:Production", "1:EU prod", "1:web", "1:prod-db", "0:lab-prod"]);
    expect(row(production.id).getAttribute("aria-expanded")).toBe("true");
  });

  it("shows the folders above a folder beside its name", () => {
    const deep = folder("f-deep", "Deep", eu.id);
    renderResults([eu, deep, lab], [], [eu.id, deep.id, lab.id], [...folders, deep]);

    const found = row(eu.id);
    const beside = found.querySelector(".tree-path");
    expect(beside?.textContent).toBe(iso("Customers") + SEP + iso("Production"));
    // Left to right, whatever the names are written in.
    expect(beside?.getAttribute("dir")).toBe("ltr");
    expect(found.querySelector(".tree-label")?.getAttribute("title")).toBe(
      iso("Customers") + SEP + iso("Production") + SEP + iso("EU prod"),
    );
    // Of a longer path the nearest two, and all of it as the tooltip.
    const deeper = row(deep.id).querySelector(".tree-path");
    expect(deeper?.textContent).toBe(ELLIPSIS + SEP + iso("Production") + SEP + iso("EU prod"));
    expect(deeper?.getAttribute("title")).toBe(
      iso("Customers") + SEP + iso("Production") + SEP + iso("EU prod"),
    );
    // A folder at the top has none above it.
    expect(row(lab.id).querySelector(".tree-path")).toBeNull();
    expect(row(lab.id).querySelector(".tree-label")?.getAttribute("title")).toBe(iso("Lab"));
  });

  it("shows that a folder is in another also where that one has no name", () => {
    const nameless = folder("f-nameless", "");
    const inside = folder("f-inside", "Inside", nameless.id);
    renderResults([inside], [], [inside.id], [nameless, inside]);

    expect(row(inside.id).querySelector(".tree-path")?.textContent).toBe(iso(""));
  });

  it("lists a session under its folder and as a result of its own", () => {
    renderResults([production], [prodDb]);

    expect(rows()).toEqual(["0:Production", "1:EU prod", "1:web", "1:prod-db", "0:prod-db"]);
    const twice = copies(prodDb.id);
    expect(twice).toHaveLength(2);
    // As a result it shows its host, as before.
    expect(twice[1].querySelector(".tree-meta")?.textContent).toBe("prod-db.example");
  });

  it("lists a folder that was found inside another by its name there, and apart with its content", () => {
    // Open in the tree: under the other folder it is not opened all the same.
    useSessionStore.setState({ expandedFolderIds: new Set([eu.id]) });
    renderResults([production, eu], []);

    expect(rows()).toEqual([
      "0:Production",
      "1:EU prod",
      "1:web",
      "1:prod-db",
      "0:EU prod",
      "1:eu-gw",
    ]);
    const [inside, apart] = copies(eu.id);
    // Under the other folder it neither opens nor says that it could.
    expect(inside.hasAttribute("aria-expanded")).toBe(false);
    expect(within(inside).queryByRole("button")).toBeNull();
    fireEvent.doubleClick(inside);
    expect(useSessionStore.getState().expandedFolderIds).toEqual(new Set([eu.id]));
    expect(onToggleFolder).not.toHaveBeenCalled();
    expect(rows()).toHaveLength(6);
    // It is still a folder to select and to open the menu of.
    fireEvent.contextMenu(inside);
    expect(onContextMenu.mock.calls[0].slice(1)).toEqual([eu.id, "folder"]);
    // As a result of its own it is open, and closes there.
    expect(apart.getAttribute("aria-expanded")).toBe("true");
    fireEvent.doubleClick(apart);
    expect(onToggleFolder).toHaveBeenCalledExactlyOnceWith(eu.id);
  });

  it("lists a chain of folders that were all found once each, not once for every folder", () => {
    // Forty folders one in the other, all found and all open in the tree.
    const links = chain(40, "a");
    useSessionStore.setState({ expandedFolderIds: new Set(links.map((f) => f.id)) });
    renderResults(links, [], [], links);

    // Each as a result, and each but the first once more by its name under
    // the one it is in: twice the chain, not its length squared.
    expect(screen.getAllByRole("treeitem")).toHaveLength(40 + 39);
    expect(copies(links[39].id)).toHaveLength(2);
    expect(copies(links[0].id)).toHaveLength(1);
  });

  it("has the folders in a folder open or closed as they are in the tree", () => {
    renderResults([production], []);
    expect(rows()).not.toContain("2:eu-gw");

    // Opening one here opens it in the tree, as it is the same folder.
    const inside = row(eu.id);
    fireEvent.click(within(inside).getByRole("button", { name: "Expand" }));
    expect(useSessionStore.getState().expandedFolderIds.has(eu.id)).toBe(true);
    expect(rows()).toEqual(["0:Production", "1:EU prod", "2:eu-gw", "1:web", "1:prod-db"]);
    expect(onToggleFolder).not.toHaveBeenCalled();
  });

  it("lists nothing under a folder that was closed here", () => {
    renderResults([production, lab], [labProd], [production.id]);

    expect(rows()).toEqual(["0:Production", "0:Lab", "1:lab-prod", "0:lab-prod"]);
    expect(row(production.id).getAttribute("aria-expanded")).toBe("false");
    expect(within(row(production.id)).getByRole("button", { name: "Expand" })).toBeTruthy();
  });

  it("opens a folder that was found whatever it is in the tree, and leaves the tree alone", () => {
    // Closed in the tree, as every folder is here.
    renderResults([production], []);
    expect(rows()).toContain("1:web");

    const found = row(production.id);
    fireEvent.click(within(found).getByRole("button", { name: "Collapse" }));
    expect(onToggleFolder).toHaveBeenCalledExactlyOnceWith(production.id);
    fireEvent.doubleClick(found);
    expect(onToggleFolder).toHaveBeenCalledTimes(2);
    // Neither is the tree's to remember.
    expect(useSessionStore.getState().expandedFolderIds.size).toBe(0);
    // The chevron does not select the folder; a click on the row does.
    expect(useSessionStore.getState().selectedItemId).toBeNull();
    fireEvent.click(found);
    expect(useSessionStore.getState().selectedItemId).toBe(production.id);
    expect(useSessionStore.getState().selectedItemType).toBe("folder");
    expect(row(production.id).className).toContain("tree-item-selected");
  });

  it("opens the folder's menu for a folder that was found", () => {
    renderResults([lab], []);

    fireEvent.contextMenu(row(lab.id));
    expect(onContextMenu).toHaveBeenCalledOnce();
    expect(onContextMenu.mock.calls[0].slice(1)).toEqual([lab.id, "folder"]);
    expect(useSessionStore.getState().selectedItemId).toBe(lab.id);
  });

  it("connects to a session on a double click, under a folder and as a result", () => {
    renderResults([production], [labProd]);

    fireEvent.doubleClick(row(web.id));
    expect(onSessionDoubleClick).toHaveBeenLastCalledWith(web.id);
    fireEvent.doubleClick(row(labProd.id));
    expect(onSessionDoubleClick).toHaveBeenLastCalledWith(labProd.id);
    expect(onSessionDoubleClick).toHaveBeenCalledTimes(2);
  });

  it("connects once on Enter at a session that was found", () => {
    renderResults([], [labProd]);

    // The tree connects to the selected session on Enter as well, which is
    // this one after a click on it.
    fireEvent.click(row(labProd.id));
    fireEvent.keyDown(row(labProd.id), { key: "Enter" });
    expect(onSessionDoubleClick).toHaveBeenCalledExactlyOnceWith(labProd.id);
    expect(onTreeKeyDown).not.toHaveBeenCalled();
    // Any other key is the tree's.
    fireEvent.keyDown(row(labProd.id), { key: "Delete" });
    expect(onTreeKeyDown).toHaveBeenCalledOnce();
    expect(onSessionDoubleClick).toHaveBeenCalledOnce();
  });

  it("starts a drag at a folder of the tree", () => {
    useSessionStore.setState({ expandedFolderIds: new Set([customers.id, production.id]) });
    renderTree();

    const inTree = row(production.id);
    expect(inTree.getAttribute("aria-roledescription")).toBe("sortable");
    expect(inTree.getAttribute("role")).toBe("treeitem");
    press(inTree);
    expect(onDragStart).toHaveBeenCalledOnce();
  });

  it("starts a drag at a session of the tree", () => {
    useSessionStore.setState({ expandedFolderIds: new Set([customers.id, production.id]) });
    renderTree();

    press(row(web.id));
    expect(onDragStart).toHaveBeenCalledOnce();
  });

  it("starts no drag at anything in the results", () => {
    useSessionStore.setState({ expandedFolderIds: new Set([eu.id]) });
    renderResults([production, eu], [euGw]);

    // A session and a folder twice each, and every kind of row there is.
    expect(copies(euGw.id)).toHaveLength(2);
    expect(copies(eu.id)).toHaveLength(2);
    expect(rows()).toHaveLength(7);
    for (const item of screen.getAllByRole("treeitem")) {
      press(item);
    }
    expect(onDragStart).not.toHaveBeenCalled();
    // Nor is any of them announced as sortable, or as something disabled.
    expect(document.querySelectorAll("[aria-roledescription]")).toHaveLength(0);
    expect(document.querySelectorAll("[aria-disabled]")).toHaveLength(0);
  });

  it("lists nothing where nothing was found", () => {
    renderResults([], []);
    expect(screen.queryAllByRole("treeitem")).toHaveLength(0);
  });
});
