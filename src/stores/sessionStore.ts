import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { useAppStore } from "./appStore";
import { useTerminalStore, type TabMeta } from "./terminalStore";
import { invokeConnect } from "../page";

export interface Folder {
  id: string;
  name: string;
  parent_id: string | null;
  sort_order: number;
  owner: string;
  visibility: "personal" | "shared";
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
  sort_order: number;
  highlight_profile_id: string | null;
  credential_profile_id: string | null;
  login_sequence_id: string | null;
  legacy_algorithms: boolean;
  owner: string;
  visibility: "personal" | "shared";
}

export function sessionHasTag(session: Session, tag: string): boolean {
  try {
    const parsed: unknown = JSON.parse(session.tags || "[]");
    return (
      Array.isArray(parsed) &&
      parsed.some((t) => typeof t === "string" && t.toLowerCase() === tag.toLowerCase())
    );
  } catch {
    return false;
  }
}

/**
 * Whether a session is offered as a jump host: an SSH session tagged
 * `jumphost`.
 *
 * In PostgreSQL mode it must also be one of the user's own sessions. A
 * session's jump host has to be a session of the same owner, the dialogs
 * that set one are for the user's own sessions, and the backend and the
 * database refuse any other. `pgUser` is the role the app connected as, or
 * `null` when the app does not run against PostgreSQL; as long as that role
 * is not known, no session is taken for the user's own.
 */
export function isJumpHostCandidate(
  session: Session,
  pgMode: boolean,
  pgUser: string | null,
): boolean {
  if (session.protocol !== "ssh") return false;
  if (pgMode && session.owner !== pgUser) return false;
  return sessionHasTag(session, "jumphost");
}

/**
 * The sessions the session dialog lists as jump hosts: the candidates, without
 * the session being edited (`editedId`), which is no jump host for itself.
 *
 * The jump host that is set (`assignedId`, empty for none) stays in the list
 * even if it is no candidate: one that lost its tag, or, in PostgreSQL mode,
 * another user's session named before that was refused. The user sees what is
 * set and can change it. Saving the session with it unchanged is not refused;
 * picking it for another session is.
 */
export function jumpHostChoices(
  sessions: Session[],
  editedId: string | undefined,
  assignedId: string,
  pgMode: boolean,
  pgUser: string | null,
): Session[] {
  return sessions.filter((s) => {
    if (s.id === editedId) return false;
    if (s.id === assignedId) return s.protocol === "ssh";
    return isJumpHostCandidate(s, pgMode, pgUser);
  });
}

/**
 * The jump host a copy of `session` starts with: the session's own, or none.
 *
 * In PostgreSQL mode a session can still name another user's session from
 * before that was refused. The copy is a new session, for which it is
 * refused, so the copy starts without it. So does the copy of a session whose
 * jump host is not among `sessions`.
 */
export function jumpHostForCopy(
  session: Session,
  sessions: Session[],
  pgMode: boolean,
  pgUser: string | null,
): string | null {
  if (!pgMode || session.jump_host_id === null) return session.jump_host_id;
  const jumpHost = sessions.find((s) => s.id === session.jump_host_id);
  return jumpHost?.owner === pgUser ? session.jump_host_id : null;
}

/** As many jump hosts as the backend connects through (`MAX_JUMP_HOPS`). */
const MAX_JUMP_HOPS = 10;

export interface JumpHostChain {
  /** The jump hosts of a session, the one it names first. */
  hops: Session[];
  /**
   * Whether that is the whole chain. It is not where the chain goes on to a
   * session that is not among those the user can see, to one of another
   * owner than the session naming it, back to a session it has been through,
   * or beyond what the backend connects through. A connect is refused at such
   * a link.
   */
  complete: boolean;
}

/**
 * The jump hosts that connecting to `session` goes through, as far as the
 * user can connect through them.
 *
 * This is for another user's shared session in PostgreSQL mode, whose jump
 * hosts are that user's sessions too, link by link. The connecting user needs
 * a credential profile of their own for each, so the dialog that assigns them
 * lists these. It follows the rule the backend connects by
 * (`jump_host_is_its_owners`), and stops where the backend refuses.
 */
