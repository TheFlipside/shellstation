/**
 * Dragging in the sidebar's tree, from the press to what the backend is
 * asked for.
 *
 * Where a drop goes is decided in `treeDrop.ts` and pinned in its own test.
 * The contract pinned here is what the sidebar makes of it: an item is picked
 * up with the pointer and not with a key, the place a drop would go to is
 * marked while it is dragged, a drop asks the backend for a move, for a new
 * order, or for both in that order, and for nothing where it goes nowhere; a
 * closed folder opens when an item is held over it; what was moved is shown
 * and selected; what the backend refuses is told; a dialog that opens ends
 * the drag; nothing is picked up while a drop is carried out; and in
 * PostgreSQL mode another user's items are not picked up.
 *
 * jsdom lays nothing out, so a test says what is under the pointer and how
 * large that row is.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { useAppStore } from "../stores/appStore";
import { useSessionStore, type Folder, type Session } from "../stores/sessionStore";
import { useToastStore } from "../stores/toastStore";
import { DRAG_OPEN_MS, SessionSidebar } from "./SessionSidebar";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({
  readText: vi.fn(() => Promise.resolve("")),
  writeText: vi.fn(() => Promise.resolve()),
}));
// Translations resolve to their keys, so assertions can match on key names.
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));
vi.mock("../i18n", () => ({
  default: { t: (key: string) => key, changeLanguage: vi.fn() },
}));

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

// Lab        lab-1 lab-2 lab-3
//   Racks
// Prod       prod-1
// Spare
const initialFolders = [folder("Lab"), folder("Racks", "Lab"), folder("Prod"), folder("Spare")];
const initialSessions = [
  session("lab-1", "Lab"),
  session("lab-2", "Lab"),
  session("lab-3", "Lab"),
  session("prod-1", "Prod"),
];

/** What the backend has, in the order it lists it. */
let stored: { folders: Folder[]; sessions: Session[] };
/** The commands that change something, as they were asked for. */
let changes: { command: string; args: unknown }[];
/** A command the backend refuses, and what it says. */
let refused: { command: string; message: string } | null;

/** Puts the listed ones first, in the order given; the rest keep theirs. */
function reordered<T extends { id: string }>(items: T[], order: string[]): T[] {
  const listed = order.flatMap((id) => items.filter((item) => item.id === id));
  return [...listed, ...items.filter((item) => !order.includes(item.id))];
}

/** The backend's commands, as far as the tree's drags use them. */
function backend(command: string, args?: unknown): Promise<unknown> {
  if (command === "folder_list") return Promise.resolve(stored.folders);
  if (command === "session_list_all") return Promise.resolve(stored.sessions);
  const changing = ["folder_move", "session_move", "folder_reorder", "session_reorder"];
  if (!changing.includes(command)) {
    // The lists of profiles and sequences, and whatever else is asked.
    return Promise.resolve([]);
  }
  changes.push({ command, args });
  if (refused?.command === command) return Promise.reject(new Error(refused.message));
  if (command === "session_move") {
    const { id, newFolderId } = args as { id: string; newFolderId: string };
    // A move puts it at the end of where it goes.
    const moved = stored.sessions.filter((s) => s.id === id);
    stored = {
      ...stored,
      sessions: [
        ...stored.sessions.filter((s) => s.id !== id),
        ...moved.map((s) => ({ ...s, folder_id: newFolderId })),
      ],
    };
  } else if (command === "folder_move") {
    const { id, newParentId } = args as { id: string; newParentId: string | null };
    const moved = stored.folders.filter((f) => f.id === id);
    stored = {
      ...stored,
      folders: [
        ...stored.folders.filter((f) => f.id !== id),
        ...moved.map((f) => ({ ...f, parent_id: newParentId })),
      ],
    };
  } else if (command === "session_reorder") {
    const { orderedIds } = args as { orderedIds: string[] };
    stored = { ...stored, sessions: reordered(stored.sessions, orderedIds) };
  } else {
    const { orderedIds } = args as { orderedIds: string[] };
    stored = { ...stored, folders: reordered(stored.folders, orderedIds) };
  }
  return Promise.resolve(null);
}

function rows(): string[] {
  return screen.getAllByRole("treeitem").map((item) => {
    const depth = item.style.getPropertyValue("--tree-depth") || "0";
    const label = item.querySelector(".tree-label")?.textContent ?? "";
    return `${depth}:${label}`;
  });
}

function row(id: string): HTMLElement {
  const found = document.querySelector<HTMLElement>(`.sidebar-tree [data-item-id="${id}"]`);
  if (!found) throw new Error(`no row ${id}`);
  return found;
}

function tree(): HTMLElement {
  return screen.getByRole("tree");
}

function preview(): HTMLElement | null {
  return document.querySelector<HTMLElement>("body > .tree-item-drag-overlay");
}

