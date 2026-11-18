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
  /**
   * The ids of the sequences whose change or deletion is under way, one entry
   * for each. It is kept here and not by the dialog that sent it, which can
   * be closed and opened again before it has its answer.
   */
  updating: string[];
  loadAll: () => Promise<void>;
  createSequence: (params: CreateLoginSequenceParams) => Promise<LoginSequence>;
  updateSequence: (id: string, params: UpdateLoginSequenceParams) => Promise<void>;
  deleteSequence: (id: string) => Promise<void>;
  getSequenceById: (id: string) => LoginSequence | undefined;
}

/** The list without one of its entries that are this id. */
function withoutOne(ids: string[], id: string): string[] {
  const at = ids.indexOf(id);
  return at === -1 ? ids : [...ids.slice(0, at), ...ids.slice(at + 1)];
}

export const useLoginSequenceStore = create<LoginSequenceState>((set, get) => ({
  sequences: [],
  updating: [],

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
    set((s) => ({ updating: [...s.updating, id] }));
    try {
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
            ...(params.sendInitialCr !== undefined
              ? { send_initial_cr: params.sendInitialCr }
              : {}),
            ...(params.steps !== undefined ? { steps: params.steps } : {}),
          };
        }),
      }));
      // A change of a sequence is one step in the backend, so one that failed
      // was not made in part, and the list is not read again as the profiles'
      // is.
    } finally {
      // Once, for this change: another of the same sequence may be under way.
      set((s) => ({ updating: withoutOne(s.updating, id) }));
    }
  },

  deleteSequence: async (id) => {
    set((s) => ({ updating: [...s.updating, id] }));
    try {
      await invoke("login_sequence_delete", { id });
      set((s) => ({ sequences: s.sequences.filter((seq) => seq.id !== id) }));
    } finally {
      set((s) => ({ updating: withoutOne(s.updating, id) }));
    }
  },

  getSequenceById: (id) => get().sequences.find((seq) => seq.id === id),
}));
