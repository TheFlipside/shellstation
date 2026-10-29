/**
 * Settings store: the data export / import helpers, and the connection
 * settings the backend keeps.
 *
 * An imported file is untrusted input that lands in persisted state, so
 * these tests pin that every value goes through the same validation as a
 * change made in the Settings dialog, and that a command button import adds
 * buttons without ever changing or duplicating the ones already there.
 *
 * The connection settings are only shown by the store: these tests pin that
 * it never persists them, that it follows the backend's answer when the
 * private-address check is switched off, and that the values an older
 * version left in localStorage reach the backend without ever switching the
 * check off.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke, type InvokeArgs } from "@tauri-apps/api/core";
import {
  getExportableCommandButtons,
  getExportableSettings,
  handOverLegacyConnectionSettings,
  importCommandButtons,
  importSettings,
  loadConnectionSettings,
  useSettingsStore,
  type CommandButton,
} from "./settingsStore";

// Held separately so assertions never reference it as an unbound method.
const { changeLanguage } = vi.hoisted(() => ({
  changeLanguage: vi.fn(() => Promise.resolve()),
}));
vi.mock("../i18n", () => ({
  default: { t: (key: string) => key, changeLanguage, resolvedLanguage: "de" },
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const SETTINGS_KEY = "shellstation-settings";
const LEGACY_KEY = "shellstation-legacy-connection";
const BACKEND_KEYS = ["restrictPrivateIps", "connectTimeout", "keepaliveInterval", "keepaliveMax"];
const DEFAULT_CONNECTION = {
  restrict_private_ips: false,
  connect_timeout_secs: 10,
  keepalive_interval_secs: 15,
  keepalive_max: 3,
};

/** What the backend holds, and how the user answers its native dialog. */
const backend = {
  connection: { ...DEFAULT_CONNECTION },
  userAllowsOff: true,
};

/**
 * Refuse a value outside the range `ConnectionConfig` allows, as the backend
 * does. The ranges below copy its constants in `src-tauri/src/config.rs`.
 */
function checkRange(name: string, value: unknown, min: number, max: number): void {
  if (value === undefined) return;
  if (typeof value !== "number" || !Number.isInteger(value) || value < min || value > max) {
    throw new Error(`Invalid ${name}: ${JSON.stringify(value)}`);
  }
}

/** The connection commands, answered the way `commands/connection.rs` does. */
function respond(cmd: string, args: Record<string, unknown>): unknown {
  const connection = backend.connection;
  switch (cmd) {
    case "connection_get_config":
      return { ...connection };
    case "connection_save_config":
      checkRange("connect timeout", args.connectTimeoutSecs, 1, 300);
      checkRange("keepalive interval", args.keepaliveIntervalSecs, 0, 3600);
      checkRange("keepalive count", args.keepaliveMax, 1, 100);
      if (typeof args.connectTimeoutSecs === "number") {
        connection.connect_timeout_secs = args.connectTimeoutSecs;
      }
      if (typeof args.keepaliveIntervalSecs === "number") {
        connection.keepalive_interval_secs = args.keepaliveIntervalSecs;
      }
      if (typeof args.keepaliveMax === "number") connection.keepalive_max = args.keepaliveMax;
      return { ...connection };
    case "connection_set_restrict_private_ips":
      // Switching the check off only happens when the user agrees.
      if (args.enabled === true || backend.userAllowsOff) {
        connection.restrict_private_ips = args.enabled === true;
      }
      return connection.restrict_private_ips;
    default:
      throw new Error(`unexpected command ${cmd}`);
  }
}

function fakeBackend<T>(cmd: string, args?: InvokeArgs): Promise<T> {
  return new Promise((resolve) => {
    resolve(respond(cmd, (args ?? {}) as Record<string, unknown>) as T);
  });
}

/** The fake backend, except that `failing` rejects with `message`. */
function backendFailing(failing: string, message: string) {
  return <T>(cmd: string, args?: InvokeArgs): Promise<T> =>
    cmd === failing ? Promise.reject(new Error(message)) : fakeBackend<T>(cmd, args);
}

