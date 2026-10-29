import { invoke } from "@tauri-apps/api/core";
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

// The connection settings are Rust integers (u64 / u32) in the backend's
// config, which checks the same ranges (`ConnectionConfig` in
// src-tauri/src/config.rs). Checking here first keeps a value the backend
// would refuse from ever being sent, so only whole numbers in range pass.
function isWholeNumberInRange(value: unknown, min: number, max: number): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= min && value <= max;
}
function isConnectTimeout(seconds: unknown): seconds is number {
  return isWholeNumberInRange(seconds, 1, 300);
}
function isKeepaliveInterval(seconds: unknown): seconds is number {
  return isWholeNumberInRange(seconds, 0, 3600);
}
function isKeepaliveMax(count: unknown): count is number {
  return isWholeNumberInRange(count, 1, 100);
}

function isValidButtonShape(
  b: Partial<CommandButton>,
): b is Partial<CommandButton> & Omit<CommandButton, "id"> {
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
  /** Hex `#RRGGBB`, or `null` to use the active app theme's default. */
  terminalBackgroundColor: string | null;
  /** Hex `#RRGGBB`, or `null` to use the active app theme's default. */
  terminalForegroundColor: string | null;
  copyOnSelect: boolean;
  pasteOnRightClick: boolean;
  confirmMultilinePaste: boolean;
  /**
   * This and the other three connection settings are the backend's: the
   * store only shows them (see `loadConnectionSettings`), and their setters
   * change them in the backend.
   */
  restrictPrivateIps: boolean;
  autoRefreshInterval: number;
  connectTimeout: number;
  keepaliveInterval: number;
  keepaliveMax: number;
  sidebarWidth: number;
  toastAutoDismiss: boolean;
  toastDismissSeconds: number;
  /** Check for app updates automatically on launch. Default `true`. */
  autoCheckForUpdates: boolean;
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
  setTerminalBackgroundColor: (color: string | null) => void;
  setTerminalForegroundColor: (color: string | null) => void;
  setCopyOnSelect: (value: boolean) => void;
  setPasteOnRightClick: (value: boolean) => void;
  setConfirmMultilinePaste: (value: boolean) => void;
  /**
   * Switching the check off makes the backend ask in a native dialog; the
   * store follows whatever the user chose there.
   */
  setRestrictPrivateIps: (value: boolean) => Promise<void>;
  setAutoRefreshInterval: (seconds: number) => void;
  setConnectTimeout: (seconds: number) => Promise<void>;
  setKeepaliveInterval: (seconds: number) => Promise<void>;
  setKeepaliveMax: (count: number) => Promise<void>;
  setSidebarWidth: (width: number) => void;
  setToastAutoDismiss: (value: boolean) => void;
  setToastDismissSeconds: (seconds: number) => void;
  setAutoCheckForUpdates: (value: boolean) => void;
}

/** The backend's `ConnectionConfig` (src-tauri/src/config.rs). */
interface ConnectionConfig {
  restrict_private_ips: boolean;
  connect_timeout_secs: number;
  keepalive_interval_secs: number;
  keepalive_max: number;
}

/** Settings the backend keeps. The store shows them, but never persists them. */
const BACKEND_SETTING_KEYS = [
  "restrictPrivateIps",
  "connectTimeout",
  "keepaliveInterval",
  "keepaliveMax",
] as const satisfies readonly (keyof SettingsState)[];
const BACKEND_SETTING_KEY_SET: ReadonlySet<string> = new Set(BACKEND_SETTING_KEYS);

type PersistedSettings = Omit<SettingsState, (typeof BACKEND_SETTING_KEYS)[number]>;

function withoutBackendSettings(state: SettingsState): PersistedSettings {
  return Object.fromEntries(
    Object.entries(state).filter(([key]) => !BACKEND_SETTING_KEY_SET.has(key)),
  ) as PersistedSettings;
}

/** Change the given connection settings; a value left out keeps its setting. */
function saveConnectionConfig(values: {
  connectTimeoutSecs?: number | undefined;
  keepaliveIntervalSecs?: number | undefined;
  keepaliveMax?: number | undefined;
}): Promise<ConnectionConfig> {
  return invoke<ConnectionConfig>("connection_save_config", values);
}

/**
 * Older versions kept the connection settings in localStorage. Migrating the
 * persisted state parks them under this key until
 * `handOverLegacyConnectionSettings` has given them to the backend, so a
 * launch that fails to do that tries again on the next one.
 */
