/**
 * Data export / import: reading export files and running an import.
 *
 * parseExportFile decides which sections the import dialog offers, so it
 * must recognise every shape a real file can have (including the bare
 * database export written before the envelope existed) and refuse anything
 * else before a single section is applied.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import {
  EXPORT_FORMAT,
  EXPORT_VERSION,
  ExportFileError,
  exportData,
  hasSection,
  importData,
  parseExportFile,
  type ParsedExportFile,
} from "./dataTransfer";
import { getExportableSettings, useSettingsStore } from "./stores/settingsStore";

const { loadSessions, loadProfiles, loadSequences } = vi.hoisted(() => ({
  loadSessions: vi.fn(() => Promise.resolve()),
  loadProfiles: vi.fn(() => Promise.resolve()),
  loadSequences: vi.fn(() => Promise.resolve()),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("./i18n", () => ({
  default: { t: (key: string) => key, changeLanguage: vi.fn(() => Promise.resolve()) },
}));
vi.mock("./stores/sessionStore", () => ({
  useSessionStore: { getState: () => ({ loadAll: loadSessions }) },
}));
vi.mock("./stores/highlightStore", () => ({
  useHighlightStore: { getState: () => ({ loadProfiles }) },
}));
vi.mock("./stores/loginSequenceStore", () => ({
  useLoginSequenceStore: { getState: () => ({ loadAll: loadSequences }) },
}));

const invokeMock = vi.mocked(invoke);
const BUTTON = {
  id: "9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d",
  name: "a",
  command: "ls",
  color: "#000000",
};
const DATABASE = { folders: [{ id: "f" }], sessions: [{ id: "s1" }, { id: "s2" }] };

function envelope(sections: Record<string, unknown>): string {
  return JSON.stringify({
    format: EXPORT_FORMAT,
    version: EXPORT_VERSION,
    app_version: "0.30.0",
    exported_at: "2026-10-27T12:00:00+00:00",
    ...sections,
  });
}

function parseError(text: string): string {
  try {
    parseExportFile(text);
  } catch (e) {
    if (e instanceof ExportFileError) return e.code;
    throw e;
  }
  throw new Error("expected parseExportFile to throw");
}

beforeEach(() => {
  vi.clearAllMocks();
  invokeMock.mockImplementation(() => Promise.resolve());
  useSettingsStore.setState(useSettingsStore.getInitialState(), true);
});

describe("parseExportFile", () => {
  it("reads every section of a full export", () => {
    const parsed = parseExportFile(
      envelope({ database: DATABASE, settings: { themeMode: "light" }, command_buttons: [BUTTON] }),
    );
    expect(parsed).toEqual({
      legacy: false,
      appVersion: "0.30.0",
      exportedAt: "2026-10-27T12:00:00+00:00",
      database: DATABASE,
      settings: { themeMode: "light" },
      commandButtons: [BUTTON],
    });
  });

  it("leaves out the sections the file does not hold", () => {
    const parsed = parseExportFile(envelope({ command_buttons: [BUTTON] }));
    expect(parsed.database).toBeUndefined();
    expect(parsed.settings).toBeUndefined();
    expect(parsed.commandButtons).toEqual([BUTTON]);
    expect(hasSection(parsed, "database")).toBe(false);
    expect(hasSection(parsed, "settings")).toBe(false);
    expect(hasSection(parsed, "commandButtons")).toBe(true);
  });

  it("keeps a section that was exported empty", () => {
    const parsed = parseExportFile(envelope({ command_buttons: [] }));
    expect(parsed.commandButtons).toEqual([]);
    expect(hasSection(parsed, "commandButtons")).toBe(true);
  });

  it("reads a bare database export from before the envelope as database-only", () => {
    const parsed = parseExportFile(JSON.stringify({ ...DATABASE, credentials: [] }));
    expect(parsed).toEqual({
      legacy: true,
      appVersion: null,
      exportedAt: null,
      database: { ...DATABASE, credentials: [] },
    });
  });

  it.each([
    ["too long", "1".repeat(33)],
    ["not a version", "trusted by IT"],
  ])("drops an app version that is %s", (_label, appVersion) => {
    const parsed = parseExportFile(envelope({ app_version: appVersion, settings: {} }));
    expect(parsed.appVersion).toBeNull();
  });

  it("keeps a pre-release app version", () => {
    const parsed = parseExportFile(envelope({ app_version: "0.31.0-beta.2", settings: {} }));
    expect(parsed.appVersion).toBe("0.31.0-beta.2");
  });

  it.each([
    ["not a date", "yesterday"],
    // A valid date that only the length cap refuses.
    ["too long", `2026-10-27T12:00:00.${"0".repeat(60)}Z`],
  ])("drops an export date that is %s", (_label, exportedAt) => {
    const parsed = parseExportFile(envelope({ exported_at: exportedAt, settings: {} }));
    expect(parsed.exportedAt).toBeNull();
  });

  const tooManySettings = Object.fromEntries(
    Array.from({ length: 65 }, (_, i) => [`k${String(i)}`, i]),
  );
  const tooManyButtons = Array.from({ length: 1001 }, () => BUTTON);

  it.each([
    ["not JSON", "{", "invalidJson"],
    ["an array", "[]", "unknownFormat"],
    ["an unrelated object", JSON.stringify({ hello: 1 }), "unknownFormat"],
    ["another format", JSON.stringify({ format: "other", version: 1 }), "unknownFormat"],
    ["a missing version", JSON.stringify({ format: EXPORT_FORMAT, settings: {} }), "unknownFormat"],
    [
      "a newer version",
      JSON.stringify({ format: EXPORT_FORMAT, version: EXPORT_VERSION + 1, settings: {} }),
      "newerVersion",
    ],
    ["a malformed database", envelope({ database: { folders: [] } }), "invalidSection"],
    ["settings that are not an object", envelope({ settings: [] }), "invalidSection"],
    [
      "more settings than an export holds",
      envelope({ settings: tooManySettings }),
      "invalidSection",
    ],
    ["buttons that are not a list", envelope({ command_buttons: {} }), "invalidSection"],
    [
      "more buttons than the bar holds",
      envelope({ command_buttons: tooManyButtons }),
      "invalidSection",
    ],
    ["no sections", envelope({}), "empty"],
  ])("refuses %s", (_name, text, code) => {
    expect(parseError(text)).toBe(code);
  });
});

describe("exportData", () => {
  it("sends only the selected client-side sections", async () => {
    useSettingsStore.setState({ commandButtons: [BUTTON] });
    invokeMock.mockResolvedValueOnce({ folders: 0, sessions: 0 });

    const result = await exportData(
      { database: false, settings: false, commandButtons: true },
      "Export Data",
    );

    expect(invokeMock).toHaveBeenCalledWith("data_export_file", {
      title: "Export Data",
      includeDatabase: false,
      settings: null,
      commandButtons: [BUTTON],
    });
    expect(result).toEqual({ folders: 0, sessions: 0, commandButtons: 1 });
  });

  it("asks the backend for the database and sends the settings", async () => {
    invokeMock.mockResolvedValueOnce({ folders: 2, sessions: 5 });

    const result = await exportData(
      { database: true, settings: true, commandButtons: false },
      "Export Data",
    );

    expect(invokeMock).toHaveBeenCalledWith("data_export_file", {
      title: "Export Data",
      includeDatabase: true,
      settings: getExportableSettings(),
      commandButtons: null,
    });
    expect(result).toEqual({ folders: 2, sessions: 5, commandButtons: null });
  });

  it("resolves to null when the save dialog is cancelled", async () => {
    invokeMock.mockResolvedValueOnce(null);
    const result = await exportData(
      { database: true, settings: true, commandButtons: true },
      "Export Data",
    );
    expect(result).toBeNull();
  });
});

describe("importData", () => {
  const ALL = { database: true, settings: true, commandButtons: true };
  const file: ParsedExportFile = {
    legacy: false,
    appVersion: "0.30.0",
    exportedAt: null,
    database: DATABASE,
    settings: { themeMode: "light" },
    commandButtons: [BUTTON],
  };

  function expectStoresReloaded(): void {
    expect(loadSessions).toHaveBeenCalledOnce();
    expect(loadProfiles).toHaveBeenCalledOnce();
    expect(loadSequences).toHaveBeenCalledOnce();
  }

  it("imports every section and reloads what the database feeds", async () => {
    invokeMock.mockImplementation((cmd) =>
      cmd === "db_import" ? Promise.resolve("Imported 1 folder, 2 sessions") : Promise.resolve(),
    );

    const results = await importData(file, ALL);

    expect(results).toEqual([
      { section: "database", ok: true, summary: "Imported 1 folder, 2 sessions" },
      { section: "settings", ok: true },
      { section: "commandButtons", ok: true, added: 1, skipped: 0 },
    ]);
    expect(invokeMock).toHaveBeenCalledWith("db_import", { data: DATABASE });
    expectStoresReloaded();
    expect(useSettingsStore.getState().themeMode).toBe("light");
    expect(useSettingsStore.getState().commandButtons).toEqual([BUTTON]);
  });

  it("imports only the selected sections", async () => {
    const results = await importData(file, {
      database: false,
      settings: true,
      commandButtons: false,
    });
    expect(results).toEqual([{ section: "settings", ok: true }]);
    expect(invokeMock).not.toHaveBeenCalled();
    expect(loadSessions).not.toHaveBeenCalled();
    expect(useSettingsStore.getState().themeMode).toBe("light");
    expect(useSettingsStore.getState().commandButtons).toEqual([]);
  });

  it("leaves out a selected section the file does not hold", async () => {
    const buttonsOnly: ParsedExportFile = { ...file, database: undefined, settings: undefined };
    const results = await importData(buttonsOnly, ALL);
    expect(results).toEqual([{ section: "commandButtons", ok: true, added: 1, skipped: 0 }]);
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("imports a bare database export as its database section", async () => {
    invokeMock.mockResolvedValueOnce("Imported");
    const legacy = parseExportFile(JSON.stringify(DATABASE));
    const results = await importData(legacy, ALL);
    expect(results).toEqual([{ section: "database", ok: true, summary: "Imported" }]);
    expect(invokeMock).toHaveBeenCalledWith("db_import", { data: DATABASE });
  });

  it("reports a failed database import, reloads anyway and still imports the rest", async () => {
    // Tauri rejects with the command's error string; db_import is the
    // first command this import sends.
    invokeMock.mockRejectedValueOnce("boom");

    const results = await importData(file, ALL);

    expect(results).toEqual([
      { section: "database", ok: false, error: "boom" },
      { section: "settings", ok: true },
      { section: "commandButtons", ok: true, added: 1, skipped: 0 },
    ]);
    expectStoresReloaded();
  });

  it("logs a reload that fails after the import", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    loadSessions.mockRejectedValueOnce("offline");
    invokeMock.mockResolvedValueOnce("Imported");

    const results = await importData(file, {
      database: true,
      settings: false,
      commandButtons: false,
    });

    expect(results).toEqual([{ section: "database", ok: true, summary: "Imported" }]);
    expect(warn).toHaveBeenCalledWith("reload after import failed:", "offline");
    warn.mockRestore();
  });

  it("reports a failing settings section without stopping the buttons", async () => {
    const settings = {
      get themeMode(): string {
        throw new Error("unreadable");
      },
    };
    const results = await importData({ ...file, database: undefined, settings }, ALL);
    expect(results).toEqual([
      { section: "settings", ok: false, error: "unreadable" },
      { section: "commandButtons", ok: true, added: 1, skipped: 0 },
    ]);
  });
});