const invokeMock = vi.mocked(invoke);

/** The arguments of every call to `cmd`, oldest first. */
function callsTo(cmd: string): unknown[] {
  return invokeMock.mock.calls.filter(([c]) => c === cmd).map(([, args]) => args);
}

/** The settings as the persist middleware last wrote them. */
function stored(): { state: Record<string, unknown>; version: number } {
  return JSON.parse(localStorage.getItem(SETTINGS_KEY) ?? "null") as {
    state: Record<string, unknown>;
    version: number;
  };
}

const ID_A = "9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d";
const ID_B = "1b9d6bcd-bbfd-4b2d-9b5d-ab8dfbbd4bed";
const ID_C = "6ec0bd7f-11c0-43da-975e-2a8ad9ebae0b";

function button(id: string, name: string, command = "uptime\\r"): CommandButton {
  return { id, name, command, color: "#89b4fa" };
}

beforeEach(() => {
  backend.connection = { ...DEFAULT_CONNECTION };
  backend.userAllowsOff = true;
  invokeMock.mockReset();
  invokeMock.mockImplementation(fakeBackend);
  changeLanguage.mockClear();
  localStorage.clear();
  useSettingsStore.setState(useSettingsStore.getInitialState(), true);
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("getExportableSettings", () => {
  it("carries every persisted setting except the command buttons", () => {
    useSettingsStore.setState({ themeMode: "light", commandButtons: [button(ID_A, "a")] });
    const exported = getExportableSettings();
    expect(exported).not.toHaveProperty("commandButtons");
    expect(exported.themeMode).toBe("light");
    // No actions leak into the file.
    for (const value of Object.values(exported)) {
      expect(typeof value).not.toBe("function");
    }
    const state = useSettingsStore.getState() as unknown as Record<string, unknown>;
    const dataKeys = Object.keys(state).filter(
      (key) => typeof state[key] !== "function" && key !== "commandButtons",
    );
    expect(Object.keys(exported).sort()).toEqual(dataKeys.sort());
  });
});

describe("importSettings", () => {
  it("round-trips an export", async () => {
    useSettingsStore.setState({
      themeMode: "light",
      uiScale: 110,
      terminalFontFamily: "Fira Code",
      terminalBackgroundColor: "#101010",
      copyOnSelect: true,
      keepaliveInterval: 30,
    });
    const exported = getExportableSettings();
    useSettingsStore.setState(useSettingsStore.getInitialState(), true);

    await importSettings(exported);

    expect(getExportableSettings()).toEqual(exported);
  });

  it("ignores values of the wrong type and keeps the current one", async () => {
    await importSettings({
      themeMode: "neon",
      copyOnSelect: "yes",
      uiScale: "110",
      terminalFontFamily: "Comic Sans MS",
      terminalBackgroundColor: "red",
      sidebarWidth: 5000,
    });
    const initial = useSettingsStore.getInitialState();
    const state = useSettingsStore.getState();
    expect(state.themeMode).toBe(initial.themeMode);
    expect(state.copyOnSelect).toBe(initial.copyOnSelect);
    expect(state.uiScale).toBe(initial.uiScale);
    expect(state.terminalFontFamily).toBe(initial.terminalFontFamily);
    expect(state.terminalBackgroundColor).toBe(initial.terminalBackgroundColor);
    expect(state.sidebarWidth).toBe(initial.sidebarWidth);
  });

  it("only accepts whole numbers in range for values the backend reads as integers", async () => {
    await importSettings({ connectTimeout: 1.5, keepaliveInterval: 1e12, keepaliveMax: 101 });
    const initial = useSettingsStore.getInitialState();
    expect(useSettingsStore.getState().connectTimeout).toBe(initial.connectTimeout);
    expect(useSettingsStore.getState().keepaliveInterval).toBe(initial.keepaliveInterval);
    expect(useSettingsStore.getState().keepaliveMax).toBe(initial.keepaliveMax);
    expect(callsTo("connection_save_config")).toEqual([]);

    await importSettings({ connectTimeout: 60, keepaliveInterval: 0, keepaliveMax: 10 });
    expect(useSettingsStore.getState().connectTimeout).toBe(60);
    expect(useSettingsStore.getState().keepaliveInterval).toBe(0);
    expect(useSettingsStore.getState().keepaliveMax).toBe(10);
    expect(backend.connection).toMatchObject({
      connect_timeout_secs: 60,
      keepalive_interval_secs: 0,
      keepalive_max: 10,
    });
  });

  it.each([
    ["connectTimeout", 300, [0, 301]],
    ["keepaliveInterval", 3600, [-1, 3601]],
    ["keepaliveMax", 100, [0, 101]],
  ] as const)("takes %s up to %d and nothing outside its range", async (key, max, rejected) => {
    await importSettings({ [key]: max });
    expect(useSettingsStore.getState()[key]).toBe(max);
    for (const value of rejected) {
      await importSettings({ [key]: value });
      expect(useSettingsStore.getState()[key]).toBe(max);
    }
  });

  it("clamps the UI scale the way the Settings dialog does", async () => {
    await importSettings({ uiScale: 1000 });
    expect(useSettingsStore.getState().uiScale).toBe(125);
  });

  it.each(["restrictPrivateIps", "confirmMultilinePaste", "autoCheckForUpdates"] as const)(
    "can switch the safety setting %s on, but never off",
    async (key) => {
      useSettingsStore.setState({ [key]: false });
      await importSettings({ [key]: true });
      expect(useSettingsStore.getState()[key]).toBe(true);
      await importSettings({ [key]: false });
      expect(useSettingsStore.getState()[key]).toBe(true);
    },
  );

  it("never asks the backend to switch the private-address check off", async () => {
    await importSettings({ restrictPrivateIps: false });
    expect(callsTo("connection_set_restrict_private_ips")).toEqual([]);
  });

  it("applies the other settings when the backend fails one, then reports it", async () => {
    invokeMock.mockImplementation(backendFailing("connection_save_config", "disk full"));
    await expect(importSettings({ connectTimeout: 60, themeMode: "light" })).rejects.toThrow(
      "disk full",
    );
    expect(useSettingsStore.getState().themeMode).toBe("light");
    expect(useSettingsStore.getState().connectTimeout).toBe(10);
  });

  it("switches the UI language, but keeps it when the file follows the system", async () => {
    await importSettings({ language: "de" });
    expect(useSettingsStore.getState().language).toBe("de");
    expect(changeLanguage).toHaveBeenCalledWith("de");

    await importSettings({ language: "" });
    expect(useSettingsStore.getState().language).toBe("de");

    await importSettings({ language: "xx" });
    expect(useSettingsStore.getState().language).toBe("de");
  });

  it("ignores keys it does not know, including the command buttons", async () => {
    await importSettings({ commandButtons: [button(ID_A, "a")], somethingNew: 1 });
    expect(useSettingsStore.getState().commandButtons).toEqual([]);
    expect(useSettingsStore.getState()).not.toHaveProperty("somethingNew");
  });
});

describe("connection settings", () => {
  it("are never written to localStorage", async () => {
    await useSettingsStore.getState().setConnectTimeout(30);
    useSettingsStore.setState({ restrictPrivateIps: true, themeMode: "light" });
    const { state, version } = stored();
    expect(version).toBe(1);
    expect(state.themeMode).toBe("light");
    for (const key of BACKEND_KEYS) {
      expect(state).not.toHaveProperty(key);
    }
  });

  it("load what the backend holds", async () => {
    backend.connection = {
      restrict_private_ips: true,
      connect_timeout_secs: 60,
      keepalive_interval_secs: 0,
      keepalive_max: 10,
    };
    await loadConnectionSettings();
    const state = useSettingsStore.getState();
    expect(state.restrictPrivateIps).toBe(true);
    expect(state.connectTimeout).toBe(60);
    expect(state.keepaliveInterval).toBe(0);
    expect(state.keepaliveMax).toBe(10);
  });

  it("follow the user's answer when the private-address check is switched off", async () => {
    backend.connection.restrict_private_ips = true;
    useSettingsStore.setState({ restrictPrivateIps: true });

    backend.userAllowsOff = false;
    await useSettingsStore.getState().setRestrictPrivateIps(false);
    expect(useSettingsStore.getState().restrictPrivateIps).toBe(true);
    // The UI language picks the dialog's wording.
    expect(callsTo("connection_set_restrict_private_ips")).toEqual([
      { enabled: false, language: "de" },
    ]);

    backend.userAllowsOff = true;
    await useSettingsStore.getState().setRestrictPrivateIps(false);
    expect(useSettingsStore.getState().restrictPrivateIps).toBe(false);
  });

  it.each([
    ["setConnectTimeout", "connectTimeout", "connectTimeoutSecs", 60, 301],
    ["setKeepaliveInterval", "keepaliveInterval", "keepaliveIntervalSecs", 0, 3601],
    ["setKeepaliveMax", "keepaliveMax", "keepaliveMax", 5, 0],
  ] as const)(
    "%s saves its own value only, and only in range",
    async (setter, key, arg, valid, outOfRange) => {
      const before = useSettingsStore.getState()[key];
      await useSettingsStore.getState()[setter](outOfRange);
      expect(callsTo("connection_save_config")).toEqual([]);
      expect(useSettingsStore.getState()[key]).toBe(before);

      await useSettingsStore.getState()[setter](valid);
      expect(callsTo("connection_save_config")).toEqual([{ [arg]: valid }]);
      expect(useSettingsStore.getState()[key]).toBe(valid);
    },
  );

  it("are never read back from localStorage", async () => {
    // Storage of the current version that holds them anyway.
    localStorage.setItem(
      SETTINGS_KEY,
      JSON.stringify({ state: { themeMode: "light", restrictPrivateIps: true }, version: 1 }),
    );
    await useSettingsStore.persist.rehydrate();
    expect(useSettingsStore.getState().themeMode).toBe("light");
    expect(useSettingsStore.getState().restrictPrivateIps).toBe(false);
    expect(localStorage.getItem(LEGACY_KEY)).toBeNull();
  });

  describe("left in localStorage by an older version", () => {
    function storeLegacy(state: Record<string, unknown>): void {
      localStorage.setItem(SETTINGS_KEY, JSON.stringify({ state, version: 0 }));
    }

    /** What App.tsx does at startup. */
    async function startUp(): Promise<void> {
      await handOverLegacyConnectionSettings();
      await loadConnectionSettings();
    }

    it("move to the backend", async () => {
      storeLegacy({
        themeMode: "light",
        restrictPrivateIps: true,
        connectTimeout: 30,
        keepaliveInterval: 0,
        keepaliveMax: 5,
      });
      await useSettingsStore.persist.rehydrate();

      // The persisted settings lose them at once; the others stay.
      expect(stored().version).toBe(1);
      for (const key of BACKEND_KEYS) {
        expect(stored().state).not.toHaveProperty(key);
      }
      expect(useSettingsStore.getState().themeMode).toBe("light");

      await startUp();
      expect(backend.connection).toEqual({
        restrict_private_ips: true,
        connect_timeout_secs: 30,
        keepalive_interval_secs: 0,
        keepalive_max: 5,
      });
      expect(useSettingsStore.getState().restrictPrivateIps).toBe(true);
      expect(localStorage.getItem(LEGACY_KEY)).toBeNull();
    });

    it("never switch the check off, and skip values out of range", async () => {
      backend.connection.restrict_private_ips = true;
      storeLegacy({
        restrictPrivateIps: false,
        // Above even what older versions allowed.
        connectTimeout: 3601,
        keepaliveInterval: -1,
        keepaliveMax: "5",
      });
      await useSettingsStore.persist.rehydrate();
      await startUp();

      expect(callsTo("connection_set_restrict_private_ips")).toEqual([]);
      // Nothing valid is left, so nothing is saved.
      expect(callsTo("connection_save_config")).toEqual([]);
      expect(backend.connection).toEqual({ ...DEFAULT_CONNECTION, restrict_private_ips: true });
      expect(useSettingsStore.getState().restrictPrivateIps).toBe(true);
      expect(localStorage.getItem(LEGACY_KEY)).toBeNull();
    });

    it("carry a longer timeout over as the 300 s older versions connected with", async () => {
      storeLegacy({ connectTimeout: 3600 });
      await useSettingsStore.persist.rehydrate();
      await startUp();

      expect(callsTo("connection_save_config")).toEqual([{ connectTimeoutSecs: 300 }]);
      expect(useSettingsStore.getState().connectTimeout).toBe(300);
    });

    it("hand over the values in range and skip the rest", async () => {
      storeLegacy({ connectTimeout: 30, keepaliveInterval: 1.5, keepaliveMax: 101 });
      await useSettingsStore.persist.rehydrate();
      await startUp();

      expect(callsTo("connection_save_config")).toEqual([{ connectTimeoutSecs: 30 }]);
      expect(backend.connection).toEqual({ ...DEFAULT_CONNECTION, connect_timeout_secs: 30 });
    });

    it("are kept for the next launch when the backend cannot take them", async () => {
      storeLegacy({ restrictPrivateIps: true });
      await useSettingsStore.persist.rehydrate();

      invokeMock.mockImplementation(
        backendFailing("connection_set_restrict_private_ips", "config locked"),
      );
      // Rejected, so that startup can tell the user.
      await expect(handOverLegacyConnectionSettings()).rejects.toThrow("config locked");
      expect(backend.connection.restrict_private_ips).toBe(false);
      expect(localStorage.getItem(LEGACY_KEY)).not.toBeNull();

      invokeMock.mockImplementation(fakeBackend);
      await startUp();
      expect(backend.connection.restrict_private_ips).toBe(true);
      expect(localStorage.getItem(LEGACY_KEY)).toBeNull();
    });

    it("still move when localStorage refuses to park them", async () => {
      storeLegacy({ restrictPrivateIps: true });
      const setItem = vi.spyOn(Storage.prototype, "setItem").mockImplementation((key) => {
        if (key === LEGACY_KEY) throw new Error("quota exceeded");
      });
      await useSettingsStore.persist.rehydrate();
      setItem.mockRestore();
      expect(localStorage.getItem(LEGACY_KEY)).toBeNull();

      await startUp();
      expect(backend.connection.restrict_private_ips).toBe(true);
    });
  });
});

describe("importCommandButtons", () => {
  it("adds buttons after the existing ones", () => {
    useSettingsStore.setState({ commandButtons: [button(ID_A, "a")] });
    const result = importCommandButtons([button(ID_B, "b"), button(ID_C, "c")]);
    expect(result).toEqual({ added: 2, skipped: 0 });
    expect(useSettingsStore.getState().commandButtons.map((b) => b.name)).toEqual(["a", "b", "c"]);
  });

  it("never changes an existing button with the same id", () => {
    useSettingsStore.setState({ commandButtons: [button(ID_A, "local", "ls\\r")] });
    const result = importCommandButtons([button(ID_A.toUpperCase(), "remote", "rm\\r")]);
    expect(result).toEqual({ added: 0, skipped: 1 });
    expect(useSettingsStore.getState().commandButtons).toEqual([button(ID_A, "local", "ls\\r")]);
  });

  it("matches an existing id written in uppercase too", () => {
    const upper = ID_A.toUpperCase();
    useSettingsStore.setState({ commandButtons: [button(upper, "local", "ls\\r")] });
    const result = importCommandButtons([button(ID_A, "remote", "rm\\r")]);
    expect(result).toEqual({ added: 0, skipped: 1 });
    expect(useSettingsStore.getState().commandButtons).toEqual([button(upper, "local", "ls\\r")]);
  });

  it("skips a button with the same name and command under another id", () => {
    useSettingsStore.setState({ commandButtons: [button(ID_A, "a", "uptime\\r")] });
    const result = importCommandButtons([
      button(ID_B, "a", "uptime\\r"),
      button(ID_C, "a", "uptime -p\\r"),
    ]);
    expect(result).toEqual({ added: 1, skipped: 1 });
    expect(useSettingsStore.getState().commandButtons.map((b) => b.id)).toEqual([ID_A, ID_C]);
  });

  it("skips duplicates within the file itself", () => {
    const result = importCommandButtons([button(ID_A, "a"), button(ID_A, "a")]);
    expect(result).toEqual({ added: 1, skipped: 1 });
  });

  it("skips malformed entries and stores only the known fields", () => {
    const result = importCommandButtons([
      null,
      "uptime",
      { ...button("not-a-uuid", "a") },
      { ...button(ID_B, "b"), color: "red" },
      { ...button(ID_C, "x".repeat(33)) },
      { ...button(ID_A, "a"), extra: "<script>" },
    ]);
    expect(result).toEqual({ added: 1, skipped: 5 });
    expect(useSettingsStore.getState().commandButtons).toEqual([button(ID_A, "a")]);
  });

  it.each([
    ["a control character", "uptime\r"],
    ["a zero-width space", "upti\u200bme"],
    ["a text-direction override", "\u202eemitpu"],
    ["a line separator", "up\u2028time"],
    ["a tag character", "uptime\u{E0041}"],
    ["a no-break space", "uptime\u00a0-p"],
    ["a blank braille pattern", "uptime\u2800"],
    ["a lone surrogate", "uptime\ud800"],
  ])("skips a button whose name or command holds %s", (_label, text) => {
    const result = importCommandButtons([button(ID_A, text), button(ID_B, "b", text)]);
    expect(result).toEqual({ added: 0, skipped: 2 });
  });

  it("keeps emoji sequences in a name, but not the joiner in a command", () => {
    const family = "\u{1F468}\u200d\u{1F469}";
    const result = importCommandButtons([button(ID_A, family), button(ID_B, "b", family)]);
    expect(result).toEqual({ added: 1, skipped: 1 });
    expect(useSettingsStore.getState().commandButtons[0]?.name).toBe(family);
  });

  it("keeps icon-font glyphs in a name, but not in a command", () => {
    const result = importCommandButtons([
      button(ID_A, "\ue0b0 deploy"),
      button(ID_B, "b", "uptime\ue0b0"),
      button(ID_C, "c", "uptime\ufe0f"),
    ]);
    expect(result).toEqual({ added: 1, skipped: 2 });
  });

  it("stops at the button limit", () => {
    const full = Array.from({ length: 1000 }, (_, i) =>
      button(`00000000-0000-4000-8000-${String(i).padStart(12, "0")}`, `b${String(i)}`),
    );
    useSettingsStore.setState({ commandButtons: full });
    const result = importCommandButtons([button(ID_A, "a")]);
    expect(result).toEqual({ added: 0, skipped: 1 });
    expect(useSettingsStore.getState().commandButtons).toHaveLength(1000);
  });

  it("fills a nearly full bar up to the limit and skips the rest", () => {
    const almostFull = Array.from({ length: 999 }, (_, i) =>
      button(`00000000-0000-4000-8000-${String(i).padStart(12, "0")}`, `b${String(i)}`),
    );
    useSettingsStore.setState({ commandButtons: almostFull });
    const result = importCommandButtons([button(ID_A, "a"), button(ID_B, "b"), button(ID_C, "c")]);
    expect(result).toEqual({ added: 1, skipped: 2 });
    const buttons = useSettingsStore.getState().commandButtons;
    expect(buttons).toHaveLength(1000);
    expect(buttons[999]?.id).toBe(ID_A);
  });

  it("round-trips an export into an empty bar", () => {
    useSettingsStore.setState({ commandButtons: [button(ID_A, "a"), button(ID_B, "b")] });
    const exported = getExportableCommandButtons();
    useSettingsStore.setState({ commandButtons: [] });
    importCommandButtons(exported);
    expect(getExportableCommandButtons()).toEqual(exported);
  });
});