const LEGACY_CONNECTION_KEY = "shellstation-legacy-connection";

/**
 * The parked settings, also kept here for when localStorage refuses them:
 * the migrated state no longer holds them, so they would otherwise be lost
 * before this launch could hand them over.
 */
let parkedConnectionSettings: Record<string, unknown> | null = null;

function stashLegacyConnectionSettings(state: Readonly<Record<string, unknown>>): void {
  const legacy = Object.fromEntries(
    Object.entries(state).filter(([key]) => BACKEND_SETTING_KEY_SET.has(key)),
  );
  if (Object.keys(legacy).length === 0) return;
  parkedConnectionSettings = legacy;
  try {
    localStorage.setItem(LEGACY_CONNECTION_KEY, JSON.stringify(legacy));
  } catch {
    // No storage: this launch still hands over the copy above.
  }
}

function readParkedConnectionSettings(): Record<string, unknown> | null {
  let raw: string | null = null;
  try {
    raw = localStorage.getItem(LEGACY_CONNECTION_KEY);
  } catch {
    // No storage: fall back to the copy kept in memory.
  }
  if (raw === null) return parkedConnectionSettings;
  try {
    const parsed: unknown = JSON.parse(raw);
    return typeof parsed === "object" && parsed !== null ? (parsed as Record<string, unknown>) : {};
  } catch {
    // Unreadable: there is nothing to hand over.
    return {};
  }
}

let handOverInFlight: Promise<void> | null = null;

/**
 * Give the backend the connection settings an older version left in
 * localStorage, if any. Call once at startup, before
 * `loadConnectionSettings`. If it fails, an older "on" for the private-address
 * check has not reached the backend yet, so the caller should say so; it is
 * tried again on the next launch.
 */
export function handOverLegacyConnectionSettings(): Promise<void> {
  // React's StrictMode runs the startup effect twice in development.
  handOverInFlight ??= handOver().finally(() => {
    handOverInFlight = null;
  });
  return handOverInFlight;
}

