import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";

export interface CredentialProfile {
  id: string;
  name: string;
  auth_type: string;
  username: string;
  keychain_ref: string;
  key_path: string;
  sort_order: number;
}

export interface CreateProfileParams {
  name: string;
  authType: string;
  username: string;
  keyPath: string;
  secret: string;
}

export interface UpdateProfileParams {
  name?: string;
  authType?: string;
  username?: string;
  keyPath?: string;
  secret?: string;
}

interface CredentialProfilesState {
  profiles: CredentialProfile[];
  /**
   * The ids of the profiles whose change is under way. It is kept here and
   * not by the dialog that sent the change, which can be closed and opened
   * again before the change has its answer.
   */
  updating: string[];
  loadAll: () => Promise<void>;
  createProfile: (params: CreateProfileParams) => Promise<CredentialProfile>;
  updateProfile: (id: string, params: UpdateProfileParams) => Promise<void>;
  deleteProfile: (id: string) => Promise<void>;
  getSecret: (id: string) => Promise<string>;
  getProfileById: (id: string) => CredentialProfile | undefined;
}

/** The list without one of its entries that are this id. */
function withoutOne(ids: string[], id: string): string[] {
  const at = ids.indexOf(id);
  return at === -1 ? ids : [...ids.slice(0, at), ...ids.slice(at + 1)];
}

export const useCredentialProfilesStore = create<CredentialProfilesState>((set, get) => ({
  profiles: [],
  updating: [],

  loadAll: async () => {
    const profiles = await invoke<CredentialProfile[]>("credential_profile_list");
    set({ profiles });
  },

  createProfile: async (params) => {
    const profile = await invoke<CredentialProfile>("credential_profile_create", {
      name: params.name,
      authType: params.authType,
      username: params.username,
      keyPath: params.keyPath,
      secret: params.secret,
    });
    set((s) => ({ profiles: [...s.profiles, profile] }));
    return profile;
  },

  updateProfile: async (id, params) => {
    set((s) => ({ updating: [...s.updating, id] }));
    try {
      await invoke("credential_profile_update", {
        id,
        name: params.name ?? null,
        authType: params.authType ?? null,
        username: params.username ?? null,
        keyPath: params.keyPath ?? null,
        secret: params.secret ?? null,
      });
      set((s) => ({
        profiles: s.profiles.map((p) => {
          if (p.id !== id) return p;
          return {
            ...p,
            ...(params.name !== undefined ? { name: params.name } : {}),
            ...(params.authType !== undefined ? { auth_type: params.authType } : {}),
            ...(params.username !== undefined ? { username: params.username } : {}),
            ...(params.keyPath !== undefined ? { key_path: params.keyPath } : {}),
          };
        }),
      }));
    } finally {
      // Once, for this change: another of the same profile may be under way.
      set((s) => ({ updating: withoutOne(s.updating, id) }));
    }
  },

  deleteProfile: async (id) => {
    await invoke("credential_profile_delete", { id });
    set((s) => ({ profiles: s.profiles.filter((p) => p.id !== id) }));
  },

  getSecret: async (id) => {
    return await invoke<string>("credential_profile_get_secret", { id });
  },

  getProfileById: (id) => get().profiles.find((p) => p.id === id),
}));