export function jumpHostChain(session: Session, sessions: Session[]): JumpHostChain {
  const hops: Session[] = [];
  // As the backend has it: only a Telnet session goes through none.
  if (session.protocol === "telnet") return { hops, complete: true };
  // The session itself counts as been through: a chain that comes back to it
  // is refused, and it is not listed as its own jump host. (The backend lists
  // it once more before it refuses; the refusal is the same.)
  const seen = new Set<string>([session.id]);
  let current = session;
  while (current.jump_host_id !== null) {
    const jumpHostId = current.jump_host_id;
    const next = sessions.find((s) => s.id === jumpHostId);
    // No session of that ID among them counts as one of another owner.
    if (next?.owner !== current.owner || seen.has(next.id) || hops.length >= MAX_JUMP_HOPS) {
      return { hops, complete: false };
    }
    seen.add(next.id);
    hops.push(next);
    current = next;
  }
  return { hops, complete: true };
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

  // Bulk operations
  folderApplyCredentialProfile: (folderId: string, profileId: string | null) => Promise<number>;
  folderApplyLoginSequence: (folderId: string, sequenceId: string | null) => Promise<number>;
  folderBulkEditSessions: (folderId: string, edit: BulkSessionEdit) => Promise<number>;

  // Reordering
  reorderFolders: (parentId: string | null, orderedIds: string[]) => Promise<void>;
  reorderSessions: (folderId: string, orderedIds: string[]) => Promise<void>;
  sortFolderAlphabetically: (parentId: string | null, recursive?: boolean) => Promise<void>;
  sortSessionsAlphabetically: (folderId: string) => Promise<void>;
  sortSessionsByHostname: (folderId: string) => Promise<void>;

  // UI
  toggleFolder: (id: string) => void;
  expandFolder: (id: string) => void;
  collapseFolder: (id: string) => void;
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
  username?: string;
  tags: string;
  icon: string;
  jumpHostId?: string | null;
  highlightProfileId?: string | null;
  credentialProfileId?: string | null;
  loginSequenceId?: string | null;
  legacyAlgorithms?: boolean;
}

export interface BulkSessionEdit {
  /** undefined = leave alone; null = clear; string = set */
  jumpHostId?: string | null;
  /** undefined = leave alone; null = clear; string = set */
  highlightProfileId?: string | null;
  /** undefined = leave alone; null = clear; string = set */
  loginSequenceId?: string | null;
  icon?: string;
}

export interface UpdateSessionParams {
  name?: string;
  hostname?: string;
  port?: number;
  protocol?: string;
  username?: string;
  tags?: string;
  icon?: string;
  jumpHostId?: string | null;
  highlightProfileId?: string | null;
  credentialProfileId?: string | null;
  loginSequenceId?: string | null;
  legacyAlgorithms?: boolean;
}

