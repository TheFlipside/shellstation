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

export const SUPPORTED_LANGUAGES: readonly string[] = [
  "en",
  "de",
  "es",
  "fr",
  "it",
  "ja",
  "ko",
  "nl",
  "pl",
  "pt",
  "ru",
  "sv",
  "tr",
  "zh",
];
const SUPPORTED_LANGUAGES_SET: ReadonlySet<string> = new Set(SUPPORTED_LANGUAGES);

const MAX_COMMAND_BUTTONS = 1000;
const MAX_BUTTON_NAME_LENGTH = 32;
const MAX_BUTTON_COMMAND_LENGTH = 1024;
const HEX_COLOR_RE = /^#[\da-f]{6}$/i;
const UUID_RE = /^[\da-f]{8}-[\da-f]{4}-[\da-f]{4}-[\da-f]{4}-[\da-f]{12}$/i;

function isValidButtonShape(b: Partial<CommandButton>): boolean {
  return (
    typeof b.name === "string" &&
    b.name.length > 0 &&
    b.name.length <= MAX_BUTTON_NAME_LENGTH &&
    typeof b.command === "string" &&
    b.command.length > 0 &&
    b.command.length <= MAX_BUTTON_COMMAND_LENGTH &&
    typeof b.color === "string" &&
    HEX_COLOR_RE.test(b.color)
  );
}

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
  confirmMultilinePaste: boolean;
  restrictPrivateIps: boolean;
  autoRefreshInterval: number;
  connectTimeout: number;
  keepaliveInterval: number;
  keepaliveMax: number;
  sidebarWidth: number;
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
  setConfirmMultilinePaste: (value: boolean) => void;
  setRestrictPrivateIps: (value: boolean) => void;
  setAutoRefreshInterval: (seconds: number) => void;
  setConnectTimeout: (seconds: number) => void;
  setKeepaliveInterval: (seconds: number) => void;
  setKeepaliveMax: (count: number) => void;
  setSidebarWidth: (width: number) => void;
  setToastAutoDismiss: (value: boolean) => void;
  setToastDismissSeconds: (seconds: number) => void;
}

