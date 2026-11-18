/**
 * The sidebar's search, from the field to the list.
 *
 * `session_search` returns the folders it found with the sessions. The
 * contract pinned here: the sidebar lists a folder that was found with what
 * is in it and then the sessions, leaves out a folder that the tree does not
 * show, opens and closes a folder that was found on Enter in the results and
 * not in the tree, keeps it closed while the search is on, and starts the
 * next search after the field was cleared with every folder open again.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { useSessionStore, type Folder, type Session } from "../stores/sessionStore";
import { SessionSidebar } from "./SessionSidebar";

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

const customers = folder("f-customers", "Customers");
const production = folder("f-production", "Production", customers.id);
const lab = folder("f-lab", "Lab");
// A folder whose parent is not among those the user can see: the tree does
// not show it.
const adrift = folder("f-adrift", "Production spare", "f-not-visible");
const folders = [customers, production, lab, adrift];

const web = session("s-web", "web", production.id);
const labProd = session("s-lab-prod", "lab-prod", lab.id);
const spare = session("s-spare", "prod-spare", adrift.id);
const sessions = [web, labProd, spare];

/** What the backend has just now; a test can put something else there. */
let stored: { folders: Folder[]; sessions: Session[] } = { folders, sessions };

/** What the backend finds: by the name, whatever the case. */
function searched(query: string): { folders: Folder[]; sessions: Session[] } {
  const q = query.toLowerCase();
  return {
    folders: stored.folders.filter((f) => f.name.toLowerCase().includes(q)),
    sessions: stored.sessions.filter((s) => s.name.toLowerCase().includes(q)),
  };
}

const searches: string[] = [];

function rows(): string[] {
  return screen.getAllByRole("treeitem").map((row) => {
    const depth = row.style.getPropertyValue("--tree-depth") || "0";
    const label = row.querySelector(".tree-label")?.textContent ?? "";
    return `${depth}:${label}`;
  });
}

function field(): HTMLInputElement {
  return screen.getByPlaceholderText<HTMLInputElement>("sidebar.searchPlaceholder");
}

async function search(query: string): Promise<void> {
  const before = searches.length;
  fireEvent.change(field(), { target: { value: query } });
  await waitFor(() => {
    expect(searches.length).toBeGreaterThan(before);
  });
  // The answer, and the list made from it.
  await act(async () => {
    await Promise.resolve();
  });
}

async function renderSidebar(): Promise<void> {
  render(<SessionSidebar />);
  await waitFor(() => {
    expect(useSessionStore.getState().folders).toHaveLength(folders.length);
  });
}

function pressInTree(key: string): void {
  fireEvent.keyDown(screen.getByRole("tree"), { key });
}

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  searches.length = 0;
  stored = { folders, sessions };
  useSessionStore.setState({
    folders: [],
    sessions: [],
    expandedFolderIds: new Set<string>(),
    selectedItemId: null,
    selectedItemType: null,
    searchQuery: "",
    searchResults: null,
  });
  vi.mocked(invoke).mockImplementation((command: string, args?: unknown) => {
    switch (command) {
      case "folder_list":
        return Promise.resolve(stored.folders);
      case "session_list_all":
        return Promise.resolve(stored.sessions);
      case "session_search": {
        const { query } = args as { query: string };
        searches.push(query);
        return Promise.resolve(searched(query));
      }
      default:
        // The lists of profiles and sequences, and whatever else is asked.
        return Promise.resolve([]);
    }
  });
});

describe("the sidebar's search", () => {
  it("lists a folder that was found with what is in it, and then the sessions", async () => {
    await renderSidebar();
    // The tree, with every folder closed.
    expect(rows()).toEqual(["0:Customers", "0:Lab"]);

    await search("prod");
    expect(searches).toEqual(["prod"]);
    // The folder that was found, open though it is closed in the tree, and
    // the session that was found by its own name. Neither the folder the
    // tree does not show nor the session in it.
    expect(rows()).toEqual(["0:Production", "1:web", "0:lab-prod"]);
    expect(document.querySelector(".tree-path")?.textContent).toContain("Customers");
    expect(useSessionStore.getState().expandedFolderIds.size).toBe(0);
  });

  it("opens and closes a folder that was found on Enter, in the results", async () => {
    await renderSidebar();
    await search("prod");

    fireEvent.click(screen.getByText("Production"));
    expect(useSessionStore.getState().selectedItemId).toBe(production.id);
    pressInTree("Enter");
    expect(rows()).toEqual(["0:Production", "0:lab-prod"]);
    pressInTree("Enter");
    expect(rows()).toEqual(["0:Production", "1:web", "0:lab-prod"]);
    // The tree's own open folders are as they were.
    expect(useSessionStore.getState().expandedFolderIds.size).toBe(0);
    expect(localStorage.getItem("shellstation:expandedFolders")).toBeNull();
  });

  it("keeps a folder closed while the search is on, and not into the next one", async () => {
    await renderSidebar();
    await search("prod");
    fireEvent.click(screen.getByText("Production"));
    pressInTree("Enter");
    expect(rows()).toEqual(["0:Production", "0:lab-prod"]);

    // Another query that finds it: still closed.
    await search("produ");
    expect(rows()).toEqual(["0:Production"]);

    // The field cleared: the tree again, as it was.
    fireEvent.change(field(), { target: { value: "" } });
    expect(useSessionStore.getState().searchResults).toBeNull();
    expect(rows()).toEqual(["0:Customers", "0:Lab"]);

    // And the next search starts with the folder open.
    await search("prod");
    expect(rows()).toEqual(["0:Production", "1:web", "0:lab-prod"]);
  });

  it("opens a folder of the tree in the tree on Enter, as before", async () => {
    await renderSidebar();

    fireEvent.click(screen.getByText("Lab"));
    pressInTree("Enter");
    expect(useSessionStore.getState().expandedFolderIds.has(lab.id)).toBe(true);
    expect(rows()).toEqual(["0:Customers", "0:Lab", "1:lab-prod"]);
  });

  it("searches again after a change, and lists what is found then", async () => {
    await renderSidebar();
    await search("lab");
    expect(rows()).toEqual(["0:Lab", "1:lab-prod", "0:lab-prod"]);

    // Someone renames the folder and adds a session to it. A refresh reads
    // everything and runs the search once more.
    const renamed = { ...lab, name: "Laboratory" };
    const added = session("s-lab-two", "lab-two", lab.id);
    stored = {
      folders: [customers, production, renamed, adrift],
      sessions: [...sessions, added],
    };
    await act(async () => {
      await useSessionStore.getState().loadAll();
    });
    expect(searches).toEqual(["lab", "lab"]);
    expect(useSessionStore.getState().searchResults).toEqual({
      folders: [renamed],
      sessions: [labProd, added],
    });
    expect(rows()).toEqual(["0:Laboratory", "1:lab-prod", "1:lab-two", "0:lab-prod", "0:lab-two"]);
  });
});
