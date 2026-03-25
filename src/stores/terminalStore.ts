import { create } from "zustand";

export type SessionType = "local" | "ssh";

export interface SshMeta {
  host: string;
  username: string;
}

export interface TerminalTab {
  id: string;
  title: string;
  type: SessionType;
  meta?: SshMeta;
}

interface TerminalState {
  tabs: TerminalTab[];
  activeTabId: string | null;
  addTab: (id: string, title: string, type: SessionType, meta?: SshMeta) => void;
  removeTab: (id: string) => void;
  setActiveTab: (id: string) => void;
  updateTabTitle: (id: string, title: string) => void;
  reorderTabs: (fromIndex: number, toIndex: number) => void;
}

export const useTerminalStore = create<TerminalState>((set, get) => ({
  tabs: [],
  activeTabId: null,

  addTab: (id: string, title: string, type_: SessionType, meta?: SshMeta) => {
    set((state) => ({
      tabs: [...state.tabs, { id, title, type: type_, meta }],
      activeTabId: id,
    }));
  },

  removeTab: (id: string) => {
    const { tabs, activeTabId } = get();
    const index = tabs.findIndex((t) => t.id === id);
    const newTabs = tabs.filter((t) => t.id !== id);
    let newActive = activeTabId;

    if (activeTabId === id) {
      if (newTabs.length === 0) {
        newActive = null;
      } else if (index >= newTabs.length) {
        newActive = newTabs[newTabs.length - 1].id;
      } else {
        newActive = newTabs[index].id;
      }
    }

    set({ tabs: newTabs, activeTabId: newActive });
  },

  setActiveTab: (id: string) => {
    set({ activeTabId: id });
  },

  updateTabTitle: (id: string, title: string) => {
    set((state) => ({
      tabs: state.tabs.map((t) => (t.id === id ? { ...t, title } : t)),
    }));
  },

  reorderTabs: (fromIndex: number, toIndex: number) => {
    set((state) => {
      if (fromIndex === toIndex) return state;
      const newTabs = [...state.tabs];
      const [moved] = newTabs.splice(fromIndex, 1);
      newTabs.splice(toIndex, 0, moved);
      return { tabs: newTabs };
    });
  },
}));