export const useSettingsStore = create<SettingsState>()(
  persist(
    (set) => ({
      language: "",
      uiScale: 100,
      themeMode: "system",
      closeOnDisconnect: false,
      openLocalOnStartup: false,
      confirmOnQuit: true,
      confirmOnCloseTab: true,
      terminalFontFamily: "JetBrains Mono",
      terminalFontSize: 14,
      copyOnSelect: false,
      pasteOnRightClick: false,
      confirmMultilinePaste: false,
      restrictPrivateIps: false,
      autoRefreshInterval: 0,
      connectTimeout: 10,
      keepaliveInterval: 15,
      keepaliveMax: 3,
      sidebarWidth: 260,
      toastAutoDismiss: true,
      toastDismissSeconds: 5,
      commandButtons: [],
      addCommandButton: (button: CommandButton) => {
        if (typeof button.id !== "string" || !UUID_RE.test(button.id)) return;
        if (!isValidButtonShape(button)) return;
        set((state) => {
          if (state.commandButtons.length >= MAX_COMMAND_BUTTONS) return state;
          if (state.commandButtons.some((b) => b.id === button.id)) return state;
          return { commandButtons: [...state.commandButtons, button] };
        });
      },
      updateCommandButton: (id: string, updates: Partial<Omit<CommandButton, "id">>) => {
        set((state) => {
          const idx = state.commandButtons.findIndex((b) => b.id === id);
          if (idx === -1) return state;
          const merged = { ...state.commandButtons[idx], ...updates };
          if (!isValidButtonShape(merged)) return state;
          const next = state.commandButtons.slice();
          next[idx] = merged;
          return { commandButtons: next };
        });
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
          const len = state.commandButtons.length;
          if (
            !Number.isInteger(fromIndex) ||
            !Number.isInteger(toIndex) ||
            fromIndex < 0 ||
            toIndex < 0 ||
            fromIndex >= len ||
            toIndex >= len
          ) {
            return state;
          }
          const next = [...state.commandButtons];
          const [moved] = next.splice(fromIndex, 1);
          next.splice(toIndex, 0, moved);
          return { commandButtons: next };
        });
      },
      setLanguage: (lang: string) => {
        if (!SUPPORTED_LANGUAGES_SET.has(lang)) return;
        void i18n.changeLanguage(lang);
        set({ language: lang });
      },
      setUiScale: (scale: number) => {
        // 150% is intentionally excluded; see SettingsDialog UI_SCALE_OPTIONS.
        const clamped = scale > 125 ? 125 : scale < 75 ? 75 : scale;
        set({ uiScale: clamped });
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
        if (Number.isFinite(size) && size >= 6 && size <= 72) {
          set({ terminalFontSize: size });
        }
      },
      setCopyOnSelect: (value: boolean) => {
        set({ copyOnSelect: value });
      },
      setPasteOnRightClick: (value: boolean) => {
        set({ pasteOnRightClick: value });
      },
      setConfirmMultilinePaste: (value: boolean) => {
        set({ confirmMultilinePaste: value });
      },
      setRestrictPrivateIps: (value: boolean) => {
        set({ restrictPrivateIps: value });
      },
      setAutoRefreshInterval: (seconds: number) => {
        // 0 disables polling; any active interval must be >= 5s to prevent
        // IPC saturation from a corrupted persisted value or fractional input.
        if (!Number.isFinite(seconds)) return;
        if (seconds === 0 || (seconds >= 5 && seconds <= 3600)) {
          set({ autoRefreshInterval: seconds });
        }
      },
      setConnectTimeout: (seconds: number) => {
        if (Number.isFinite(seconds) && seconds > 0) {
          set({ connectTimeout: seconds });
        }
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
      setSidebarWidth: (width: number) => {
        if (Number.isFinite(width) && width >= 160 && width <= 600) {
          set({ sidebarWidth: width });
        }
      },
      setToastAutoDismiss: (value: boolean) => {
        set({ toastAutoDismiss: value });
      },
      setToastDismissSeconds: (seconds: number) => {
        if (Number.isFinite(seconds) && seconds >= 1 && seconds <= 300) {
          set({ toastDismissSeconds: seconds });
        }
      },
    }),
    {
      name: "shellstation-settings",
      onRehydrateStorage: () => {
        return (state?: SettingsState) => {
          if (!state) return;
          // Validate persisted language against the bundled-locales allowlist
          // before passing to i18next, which would otherwise key its resource
          // lookup map with an attacker-controlled string.
          if (typeof state.language === "string" && state.language !== "") {
            if (SUPPORTED_LANGUAGES_SET.has(state.language)) {
              void i18n.changeLanguage(state.language);
            } else {
              state.language = "";
            }
          }
          // Sanitize persisted keepalive values against corrupted storage.
          if (!Number.isFinite(state.keepaliveInterval) || state.keepaliveInterval < 0) {
            state.keepaliveInterval = 15;
          }
          if (!Number.isFinite(state.keepaliveMax) || state.keepaliveMax < 1) {
            state.keepaliveMax = 3;
          }
          // Clamp uiScale to the supported range (75-125). 150% used to be
          // selectable but was dropped because dnd-kit overlay positioning
          // leaves a residual offset at that exact ratio under wry's webview.
          if (!Number.isFinite(state.uiScale) || state.uiScale > 125) {
            state.uiScale = 125;
          } else if (state.uiScale < 75) {
            state.uiScale = 75;
          }
          // Sanitize persisted sidebar width.
          if (
            !Number.isFinite(state.sidebarWidth) ||
            state.sidebarWidth < 160 ||
            state.sidebarWidth > 600
          ) {
            state.sidebarWidth = 260;
          }
          // Sanitize autoRefreshInterval: 0 (disabled) or 5-3600s. Prevents a
          // tampered persisted value (e.g. 0.001) from driving a 1ms polling
          // storm against the Tauri IPC bridge.
          if (
            !Number.isFinite(state.autoRefreshInterval) ||
            (state.autoRefreshInterval !== 0 &&
              (state.autoRefreshInterval < 5 || state.autoRefreshInterval > 3600))
          ) {
            state.autoRefreshInterval = 0;
          }
          // Sanitize toast dismiss seconds (must be a positive finite number
          // within a sane upper bound).
          if (
            !Number.isFinite(state.toastDismissSeconds) ||
            state.toastDismissSeconds < 1 ||
            state.toastDismissSeconds > 300
          ) {
            state.toastDismissSeconds = 5;
          }
          // Sanitize persisted commandButtons against corrupted localStorage:
          // cap total entries, require UUID-format ids, enforce per-field
          // length limits, and validate color format. Prevents startup hangs
          // from pathological payloads.
          if (Array.isArray(state.commandButtons)) {
            state.commandButtons = state.commandButtons
              .slice(0, MAX_COMMAND_BUTTONS)
              .filter(
                (b) => typeof b.id === "string" && UUID_RE.test(b.id) && isValidButtonShape(b),
              );
          }
          // Sanitize persisted font family against the allowlist.
          if (!ALLOWED_TERMINAL_FONTS_SET.has(state.terminalFontFamily)) {
            state.terminalFontFamily = "JetBrains Mono";
          }
        };
      },
    },
  ),
);
