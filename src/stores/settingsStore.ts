import { create } from "zustand";
import { persist } from "zustand/middleware";
import i18n from "../i18n";

export type ThemeMode = "dark" | "light" | "system";

export interface CommandButton {
  id: string;
  name: string;
  command: string;
  color: string;
}

interface SettingsState {
  language: string;
  uiScale: number;
  themeMode: ThemeMode;
  closeOnDisconnect: boolean;
  openLocalOnStartup: boolean;
  confirmOnQuit: boolean;
  confirmOnCloseTab: boolean;
  terminalFontFamily: string;
  terminalFontSize: number;
  copyOnSelect: boolean;
  pasteOnRightClick: boolean;
  restrictPrivateIps: boolean;
  autoRefreshInterval: number;
  connectTimeout: number;
  toastAutoDismiss: boolean;
  toastDismissSeconds: number;
  commandButtons: CommandButton[];
  addCommandButton: (button: CommandButton) => void;
  updateCommandButton: (id: string, button: Partial<Omit<CommandButton, "id">>) => void;
  removeCommandButton: (id: string) => void;
  duplicateCommandButton: (id: string) => void;
  reorderCommandButtons: (fromIndex: number, toIndex: number) => void;
  setLanguage: (lang: string) => void;
  setUiScale: (scale: number) => void;
  setThemeMode: (mode: ThemeMode) => void;
  setCloseOnDisconnect: (value: boolean) => void;
  setOpenLocalOnStartup: (value: boolean) => void;
  setConfirmOnQuit: (value: boolean) => void;
  setConfirmOnCloseTab: (value: boolean) => void;
  setTerminalFontFamily: (family: string) => void;
  setTerminalFontSize: (size: number) => void;
  setCopyOnSelect: (value: boolean) => void;
  setPasteOnRightClick: (value: boolean) => void;
  setRestrictPrivateIps: (value: boolean) => void;
  setAutoRefreshInterval: (seconds: number) => void;
  setConnectTimeout: (seconds: number) => void;
  setToastAutoDismiss: (value: boolean) => void;
  setToastDismissSeconds: (seconds: number) => void;
}

export const useSettingsStore = create<SettingsState>()(
  persist(
    (set) => ({
      language: "",
      uiScale: 100,
      themeMode: "system" as ThemeMode,
      closeOnDisconnect: false,
      openLocalOnStartup: false,
      confirmOnQuit: true,
      confirmOnCloseTab: true,
      terminalFontFamily: "JetBrains Mono",
      terminalFontSize: 14,
      copyOnSelect: false,
      pasteOnRightClick: false,
      restrictPrivateIps: false,
      autoRefreshInterval: 0,
      connectTimeout: 10,
      toastAutoDismiss: true,
      toastDismissSeconds: 5,
      commandButtons: [],
      addCommandButton: (button: CommandButton) => {
        set((state) => ({ commandButtons: [...state.commandButtons, button] }));
      },
      updateCommandButton: (id: string, updates: Partial<Omit<CommandButton, "id">>) => {
        set((state) => ({
          commandButtons: state.commandButtons.map((b) => (b.id === id ? { ...b, ...updates } : b)),
        }));
      },
      removeCommandButton: (id: string) => {
        set((state) => ({
          commandButtons: state.commandButtons.filter((b) => b.id !== id),
        }));
      },
      duplicateCommandButton: (id: string) => {
        set((state) => {
          const source = state.commandButtons.find((b) => b.id === id);
          if (!source) return state;
          const copy: CommandButton = {
            ...source,
            id: crypto.randomUUID(),
            name: source.name + " (copy)",
          };
          return { commandButtons: [...state.commandButtons, copy] };
        });
      },
      reorderCommandButtons: (fromIndex: number, toIndex: number) => {
        set((state) => {
          if (fromIndex === toIndex) return state;
          const next = [...state.commandButtons];
          const [moved] = next.splice(fromIndex, 1);
          next.splice(toIndex, 0, moved);
          return { commandButtons: next };
        });
      },
      setLanguage: (lang: string) => {
        void i18n.changeLanguage(lang);
        set({ language: lang });
      },
      setUiScale: (scale: number) => {
        set({ uiScale: scale });
      },
      setThemeMode: (mode: ThemeMode) => {
        set({ themeMode: mode });
      },
      setCloseOnDisconnect: (value: boolean) => {
        set({ closeOnDisconnect: value });
      },
      setOpenLocalOnStartup: (value: boolean) => {
        set({ openLocalOnStartup: value });
      },
      setConfirmOnQuit: (value: boolean) => {
        set({ confirmOnQuit: value });
      },
      setConfirmOnCloseTab: (value: boolean) => {
        set({ confirmOnCloseTab: value });
      },
      setTerminalFontFamily: (family: string) => {
        set({ terminalFontFamily: family });
      },
      setTerminalFontSize: (size: number) => {
        set({ terminalFontSize: size });
      },
      setCopyOnSelect: (value: boolean) => {
        set({ copyOnSelect: value });
      },
      setPasteOnRightClick: (value: boolean) => {
        set({ pasteOnRightClick: value });
      },
      setRestrictPrivateIps: (value: boolean) => {
        set({ restrictPrivateIps: value });
      },
      setAutoRefreshInterval: (seconds: number) => {
        set({ autoRefreshInterval: seconds });
      },
      setConnectTimeout: (seconds: number) => {
        set({ connectTimeout: seconds });
      },
      setToastAutoDismiss: (value: boolean) => {
        set({ toastAutoDismiss: value });
      },
      setToastDismissSeconds: (seconds: number) => {
        set({ toastDismissSeconds: seconds });
      },
    }),
    {
      name: "shellstation-settings",
      onRehydrateStorage: () => {
        return (state?: SettingsState) => {
          if (state?.language) {
            void i18n.changeLanguage(state.language);
          }
          // Sanitize persisted commandButtons: drop entries with invalid color
          // values to guard against corrupted localStorage data.
          if (state && Array.isArray(state.commandButtons)) {
            const hexColor = /^#[\da-f]{6}$/i;
            state.commandButtons = state.commandButtons.filter(
              (b) =>
                typeof b.id === "string" &&
                typeof b.name === "string" &&
                typeof b.command === "string" &&
                typeof b.color === "string" &&
                hexColor.test(b.color),
            );
          }
        };
      },
    },
  ),
);
