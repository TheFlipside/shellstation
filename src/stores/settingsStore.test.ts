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
  PrivateIpCheckHandOverError,
  startUpConnectionSettings,
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
      openLocalOnStartup: true,
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

  it.each([
    "restrictPrivateIps",
    "confirmMultilinePaste",
    "autoCheckForUpdates",
    "confirmOnQuit",
    "confirmOnCloseTab",
  ] as const)("can switch the safety setting %s on, but never off", async (key) => {
    useSettingsStore.setState({ [key]: false });
    await importSettings({ [key]: true });
    expect(useSettingsStore.getState()[key]).toBe(true);
    await importSettings({ [key]: false });
    expect(useSettingsStore.getState()[key]).toBe(true);
  });

  it.each(["pasteOnRightClick", "copyOnSelect", "closeOnDisconnect"] as const)(
    "can switch %s off, but never on",
    async (key) => {
      useSettingsStore.setState({ [key]: true });
      await importSettings({ [key]: false });
      expect(useSettingsStore.getState()[key]).toBe(false);
      await importSettings({ [key]: true });
      expect(useSettingsStore.getState()[key]).toBe(false);
    },
  );

  it("leaves out what an export would weaken, even this machine's own", async () => {
    useSettingsStore.setState({
      confirmOnQuit: false,
      confirmOnCloseTab: false,
      pasteOnRightClick: true,
      copyOnSelect: true,
      closeOnDisconnect: true,
    });
    const exported = getExportableSettings();
    useSettingsStore.setState(useSettingsStore.getInitialState(), true);

    await importSettings(exported);
    expect(useSettingsStore.getState()).toMatchObject({
      confirmOnQuit: true,
      confirmOnCloseTab: true,
      pasteOnRightClick: false,
      copyOnSelect: false,
      closeOnDisconnect: false,
    });
  });

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
    // The UI language ("de" in the i18n mock) is not sent: the backend words
    // the dialog in the operating system's language.
    expect(callsTo("connection_set_restrict_private_ips")).toStrictEqual([{ enabled: false }]);

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

    // The store keeps parked settings in memory too. Hand over whatever a
    // failing test left there, so the next test starts without any.
    afterEach(async () => {
      localStorage.clear();
      invokeMock.mockImplementation(fakeBackend);
      await handOverLegacyConnectionSettings();
    });

    /** What App.tsx does at startup, which here must find nothing to report. */
    async function startUp(): Promise<void> {
      expect(await startUpConnectionSettings()).toEqual({ errors: [], privateIpCheckOff: false });
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
      // Switching the check on opens no dialog, and sends no language either.
      expect(callsTo("connection_set_restrict_private_ips")).toStrictEqual([{ enabled: true }]);
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
      // Rejected as such, so that startup can tell the user the check is off,
      // with the backend's words kept for the log but out of the message.
      const handOver = handOverLegacyConnectionSettings();
      await expect(handOver).rejects.toThrow(PrivateIpCheckHandOverError);
      await expect(handOver).rejects.not.toThrow("config locked");
      await expect(handOver).rejects.toHaveProperty("reason.message", "config locked");
      expect(backend.connection.restrict_private_ips).toBe(false);
      expect(localStorage.getItem(LEGACY_KEY)).not.toBeNull();

      invokeMock.mockImplementation(fakeBackend);
      await startUp();
      expect(backend.connection.restrict_private_ips).toBe(true);
      expect(localStorage.getItem(LEGACY_KEY)).toBeNull();
    });

    describe("when the backend cannot switch the check on", () => {
      const warn = vi.fn();

      beforeEach(async () => {
        storeLegacy({ restrictPrivateIps: true, connectTimeout: 30 });
        await useSettingsStore.persist.rehydrate();
        warn.mockReset();
        vi.spyOn(console, "warn").mockImplementation(warn);
        invokeMock.mockImplementation(
          backendFailing("connection_set_restrict_private_ips", "Failed to write config: denied"),
        );
      });

      it("say the check is off instead of showing the backend's error", async () => {
        expect(await startUpConnectionSettings()).toEqual({
          errors: [],
          privateIpCheckOff: true,
        });
        expect(useSettingsStore.getState().restrictPrivateIps).toBe(false);
        expect(localStorage.getItem(LEGACY_KEY)).not.toBeNull();
        // The backend's words go to devtools only.
        expect(warn).toHaveBeenCalledWith(
          expect.stringContaining("private-address check"),
          new Error("Failed to write config: denied"),
        );
      });

      it("say nothing of it when the check is on anyway", async () => {
        backend.connection.restrict_private_ips = true;
        expect(await startUpConnectionSettings()).toEqual({
          errors: [],
          privateIpCheckOff: false,
        });
        expect(useSettingsStore.getState().restrictPrivateIps).toBe(true);
      });

      it("say the check is off when the backend cannot tell either", async () => {
        invokeMock.mockImplementation((cmd) =>
          Promise.reject(new Error(`${cmd}: config lock poisoned`)),
        );
        const { errors, privateIpCheckOff } = await startUpConnectionSettings();
        expect(privateIpCheckOff).toBe(true);
        expect(errors.map(String)).toEqual(["Error: connection_get_config: config lock poisoned"]);
      });
    });

    it("report other failures as they are, with the check on", async () => {
      storeLegacy({ restrictPrivateIps: true, connectTimeout: 30 });
      await useSettingsStore.persist.rehydrate();
      invokeMock.mockImplementation(backendFailing("connection_save_config", "disk full"));

      const { errors, privateIpCheckOff } = await startUpConnectionSettings();
      expect(privateIpCheckOff).toBe(false);
      expect(errors.map(String)).toEqual(["Error: disk full"]);
      expect(useSettingsStore.getState().restrictPrivateIps).toBe(true);
      // Tried again on the next launch.
      expect(localStorage.getItem(LEGACY_KEY)).not.toBeNull();
    });

    it("report a failed load, with no word of the check when it was handed over", async () => {
      storeLegacy({ restrictPrivateIps: true });
      await useSettingsStore.persist.rehydrate();
      invokeMock.mockImplementation(backendFailing("connection_get_config", "lock poisoned"));

      const { errors, privateIpCheckOff } = await startUpConnectionSettings();
      expect(privateIpCheckOff).toBe(false);
      expect(errors.map(String)).toEqual(["Error: lock poisoned"]);
      expect(backend.connection.restrict_private_ips).toBe(true);
    });

    it.each([
      ["not JSON", "{not json"],
      ["not an object", "[true]"],
    ])("switch the check on when what was parked is %s", async (_, parked) => {
      localStorage.setItem(LEGACY_KEY, parked);
      await startUp();
      // Only the check, which fails closed; nothing else is guessed.
      expect(callsTo("connection_set_restrict_private_ips")).toStrictEqual([{ enabled: true }]);
      expect(callsTo("connection_save_config")).toEqual([]);
      expect(backend.connection).toEqual({ ...DEFAULT_CONNECTION, restrict_private_ips: true });
      expect(localStorage.getItem(LEGACY_KEY)).toBeNull();
    });

    it("prefer this launch's own copy to a parked one that cannot be read", async () => {
      storeLegacy({ connectTimeout: 30 });
      await useSettingsStore.persist.rehydrate();
      localStorage.setItem(LEGACY_KEY, "{damaged");
      await startUp();
      // What this launch read: a timeout, and no word of the check.
      expect(callsTo("connection_set_restrict_private_ips")).toEqual([]);
      expect(callsTo("connection_save_config")).toEqual([{ connectTimeoutSecs: 30 }]);
    });

    describe("and changed in this version before they could move", () => {
      /** A first start whose hand-over failed at `failing`, leaving them parked. */
      async function failedFirstStart(
        legacy: Record<string, unknown>,
        failing: string,
      ): Promise<void> {
        storeLegacy(legacy);
        await useSettingsStore.persist.rehydrate();
        vi.spyOn(console, "warn").mockImplementation(() => undefined);
        invokeMock.mockImplementation(backendFailing(failing, "config locked"));
        await startUpConnectionSettings();
        expect(localStorage.getItem(LEGACY_KEY)).not.toBeNull();
        invokeMock.mockImplementation(fakeBackend);
        invokeMock.mockClear();
      }

      const ALL_PARKED = {
        restrictPrivateIps: true,
        connectTimeout: 30,
        keepaliveInterval: 0,
        keepaliveMax: 5,
      };

      it.each([
        ["setConnectTimeout", "connectTimeoutSecs", "connect_timeout_secs", 60],
        ["setKeepaliveInterval", "keepaliveIntervalSecs", "keepalive_interval_secs", 45],
        ["setKeepaliveMax", "keepaliveMax", "keepalive_max", 7],
      ] as const)(
        "keep a value saved with %s, and still move the rest",
        async (setter, arg, field, value) => {
          await failedFirstStart(ALL_PARKED, "connection_set_restrict_private_ips");
          await useSettingsStore.getState()[setter](value);

          await startUp();
          expect(callsTo("connection_set_restrict_private_ips")).toStrictEqual([{ enabled: true }]);
          // A value left out is undefined, which IPC does not send.
          const saves = callsTo("connection_save_config");
          const handedOver = saves[saves.length - 1] as Record<string, unknown>;
          expect(handedOver[arg]).toBeUndefined();
          expect(Object.values(handedOver).filter((v) => v !== undefined)).toHaveLength(2);
          expect(backend.connection[field]).toBe(value);
        },
      );

      it("keep a value set by an import", async () => {
        await failedFirstStart(ALL_PARKED, "connection_set_restrict_private_ips");
        await importSettings({ connectTimeout: 60 });

        await startUp();
        expect(backend.connection).toEqual({
          restrict_private_ips: true,
          connect_timeout_secs: 60,
          keepalive_interval_secs: 0,
          keepalive_max: 5,
        });
      });

      it("still move a value whose change was refused", async () => {
        await failedFirstStart({ connectTimeout: 30 }, "connection_save_config");
        // Out of range: refused before it reaches the backend.
        await useSettingsStore.getState().setConnectTimeout(301);
        // Refused by the backend.
        invokeMock.mockImplementation(backendFailing("connection_save_config", "disk full"));
        await expect(useSettingsStore.getState().setConnectTimeout(60)).rejects.toThrow();
        invokeMock.mockImplementation(fakeBackend);

        await startUp();
        expect(backend.connection.connect_timeout_secs).toBe(30);
      });

      it("keep a value saved while the check is being handed over", async () => {
        storeLegacy({ restrictPrivateIps: true, connectTimeout: 30 });
        await useSettingsStore.persist.rehydrate();
        let open = (): void => undefined;
        const opened = new Promise<void>((resolve) => {
          open = resolve;
        });
        invokeMock.mockImplementation(
          <T>(cmd: string, args?: InvokeArgs): Promise<T> =>
            cmd === "connection_set_restrict_private_ips"
              ? opened.then(() => fakeBackend<T>(cmd, args))
              : fakeBackend<T>(cmd, args),
        );

        const startup = startUpConnectionSettings();
        await useSettingsStore.getState().setConnectTimeout(60);
        open();
        expect(await startup).toEqual({ errors: [], privateIpCheckOff: false });
        expect(callsTo("connection_save_config")).toEqual([{ connectTimeoutSecs: 60 }]);
        expect(backend.connection).toMatchObject({
          restrict_private_ips: true,
          connect_timeout_secs: 60,
        });
      });

      it("never undo switching the check off that the user confirmed", async () => {
        // The check moved, the timeout did not.
        await failedFirstStart(
          { restrictPrivateIps: true, connectTimeout: 30 },
          "connection_save_config",
        );
        expect(backend.connection.restrict_private_ips).toBe(true);
        expect(useSettingsStore.getState().restrictPrivateIps).toBe(true);
        await useSettingsStore.getState().setRestrictPrivateIps(false);

        await startUp();
        expect(callsTo("connection_set_restrict_private_ips")).toStrictEqual([{ enabled: false }]);
        expect(backend.connection).toMatchObject({
          restrict_private_ips: false,
          connect_timeout_secs: 30,
        });
      });

      it("forget an older on once the check is on, as when the user keeps it on", async () => {
        await failedFirstStart(
          { restrictPrivateIps: true, connectTimeout: 30 },
          "connection_save_config",
        );
        expect(useSettingsStore.getState().restrictPrivateIps).toBe(true);
        backend.userAllowsOff = false;
        await useSettingsStore.getState().setRestrictPrivateIps(false);

        await startUp();
        // Nothing left to send: no second, handed-over "on".
        expect(callsTo("connection_set_restrict_private_ips")).toStrictEqual([{ enabled: false }]);
        expect(backend.connection.restrict_private_ips).toBe(true);
      });

      it("still move an older on when a check that is off is switched off", async () => {
        await failedFirstStart({ restrictPrivateIps: true }, "connection_set_restrict_private_ips");
        expect(useSettingsStore.getState().restrictPrivateIps).toBe(false);
        // The backend asks nothing here, so this is no one's confirmation.
        await useSettingsStore.getState().setRestrictPrivateIps(false);

        await startUp();
        expect(callsTo("connection_set_restrict_private_ips")).toStrictEqual([
          { enabled: false },
          { enabled: true },
        ]);
        expect(backend.connection.restrict_private_ips).toBe(true);
      });

      it("keep the check failing closed for what could not be read", async () => {
        localStorage.setItem(LEGACY_KEY, "{damaged");
        await useSettingsStore.getState().setKeepaliveMax(7);
        expect(localStorage.getItem(LEGACY_KEY)).toBe("{damaged");

        invokeMock.mockClear();
        await startUp();
        expect(callsTo("connection_set_restrict_private_ips")).toStrictEqual([{ enabled: true }]);
        expect(backend.connection.keepalive_max).toBe(7);
      });

      it("let go of what could not be read once the check is on", async () => {
        localStorage.setItem(LEGACY_KEY, "{damaged");
        await useSettingsStore.getState().setRestrictPrivateIps(true);
        expect(localStorage.getItem(LEGACY_KEY)).toBeNull();
      });

      it("are gone once the user has set every one of them", async () => {
        await failedFirstStart(
          { restrictPrivateIps: true, keepaliveInterval: 0 },
          "connection_set_restrict_private_ips",
        );
        await useSettingsStore.getState().setRestrictPrivateIps(true);
        expect(JSON.parse(localStorage.getItem(LEGACY_KEY) ?? "null")).toEqual({
          keepaliveInterval: 0,
        });
        await useSettingsStore.getState().setKeepaliveInterval(30);
        expect(localStorage.getItem(LEGACY_KEY)).toBeNull();

        invokeMock.mockClear();
        await startUp();
        expect(callsTo("connection_set_restrict_private_ips")).toEqual([]);
        expect(callsTo("connection_save_config")).toEqual([]);
      });
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

describe("command buttons left in localStorage", () => {
  function storeState(state: Record<string, unknown>): void {
    localStorage.setItem(SETTINGS_KEY, JSON.stringify({ state, version: 1 }));
  }

  it("keeps the buttons of a list that also holds other values", async () => {
    storeState({
      commandButtons: [
        null,
        button(ID_A, "a"),
        "uptime",
        7,
        [],
        {},
        { ...button(ID_B, "b"), color: "red" },
        button(ID_C, "c"),
      ],
    });
    await useSettingsStore.persist.rehydrate();
    expect(useSettingsStore.getState().commandButtons).toEqual([
      button(ID_A, "a"),
      button(ID_C, "c"),
    ]);
  });

  it("still checks the settings that come after a list holding null", async () => {
    storeState({
      commandButtons: [null],
      terminalFontFamily: "Comic Sans MS",
      terminalBackgroundColor: "red",
      autoCheckForUpdates: "no",
    });
    await useSettingsStore.persist.rehydrate();
    const state = useSettingsStore.getState();
    expect(state.commandButtons).toEqual([]);
    expect(state.terminalFontFamily).toBe("JetBrains Mono");
    expect(state.terminalBackgroundColor).toBeNull();
    expect(state.autoCheckForUpdates).toBe(true);
    expect(useSettingsStore.persist.hasHydrated()).toBe(true);
  });

  it.each([
    ["null", null],
    ["a string", "uptime"],
    ["a number", 7],
    ["an object", { 0: button(ID_A, "a"), length: 1 }],
  ])("starts with no buttons when %s is stored in place of the list", async (_label, value) => {
    storeState({ commandButtons: value });
    await useSettingsStore.persist.rehydrate();
    expect(useSettingsStore.getState().commandButtons).toEqual([]);
  });

  it("takes no more entries than the bar holds", async () => {
    storeState({
      commandButtons: Array.from({ length: 1001 }, (_, i) =>
        button(`00000000-0000-4000-8000-${String(i).padStart(12, "0")}`, `b${String(i)}`),
      ),
    });
    await useSettingsStore.persist.rehydrate();
    expect(useSettingsStore.getState().commandButtons).toHaveLength(1000);
  });

  // The cut comes before the check, so a huge list costs no more than a full bar.
  it("does not look past the limit for buttons", async () => {
    storeState({
      commandButtons: [...Array.from({ length: 1000 }, () => null), button(ID_A, "a")],
    });
    await useSettingsStore.persist.rehydrate();
    expect(useSettingsStore.getState().commandButtons).toEqual([]);
  });

  it("keeps only a button's own fields", async () => {
    storeState({ commandButtons: [{ ...button(ID_A, "a"), extra: "<script>" }] });
    await useSettingsStore.persist.rehydrate();
    expect(useSettingsStore.getState().commandButtons).toEqual([button(ID_A, "a")]);
  });

  // The hidden-character check is for buttons from someone else's file. A
  // button made in the app may hold such a character, and must not vanish.
  it("keeps a button the import would refuse for a hidden character", async () => {
    const own = button(ID_A, "up\u00a0time");
    expect(importCommandButtons([own])).toEqual({ added: 0, skipped: 1 });
    storeState({ commandButtons: [own] });
    await useSettingsStore.persist.rehydrate();
    expect(useSettingsStore.getState().commandButtons).toEqual([own]);
  });

  it("drops a button whose id an earlier one has, in either letter case", async () => {
    storeState({
      commandButtons: [
        button(ID_A, "first"),
        button(ID_A, "same id"),
        button(ID_A.toUpperCase(), "same id in uppercase"),
        button(ID_B, "b"),
      ],
    });
    await useSettingsStore.persist.rehydrate();
    expect(useSettingsStore.getState().commandButtons).toEqual([
      button(ID_A, "first"),
      button(ID_B, "b"),
    ]);
  });
});

describe("settings left in localStorage", () => {
  function storeState(state: Record<string, unknown>): void {
    localStorage.setItem(SETTINGS_KEY, JSON.stringify({ state, version: 1 }));
  }

  const ON_OFF_DEFAULTS = {
    closeOnDisconnect: false,
    openLocalOnStartup: false,
    confirmOnQuit: true,
    confirmOnCloseTab: true,
    copyOnSelect: false,
    pasteOnRightClick: false,
    confirmMultilinePaste: false,
    toastAutoDismiss: true,
    autoCheckForUpdates: true,
  };
  const ON_OFF_KEYS = Object.keys(ON_OFF_DEFAULTS) as (keyof typeof ON_OFF_DEFAULTS)[];
  // What a setting gets when the stored value is no boolean: the careful
  // side. Only the multi-line paste confirmation differs from its default.
  const ON_OFF_FALLBACKS = { ...ON_OFF_DEFAULTS, confirmMultilinePaste: true };

  it("keeps the defaults of settings that are not stored", async () => {
    storeState({});
    await useSettingsStore.persist.rehydrate();
    expect(useSettingsStore.getState()).toMatchObject({
      ...ON_OFF_DEFAULTS,
      language: "",
      uiScale: 100,
      themeMode: "system",
      terminalFontSize: 14,
      commandButtons: [],
    });
  });

  it("lists every on/off setting the store persists", () => {
    const initial = useSettingsStore.getInitialState();
    const booleans = Object.entries(initial)
      .filter(([key, value]) => typeof value === "boolean" && !BACKEND_KEYS.includes(key))
      .map(([key]) => key);
    expect(booleans.sort()).toEqual([...ON_OFF_KEYS].sort());
    expect(initial).toMatchObject(ON_OFF_DEFAULTS);
  });

  it("reads each on/off setting when it is a boolean", async () => {
    const flipped = Object.fromEntries(ON_OFF_KEYS.map((key) => [key, !ON_OFF_DEFAULTS[key]]));
    storeState(flipped);
    await useSettingsStore.persist.rehydrate();
    expect(useSettingsStore.getState()).toMatchObject(flipped);
  });

  // A string is truthy, "false" included, so it would count as on. Whatever
  // is stored, a confirmation must not end up off, nor pasting on a
  // right-click, copying on selection or closing a tab on disconnect on.
  it.each([
    ["the string of its value", (on: boolean) => String(on)],
    ["the string of its opposite", (on: boolean) => String(!on)],
    ["a number", (on: boolean) => (on ? 0 : 1)],
    ["null", () => null],
    ["an object", () => ({})],
  ])("takes the careful side for an on/off setting stored as %s", async (_label, damaged) => {
    storeState(Object.fromEntries(ON_OFF_KEYS.map((key) => [key, damaged(ON_OFF_DEFAULTS[key])])));
    await useSettingsStore.persist.rehydrate();
    expect(useSettingsStore.getState()).toMatchObject(ON_OFF_FALLBACKS);
  });

  it.each([["sepia"], [7], [null], [["dark"]]])(
    "follows the system theme when %j is stored as the theme",
    async (value) => {
      storeState({ themeMode: value });
      await useSettingsStore.persist.rehydrate();
      expect(useSettingsStore.getState().themeMode).toBe("system");
    },
  );

  it.each([[5], [73], [5.99], ["20"], [null]])(
    "uses the default font size when %j is stored as the size",
    async (value) => {
      storeState({ terminalFontSize: value });
      await useSettingsStore.persist.rehydrate();
      expect(useSettingsStore.getState().terminalFontSize).toBe(14);
    },
  );

  it.each([[6], [20], [72]])("reads a stored font size of %d", async (value) => {
    storeState({ terminalFontSize: value });
    await useSettingsStore.persist.rehydrate();
    expect(useSettingsStore.getState().terminalFontSize).toBe(value);
  });

  it.each([["dark"], ["light"], ["system"]])("reads the stored theme %j", async (value) => {
    storeState({ themeMode: value });
    await useSettingsStore.persist.rehydrate();
    expect(useSettingsStore.getState().themeMode).toBe(value);
  });

  it.each([
    [75, 75],
    [110, 110],
    [125, 125],
    [150, 125],
    [10, 75],
    ["110", 100],
    [null, 100],
    [{}, 100],
  ])("reads a stored UI scale of %j as %d", async (value, expected) => {
    storeState({ uiScale: value });
    await useSettingsStore.persist.rehydrate();
    expect(useSettingsStore.getState().uiScale).toBe(expected);
  });

  it.each([["xx"], [7], [null], [["de"]]])(
    "follows the system language when %j is stored as the language",
    async (value) => {
      storeState({ language: value });
      await useSettingsStore.persist.rehydrate();
      expect(useSettingsStore.getState().language).toBe("");
      expect(changeLanguage).not.toHaveBeenCalled();
    },
  );

  it("never lets a stored value replace an action", async () => {
    storeState({ addCommandButton: "x", setThemeMode: null, themeMode: "light" });
    await useSettingsStore.persist.rehydrate();
    const state = useSettingsStore.getState();
    expect(state.themeMode).toBe("light");
    expect(typeof state.addCommandButton).toBe("function");
    expect(typeof state.setThemeMode).toBe("function");
  });

  // Written as text, since `__proto__` in an object literal would set the
  // prototype instead of a key; parsed from JSON it is a key like any other.
  // The last three are names every object answers to without having them.
  it("takes nothing stored under a name it does not know", async () => {
    const unknown = ["somethingElse", "__proto__", "constructor", "toString"];
    localStorage.setItem(
      SETTINGS_KEY,
      `{"state":{"somethingElse":1,"__proto__":{"polluted":true},"constructor":"x",` +
        `"toString":"x","themeMode":"light"},"version":1}`,
    );
    expect(Object.keys(stored().state)).toEqual([...unknown, "themeMode"]);
    await useSettingsStore.persist.rehydrate();
    const state = useSettingsStore.getState();
    expect(state.themeMode).toBe("light");
    expect(Object.keys(state).filter((key) => unknown.includes(key))).toEqual([]);
    expect(Object.getPrototypeOf(state)).toBe(Object.prototype);
    // Nor is it written back with the next change.
    state.setThemeMode("dark");
    expect(Object.keys(stored().state).filter((key) => unknown.includes(key))).toEqual([]);
  });

  // At startup the check runs while the store is being created, before the
  // rest of the module has run. A fresh copy of the module shows that it
  // gets through there too; `rehydrate` above only ever runs later.
  it("is checked when the app starts", async () => {
    storeState({
      language: 7,
      themeMode: "sepia",
      pasteOnRightClick: "false",
      confirmOnQuit: "false",
      confirmMultilinePaste: 0,
      uiScale: "big",
      terminalFontSize: 500,
      terminalFontFamily: "Comic Sans MS",
      terminalBackgroundColor: "red",
      commandButtons: [null, button(ID_A, "a"), button(ID_A, "again")],
      setThemeMode: "x",
    });
    // The fresh copy stays out of the other tests: they use the copy this
    // file imported, it is not changed here, so it writes nothing, and it
    // calls no backend command while it loads.
    vi.resetModules();
    const fresh = await import("./settingsStore");
    expect(fresh.useSettingsStore).not.toBe(useSettingsStore);
    expect(fresh.useSettingsStore.persist.hasHydrated()).toBe(true);
    const state = fresh.useSettingsStore.getState();
    expect(state).toMatchObject({
      language: "",
      themeMode: "system",
      pasteOnRightClick: false,
      confirmOnQuit: true,
      confirmMultilinePaste: true,
      uiScale: 100,
      terminalFontSize: 14,
      terminalFontFamily: "JetBrains Mono",
      terminalBackgroundColor: null,
      commandButtons: [button(ID_A, "a")],
    });
    expect(typeof state.setThemeMode).toBe("function");
  });
});

describe("setUiScale", () => {
  it.each([
    [100, 100],
    [75, 75],
    [125, 125],
    [150, 125],
    [10, 75],
  ])("stores %d as %d", (scale, expected) => {
    useSettingsStore.getState().setUiScale(scale);
    expect(useSettingsStore.getState().uiScale).toBe(expected);
  });

  it.each([[Number.NaN], [Number.POSITIVE_INFINITY], [Number.NEGATIVE_INFINITY]])(
    "ignores %d",
    (scale) => {
      useSettingsStore.getState().setUiScale(110);
      useSettingsStore.getState().setUiScale(scale);
      expect(useSettingsStore.getState().uiScale).toBe(110);
    },
  );
});

describe("addCommandButton", () => {
  it("adds a button with its own fields only", () => {
    const withExtra = { ...button(ID_A, "a"), extra: "<script>" };
    useSettingsStore.getState().addCommandButton(withExtra);
    expect(useSettingsStore.getState().commandButtons).toEqual([button(ID_A, "a")]);
  });

  it.each([
    ["null", null],
    ["a string", "uptime"],
    ["an empty object", {}],
    ["a button without a UUID", button("not-a-uuid", "a")],
    ["a button with a bad color", { ...button(ID_A, "a"), color: "red" }],
    ["a button with too long a name", button(ID_A, "x".repeat(33))],
  ])("ignores %s", (_label, value) => {
    useSettingsStore.setState({ commandButtons: [button(ID_B, "b")] });
    useSettingsStore.getState().addCommandButton(value as CommandButton);
    expect(useSettingsStore.getState().commandButtons).toEqual([button(ID_B, "b")]);
  });

  // The startup check keeps an id as it is stored, so the bar can hold one
  // in uppercase; either side of the comparison may be the uppercase one.
  it.each([
    ["the same id", ID_A, ID_A],
    ["its id in uppercase", ID_A, ID_A.toUpperCase()],
    ["its id in lowercase", ID_A.toUpperCase(), ID_A],
  ])("ignores a button when one in the bar has %s", (_label, inBar, added) => {
    useSettingsStore.setState({ commandButtons: [button(inBar, "a")] });
    useSettingsStore.getState().addCommandButton(button(added, "other", "ls\\r"));
    expect(useSettingsStore.getState().commandButtons).toEqual([button(inBar, "a")]);
  });

  it("ignores a button when the bar is full", () => {
    const full = Array.from({ length: 1000 }, (_, i) =>
      button(`00000000-0000-4000-8000-${String(i).padStart(12, "0")}`, `b${String(i)}`),
    );
    useSettingsStore.setState({ commandButtons: full });
    useSettingsStore.getState().addCommandButton(button(ID_A, "a"));
    expect(useSettingsStore.getState().commandButtons).toHaveLength(1000);
  });
});

describe("updateCommandButton", () => {
  beforeEach(() => {
    useSettingsStore.setState({ commandButtons: [button(ID_A, "a"), button(ID_B, "b")] });
  });

  it("changes the fields it is given", () => {
    useSettingsStore.getState().updateCommandButton(ID_B, { name: "new", color: "#a6e3a1" });
    expect(useSettingsStore.getState().commandButtons).toEqual([
      button(ID_A, "a"),
      { ...button(ID_B, "new"), color: "#a6e3a1" },
    ]);
  });

  // The type keeps an id out of the changes; this is for a caller that
  // passes one anyway, which would leave two buttons with one id.
  it("keeps the id, and takes nothing but a button's own fields", () => {
    const changes = { name: "new", id: ID_A, extra: "<script>" };
    useSettingsStore.getState().updateCommandButton(ID_B, changes);
    expect(useSettingsStore.getState().commandButtons).toEqual([
      button(ID_A, "a"),
      button(ID_B, "new"),
    ]);
  });

  it.each([
    ["an empty name", { name: "" }],
    ["too long a name", { name: "x".repeat(33) }],
    ["an empty command", { command: "" }],
    ["too long a command", { command: "x".repeat(1025) }],
    ["a bad color", { color: "red" }],
    ["a name that is no string", { name: 7 as unknown as string }],
    ["a name set to undefined", { name: undefined as unknown as string }],
  ])("leaves the button as it is for %s", (_label, changes) => {
    useSettingsStore.getState().updateCommandButton(ID_B, changes);
    expect(useSettingsStore.getState().commandButtons).toEqual([
      button(ID_A, "a"),
      button(ID_B, "b"),
    ]);
  });

  it("does nothing for an id that is not there", () => {
    useSettingsStore.getState().updateCommandButton(ID_C, { name: "new" });
    expect(useSettingsStore.getState().commandButtons).toEqual([
      button(ID_A, "a"),
      button(ID_B, "b"),
    ]);
  });
});

describe("duplicateCommandButton", () => {
  const UUID = /^[\da-f]{8}-[\da-f]{4}-[\da-f]{4}-[\da-f]{4}-[\da-f]{12}$/;

  /** Duplicate the only button, named `name`, and return the copy's name. */
  function copyNameOf(name: string): string | undefined {
    useSettingsStore.setState({ commandButtons: [button(ID_A, name)] });
    useSettingsStore.getState().duplicateCommandButton(ID_A);
    return useSettingsStore.getState().commandButtons[1]?.name;
  }

  it("adds a copy under a new id after the existing buttons", () => {
    useSettingsStore.setState({ commandButtons: [button(ID_A, "a", "ls\\r"), button(ID_B, "b")] });
    useSettingsStore.getState().duplicateCommandButton(ID_A);
    const buttons = useSettingsStore.getState().commandButtons;
    expect(buttons).toHaveLength(3);
    const copyId = buttons[2]?.id;
    expect(buttons[2]).toEqual({ ...button(ID_A, "a (copy)", "ls\\r"), id: copyId });
    expect(copyId).toMatch(UUID);
    expect([ID_A, ID_B]).not.toContain(copyId);
  });

  // A name that fits is kept as it is, a space at its end included.
  it("does not touch a name that leaves room for the suffix", () => {
    expect(copyNameOf("ls ")).toBe("ls  (copy)");
  });

  // Such a button can only come from a store filled past its own checks.
  it("copies only a button's own fields, and nothing that is no button", () => {
    const withExtra = { ...button(ID_A, "a"), extra: "<script>" };
    useSettingsStore.setState({ commandButtons: [withExtra] });
    useSettingsStore.getState().duplicateCommandButton(ID_A);
    const afterCopy = useSettingsStore.getState().commandButtons;
    expect(afterCopy).toHaveLength(2);
    expect(Object.keys(afterCopy[1]).sort()).toEqual(["color", "command", "id", "name"]);

    const malformed = { ...button(ID_B, "b"), color: "red" };
    useSettingsStore.setState({ commandButtons: [malformed] });
    useSettingsStore.getState().duplicateCommandButton(ID_B);
    expect(useSettingsStore.getState().commandButtons).toEqual([malformed]);
  });

  it.each([
    ["a name with room for the suffix", "x".repeat(25), "x".repeat(25) + " (copy)"],
    ["a name one character too long for it", "x".repeat(26), "x".repeat(25) + " (copy)"],
    ["a name of the full length", "x".repeat(32), "x".repeat(25) + " (copy)"],
    ["a name cut at a space", "x".repeat(24) + " yz", "x".repeat(24) + " (copy)"],
    // The emoji is two UTF-16 units, and the cut would fall between them.
    ["a name cut at an emoji", "x".repeat(24) + "\u{1F600}yz", "x".repeat(24) + " (copy)"],
  ])("shortens %s so that the copy's name fits", (_label, name, expected) => {
    const copy = copyNameOf(name);
    expect(copy).toBe(expected);
    expect(copy?.length).toBeLessThanOrEqual(32);
  });

  it("makes a copy that is still there after a restart", async () => {
    copyNameOf("x".repeat(32));
    const before = useSettingsStore.getState().commandButtons;
    expect(before).toHaveLength(2);
    await useSettingsStore.persist.rehydrate();
    expect(useSettingsStore.getState().commandButtons).toEqual(before);
  });

  it("does nothing when the bar is full", () => {
    const full = Array.from({ length: 1000 }, (_, i) =>
      button(`00000000-0000-4000-8000-${String(i).padStart(12, "0")}`, `b${String(i)}`),
    );
    useSettingsStore.setState({ commandButtons: full });
    useSettingsStore.getState().duplicateCommandButton(full[0]?.id ?? "");
    expect(useSettingsStore.getState().commandButtons).toHaveLength(1000);
  });

  it("does nothing for an id that is not there", () => {
    useSettingsStore.setState({ commandButtons: [button(ID_A, "a")] });
    useSettingsStore.getState().duplicateCommandButton(ID_B);
    expect(useSettingsStore.getState().commandButtons).toEqual([button(ID_A, "a")]);
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
    ["a Khmer inherent vowel, which shows nothing", "uptime\u17b4"],
    ["the other Khmer inherent vowel", "uptime\u17b5"],
  ])("skips a button whose name or command holds %s", (_label, text) => {
    const result = importCommandButtons([button(ID_A, text), button(ID_B, "b", text)]);
    expect(result).toEqual({ added: 0, skipped: 2 });
  });

  // Code points Unicode sets aside for characters that show nothing, but
  // has not assigned yet: no category covers them, only that property does.
  it.each([[0x2065], [0xfff0], [0xe0fff]])(
    "skips a button whose name or command holds the unassigned invisible character %i",
    (codePoint) => {
      const text = `up${String.fromCodePoint(codePoint)}time`;
      const result = importCommandButtons([button(ID_A, text), button(ID_B, "b", text)]);
      expect(result).toEqual({ added: 0, skipped: 2 });
    },
  );

  // An emoji's presentation selector, a Mongolian one and an ideographic one.
  it.each([[0xfe0f], [0x180b], [0xe0100]])(
    "keeps the variation selector %i in a name, but not in a command",
    (codePoint) => {
      const text = `a${String.fromCodePoint(codePoint)}`;
      const result = importCommandButtons([button(ID_A, text), button(ID_B, "b", text)]);
      expect(result).toEqual({ added: 1, skipped: 1 });
      expect(useSettingsStore.getState().commandButtons[0]?.name).toBe(text);
    },
  );

  it.each([
    ["a space", " "],
    ["spaces", "   "],
    ["an emoji joiner", String.fromCodePoint(0x200d)],
    [
      "emoji joiners and spaces",
      ` ${String.fromCodePoint(0x200d)} ${String.fromCodePoint(0x200d)}`,
    ],
  ])("skips a button whose name is nothing but %s", (_label, name) => {
    const result = importCommandButtons([button(ID_A, name)]);
    expect(result).toEqual({ added: 0, skipped: 1 });
  });

  it("keeps a name with spaces around what it shows", () => {
    const result = importCommandButtons([button(ID_A, " a b ")]);
    expect(result).toEqual({ added: 1, skipped: 0 });
    expect(useSettingsStore.getState().commandButtons[0]?.name).toBe(" a b ");
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
