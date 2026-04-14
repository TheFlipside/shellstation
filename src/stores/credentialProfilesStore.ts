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
  loadAll: () => Promise<void>;
  createProfile: (params: CreateProfileParams) => Promise<CredentialProfile>;
  updateProfile: (id: string, params: UpdateProfileParams) => Promise<void>;
  deleteProfile: (id: string) => Promise<void>;
  getSecret: (id: string) => Promise<string>;
  getProfileById: (id: string) => CredentialProfile | undefined;
}

export const useCredentialProfilesStore = create<CredentialProfilesState>((set, get) => ({
  profiles: [],

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