async function handOver(): Promise<void> {
  const legacy = readParkedConnectionSettings();
  if (legacy === null) return;
  // The check is only ever carried over as "on". Switching it off takes the
  // user's confirmation, and off is the backend's default anyway, so
  // nothing written to this key can weaken it. Switching it on asks
  // nothing, so the dialog's language does not matter here.
  if (legacy.restrictPrivateIps === true) {
    await invoke("connection_set_restrict_private_ips", { enabled: true, language: "en" });
  }
  const { keepaliveInterval, keepaliveMax } = legacy;
  // Older versions stored up to 3600 s but connected with at most 300 s, so
  // a longer timeout carries over as the 300 s it already was.
  const connectTimeout =
    typeof legacy.connectTimeout === "number" && legacy.connectTimeout <= 3600
      ? Math.min(legacy.connectTimeout, 300)
      : legacy.connectTimeout;
  const values = {
    connectTimeoutSecs: isConnectTimeout(connectTimeout) ? connectTimeout : undefined,
    keepaliveIntervalSecs: isKeepaliveInterval(keepaliveInterval) ? keepaliveInterval : undefined,
    keepaliveMax: isKeepaliveMax(keepaliveMax) ? keepaliveMax : undefined,
  };
  if (Object.values(values).some((value) => value !== undefined)) {
    await saveConnectionConfig(values);
  }
  parkedConnectionSettings = null;
  try {
    localStorage.removeItem(LEGACY_CONNECTION_KEY);
  } catch {
    // No storage: there is no key to remove.
  }
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
      terminalBackgroundColor: null,
      terminalForegroundColor: null,
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
      autoCheckForUpdates: true,
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
      setTerminalBackgroundColor: (color: string | null) => {
        if (color === null || HEX_COLOR_RE.test(color)) {
          set({ terminalBackgroundColor: color });
        }
      },
      setTerminalForegroundColor: (color: string | null) => {
        if (color === null || HEX_COLOR_RE.test(color)) {
          set({ terminalForegroundColor: color });
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
      setRestrictPrivateIps: async (value: boolean) => {
        const on = await invoke<boolean>("connection_set_restrict_private_ips", {
          enabled: value,
          language: i18n.resolvedLanguage ?? "en",
        });
        set({ restrictPrivateIps: on });
      },
      setAutoRefreshInterval: (seconds: number) => {
        // 0 disables polling; any active interval must be >= 5s to prevent
        // IPC saturation from a corrupted persisted value or fractional input.
        if (!Number.isFinite(seconds)) return;
        if (seconds === 0 || (seconds >= 5 && seconds <= 3600)) {
          set({ autoRefreshInterval: seconds });
        }
      },
      // Each sends only its own value, so two changes in flight at once
      // cannot overwrite each other with a stale copy.
      setConnectTimeout: async (seconds: number) => {
        if (!isConnectTimeout(seconds)) return;
        const saved = await saveConnectionConfig({ connectTimeoutSecs: seconds });
        set({ connectTimeout: saved.connect_timeout_secs });
      },
      setKeepaliveInterval: async (seconds: number) => {
        if (!isKeepaliveInterval(seconds)) return;
        const saved = await saveConnectionConfig({ keepaliveIntervalSecs: seconds });
        set({ keepaliveInterval: saved.keepalive_interval_secs });
      },
      setKeepaliveMax: async (count: number) => {
        if (!isKeepaliveMax(count)) return;
        const saved = await saveConnectionConfig({ keepaliveMax: count });
        set({ keepaliveMax: saved.keepalive_max });
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
      setAutoCheckForUpdates: (value: boolean) => {
        set({ autoCheckForUpdates: value });
      },
    }),
    {
      name: "shellstation-settings",
      // Version 1 moved the connection settings to the backend.
      version: 1,
      partialize: withoutBackendSettings,
      // Never read them back either, even from storage that holds them.
      merge: (persisted, current) => ({
        ...current,
        ...(typeof persisted === "object" && persisted !== null
          ? withoutBackendSettings(persisted as SettingsState)
          : {}),
      }),
      migrate: (persisted, version) => {
        const state: Record<string, unknown> =
          typeof persisted === "object" && persisted !== null ? { ...persisted } : {};
        if (version < 1) stashLegacyConnectionSettings(state);
        return withoutBackendSettings(state as unknown as SettingsState);
      },
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
          // Sanitize persisted terminal color overrides — null means "use
          // theme default", otherwise must be a `#RRGGBB` hex string. The
          // explicit typeof guard handles older app versions where the
          // field was absent (undefined) and matches the style used for
          // every other field in this sanitizer.
          if (
            state.terminalBackgroundColor !== null &&
            (typeof state.terminalBackgroundColor !== "string" ||
              !HEX_COLOR_RE.test(state.terminalBackgroundColor))
          ) {
            state.terminalBackgroundColor = null;
          }
          if (
            state.terminalForegroundColor !== null &&
            (typeof state.terminalForegroundColor !== "string" ||
              !HEX_COLOR_RE.test(state.terminalForegroundColor))
          ) {
            state.terminalForegroundColor = null;
          }
          // Older app versions didn't have this field; default to on (the
          // documented user-friendly behavior described in the changelog
          // for the updater feature).
          if (typeof state.autoCheckForUpdates !== "boolean") {
            state.autoCheckForUpdates = true;
          }
        };
      },
    },
  ),
);

/** Fill the store with the backend's connection settings. */
export async function loadConnectionSettings(): Promise<void> {
  const config = await invoke<ConnectionConfig>("connection_get_config");
  useSettingsStore.setState({
    restrictPrivateIps: config.restrict_private_ips,
    connectTimeout: config.connect_timeout_secs,
    keepaliveInterval: config.keepalive_interval_secs,
    keepaliveMax: config.keepalive_max,
  });
}

// ── Data export / import ─────────────────────────────────────────────────

/** The persisted values of SettingsState, without its actions. */
type SettingsData = {
  [K in keyof SettingsState as SettingsState[K] extends (...args: never[]) => unknown
    ? never
    : K]: SettingsState[K];
};

/** Settings carried by a data export. Command buttons are a section of their own. */
export type ExportableSettingKey = Exclude<keyof SettingsData, "commandButtons">;

/** What an importer applies a value with: a setter, which may be async. */
type ApplySetting<T> = (state: SettingsState, value: T) => void | Promise<void>;
type SettingImporter = (state: SettingsState, value: unknown) => Promise<void>;

function importBoolean(apply: ApplySetting<boolean>): SettingImporter {
  return async (state, value) => {
    if (typeof value === "boolean") await apply(state, value);
  };
}

function importNumber(apply: ApplySetting<number>): SettingImporter {
  return async (state, value) => {
    if (typeof value === "number" && Number.isFinite(value)) await apply(state, value);
  };
}