/** The rows that are marked as where a drop goes, each with how. */
function marks(): string[] {
  const kinds = ["into", "before", "after"];
  const marked = Array.from(document.querySelectorAll<HTMLElement>("[data-item-id]")).flatMap(
    (item) =>
      kinds
        .filter((kind) => item.classList.contains(`tree-item-drop-${kind}`))
        .map((kind) => `${kind} ${item.dataset.itemId ?? ""}`),
  );
  if (tree().classList.contains("sidebar-tree-drop-end")) marked.push("end");
  return marked;
}

const ROW_TOP = 200;
const ROW_HEIGHT = 20;

/**
 * Puts the pointer over `target`, `ratio` of the way down it: what is hit
 * there is the target, whose box is `ROW_HEIGHT` high at `ROW_TOP`.
 */
function moveOver(target: HTMLElement, ratio = 0.5): void {
  vi.spyOn(document, "elementFromPoint").mockReturnValue(target);
  vi.spyOn(target, "getBoundingClientRect").mockReturnValue({
    top: ROW_TOP,
    height: ROW_HEIGHT,
    bottom: ROW_TOP + ROW_HEIGHT,
    left: 0,
    right: 200,
    width: 200,
    x: 0,
    y: ROW_TOP,
    toJSON: () => ({}),
  });
  fireEvent.pointerMove(document, {
    isPrimary: true,
    clientX: 60,
    clientY: ROW_TOP + ROW_HEIGHT * ratio,
  });
}

/** Presses on a row and moves far enough for the drag to begin. */
function pickUp(id: string): void {
  fireEvent.pointerDown(row(id), { isPrimary: true, button: 0, clientX: 40, clientY: 40 });
  fireEvent.pointerMove(document, { isPrimary: true, clientX: 40, clientY: 60 });
}

function release(): void {
  fireEvent.pointerUp(document, { isPrimary: true, clientX: 60, clientY: ROW_TOP });
}

/**
 * What follows a release: the backend's answers, and the tree read again.
 * The stand-in backend answers at once, so all of it is done by the time a
 * timer that was due at once has run, however many steps it takes.
 */
async function settle(): Promise<void> {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

/**
 * The sidebar with the clock stopped (the caller starts and stops it), for
 * what goes by how long an item is held.
 */
async function renderHeld(open: string[]): Promise<void> {
  useSessionStore.setState({ expandedFolderIds: new Set(open) });
  render(<SessionSidebar />);
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0);
  });
  expect(useSessionStore.getState().folders).toHaveLength(stored.folders.length);
}

