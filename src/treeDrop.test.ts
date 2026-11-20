/**
 * Where an item of the session tree goes when it is dropped.
 *
 * The contract pinned here: a session dropped on a folder goes into it, and
 * dropped on a session goes before or after that one, in that session's
 * folder. A folder dropped on the middle of a folder goes into it, and on its
 * upper or lower edge before or after it; under the row of an open folder is
 * what it lists, so there the lower edge goes into it too. A folder dropped
 * on a session goes into that session's folder, and below the rows to the end
 * of the top level. Nothing goes where it already is, no folder goes into
 * itself or below itself, no session goes to the top level, and in PostgreSQL
 * mode a user moves only what is their own, into folders that are their own
 * up to the top, and to a place beside a row of their own: a reorder changes
 * only the user's own rows, so the place beside another user's is not theirs
 * to give.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Folder, Session } from "./stores/sessionStore";
import { indexTree } from "./components/SessionTree";
import {
  canDrag,
  dropAt,
  dropPointAt,
  dropTree,
  sameDrop,
  type DragItem,
  type Drop,
  type DropPoint,
  type DropTree,
} from "./treeDrop";

function folder(id: string, parentId: string | null = null, owner = "local"): Folder {
  return { id, name: id, parent_id: parentId, sort_order: 0, owner, visibility: "personal" };
}

function session(id: string, folderId: string, owner = "local"): Session {
  return {
    id,
    folder_id: folderId,
    name: id,
    hostname: `${id}.example`,
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
    owner,
    visibility: "personal",
  };
}

// A          a1 a2 a3
//   A1       x1
//   A2
// B          b1 b2
// C
const folders = [folder("A"), folder("A1", "A"), folder("A2", "A"), folder("B"), folder("C")];
const sessions = [
  session("a1", "A"),
  session("a2", "A"),
  session("a3", "A"),
  session("x1", "A1"),
  session("b1", "B"),
  session("b2", "B"),
];

function tree(
  open: string[] = [],
  own: { pgMode?: boolean; pgUser?: string | null } = {},
  all: { folders: Folder[]; sessions: Session[] } = { folders, sessions },
): DropTree {
  return dropTree(
    all.folders,
    all.sessions,
    indexTree(all.folders, all.sessions),
    new Set(open),
    own.pgMode ?? false,
    own.pgUser ?? null,
  );
}

const aFolder = (id: string): DragItem => ({ type: "folder", id });
const aSession = (id: string): DragItem => ({ type: "session", id });
const onFolder = (id: string, ratio = 0.5): DropPoint => ({ at: "row", type: "folder", id, ratio });
const onSession = (id: string, ratio = 0.5): DropPoint => ({
  at: "row",
  type: "session",
  id,
  ratio,
});
const below: DropPoint = { at: "empty" };

describe("a session that is dropped", () => {
  it("goes into the folder it is dropped on, wherever on its row", () => {
    for (const ratio of [0, 0.1, 0.5, 0.9, 1]) {
      expect(dropAt(aSession("a1"), onFolder("B", ratio), tree())).toEqual({
        mark: { kind: "into", folderId: "B" },
        item: aSession("a1"),
        parentId: "B",
        moves: true,
        order: null,
      });
    }
    // Also a folder inside its own, and one that holds nothing.
    expect(dropAt(aSession("a1"), onFolder("A1"), tree())?.parentId).toBe("A1");
    expect(dropAt(aSession("a1"), onFolder("C"), tree())?.parentId).toBe("C");
  });

  it("does not go into the folder it is in", () => {
    expect(dropAt(aSession("a1"), onFolder("A"), tree(["A"]))).toBeNull();
  });

  it("goes before or after a session of another folder, into that folder", () => {
    expect(dropAt(aSession("a1"), onSession("b2", 0.2), tree(["B"]))).toEqual({
      mark: { kind: "line", rowId: "b2", edge: "top" },
      item: aSession("a1"),
      parentId: "B",
      moves: true,
      order: ["b1", "a1", "b2"],
    });
    expect(dropAt(aSession("a1"), onSession("b2", 0.8), tree(["B"]))).toEqual({
      mark: { kind: "line", rowId: "b2", edge: "bottom" },
      item: aSession("a1"),
      parentId: "B",
      moves: true,
      order: ["b1", "b2", "a1"],
    });
    // The middle of a session's row is its lower half.
    expect(dropAt(aSession("a1"), onSession("b1", 0.5), tree(["B"]))?.order).toEqual([
      "b1",
      "a1",
      "b2",
    ]);
  });

  it("is put in another place among the sessions of its folder", () => {
    expect(dropAt(aSession("a1"), onSession("a3", 0.9), tree(["A"]))).toEqual({
      mark: { kind: "line", rowId: "a3", edge: "bottom" },
      item: aSession("a1"),
      parentId: "A",
      moves: false,
      order: ["a2", "a3", "a1"],
    });
    expect(dropAt(aSession("a3"), onSession("a1", 0.1), tree(["A"]))?.order).toEqual([
      "a3",
      "a1",
      "a2",
    ]);
    expect(dropAt(aSession("a1"), onSession("a2", 0.9), tree(["A"]))?.order).toEqual([
      "a2",
      "a1",
      "a3",
    ]);
  });

  it("goes nowhere where it already is", () => {
    // On itself, before the one after it, after the one before it.
    expect(dropAt(aSession("a2"), onSession("a2", 0.1), tree(["A"]))).toBeNull();
    expect(dropAt(aSession("a2"), onSession("a2", 0.9), tree(["A"]))).toBeNull();
    expect(dropAt(aSession("a2"), onSession("a3", 0.1), tree(["A"]))).toBeNull();
    expect(dropAt(aSession("a2"), onSession("a1", 0.9), tree(["A"]))).toBeNull();
    // And the other side of each is somewhere else.
    expect(dropAt(aSession("a2"), onSession("a3", 0.9), tree(["A"]))).not.toBeNull();
    expect(dropAt(aSession("a2"), onSession("a1", 0.1), tree(["A"]))).not.toBeNull();
  });

  it("does not go to the top level", () => {
    for (const id of ["a1", "x1", "b2"]) {
      expect(dropAt(aSession(id), below, tree(["A", "A1", "B"]))).toBeNull();
    }
  });

  it("does not go beside a session whose folder is not in the tree", () => {
    const adrift = session("s-adrift", "not-listed");
    const all = { folders, sessions: [...sessions, adrift] };
    expect(dropAt(aSession("a1"), onSession("s-adrift"), tree([], {}, all))).toBeNull();
    expect(dropAt(aSession("a1"), onFolder("not-listed"), tree([], {}, all))).toBeNull();
  });
});

describe("a folder that is dropped", () => {
  it("goes into the folder whose middle it is dropped on", () => {
    for (const ratio of [0.25, 0.5, 0.75]) {
      expect(dropAt(aFolder("C"), onFolder("B", ratio), tree())).toEqual({
        mark: { kind: "into", folderId: "B" },
        item: aFolder("C"),
        parentId: "B",
        moves: true,
        order: null,
      });
    }
    // Out of a folder into another, and deeper into its own.
    expect(dropAt(aFolder("A1"), onFolder("B"), tree(["A"]))?.parentId).toBe("B");
    expect(dropAt(aFolder("A1"), onFolder("A2"), tree(["A"]))?.parentId).toBe("A2");
  });

  it("goes before or after the folder whose edge it is dropped on", () => {
    expect(dropAt(aFolder("C"), onFolder("B", 0.1), tree())).toEqual({
      mark: { kind: "line", rowId: "B", edge: "top" },
      item: aFolder("C"),
      parentId: null,
      moves: false,
      order: ["A", "C", "B"],
    });
    expect(dropAt(aFolder("C"), onFolder("A", 0.9), tree())?.order).toEqual(["A", "C", "B"]);
    expect(dropAt(aFolder("A"), onFolder("C", 0.9), tree())).toEqual({
      mark: { kind: "line", rowId: "C", edge: "bottom" },
      item: aFolder("A"),
      parentId: null,
      moves: false,
      order: ["B", "C", "A"],
    });
  });

  it("comes out of its folder at the edge of a folder of the top level", () => {
    expect(dropAt(aFolder("A1"), onFolder("B", 0.1), tree(["A"]))).toEqual({
      mark: { kind: "line", rowId: "B", edge: "top" },
      item: aFolder("A1"),
      parentId: null,
      moves: true,
      order: ["A", "A1", "B", "C"],
    });
  });

  it("goes beside a folder inside another, as one of that folder's", () => {
    expect(dropAt(aFolder("C"), onFolder("A2", 0.1), tree(["A"]))).toEqual({
      mark: { kind: "line", rowId: "A2", edge: "top" },
      item: aFolder("C"),
      parentId: "A",
      moves: true,
      order: ["A1", "C", "A2"],
    });
  });

  it("goes into an open folder from its lower edge, since what it lists is under it", () => {
    // Open and with something in it: the lower edge is the folder's inside.
    expect(dropAt(aFolder("C"), onFolder("B", 0.9), tree(["B"]))?.mark).toEqual({
      kind: "into",
      folderId: "B",
    });
    // Closed, or open with nothing in it: it is what follows the folder.
    expect(dropAt(aFolder("A"), onFolder("B", 0.9), tree())?.mark).toEqual({
      kind: "line",
      rowId: "B",
      edge: "bottom",
    });
    expect(dropAt(aFolder("A"), onFolder("C", 0.9), tree(["C"]))?.mark).toEqual({
      kind: "line",
      rowId: "C",
      edge: "bottom",
    });
    // The upper edge is before the folder either way.
    expect(dropAt(aFolder("C"), onFolder("B", 0.1), tree(["B"]))?.mark).toEqual({
      kind: "line",
      rowId: "B",
      edge: "top",
    });
  });

  it("goes into the folder of a session it is dropped on", () => {
    expect(dropAt(aFolder("C"), onSession("b1", 0.1), tree(["B"]))).toEqual({
      mark: { kind: "into", folderId: "B" },
      item: aFolder("C"),
      parentId: "B",
      moves: true,
      order: null,
    });
    // Not the folder it is in already.
    expect(dropAt(aFolder("A1"), onSession("a1"), tree(["A"]))).toBeNull();
  });

  it("goes neither into itself nor below itself", () => {
    const open = tree(["A", "A1"]);
    expect(dropAt(aFolder("A"), onFolder("A", 0.5), open)).toBeNull();
    expect(dropAt(aFolder("A"), onFolder("A1", 0.5), open)).toBeNull();
    // Beside a folder inside it is inside it as well.
    expect(dropAt(aFolder("A"), onFolder("A1", 0.1), open)).toBeNull();
    expect(dropAt(aFolder("A"), onFolder("A2", 0.9), open)).toBeNull();
    // As is the folder of a session inside it.
    expect(dropAt(aFolder("A"), onSession("a1"), open)).toBeNull();
    expect(dropAt(aFolder("A"), onSession("x1"), open)).toBeNull();
  });

  it("goes nowhere where it already is", () => {
    expect(dropAt(aFolder("A1"), onFolder("A", 0.5), tree(["A"]))).toBeNull();
    expect(dropAt(aFolder("B"), onFolder("C", 0.1), tree())).toBeNull();
    expect(dropAt(aFolder("B"), onFolder("A", 0.9), tree())).toBeNull();
    expect(dropAt(aFolder("A1"), onFolder("A2", 0.1), tree(["A"]))).toBeNull();
  });

  it("goes to the end of the top level when dropped below the rows", () => {
    expect(dropAt(aFolder("A1"), below, tree(["A"]))).toEqual({
      mark: { kind: "end" },
      item: aFolder("A1"),
      parentId: null,
      moves: true,
      order: null,
    });
    expect(dropAt(aFolder("A"), below, tree())).toEqual({
      mark: { kind: "end" },
      item: aFolder("A"),
      parentId: null,
      moves: false,
      order: ["B", "C", "A"],
    });
    // The last one is there already.
    expect(dropAt(aFolder("C"), below, tree())).toBeNull();
  });
});

describe("a tree that is not as it should be", () => {
  it("takes nothing into a folder that does not lead to the top", () => {
    const all = {
      folders: [
        ...folders,
        // A folder whose parent is not listed, and two that are in each other.
        folder("adrift", "not-listed"),
        folder("ring-1", "ring-2"),
        folder("ring-2", "ring-1"),
      ],
      sessions,
    };
    const broken = tree([], {}, all);
    expect(dropAt(aSession("a1"), onFolder("adrift"), broken)).toBeNull();
    expect(dropAt(aSession("a1"), onFolder("ring-1"), broken)).toBeNull();
    expect(dropAt(aFolder("C"), onFolder("ring-2"), broken)).toBeNull();
    expect(dropAt(aFolder("C"), onFolder("ring-2", 0.1), broken)).toBeNull();
  });

  it("drops nothing that is not in the tree, and on nothing that is not", () => {
    expect(dropAt(aSession("gone"), onFolder("B"), tree())).toBeNull();
    expect(dropAt(aFolder("gone"), onFolder("B"), tree())).toBeNull();
    expect(dropAt(aFolder("C"), onFolder("gone"), tree())).toBeNull();
    expect(dropAt(aFolder("C"), onSession("gone"), tree())).toBeNull();
    expect(dropAt(aSession("a1"), onSession("gone"), tree())).toBeNull();
    expect(canDrag(aSession("gone"), tree())).toBe(false);
    expect(canDrag(aFolder("gone"), tree())).toBe(false);
  });
});

describe("in PostgreSQL mode", () => {
  // alice's: Mine (m1), Mine/Sub, and Inside, a folder of hers in bob's.
  // bob's, shared: Theirs (t1, and alice's session mine-there).
  const pgFolders = [
    folder("Mine", null, "alice"),
    folder("Sub", "Mine", "alice"),
    folder("Theirs", null, "bob"),
    folder("Inside", "Theirs", "alice"),
    folder("Other", null, "alice"),
  ];
  const pgSessions = [
    session("m1", "Mine", "alice"),
    session("m2", "Mine", "alice"),
    session("t1", "Theirs", "bob"),
    session("mine-there", "Theirs", "alice"),
    // bob's session in a folder of alice's, as one from before the rule.
    session("theirs-here", "Mine", "bob"),
  ];
  const all = { folders: pgFolders, sessions: pgSessions };
  const asAlice = (open: string[] = []): DropTree =>
    tree(open, { pgMode: true, pgUser: "alice" }, all);

  it("lets a user pick up only what is their own", () => {
    expect(canDrag(aFolder("Mine"), asAlice())).toBe(true);
    expect(canDrag(aSession("m1"), asAlice())).toBe(true);
    expect(canDrag(aFolder("Theirs"), asAlice())).toBe(false);
    expect(canDrag(aSession("t1"), asAlice())).toBe(false);
    expect(dropAt(aSession("t1"), onFolder("Mine"), asAlice())).toBeNull();
    expect(dropAt(aFolder("Theirs"), onFolder("Mine"), asAlice())).toBeNull();
  });

  it("puts nothing into another user's folder, or beside what is in one", () => {
    expect(dropAt(aSession("m1"), onFolder("Theirs"), asAlice())).toBeNull();
    expect(dropAt(aSession("m1"), onSession("t1", 0.1), asAlice(["Theirs"]))).toBeNull();
    expect(dropAt(aFolder("Sub"), onFolder("Theirs"), asAlice())).toBeNull();
    expect(dropAt(aFolder("Sub"), onSession("t1"), asAlice(["Theirs"]))).toBeNull();
    expect(dropAt(aFolder("Sub"), onFolder("Inside", 0.1), asAlice(["Theirs"]))).toBeNull();
  });

  it("puts nothing into a folder of the user's own that is in another user's", () => {
    expect(dropAt(aSession("m1"), onFolder("Inside"), asAlice(["Theirs"]))).toBeNull();
    expect(dropAt(aFolder("Sub"), onFolder("Inside"), asAlice(["Theirs"]))).toBeNull();
  });

  it("moves what is the user's own out of another user's folder", () => {
    expect(dropAt(aSession("mine-there"), onFolder("Mine"), asAlice())?.parentId).toBe("Mine");
    expect(dropAt(aFolder("Inside"), onFolder("Mine"), asAlice(["Theirs"]))?.parentId).toBe("Mine");
    expect(dropAt(aFolder("Inside"), below, asAlice(["Theirs"]))?.moves).toBe(true);
  });

  it("puts nothing beside another user's row, whose place is not the user's to give", () => {
    // At the top level, which is everyone's, and in a folder of the user's own.
    expect(dropAt(aFolder("Other"), onFolder("Theirs", 0.1), asAlice())).toBeNull();
    expect(dropAt(aFolder("Mine"), onFolder("Theirs", 0.9), asAlice())).toBeNull();
    expect(dropAt(aFolder("Sub"), onFolder("Theirs", 0.1), asAlice(["Mine"]))).toBeNull();
    expect(dropAt(aSession("m1"), onSession("theirs-here", 0.1), asAlice(["Mine"]))).toBeNull();
    expect(dropAt(aSession("m1"), onSession("theirs-here", 0.9), asAlice(["Mine"]))).toBeNull();
  });

  it("puts a folder beside one of the user's own at the top level, among everyone's", () => {
    // The order has every folder there; the backend places the user's own.
    expect(dropAt(aFolder("Other"), onFolder("Mine", 0.1), asAlice())).toEqual({
      mark: { kind: "line", rowId: "Mine", edge: "top" },
      item: aFolder("Other"),
      parentId: null,
      moves: false,
      order: ["Other", "Mine", "Theirs"],
    });
    // Out of a folder to the top level.
    expect(dropAt(aFolder("Sub"), onFolder("Other", 0.9), asAlice(["Mine"]))).toEqual({
      mark: { kind: "line", rowId: "Other", edge: "bottom" },
      item: aFolder("Sub"),
      parentId: null,
      moves: true,
      order: ["Mine", "Theirs", "Other", "Sub"],
    });
  });

  it("moves within what is the user's own as without PostgreSQL", () => {
    expect(dropAt(aSession("m1"), onFolder("Sub"), asAlice(["Mine"]))?.parentId).toBe("Sub");
    // Another user's session in the folder stays in the order, where it was.
    expect(dropAt(aSession("m1"), onSession("m2", 0.9), asAlice(["Mine"]))?.order).toEqual([
      "m2",
      "m1",
      "theirs-here",
    ]);
    expect(dropAt(aFolder("Other"), onFolder("Mine"), asAlice())?.parentId).toBe("Mine");
  });

  it("takes nothing for the user's own while the role is not known", () => {
    const unknown = tree([], { pgMode: true, pgUser: null }, all);
    expect(canDrag(aFolder("Mine"), unknown)).toBe(false);
    expect(canDrag(aSession("m1"), unknown)).toBe(false);
    expect(dropAt(aSession("m1"), onFolder("Sub"), unknown)).toBeNull();
  });

  it("does not fold role names", () => {
    const asUpper = tree([], { pgMode: true, pgUser: "Alice" }, all);
    expect(canDrag(aFolder("Mine"), asUpper)).toBe(false);
  });
});

describe("sameDrop", () => {
  const drop = (over: Partial<Drop> = {}): Drop => ({
    mark: { kind: "line", rowId: "b1", edge: "top" },
    item: aSession("a1"),
    parentId: "B",
    moves: true,
    order: ["a1", "b1", "b2"],
    ...over,
  });

  it("takes two drops for the same that show and do the same", () => {
    expect(sameDrop(null, null)).toBe(true);
    expect(sameDrop(drop(), drop())).toBe(true);
    expect(
      sameDrop(
        drop({ mark: { kind: "into", folderId: "B" }, order: null }),
        drop({ mark: { kind: "into", folderId: "B" }, order: null }),
      ),
    ).toBe(true);
  });

  it("tells apart drops that differ in anything", () => {
    expect(sameDrop(drop(), null)).toBe(false);
    expect(sameDrop(null, drop())).toBe(false);
    expect(sameDrop(drop(), drop({ item: aSession("a2") }))).toBe(false);
    expect(sameDrop(drop(), drop({ item: aFolder("a1") }))).toBe(false);
    expect(sameDrop(drop(), drop({ parentId: "C" }))).toBe(false);
    expect(sameDrop(drop(), drop({ moves: false }))).toBe(false);
    expect(sameDrop(drop(), drop({ order: ["b1", "a1", "b2"] }))).toBe(false);
    expect(sameDrop(drop(), drop({ order: ["a1", "b1"] }))).toBe(false);
    expect(sameDrop(drop(), drop({ order: null }))).toBe(false);
    expect(sameDrop(drop(), drop({ mark: { kind: "line", rowId: "b1", edge: "bottom" } }))).toBe(
      false,
    );
    expect(sameDrop(drop(), drop({ mark: { kind: "line", rowId: "b2", edge: "top" } }))).toBe(
      false,
    );
    expect(sameDrop(drop(), drop({ mark: { kind: "into", folderId: "B" } }))).toBe(false);
    expect(
      sameDrop(
        drop({ mark: { kind: "into", folderId: "B" } }),
        drop({ mark: { kind: "into", folderId: "C" } }),
      ),
    ).toBe(false);
    expect(sameDrop(drop({ mark: { kind: "end" } }), drop())).toBe(false);
  });
});

describe("dropPointAt", () => {
  /** A tree of one folder row and one session row, and a row that is neither. */
  function page(): {
    tree: HTMLElement;
    folderRow: HTMLElement;
    label: HTMLElement;
    sessionRow: HTMLElement;
    untyped: HTMLElement;
    heading: HTMLElement;
    outside: HTMLElement;
  } {
    document.body.innerHTML = `
      <div id="tree">
        <div id="folder" data-item-id="A" data-item-type="folder"><span id="label">A</span></div>
        <div id="session" data-item-id="a1" data-item-type="session"></div>
        <div id="untyped" data-item-id="shared-1"></div>
        <div id="heading">Shared</div>
      </div>
      <div id="outside" data-item-id="B" data-item-type="folder"></div>`;
    const el = (id: string): HTMLElement => {
      const found = document.getElementById(id);
      if (!found) throw new Error(`no #${id}`);
      return found;
    };
    return {
      tree: el("tree"),
      folderRow: el("folder"),
      label: el("label"),
      sessionRow: el("session"),
      untyped: el("untyped"),
      heading: el("heading"),
      outside: el("outside"),
    };
  }

  /** What is at every point of the window. */
  function under(element: Element | null): void {
    vi.spyOn(document, "elementFromPoint").mockReturnValue(element);
  }

  /** A row's box as the engine reports it, and its height as laid out. */
  function box(row: HTMLElement, top: number, height: number, laidOut: number): void {
    vi.spyOn(row, "getBoundingClientRect").mockReturnValue({
      top,
      height,
      bottom: top + height,
      left: 0,
      right: 200,
      width: 200,
      x: 0,
      y: top,
      toJSON: () => ({}),
    });
    Object.defineProperty(row, "offsetHeight", { configurable: true, value: laidOut });
  }

  afterEach(() => {
    vi.restoreAllMocks();
    document.body.innerHTML = "";
  });

  it("finds the row under the pointer, and how far down it the pointer is", () => {
    const { tree: treeEl, folderRow, label, sessionRow } = page();
    box(folderRow, 100, 24, 24);
    // What is hit is something inside the row.
    under(label);
    expect(dropPointAt(50, 106, treeEl, 1)).toEqual({
      at: "row",
      type: "folder",
      id: "A",
      ratio: 0.25,
    });
    expect(dropPointAt(50, 100, treeEl, 1)).toMatchObject({ ratio: 0 });
    expect(dropPointAt(50, 124, treeEl, 1)).toMatchObject({ ratio: 1 });

    box(sessionRow, 124, 24, 24);
    under(sessionRow);
    expect(dropPointAt(50, 136, treeEl, 1)).toEqual({
      at: "row",
      type: "session",
      id: "a1",
      ratio: 0.5,
    });
  });

  it("finds the same place in a scaled sidebar, whichever box the engine reports", () => {
    const { tree: treeEl, folderRow } = page();
    under(folderRow);
    // A row laid out 24 high at 400, in a sidebar scaled to 125%: on the
    // screen it is 30 high at 500, and the pointer is a third down it.
    const pointerY = 510;
    // The box as it is on the screen.
    box(folderRow, 500, 30, 24);
    expect(dropPointAt(50, pointerY, treeEl, 1.25)?.at).toBe("row");
    expect(dropPointAt(50, pointerY, treeEl, 1.25)).toMatchObject({ ratio: 10 / 30 });
    // The box as laid out, before the scale.
    box(folderRow, 400, 24, 24);
    expect(dropPointAt(50, pointerY, treeEl, 1.25)).toMatchObject({ ratio: 10 / 30 });
    // And scaled down to 75%: 18 high at 300 on the screen.
    box(folderRow, 300, 18, 24);
    expect(dropPointAt(50, 309, treeEl, 0.75)).toMatchObject({ ratio: 0.5 });
    box(folderRow, 400, 24, 24);
    expect(dropPointAt(50, 309, treeEl, 0.75)).toMatchObject({ ratio: 0.5 });
  });

  it("takes a pointer beyond a row's box for its edge, and a row without a box for its middle", () => {
    const { tree: treeEl, folderRow } = page();
    under(folderRow);
    box(folderRow, 100, 24, 24);
    expect(dropPointAt(50, 90, treeEl, 1)).toMatchObject({ ratio: 0 });
    expect(dropPointAt(50, 200, treeEl, 1)).toMatchObject({ ratio: 1 });
    box(folderRow, 0, 0, 0);
    expect(dropPointAt(50, 10, treeEl, 1)).toMatchObject({ ratio: 0.5 });
  });

  it("takes the tree's own box for its empty part, and nothing else in it", () => {
    const { tree: treeEl, heading, untyped } = page();
    under(treeEl);
    expect(dropPointAt(50, 300, treeEl, 1)).toEqual({ at: "empty" });
    // The heading of the shared sessions, and a row of them.
    under(heading);
    expect(dropPointAt(50, 300, treeEl, 1)).toBeNull();
    under(untyped);
    expect(dropPointAt(50, 300, treeEl, 1)).toBeNull();
  });

  it("finds nothing where the pointer is not over the tree", () => {
    const { tree: treeEl, outside } = page();
    // A row elsewhere, as one of a dialog over the tree would be.
    under(outside);
    expect(dropPointAt(50, 300, treeEl, 1)).toBeNull();
    under(document.body);
    expect(dropPointAt(50, 300, treeEl, 1)).toBeNull();
    under(null);
    expect(dropPointAt(50, 300, treeEl, 1)).toBeNull();
  });
});
