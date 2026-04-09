import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";

export interface HighlightRule {
  pattern: string;
  color: string;
  case_sensitive: boolean;
  bold: boolean;
}

export interface HighlightProfile {
  id: string;
  name: string;
  rules: HighlightRule[];
  sort_order: number;
}

/** Raw shape from the Rust backend (rules is a JSON string). */
interface RawHighlightProfile {
  id: string;
  name: string;
  rules: string;
  sort_order: number;
}

function parseProfile(raw: RawHighlightProfile): HighlightProfile {
  let rules: HighlightRule[] = [];
  try {
    rules = JSON.parse(raw.rules) as HighlightRule[];
  } catch {
    /* corrupt JSON — treat as empty */
  }
  return { id: raw.id, name: raw.name, rules, sort_order: raw.sort_order };
}

interface HighlightState {
  profiles: HighlightProfile[];
  loadProfiles: () => Promise<void>;
  createProfile: (name: string, rules: HighlightRule[]) => Promise<HighlightProfile>;
  updateProfile: (id: string, name?: string, rules?: HighlightRule[]) => Promise<void>;
  deleteProfile: (id: string) => Promise<void>;
  getProfileById: (id: string) => HighlightProfile | undefined;
}

export const useHighlightStore = create<HighlightState>((set, get) => ({
  profiles: [],

  loadProfiles: async () => {
    const raw = await invoke<RawHighlightProfile[]>("highlight_profile_list");
    set({ profiles: raw.map(parseProfile) });
  },

  createProfile: async (name: string, rules: HighlightRule[]) => {
    const raw = await invoke<RawHighlightProfile>("highlight_profile_create", {
      name,
      rules: JSON.stringify(rules),
    });
    const profile = parseProfile(raw);
    set((s) => ({ profiles: [...s.profiles, profile] }));
    return profile;
  },

  updateProfile: async (id: string, name?: string, rules?: HighlightRule[]) => {
    await invoke("highlight_profile_update", {
      id,
      name: name ?? null,
      rules: rules !== undefined ? JSON.stringify(rules) : null,
    });
    set((s) => ({
      profiles: s.profiles.map((p) => {
        if (p.id !== id) return p;
        return {
          ...p,
          ...(name !== undefined ? { name } : {}),
          ...(rules !== undefined ? { rules } : {}),
        };
      }),
    }));
  },

  deleteProfile: async (id: string) => {
    await invoke("highlight_profile_delete", { id });
    set((s) => ({ profiles: s.profiles.filter((p) => p.id !== id) }));
  },

  getProfileById: (id: string) => get().profiles.find((p) => p.id === id),
}));
