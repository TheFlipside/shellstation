import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";

export interface LoginSequenceStep {
  pattern: string;
  response: string;
  append_cr: boolean;
}

export interface LoginSequence {
  id: string;
  name: string;
  send_initial_cr: boolean;
  steps: LoginSequenceStep[];
  sort_order: number;
}

export interface CreateLoginSequenceParams {
  name: string;
  sendInitialCr: boolean;
  steps: LoginSequenceStep[];
}

export interface UpdateLoginSequenceParams {
  name?: string;
  sendInitialCr?: boolean;
  steps?: LoginSequenceStep[];
}

interface LoginSequenceState {
  sequences: LoginSequence[];
  loadAll: () => Promise<void>;
  createSequence: (params: CreateLoginSequenceParams) => Promise<LoginSequence>;
  updateSequence: (id: string, params: UpdateLoginSequenceParams) => Promise<void>;
  deleteSequence: (id: string) => Promise<void>;
  getSequenceById: (id: string) => LoginSequence | undefined;
}

export const useLoginSequenceStore = create<LoginSequenceState>((set, get) => ({
  sequences: [],

  loadAll: async () => {
    const sequences = await invoke<LoginSequence[]>("login_sequence_list");
    set({ sequences });
  },

  createSequence: async (params) => {
    const sequence = await invoke<LoginSequence>("login_sequence_create", {
      name: params.name,
      sendInitialCr: params.sendInitialCr,
      steps: params.steps,
    });
    set((s) => ({ sequences: [...s.sequences, sequence] }));
    return sequence;
  },

  updateSequence: async (id, params) => {
    await invoke("login_sequence_update", {
      id,
      name: params.name ?? null,
      sendInitialCr: params.sendInitialCr ?? null,
      steps: params.steps ?? null,
    });
    set((s) => ({
      sequences: s.sequences.map((seq) => {
        if (seq.id !== id) return seq;
        return {
          ...seq,
          ...(params.name !== undefined ? { name: params.name } : {}),
          ...(params.sendInitialCr !== undefined ? { send_initial_cr: params.sendInitialCr } : {}),
          ...(params.steps !== undefined ? { steps: params.steps } : {}),
        };
      }),
    }));
  },

  deleteSequence: async (id) => {
    await invoke("login_sequence_delete", { id });
    set((s) => ({ sequences: s.sequences.filter((seq) => seq.id !== id) }));
  },

  getSequenceById: (id) => get().sequences.find((seq) => seq.id === id),
}));