async function hold(ms: number): Promise<void> {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

function isOpen(id: string): boolean {
  return useSessionStore.getState().expandedFolderIds.has(id);
}

async function dragTo(id: string, target: HTMLElement, ratio = 0.5): Promise<void> {
  pickUp(id);
  moveOver(target, ratio);
  release();
  await settle();
}

async function renderSidebar(open: string[] = ["Lab", "Prod"]): Promise<void> {
  useSessionStore.setState({ expandedFolderIds: new Set(open) });
  render(<SessionSidebar />);
  await waitFor(() => {
    expect(useSessionStore.getState().folders).toHaveLength(stored.folders.length);
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  stored = { folders: initialFolders, sessions: initialSessions };
  changes = [];
  refused = null;
  useAppStore.setState({ dbBackend: "sqlite", pgUser: null });
  useToastStore.setState({ toasts: [] });
  useSessionStore.setState({
    folders: [],
    sessions: [],
    expandedFolderIds: new Set<string>(),
    selectedItemId: null,
    selectedItemType: null,
    searchQuery: "",
    searchResults: null,
  });
  vi.mocked(invoke).mockImplementation(backend);
});

afterEach(() => {
  vi.restoreAllMocks();
  useAppStore.setState({ dbBackend: "sqlite", pgUser: null });
});

describe("dragging in the sidebar's tree", () => {
  it("lists the tree as the tests take it to be", async () => {
    await renderSidebar();
    expect(rows()).toEqual([
      "0:Lab",
      "1:Racks",
      "1:lab-1",
      "1:lab-2",
      "1:lab-3",
      "0:Prod",
      "1:prod-1",
      "0:Spare",
    ]);
  });

  it("moves a session into the folder it is dropped on, and shows it there", async () => {
    await renderSidebar(["Lab"]);
    await dragTo("lab-2", row("Spare"));

    expect(changes).toEqual([
      { command: "session_move", args: { id: "lab-2", newFolderId: "Spare" } },
    ]);
    // The folder it went into is open, and the session is selected.
    expect(rows()).toEqual([
      "0:Lab",
      "1:Racks",
      "1:lab-1",
      "1:lab-3",
      "0:Prod",
      "0:Spare",
      "1:lab-2",
    ]);
    expect(useSessionStore.getState().selectedItemId).toBe("lab-2");
    expect(useSessionStore.getState().selectedItemType).toBe("session");
  });

  it("moves a session and then puts it in its place, dropped beside another folder's session", async () => {
    stored = { ...stored, sessions: [...initialSessions, session("prod-2", "Prod")] };
    await renderSidebar();
    await dragTo("lab-1", row("prod-2"), 0.2);

    expect(changes).toEqual([
      { command: "session_move", args: { id: "lab-1", newFolderId: "Prod" } },
      {
        command: "session_reorder",
        args: { folderId: "Prod", orderedIds: ["prod-1", "lab-1", "prod-2"] },
      },
    ]);
    expect(rows().slice(-4)).toEqual(["1:prod-1", "1:lab-1", "1:prod-2", "0:Spare"]);
  });

  it("only puts a session in another place, dropped beside one of its own folder", async () => {
    await renderSidebar();
    await dragTo("lab-1", row("lab-3"), 0.8);

    expect(changes).toEqual([
      {
        command: "session_reorder",
        args: { folderId: "Lab", orderedIds: ["lab-2", "lab-3", "lab-1"] },
      },
    ]);
    expect(rows().slice(0, 5)).toEqual(["0:Lab", "1:Racks", "1:lab-2", "1:lab-3", "1:lab-1"]);
  });

  it("moves a folder into the folder whose middle it is dropped on", async () => {
    await renderSidebar(["Lab"]);
    await dragTo("Spare", row("Prod"), 0.5);

    expect(changes).toEqual([
      { command: "folder_move", args: { id: "Spare", newParentId: "Prod" } },
    ]);
    // Prod is open now, with the folder before its session.
    expect(rows().slice(-3)).toEqual(["0:Prod", "1:Spare", "1:prod-1"]);
    expect(useSessionStore.getState().selectedItemId).toBe("Spare");
  });

  it("takes a folder out to the top level, dropped at the edge of a folder there", async () => {
    await renderSidebar(["Lab"]);
    await dragTo("Racks", row("Prod"), 0.1);

    expect(changes).toEqual([
      { command: "folder_move", args: { id: "Racks", newParentId: null } },
      {
        command: "folder_reorder",
        args: { parentId: null, orderedIds: ["Lab", "Racks", "Prod", "Spare"] },
      },
    ]);
    expect(rows().filter((r) => r.startsWith("0:"))).toEqual([
      "0:Lab",
      "0:Racks",
      "0:Prod",
      "0:Spare",
    ]);
  });

  it("takes a folder to the end of the top level, dropped below the rows", async () => {
    await renderSidebar(["Lab"]);
    await dragTo("Racks", tree());

    expect(changes).toEqual([{ command: "folder_move", args: { id: "Racks", newParentId: null } }]);
    expect(rows().slice(-1)).toEqual(["0:Racks"]);
  });

  it("asks the backend for nothing where a drop goes nowhere", async () => {
    await renderSidebar();
    // Into the folder it is in, on itself, a folder into itself, a session
    // to the top level, and beside the tree.
    await dragTo("lab-1", row("Lab"));
    await dragTo("lab-1", row("lab-1"), 0.2);
    await dragTo("Lab", row("Racks"));
    await dragTo("lab-1", tree());
    await dragTo("lab-1", document.body);

    expect(changes).toEqual([]);
    expect(useSessionStore.getState().selectedItemId).toBeNull();
  });

  it("marks where a drop would go while an item is dragged, and nothing afterwards", async () => {
    await renderSidebar();
    expect(preview()).toBeNull();

    pickUp("lab-1");
    // Picked up: the preview is beside where the row was pressed (40, 40),
    // 12 to the right of it and 16 below, until the pointer moves again,
    // and nothing is marked.
    expect(preview()?.textContent).toBe("lab-1");
    expect(preview()?.style.transform).toBe("translate(52px, 56px) scale(1)");
    expect(tree().classList.contains("sidebar-tree-dragging")).toBe(true);
    expect(marks()).toEqual([]);

    moveOver(row("Prod"));
    expect(marks()).toEqual(["into Prod"]);
    expect(preview()?.style.transform).toBe("translate(72px, 226px) scale(1)");
    moveOver(row("prod-1"), 0.2);
    expect(marks()).toEqual(["before prod-1"]);
    moveOver(row("prod-1"), 0.8);
    expect(marks()).toEqual(["after prod-1"]);
    // Where it goes nowhere, nothing is marked.
    moveOver(row("Lab"));
    expect(marks()).toEqual([]);
    moveOver(row("lab-1"));
    expect(marks()).toEqual([]);

    moveOver(row("Spare"));
    expect(marks()).toEqual(["into Spare"]);
    release();
    expect(marks()).toEqual([]);
    expect(preview()).toBeNull();
    expect(tree().classList.contains("sidebar-tree-dragging")).toBe(false);
    await settle();
    expect(changes).toHaveLength(1);
  });

  it("marks the edges and the middle of a folder for a folder, and the end of the top level", async () => {
    await renderSidebar(["Lab"]);
    pickUp("Spare");
    moveOver(row("Prod"), 0.1);
    expect(marks()).toEqual(["before Prod"]);
    moveOver(row("Prod"), 0.5);
    expect(marks()).toEqual(["into Prod"]);
    // Prod is closed, so its lower edge is what follows it: where Spare is.
    moveOver(row("Prod"), 0.9);
    expect(marks()).toEqual([]);
    // Lab is open and lists something: its lower edge is its inside.
    moveOver(row("Lab"), 0.9);
    expect(marks()).toEqual(["into Lab"]);
    // A session's row is part of what its folder lists.
    moveOver(row("lab-2"));
    expect(marks()).toEqual(["into Lab"]);
    release();
    await settle();

    pickUp("Racks");
    moveOver(tree());
    expect(marks()).toEqual(["end"]);
    fireEvent.keyDown(document, { code: "Escape", key: "Escape" });
    expect(marks()).toEqual([]);
  });

  it("scales the preview with the sidebar", async () => {
    const { useSettingsStore } = await import("../stores/settingsStore");
    const before = useSettingsStore.getState().uiScale;
    useSettingsStore.setState({ uiScale: 125 });
    try {
      await renderSidebar();
      pickUp("lab-1");
      // Placed by the pointer's own coordinates, whatever the scale.
      expect(preview()?.style.transform).toBe("translate(52px, 56px) scale(1.25)");
      moveOver(row("Prod"));
      expect(preview()?.style.transform).toBe("translate(72px, 226px) scale(1.25)");
      release();
    } finally {
      useSettingsStore.setState({ uiScale: before });
    }
  });

  it("drops nothing after Escape", async () => {
    await renderSidebar();
    pickUp("lab-1");
    moveOver(row("Spare"));
    expect(marks()).toEqual(["into Spare"]);

    fireEvent.keyDown(document, { code: "Escape", key: "Escape" });
    expect(marks()).toEqual([]);
    expect(preview()).toBeNull();
    release();
    await settle();
    expect(changes).toEqual([]);
  });

  it("begins no drag before the pointer has moved, and a click still selects", async () => {
    await renderSidebar();
    fireEvent.pointerDown(row("lab-1"), { isPrimary: true, button: 0, clientX: 40, clientY: 40 });
    fireEvent.pointerMove(document, { isPrimary: true, clientX: 42, clientY: 42 });
    expect(preview()).toBeNull();
    fireEvent.pointerUp(document, { isPrimary: true, clientX: 42, clientY: 42 });
    fireEvent.click(row("lab-1"));
    expect(useSessionStore.getState().selectedItemId).toBe("lab-1");
    await settle();
    expect(changes).toEqual([]);
  });

  it("picks nothing up on a key", async () => {
    await renderSidebar();
    for (const code of ["Enter", "Space"]) {
      const target = row("lab-1");
      target.focus();
      expect(document.activeElement).toBe(target);
      fireEvent.keyDown(target, { code, key: code === "Space" ? " " : code });
      expect(preview()).toBeNull();
      expect(target.getAttribute("aria-pressed")).not.toBe("true");
      expect(tree().classList.contains("sidebar-tree-dragging")).toBe(false);
    }
    await settle();
    expect(changes).toEqual([]);
  });

  it("opens a closed folder that an item is held over", async () => {
    vi.useFakeTimers();
    try {
      await renderHeld(["Lab"]);
      expect(rows()).not.toContain("1:prod-1");

      pickUp("lab-1");
      moveOver(row("Prod"));
      // Not at once, and not a moment before the time is up.
      expect(isOpen("Prod")).toBe(false);
      await hold(DRAG_OPEN_MS - 1);
      expect(isOpen("Prod")).toBe(false);
      await hold(1);
      expect(isOpen("Prod")).toBe(true);
      expect(rows()).toContain("1:prod-1");
      // What it lists can be dropped beside in the same drag.
      moveOver(row("prod-1"), 0.2);
      expect(marks()).toEqual(["before prod-1"]);
      release();
      await hold(0);
      expect(changes.map((c) => c.command)).toEqual(["session_move", "session_reorder"]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("leaves a folder closed that an item only passes over", async () => {
    vi.useFakeTimers();
    try {
      await renderHeld(["Lab"]);
      pickUp("lab-1");
      moveOver(row("Prod"));
      await hold(DRAG_OPEN_MS - 50);
      moveOver(row("Spare"));
      await hold(DRAG_OPEN_MS - 50);
      expect(isOpen("Prod")).toBe(false);
      expect(isOpen("Spare")).toBe(false);
      // Held over the second one for the whole time, it opens.
      await hold(100);
      expect(isOpen("Spare")).toBe(true);
      expect(isOpen("Prod")).toBe(false);
      fireEvent.keyDown(document, { code: "Escape", key: "Escape" });
    } finally {
      vi.useRealTimers();
    }
  });

  it("opens no folder that the item is no longer held over", async () => {
    vi.useFakeTimers();
    try {
      await renderHeld(["Lab"]);
      // Escape, a release, and a place on the row that does not go into it.
      pickUp("lab-1");
      moveOver(row("Prod"));
      await hold(DRAG_OPEN_MS - 50);
      fireEvent.keyDown(document, { code: "Escape", key: "Escape" });
      await hold(DRAG_OPEN_MS);
      expect(isOpen("Prod")).toBe(false);

      pickUp("Racks");
      moveOver(row("Spare"), 0.5);
      await hold(DRAG_OPEN_MS - 50);
      moveOver(row("Spare"), 0.1);
      expect(marks()).toEqual(["before Spare"]);
      await hold(DRAG_OPEN_MS);
      expect(isOpen("Spare")).toBe(false);
      fireEvent.keyDown(document, { code: "Escape", key: "Escape" });

      // A folder that something else opens or closes meanwhile does not
      // start the time over for the one the item is held over.
      pickUp("lab-1");
      moveOver(row("Prod"));
      await hold(DRAG_OPEN_MS - 50);
      act(() => {
        useSessionStore.getState().expandFolder("Spare");
      });
      await hold(50);
      expect(isOpen("Prod")).toBe(true);
      fireEvent.keyDown(document, { code: "Escape", key: "Escape" });
    } finally {
      vi.useRealTimers();
    }
  });

  it("ends the drag when a dialog opens, and drops nothing under it", async () => {
    await renderSidebar();
    fireEvent.click(row("Spare"));
    pickUp("lab-1");
    moveOver(row("Spare"));
    expect(marks()).toEqual(["into Spare"]);

    // The pointer is held still, and a dialog appears: the one that renames
    // the selected folder, as a host-key prompt could.
    fireEvent.keyDown(tree(), { key: "F2" });
    expect(document.querySelector(".dialog-overlay")).not.toBeNull();
    expect(marks()).toEqual([]);
    expect(preview()).toBeNull();
    expect(tree().classList.contains("sidebar-tree-dragging")).toBe(false);

    // The row that was picked up is as it was before.
    expect(row("lab-1").style.opacity).toBe("");

    // The release where the pointer was, over what was marked.
    release();
    await settle();
    expect(changes).toEqual([]);
    expect(useSessionStore.getState().expandedFolderIds.has("Spare")).toBe(false);
  });

  it("picks nothing up under a dialog", async () => {
    await renderSidebar();
    fireEvent.click(row("Spare"));
    // The dialog opens between the press and the first move.
    fireEvent.pointerDown(row("lab-1"), { isPrimary: true, button: 0, clientX: 40, clientY: 40 });
    fireEvent.keyDown(tree(), { key: "F2" });
    expect(document.querySelector(".dialog-overlay")).not.toBeNull();
    fireEvent.pointerMove(document, { isPrimary: true, clientX: 40, clientY: 60 });
    expect(preview()).toBeNull();
    expect(tree().classList.contains("sidebar-tree-dragging")).toBe(false);
    moveOver(row("Spare"));
    expect(marks()).toEqual([]);
    release();
    await settle();
    expect(changes).toEqual([]);
  });

  it("dims the row that is dragged, and what a dragged folder lists", async () => {
    await renderSidebar();
    pickUp("lab-1");
    expect(row("lab-1").style.opacity).toBe("0.4");
    expect(row("lab-2").style.opacity).toBe("");
    fireEvent.keyDown(document, { code: "Escape", key: "Escape" });
    expect(row("lab-1").style.opacity).toBe("");

    pickUp("Lab");
    // The folder's row and its content are in one dimmed box.
    expect(row("Lab").parentElement?.style.opacity).toBe("0.4");
    expect(row("Lab").parentElement?.contains(row("lab-1"))).toBe(true);
    fireEvent.keyDown(document, { code: "Escape", key: "Escape" });
    expect(row("Lab").parentElement?.style.opacity).toBe("");
  });

  it("goes by what is under the pointer at the release, not at the last move", async () => {
    await renderSidebar();
    pickUp("lab-1");
    moveOver(row("Spare"));
    expect(marks()).toEqual(["into Spare"]);
    // Something else is there by the time the button is let go.
    vi.spyOn(document, "elementFromPoint").mockReturnValue(document.body);
    release();
    await settle();
    expect(changes).toEqual([]);
  });

  it("shows no preview of an item that is gone from the tree while it is held", async () => {
    await renderSidebar();
    pickUp("lab-1");
    moveOver(row("Spare"));
    expect(preview()?.textContent).toBe("lab-1");
    // Deleted elsewhere, and the tree read again.
    stored = { ...stored, sessions: stored.sessions.filter((s) => s.id !== "lab-1") };
    await act(async () => {
      await useSessionStore.getState().loadAll();
    });
    expect(preview()).toBeNull();
    expect(marks()).toEqual([]);
    release();
    await settle();
    expect(changes).toEqual([]);
  });

  it("tells what the backend refuses, and shows the tree as it is", async () => {
    refused = { command: "session_move", message: "You can only move sessions you own" };
    await renderSidebar(["Lab"]);
    await dragTo("lab-2", row("Spare"));

    expect(changes.map((c) => c.command)).toEqual(["session_move"]);
    expect(useToastStore.getState().toasts.map((toast) => toast.message)).toEqual([
      "You can only move sessions you own",
    ]);
    expect(rows()).toEqual([
      "0:Lab",
      "1:Racks",
      "1:lab-1",
      "1:lab-2",
      "1:lab-3",
      "0:Prod",
      "0:Spare",
    ]);
    expect(useSessionStore.getState().selectedItemId).toBeNull();
  });

  it("tells the first refusal when the tree cannot be read again either", async () => {
    stored = { ...stored, sessions: [...initialSessions, session("prod-2", "Prod")] };
    refused = { command: "session_reorder", message: "Failed to reorder sessions" };
    await renderSidebar();
    vi.mocked(invoke).mockImplementation((command: string, args?: unknown) => {
      // From the refusal on, the lists cannot be read.
      const lists = command === "folder_list" || command === "session_list_all";
      if (lists && changes.some((c) => c.command === "session_reorder")) {
        return Promise.reject(new Error("Failed to list"));
      }
      return backend(command, args);
    });
    await dragTo("lab-1", row("prod-1"), 0.2);

    expect(changes.map((c) => c.command)).toEqual(["session_move", "session_reorder"]);
    expect(useToastStore.getState().toasts.map((toast) => toast.message)).toEqual([
      "Failed to reorder sessions",
    ]);
  });

  it("shows a session where the move left it when its place is refused", async () => {
    stored = { ...stored, sessions: [...initialSessions, session("prod-2", "Prod")] };
    refused = { command: "session_reorder", message: "Failed to reorder sessions" };
    await renderSidebar();
    await dragTo("lab-1", row("prod-1"), 0.2);

    expect(changes.map((c) => c.command)).toEqual(["session_move", "session_reorder"]);
    expect(useToastStore.getState().toasts.map((toast) => toast.message)).toEqual([
      "Failed to reorder sessions",
    ]);
    // In the folder, at its end.
    expect(rows().slice(-4)).toEqual(["1:prod-1", "1:prod-2", "1:lab-1", "0:Spare"]);
  });

  it("takes the next drop once the one before it is done, and none before", async () => {
    // The backend takes its time over the first move.
    let answer: (() => void) | undefined;
    vi.mocked(invoke).mockImplementation((command: string, args?: unknown) => {
      if (command !== "session_move" || answer) return backend(command, args);
      return new Promise((resolve) => {
        answer = () => {
          resolve(backend(command, args));
        };
      });
    });
    await renderSidebar();
    await dragTo("lab-1", row("Spare"));
    expect(changes).toEqual([]);
    // Meanwhile nothing is picked up: a drop would be judged by a tree that
    // is about to change.
    pickUp("lab-2");
    expect(preview()).toBeNull();
    moveOver(row("Spare"));
    expect(marks()).toEqual([]);
    release();
    await settle();
    expect(changes).toEqual([]);

    answer?.();
    await settle();
    expect(changes.map((c) => c.args)).toEqual([{ id: "lab-1", newFolderId: "Spare" }]);
    // The next one is taken.
    await dragTo("lab-2", row("Spare"));
    expect(changes.map((c) => c.args)).toEqual([
      { id: "lab-1", newFolderId: "Spare" },
      { id: "lab-2", newFolderId: "Spare" },
    ]);
  });

  it("drops nothing on what a search lists", async () => {
    await renderSidebar();
    pickUp("lab-1");
    moveOver(row("Spare"));
    expect(marks()).toEqual(["into Spare"]);
    // The results take the tree's place while the item is held: Prod, which
    // was found, with what is in it.
    act(() => {
      useSessionStore.setState({
        searchQuery: "prod",
        searchResults: { folders: [stored.folders[2]], sessions: [] },
      });
    });
    expect(rows()).toEqual(["0:Prod", "1:prod-1"]);
    // The folder that was found, and a row of what it lists, which is a row
    // as the tree's are.
    moveOver(row("Prod"));
    expect(marks()).toEqual([]);
    expect(row("prod-1").dataset.itemType).toBe("session");
    moveOver(row("prod-1"), 0.2);
    expect(marks()).toEqual([]);
    release();
    await settle();
    expect(changes).toEqual([]);
  });
});

describe("dragging in PostgreSQL mode", () => {
  beforeEach(() => {
    stored = {
      folders: [folder("Mine", null, "alice"), folder("Theirs", null, "bob")],
      sessions: [session("mine-1", "Mine", "alice"), session("theirs-1", "Theirs", "bob")],
    };
    useAppStore.setState({ dbBackend: "postgres", pgUser: "alice" });
  });

  it("picks up the user's own items, and no one else's", async () => {
    await renderSidebar(["Mine", "Theirs"]);
    for (const id of ["Theirs", "theirs-1"]) {
      pickUp(id);
      expect(preview()).toBeNull();
      expect(tree().classList.contains("sidebar-tree-dragging")).toBe(false);
      release();
    }
    for (const id of ["Mine", "mine-1"]) {
      pickUp(id);
      expect(preview()?.textContent).toBe(id);
      release();
    }
    // No row says of itself that it is disabled, or anything but a tree item.
    for (const id of ["Mine", "mine-1", "Theirs", "theirs-1"]) {
      expect(row(id).getAttribute("role")).toBe("treeitem");
      expect(row(id).getAttribute("aria-roledescription")).toBeNull();
      expect(row(id).getAttribute("aria-disabled")).toBeNull();
    }
  });

  it("marks no place in another user's folder, and drops nothing there", async () => {
    await renderSidebar(["Mine", "Theirs"]);
    pickUp("mine-1");
    moveOver(row("Theirs"));
    expect(marks()).toEqual([]);
    moveOver(row("theirs-1"), 0.2);
    expect(marks()).toEqual([]);
    release();
    await settle();
    expect(changes).toEqual([]);
  });

  it("puts an item beside another user's row at the top level", async () => {
    stored = { ...stored, folders: [...stored.folders, folder("Mine too", null, "alice")] };
    await renderSidebar([]);
    expect(rows()).toEqual(["0:Mine", "0:Theirs", "0:Mine too"]);
    pickUp("Mine too");
    // Its own place, after Theirs, is no place to go.
    moveOver(row("Theirs"), 0.9);
    expect(marks()).toEqual([]);
    moveOver(row("Theirs"), 0.1);
    expect(marks()).toEqual(["before Theirs"]);
    release();
    await settle();
    expect(changes).toEqual([
      {
        command: "folder_reorder",
        args: { parentId: null, orderedIds: ["Mine", "Mine too", "Theirs"] },
      },
    ]);
  });

  it("picks nothing up while the role is not known", async () => {
    useAppStore.setState({ pgUser: null });
    await renderSidebar(["Mine"]);
    pickUp("mine-1");
    expect(preview()).toBeNull();
    expect(tree().classList.contains("sidebar-tree-dragging")).toBe(false);
    moveOver(row("Mine"));
    expect(marks()).toEqual([]);
    release();
    await settle();
    expect(changes).toEqual([]);
  });
});

describe("moving an item without a pointer", () => {
  function menuItem(label: string): HTMLButtonElement {
    const found = screen.getByText(label).closest("button");
    if (!found) throw new Error(`no menu item ${label}`);
    return found;
  }

  function openMenu(id: string): void {
    fireEvent.contextMenu(row(id), { clientX: 40, clientY: 40 });
  }

  function altKey(key: "ArrowUp" | "ArrowDown", more: Partial<KeyboardEventInit> = {}): void {
    fireEvent.keyDown(tree(), { key, altKey: true, ...more });
  }

  it("puts the selected session one place down on Alt+Down, and keeps it selected", async () => {
    await renderSidebar();
    fireEvent.click(row("lab-1"));
    altKey("ArrowDown");
    await settle();

    expect(changes).toEqual([
      {
        command: "session_reorder",
        args: { folderId: "Lab", orderedIds: ["lab-2", "lab-1", "lab-3"] },
      },
    ]);
    expect(rows().slice(0, 5)).toEqual(["0:Lab", "1:Racks", "1:lab-2", "1:lab-1", "1:lab-3"]);
    expect(useSessionStore.getState().selectedItemId).toBe("lab-1");
  });

  it("puts the selected folder one place up on Alt+Up", async () => {
    await renderSidebar([]);
    fireEvent.click(row("Spare"));
    altKey("ArrowUp");
    await settle();

    expect(changes).toEqual([
      {
        command: "folder_reorder",
        args: { parentId: null, orderedIds: ["Lab", "Spare", "Prod"] },
      },
    ]);
    expect(rows()).toEqual(["0:Lab", "0:Spare", "0:Prod"]);
    expect(useSessionStore.getState().selectedItemId).toBe("Spare");
  });

  it("asks for nothing beyond the first or the last, and moves no selection there", async () => {
    await renderSidebar();
    fireEvent.click(row("lab-1"));
    altKey("ArrowUp");
    expect(useSessionStore.getState().selectedItemId).toBe("lab-1");
    fireEvent.click(row("lab-3"));
    altKey("ArrowDown");
    expect(useSessionStore.getState().selectedItemId).toBe("lab-3");
    await settle();
    expect(changes).toEqual([]);
  });

  it("asks for nothing with another modifier held, or with nothing selected", async () => {
    await renderSidebar();
    for (const more of [{ ctrlKey: true }, { shiftKey: true }, { metaKey: true }]) {
      fireEvent.click(row("lab-1"));
      altKey("ArrowDown", more);
    }
    act(() => {
      useSessionStore.setState({ selectedItemId: null, selectedItemType: null });
    });
    altKey("ArrowDown");
    await settle();
    expect(changes).toEqual([]);
  });

  it("asks for one step at a time", async () => {
    await renderSidebar();
    fireEvent.click(row("lab-1"));
    altKey("ArrowDown");
    altKey("ArrowDown");
    await settle();
    expect(changes).toEqual([
      {
        command: "session_reorder",
        args: { folderId: "Lab", orderedIds: ["lab-2", "lab-1", "lab-3"] },
      },
    ]);
  });

  it("still moves the selection on an arrow without Alt", async () => {
    await renderSidebar();
    fireEvent.click(row("lab-1"));
    fireEvent.keyDown(tree(), { key: "ArrowDown" });
    await settle();
    expect(changes).toEqual([]);
    expect(useSessionStore.getState().selectedItemId).toBe("lab-2");
  });

  it("moves nothing in what a search lists", async () => {
    await renderSidebar();
    act(() => {
      useSessionStore.setState({
        searchQuery: "lab",
        searchResults: { folders: [], sessions: initialSessions.slice(0, 3) },
      });
    });
    fireEvent.click(row("lab-1"));
    expect(useSessionStore.getState().selectedItemId).toBe("lab-1");
    altKey("ArrowDown");
    await settle();
    expect(changes).toEqual([]);
    openMenu("lab-2");
    expect(menuItem("contextMenu.moveUp").disabled).toBe(true);
    expect(menuItem("contextMenu.moveDown").disabled).toBe(true);
  });

  it("offers Move Up and Move Down in the context menu, greyed out where it cannot go", async () => {
    await renderSidebar();
    openMenu("lab-1");
    expect(menuItem("contextMenu.moveUp").disabled).toBe(true);
    expect(menuItem("contextMenu.moveDown").disabled).toBe(false);
    fireEvent.click(menuItem("contextMenu.moveDown"));
    await settle();

    expect(changes).toEqual([
      {
        command: "session_reorder",
        args: { folderId: "Lab", orderedIds: ["lab-2", "lab-1", "lab-3"] },
      },
    ]);
    expect(useSessionStore.getState().selectedItemId).toBe("lab-1");

    openMenu("Prod");
    expect(menuItem("contextMenu.moveUp").disabled).toBe(false);
    expect(menuItem("contextMenu.moveDown").disabled).toBe(false);
    fireEvent.click(menuItem("contextMenu.moveUp"));
    await settle();
    expect(changes[1]).toEqual({
      command: "folder_reorder",
      args: { parentId: null, orderedIds: ["Prod", "Lab", "Spare"] },
    });
  });

  it("tells what the backend refuses", async () => {
    refused = { command: "session_reorder", message: "Failed to reorder sessions" };
    await renderSidebar();
    fireEvent.click(row("lab-2"));
    altKey("ArrowUp");
    await settle();
    expect(useToastStore.getState().toasts.map((toast) => toast.message)).toEqual([
      "Failed to reorder sessions",
    ]);
  });

  describe("in PostgreSQL mode", () => {
    beforeEach(() => {
      stored = {
        folders: [
          folder("Mine", null, "alice"),
          { ...folder("Theirs", null, "bob"), visibility: "shared" },
          folder("Mine too", null, "alice"),
        ],
        sessions: [
          session("mine-1", "Mine", "alice"),
          { ...session("theirs-1", "Theirs", "bob"), visibility: "shared" },
          { ...session("theirs-2", "Theirs", "bob"), visibility: "shared" },
        ],
      };
      useAppStore.setState({ dbBackend: "postgres", pgUser: "alice" });
    });

    it("steps past another user's folder, and moves none of theirs", async () => {
      await renderSidebar([]);
      fireEvent.click(row("Theirs"));
      altKey("ArrowDown");
      await settle();
      expect(changes).toEqual([]);

      fireEvent.click(row("Mine too"));
      altKey("ArrowUp");
      await settle();
      expect(changes).toEqual([
        {
          command: "folder_reorder",
          args: { parentId: null, orderedIds: ["Mine", "Mine too", "Theirs"] },
        },
      ]);
    });

    it("offers no step for another user's items, and takes none from the keys", async () => {
      await renderSidebar(["Theirs"]);
      openMenu("theirs-1");
      expect(screen.getByText("contextMenu.connect")).not.toBeNull();
      expect(screen.queryByText("contextMenu.moveUp")).toBeNull();
      expect(screen.queryByText("contextMenu.moveDown")).toBeNull();
      fireEvent.keyDown(document, { code: "Escape", key: "Escape" });

      openMenu("Theirs");
      expect(screen.queryByText("contextMenu.moveUp")).toBeNull();
      expect(screen.queryByText("contextMenu.moveDown")).toBeNull();
      fireEvent.keyDown(document, { code: "Escape", key: "Escape" });

      fireEvent.click(row("theirs-1"));
      altKey("ArrowDown");
      await settle();
      expect(changes).toEqual([]);
    });
  });
});