export const useSessionStore = create<SessionState>((set, get) => ({
  folders: [],
  sessions: [],
  expandedFolderIds: new Set<string>(
    (() => {
      try {
        const parsed: unknown = JSON.parse(
          localStorage.getItem("shellstation:expandedFolders") ?? "[]",
        );
        return Array.isArray(parsed)
          ? parsed.filter((x): x is string => typeof x === "string")
          : [];
      } catch {
        return [];
      }
    })(),
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
    const query = get().searchQuery;
    if (query.trim()) {
      try {
        const results = await invoke<Session[]>("session_search", { query });
        set({ searchResults: results });
      } catch {
        set({ searchResults: null });
      }
    }
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
      username: params.username ?? null,
      tags: params.tags,
      icon: params.icon,
      jumpHostId: params.jumpHostId ?? null,
      highlightProfileId: params.highlightProfileId ?? null,
      credentialProfileId: params.credentialProfileId ?? null,
      loginSequenceId: params.loginSequenceId ?? null,
      legacyAlgorithms: params.legacyAlgorithms ?? false,
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
      tags: params.tags ?? null,
      icon: params.icon ?? null,
      jumpHostId: params.jumpHostId !== undefined ? params.jumpHostId : null,
      highlightProfileId:
        params.highlightProfileId !== undefined ? params.highlightProfileId : null,
      credentialProfileId:
        params.credentialProfileId !== undefined ? params.credentialProfileId : null,
      loginSequenceId: params.loginSequenceId !== undefined ? params.loginSequenceId : null,
      legacyAlgorithms: params.legacyAlgorithms ?? null,
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

  // ── Bulk operations ─────────────────────────────────────────────────

  folderApplyCredentialProfile: async (folderId, profileId) => {
    const { dbBackend } = useAppStore.getState();
    let count: number;
    if (dbBackend === "postgres") {
      count = await invoke<number>("bulk_set_session_credentials", {
        folderId,
        credentialProfileId: profileId ?? null,
      });
    } else {
      count = await invoke<number>("folder_apply_credential_profile", {
        folderId,
        profileId: profileId ?? null,
      });
    }
    await get().loadAll();
    return count;
  },

  folderApplyLoginSequence: async (folderId, sequenceId) => {
    const { dbBackend } = useAppStore.getState();
    let count: number;
    if (dbBackend === "postgres") {
      count = await invoke<number>("bulk_set_session_login_sequences", {
        folderId,
        loginSequenceId: sequenceId ?? null,
      });
    } else {
      count = await invoke<number>("folder_apply_login_sequence", {
        folderId,
        sequenceId: sequenceId ?? null,
      });
    }
    await get().loadAll();
    return count;
  },

  folderBulkEditSessions: async (folderId, edit) => {
    const count = await invoke<number>("folder_bulk_edit_sessions", {
      folderId,
      setJumpHost: edit.jumpHostId !== undefined,
      jumpHostId: edit.jumpHostId ?? null,
      setHighlightProfile: edit.highlightProfileId !== undefined,
      highlightProfileId: edit.highlightProfileId ?? null,
      setLoginSequence: edit.loginSequenceId !== undefined,
      loginSequenceId: edit.loginSequenceId ?? null,
      icon: edit.icon ?? null,
    });
    await get().loadAll();
    return count;
  },

  // ── Reordering ──────────────────────────────────────────────────────

  reorderFolders: async (parentId, orderedIds) => {
    await invoke("folder_reorder", { parentId, orderedIds });
    await get().loadAll();
  },

  reorderSessions: async (folderId, orderedIds) => {
    await invoke("session_reorder", { folderId, orderedIds });
    await get().loadAll();
  },

  sortFolderAlphabetically: async (parentId, recursive) => {
    await invoke("folder_sort_alphabetically", {
      parentId,
      recursive: recursive ?? false,
    });
    await get().loadAll();
  },

  sortSessionsAlphabetically: async (folderId) => {
    await invoke("session_sort_alphabetically", { folderId });
    await get().loadAll();
  },

  sortSessionsByHostname: async (folderId) => {
    await invoke("session_sort_by_hostname", { folderId });
    await get().loadAll();
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

  expandFolder: (id) => {
    set((state) => {
      if (state.expandedFolderIds.has(id)) return state;
      const next = new Set(state.expandedFolderIds);
      next.add(id);
      localStorage.setItem("shellstation:expandedFolders", JSON.stringify([...next]));
      return { expandedFolderIds: next };
    });
  },

  collapseFolder: (id) => {
    set((state) => {
      if (!state.expandedFolderIds.has(id)) return state;
      const next = new Set(state.expandedFolderIds);
      next.delete(id);
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

    // The connection settings are read by the backend itself.
    const connId = await invokeConnect("session_connect", { id, cols: 80, rows: 24 });

    const { type, meta } = sessionTabKind(session);
    useTerminalStore.getState().addTab(connId, session.name, type, meta, session.id);
  },
}));

/**
 * The tab type and meta for a connection to a saved session. session_connect
 * picks the protocol from the saved session, so it, not an existing tab,
 * decides how the connection is written to and closed.
 */
export function sessionTabKind(session: Session): {
  type: "ssh" | "telnet";
  meta: TabMeta;
} {
  return session.protocol === "telnet"
    ? { type: "telnet", meta: { host: session.hostname, port: session.port } }
    : { type: "ssh", meta: { host: session.hostname, username: session.username } };
}
