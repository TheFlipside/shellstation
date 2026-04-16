import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";

interface AppState {
  dbBackend: "sqlite" | "postgres";
  pgUser: string | null;
  userIdent: string | null;
  setDbBackend: (backend: "sqlite" | "postgres") => void;
  setPgUser: (user: string | null) => void;
  loadUserIdent: () => Promise<void>;
  setUserIdent: (ident: string) => Promise<void>;
}

export const useAppStore = create<AppState>()((set) => ({
  dbBackend: "sqlite",
  pgUser: null,
  userIdent: null,
  setDbBackend: (backend) => {
    set({ dbBackend: backend });
  },
  setPgUser: (user) => {
    set({ pgUser: user });
  },
  loadUserIdent: async () => {
    try {
      const ident = await invoke<string | null>("get_user_ident");
      set({ userIdent: ident });
    } catch {
      // Ignore — user_ident not configured yet
    }
  },
  setUserIdent: async (ident: string) => {
    await invoke("set_user_ident", { userIdent: ident });
    set({ userIdent: ident });
  },
}));
