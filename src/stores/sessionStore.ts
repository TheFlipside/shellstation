import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { useSettingsStore } from "./settingsStore";
import { useTerminalStore } from "./terminalStore";

export interface Folder {
  id: string;
  name: string;
  parent_id: string | null;
}

export interface Session {
  id: string;
  folder_id: string;
  name: string;
  hostname: string;
  port: number;
  protocol: string;
  username: string;
  auth_method: string;
  jump_host_id: string | null;
  tags: string;
  icon: string;
}

interface SessionState {
  folders: Folder[];
  sessions: Session[];
  expandedFolderIds: Set<string>;
  selectedItemId: string | null;
  selectedItemType: "folder" | "session" | null;
  searchQuery: string;
  searchResults: Session[] | null;
  lastFingerprint: string;

  loadAll: () => Promise<void>;
  checkForUpdates: () => Promise<void>;

  // Folders
  createFolder: (name: string, parentId: string | null) => Promise<void>;
  renameFolder: (id: string, name: string) => Promise<void>;
  moveFolder: (id: string, newParentId: string | null) => Promise<void>;
  deleteFolder: (id: string) => Promise<void>;

  // Sessions
  createSession: (params: CreateSessionParams) => Promise<void>;
  updateSession: (id: string, params: UpdateSessionParams) => Promise<void>;
  moveSession: (id: string, newFolderId: string) => Promise<void>;
  deleteSession: (id: string) => Promise<void>;
  searchSessions: (query: string) => Promise<void>;
  clearSearch: () => void;

  // UI
  toggleFolder: (id: string) => void;
  selectItem: (id: string, type: "folder" | "session") => void;
  clearSelection: () => void;

  // Connect
  connectSession: (id: string) => Promise<void>;
}

export interface CreateSessionParams {
  folderId: string;
  name: string;
  hostname: string;
  port: number;
  protocol?: string;
  username: string;
  authMethod: string;
  tags: string;
  icon: string;
  jumpHostId?: string;
  password?: string;
  keyPath?: string;
}

export interface UpdateSessionParams {
  name?: string;
  hostname?: string;
  port?: number;
  protocol?: string;
  username?: string;
  authMethod?: string;
  tags?: string;
  icon?: string;
  jumpHostId?: string | null;
  password?: string;
  keyPath?: string;
}

export const useSessionStore = create<SessionState>((set, get) => ({
  folders: [],
  sessions: [],
  expandedFolderIds: new Set<string>(
    JSON.parse(localStorage.getItem("shellstation:expandedFolders") ?? "[]") as string[],
  ),
  selectedItemId: null,
  selectedItemType: null,
  searchQuery: "",
  searchResults: null,
  lastFingerprint: "",

  loadAll: async () => {
    const [folders, sessions] = await Promise.all([
      invoke<Folder[]>("folder_list"),
      invoke<Session[]>("session_list_all"),
    ]);
    // Prune expanded folder IDs that no longer exist (e.g. deleted externally)
    const folderIds = new Set(folders.map((f) => f.id));
    const expanded = get().expandedFolderIds;
    let pruned = false;
    const next = new Set<string>();
    for (const id of expanded) {
      if (folderIds.has(id)) {
        next.add(id);
      } else {
        pruned = true;
      }
    }
    if (pruned) {
      localStorage.setItem("shellstation:expandedFolders", JSON.stringify([...next]));
    }
    set({ folders, sessions, expandedFolderIds: pruned ? next : expanded });
  },

  checkForUpdates: async () => {
    const fp = await invoke<{ hash: string }>("session_data_fingerprint");
    const prev = get().lastFingerprint;
    if (fp.hash !== prev) {
      set({ lastFingerprint: fp.hash });
      await get().loadAll();
    }
  },

  // ── Folders ──────────────────────────────────────────────────────────

  createFolder: async (name, parentId) => {
    await invoke("folder_create", { name, parentId });
    await get().loadAll();
  },

  renameFolder: async (id, name) => {
    await invoke("folder_rename", { id, name });
    await get().loadAll();
  },

  moveFolder: async (id, newParentId) => {
    await invoke("folder_move", { id, newParentId });
    await get().loadAll();
  },

  deleteFolder: async (id) => {
    await invoke("folder_delete", { id });
    // Clean up persisted expand state for deleted folder
    set((state) => {
      const next = new Set(state.expandedFolderIds);
      next.delete(id);
      localStorage.setItem("shellstation:expandedFolders", JSON.stringify([...next]));
      return { expandedFolderIds: next };
    });
    await get().loadAll();
  },

  // ── Sessions ─────────────────────────────────────────────────────────

  createSession: async (params) => {
    await invoke("session_create", {
      folderId: params.folderId,
      name: params.name,
      hostname: params.hostname,
      port: params.port,
      protocol: params.protocol ?? "ssh",
      username: params.username,
      authMethod: params.authMethod,
      tags: params.tags,
      icon: params.icon,
      jumpHostId: params.jumpHostId ?? null,
      password: params.password ?? null,
      keyPath: params.keyPath ?? null,
    });
    await get().loadAll();
  },

  updateSession: async (id, params) => {
    await invoke("session_update", {
      id,
      name: params.name ?? null,
      hostname: params.hostname ?? null,
      port: params.port ?? null,
      protocol: params.protocol ?? null,
      username: params.username ?? null,
      authMethod: params.authMethod ?? null,
      tags: params.tags ?? null,
      icon: params.icon ?? null,
      jumpHostId: params.jumpHostId !== undefined ? params.jumpHostId : null,
      password: params.password ?? null,
      keyPath: params.keyPath ?? null,
    });
    await get().loadAll();
  },

  moveSession: async (id, newFolderId) => {
    await invoke("session_move", { id, newFolderId });
    await get().loadAll();
  },

  deleteSession: async (id) => {
    await invoke("session_delete", { id });
    await get().loadAll();
  },

  searchSessions: async (query) => {
    set({ searchQuery: query });
    if (!query.trim()) {
      set({ searchResults: null });
      return;
    }
    const results = await invoke<Session[]>("session_search", { query });
    set({ searchResults: results });
  },

  clearSearch: () => {
    set({ searchQuery: "", searchResults: null });
  },

  // ── UI ─────────────────────────────────────────────────────────────

  toggleFolder: (id) => {
    set((state) => {
      const next = new Set(state.expandedFolderIds);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      localStorage.setItem("shellstation:expandedFolders", JSON.stringify([...next]));
      return { expandedFolderIds: next };
    });
  },

  selectItem: (id, type_) => {
    set({ selectedItemId: id, selectedItemType: type_ });
  },

  clearSelection: () => {
    set({ selectedItemId: null, selectedItemType: null });
  },

  // ── Connect ────────────────────────────────────────────────────────

  connectSession: async (id) => {
    const session = get().sessions.find((s) => s.id === id);
    if (!session) return;

    const { restrictPrivateIps } = useSettingsStore.getState();
    const connId = await invoke<string>("session_connect", {
      id,
      cols: 80,
      rows: 24,
      restrictPrivateIps,
    });

    const isTelnet = session.protocol === "telnet";
    const tabType = isTelnet ? "telnet" : "ssh";
    const tabTitle = session.name;
    const meta = isTelnet
      ? { host: session.hostname, port: session.port }
      : { host: session.hostname, username: session.username };

    useTerminalStore.getState().addTab(connId, tabTitle, tabType, meta, session.id);
  },
}));