/**
 * For settings that guard against something: a file may switch them on, but
 * never off, so importing a file from someone else cannot quietly weaken
 * this machine. Turning one off stays a deliberate change in Settings.
 */
function importEnableOnly(apply: (state: SettingsState) => void | Promise<void>): SettingImporter {
  return async (state, value) => {
    if (value === true) await apply(state);
  };
}

function importString(apply: ApplySetting<string>): SettingImporter {
  return async (state, value) => {
    if (typeof value === "string") await apply(state, value);
  };
}

function importColor(apply: ApplySetting<string | null>): SettingImporter {
  return async (state, value) => {
    if (value === null || typeof value === "string") await apply(state, value);
  };
}

const THEME_MODES: ReadonlySet<string> = new Set<ThemeMode>(["dark", "light", "system"]);

/**
 * Applies one imported setting through its setter, so a value from a file
 * gets the same validation as a change made in the Settings dialog. The
 * importers only check the JSON type a setter's signature promises.
 *
 * Keyed by every persisted setting, so adding one to SettingsState does not
 * compile until it is listed here too, and with that it is exported as well.
 */
const SETTING_IMPORTERS: Readonly<Record<ExportableSettingKey, SettingImporter>> = {
  // An empty language means "follow the system language". The importing
  // machine keeps its own choice in that case instead of being reset.
  language: importString((s, v) => {
    if (v !== "") s.setLanguage(v);
  }),
  uiScale: importNumber((s, v) => {
    s.setUiScale(v);
  }),
  themeMode: importString((s, v) => {
    if (THEME_MODES.has(v)) s.setThemeMode(v as ThemeMode);
  }),
  closeOnDisconnect: importBoolean((s, v) => {
    s.setCloseOnDisconnect(v);
  }),
  openLocalOnStartup: importBoolean((s, v) => {
    s.setOpenLocalOnStartup(v);
  }),
  confirmOnQuit: importBoolean((s, v) => {
    s.setConfirmOnQuit(v);
  }),
  confirmOnCloseTab: importBoolean((s, v) => {
    s.setConfirmOnCloseTab(v);
  }),
  terminalFontFamily: importString((s, v) => {
    s.setTerminalFontFamily(v);
  }),
  terminalFontSize: importNumber((s, v) => {
    s.setTerminalFontSize(v);
  }),
  terminalBackgroundColor: importColor((s, v) => {
    s.setTerminalBackgroundColor(v);
  }),
  terminalForegroundColor: importColor((s, v) => {
    s.setTerminalForegroundColor(v);
  }),
  copyOnSelect: importBoolean((s, v) => {
    s.setCopyOnSelect(v);
  }),
  pasteOnRightClick: importBoolean((s, v) => {
    s.setPasteOnRightClick(v);
  }),
  confirmMultilinePaste: importEnableOnly((s) => {
    s.setConfirmMultilinePaste(true);
  }),
  restrictPrivateIps: importEnableOnly((s) => s.setRestrictPrivateIps(true)),
  autoRefreshInterval: importNumber((s, v) => {
    s.setAutoRefreshInterval(v);
  }),
  connectTimeout: importNumber((s, v) => s.setConnectTimeout(v)),
  keepaliveInterval: importNumber((s, v) => s.setKeepaliveInterval(v)),
  keepaliveMax: importNumber((s, v) => s.setKeepaliveMax(v)),
  sidebarWidth: importNumber((s, v) => {
    s.setSidebarWidth(v);
  }),
  toastAutoDismiss: importBoolean((s, v) => {
    s.setToastAutoDismiss(v);
  }),
  toastDismissSeconds: importNumber((s, v) => {
    s.setToastDismissSeconds(v);
  }),
  autoCheckForUpdates: importEnableOnly((s) => {
    s.setAutoCheckForUpdates(true);
  }),
};

const EXPORTABLE_SETTING_KEYS = Object.keys(SETTING_IMPORTERS) as ExportableSettingKey[];

/** The current settings as they go into an export file. */
export function getExportableSettings(): Record<ExportableSettingKey, unknown> {
  const state = useSettingsStore.getState();
  const out: Partial<Record<ExportableSettingKey, unknown>> = {};
  for (const key of EXPORTABLE_SETTING_KEYS) out[key] = state[key];
  return out as Record<ExportableSettingKey, unknown>;
}

/**
 * Apply settings read from an export file. Keys this version does not know
 * and values its setters reject are ignored, leaving the current value.
 * Every setting is tried; if the backend fails one of its own, the first
 * such error is thrown once the rest are done.
 */
