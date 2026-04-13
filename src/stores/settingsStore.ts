import { create } from "zustand";
import { persist } from "zustand/middleware";
import i18n from "../i18n";

export type ThemeMode = "dark" | "light" | "system";

export const ALLOWED_TERMINAL_FONTS: readonly string[] = [
  "JetBrains Mono",
  "Fira Code",
  "Cascadia Code",
  "Source Code Pro",
  "IBM Plex Mono",
  "Ubuntu Mono",
  "Hack",
  "Inconsolata",
  "DejaVu Sans Mono",
  "Courier New",
  "monospace",
];

const ALLOWED_TERMINAL_FONTS_SET: ReadonlySet<string> = new Set(ALLOWED_TERMINAL_FONTS);

const MAX_COMMAND_BUTTONS = 1000;
const MAX_BUTTON_NAME_LENGTH = 32;
const MAX_BUTTON_COMMAND_LENGTH = 1024;

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
  keepaliveInterval: number;
  keepaliveMax: number;
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
  setKeepaliveInterval: (seconds: number) => void;
  setKeepaliveMax: (count: number) => void;
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
      keepaliveInterval: 15,
      keepaliveMax: 3,
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
        if (ALLOWED_TERMINAL_FONTS_SET.has(family)) {
          set({ terminalFontFamily: family });
        }
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
      setKeepaliveInterval: (seconds: number) => {
        if (Number.isFinite(seconds) && seconds >= 0) {
          set({ keepaliveInterval: seconds });
        }
      },
      setKeepaliveMax: (count: number) => {
        if (Number.isFinite(count) && count >= 1) {
          set({ keepaliveMax: count });
        }
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
          // Sanitize persisted keepalive values against corrupted storage.
          if (state) {
            if (!Number.isFinite(state.keepaliveInterval) || state.keepaliveInterval < 0) {
              state.keepaliveInterval = 15;
            }
            if (!Number.isFinite(state.keepaliveMax) || state.keepaliveMax < 1) {
              state.keepaliveMax = 3;
            }
          }
          // Sanitize persisted commandButtons against corrupted localStorage:
          // cap total entries, enforce per-field length limits, and validate
          // color format. Prevents startup hangs from pathological payloads.
          if (state && Array.isArray(state.commandButtons)) {
            const hexColor = /^#[\da-f]{6}$/i;
            state.commandButtons = state.commandButtons
              .slice(0, MAX_COMMAND_BUTTONS)
              .filter(
                (b) =>
                  typeof b.id === "string" &&
                  b.id.length <= 64 &&
                  typeof b.name === "string" &&
                  b.name.length <= MAX_BUTTON_NAME_LENGTH &&
                  typeof b.command === "string" &&
                  b.command.length <= MAX_BUTTON_COMMAND_LENGTH &&
                  typeof b.color === "string" &&
                  hexColor.test(b.color),
              );
          }
          // Sanitize persisted font family against the allowlist.
          if (state && !ALLOWED_TERMINAL_FONTS_SET.has(state.terminalFontFamily)) {
            state.terminalFontFamily = "JetBrains Mono";
          }
        };
      },
    },
  ),
);