export async function importSettings(raw: Readonly<Record<string, unknown>>): Promise<void> {
  // Only the setters are used from this snapshot; each one updates the
  // live store itself.
  const state = useSettingsStore.getState();
  const applied = EXPORTABLE_SETTING_KEYS.filter((key) =>
    Object.prototype.hasOwnProperty.call(raw, key),
  ).map((key) => SETTING_IMPORTERS[key](state, raw[key]));
  const failed = (await Promise.allSettled(applied)).find(
    (result): result is PromiseRejectedResult => result.status === "rejected",
  );
  if (failed) throw failed.reason;
}

/** The current command buttons as they go into an export file. */
export function getExportableCommandButtons(): CommandButton[] {
  return useSettingsStore
    .getState()
    .commandButtons.map(({ id, name, command, color }) => ({ id, name, command, color }));
}

// Characters that change how a button looks without being seen: control
// and format characters (format covers zero-width characters, text-direction
// overrides and tag characters), line and paragraph separators, lone
// surrogates, and characters that render as blanks. Private-use characters
// are allowed: icon fonts such as Nerd Fonts put their glyphs there.
const HIDDEN_CHARACTER_RE = /[\p{Cc}\p{Cf}\p{Zl}\p{Zp}\p{Cs}\u034f\u115f\u1160\u2800\u3164\uffa0]/u;
// Every space character except the ordinary one, which the button list
// collapses, so runs of them cannot push a command out of view.
const UNUSUAL_SPACE_RE = /[\u00a0\u1680\u2000-\u200a\u202f\u205f\u3000]/u;
// A command never needs an icon glyph or a variation selector; either can
// render blank or as a lookalike, so commands refuse them on top of the above.
const COMMAND_ONLY_HIDDEN_RE = /[\p{Co}\p{Variation_Selector}]/u;
// Emoji sequences join with U+200D. In a name it only changes how emoji
// combine, so it is allowed there; in a command it is still refused.
const ZERO_WIDTH_JOINER_RE = /\u200d/g;

/**
 * Whether `text` holds characters a button from someone else's file could
 * use to look harmless while sending something else. Commands typed in the
 * app use the visible `\r`-style escapes instead, so a real button never
 * needs them.
 */
function hasHiddenCharacters(text: string): boolean {
  return HIDDEN_CHARACTER_RE.test(text) || UNUSUAL_SPACE_RE.test(text);
}

/** Whether an entry from an export file can become a command button. */
export function isImportableButton(raw: unknown): raw is CommandButton {
  if (typeof raw !== "object" || raw === null) return false;
  const b = raw as Partial<CommandButton>;
  return (
    typeof b.id === "string" &&
    UUID_RE.test(b.id) &&
    isValidButtonShape(b) &&
    !hasHiddenCharacters(b.name.replace(ZERO_WIDTH_JOINER_RE, "")) &&
    !hasHiddenCharacters(b.command) &&
    !COMMAND_ONLY_HIDDEN_RE.test(b.command)
  );
}

export interface CommandButtonImportResult {
  added: number;
  skipped: number;
}

/**
 * Add command buttons from an export file after the existing ones. Existing
 * buttons are never changed: a button is skipped when one with the same id,
 * or the same name and command, is already there, when it is malformed, or
 * when the bar is full.
 */
export function importCommandButtons(raw: readonly unknown[]): CommandButtonImportResult {
  const current = useSettingsStore.getState().commandButtons;
  // Ids are compared lowercased on both sides: the store accepts either case.
  const ids = new Set(current.map((b) => b.id.toLowerCase()));
  const contentKey = (b: CommandButton): string => JSON.stringify([b.name, b.command]);
  const contents = new Set(current.map(contentKey));
  const added: CommandButton[] = [];
  for (const item of raw) {
    if (current.length + added.length >= MAX_COMMAND_BUTTONS) break;
    if (!isImportableButton(item)) continue;
    // Normalised so ids compare the same way however the file spells them.
    const id = item.id.toLowerCase();
    if (ids.has(id) || contents.has(contentKey(item))) continue;
    // Copy the known fields only, so nothing else in the file is persisted.
    const button = { id, name: item.name, command: item.command, color: item.color };
    added.push(button);
    ids.add(id);
    contents.add(contentKey(button));
  }
  if (added.length > 0) {
    useSettingsStore.setState({ commandButtons: [...current, ...added] });
  }
  return { added: added.length, skipped: raw.length - added.length };
}
