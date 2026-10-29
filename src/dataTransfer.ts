import { invoke } from "@tauri-apps/api/core";
import {
  getExportableCommandButtons,
  getExportableSettings,
  importCommandButtons,
  importSettings,
} from "./stores/settingsStore";
import { useSessionStore } from "./stores/sessionStore";
import { useHighlightStore } from "./stores/highlightStore";
import { useLoginSequenceStore } from "./stores/loginSequenceStore";

/**
 * The data export file: any mix of the database, the client settings and the
 * command bar buttons. The backend writes it (`data_export_file`, see
 * src-tauri/src/commands/data_transfer.rs); importing happens here, because
 * settings and buttons live in this webview's localStorage.
 */

/** Must match EXPORT_FORMAT in data_transfer.rs. */
export const EXPORT_FORMAT = "shellstation-export";
/** The newest file version this app can read; matches EXPORT_VERSION. */
export const EXPORT_VERSION = 1;
/**
 * Files above this size are refused before they are read into memory. A
 * database at the backend's import caps (50,000 sessions and as many
 * credentials) comes close to this, so it is not set any lower.
 */
export const MAX_IMPORT_FILE_SIZE = 100 * 1024 * 1024;
// What an app version looks like ("0.30.0", "0.31.0-beta.2"); anything else
// in a file is not shown.
const VERSION_CORE_RE = /^\d{1,5}\.\d{1,5}\.\d{1,5}$/;
const PRE_RELEASE_RE = /^[\w.]{1,16}$/;
/** An export date longer than this is not an ISO 8601 timestamp. */
const MAX_EXPORTED_AT_LENGTH = 64;
// Mirror the limits data_transfer.rs applies when writing a file, so a file
// that could not have been exported is refused before anything is applied.
const MAX_SETTINGS_ENTRIES = 64;
const MAX_COMMAND_BUTTONS = 1000;

export type DataSection = "database" | "settings" | "commandButtons";
export const DATA_SECTIONS: readonly DataSection[] = ["database", "settings", "commandButtons"];

export type SectionSelection = Record<DataSection, boolean>;

/** The database section, as `db_import` expects it. */
export interface DatabaseExport {
  folders: unknown[];
  sessions: unknown[];
  [key: string]: unknown;
}

export interface ParsedExportFile {
  /** A bare database export, written before the export envelope existed. */
  legacy: boolean;
  /** The app version that wrote the file, if it says. */
  appVersion: string | null;
  /** When the file was written (ISO 8601), if it says. */
  exportedAt: string | null;
  database?: DatabaseExport;
  settings?: Record<string, unknown>;
  commandButtons?: unknown[];
}

export type ExportFileErrorCode =
  | "invalidJson"
  | "unknownFormat"
  | "newerVersion"
  | "invalidSection"
  | "empty";

/** A file that cannot be imported. The UI maps `code` to a translated message. */
export class ExportFileError extends Error {
  readonly code: ExportFileErrorCode;

  constructor(code: ExportFileErrorCode) {
    super(code);
    this.name = "ExportFileError";
    this.code = code;
  }
}

function isAppVersion(value: string): boolean {
  const dash = value.indexOf("-");
  if (dash === -1) return VERSION_CORE_RE.test(value);
  return VERSION_CORE_RE.test(value.slice(0, dash)) && PRE_RELEASE_RE.test(value.slice(dash + 1));
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isDatabaseExport(value: unknown): value is DatabaseExport {
  return isPlainObject(value) && Array.isArray(value.folders) && Array.isArray(value.sessions);
}

/**
 * Read an export file and work out which sections it holds. Only the shape
 * of each section is checked here; its contents are validated when that
 * section is imported.
 *
 * Files from before the export envelope existed are a bare database export
 * (`{ folders, sessions, … }`) and are read as one with only that section.
 */
export function parseExportFile(text: string): ParsedExportFile {
  let data: unknown;
  try {
    data = JSON.parse(text);
  } catch {
    throw new ExportFileError("invalidJson");
  }
  if (!isPlainObject(data)) throw new ExportFileError("unknownFormat");

  // Only the envelope has a "format" key; a bare database export never did.
  if (!("format" in data)) {
    if (!isDatabaseExport(data)) throw new ExportFileError("unknownFormat");
    return { legacy: true, appVersion: null, exportedAt: null, database: data };
  }

  if (data.format !== EXPORT_FORMAT) throw new ExportFileError("unknownFormat");
  const version = data.version;
  if (typeof version !== "number" || !Number.isInteger(version) || version < 1) {
    throw new ExportFileError("unknownFormat");
  }
  if (version > EXPORT_VERSION) throw new ExportFileError("newerVersion");

  const parsed: ParsedExportFile = {
    legacy: false,
    appVersion:
      typeof data.app_version === "string" && isAppVersion(data.app_version)
        ? data.app_version
        : null,
    exportedAt:
      typeof data.exported_at === "string" &&
      data.exported_at.length <= MAX_EXPORTED_AT_LENGTH &&
      !Number.isNaN(Date.parse(data.exported_at))
        ? data.exported_at
        : null,
  };
  if ("database" in data) {
    if (!isDatabaseExport(data.database)) throw new ExportFileError("invalidSection");
    parsed.database = data.database;
  }
  if ("settings" in data) {
    if (!isPlainObject(data.settings) || Object.keys(data.settings).length > MAX_SETTINGS_ENTRIES) {
      throw new ExportFileError("invalidSection");
    }
    parsed.settings = data.settings;
  }
  if ("command_buttons" in data) {
    if (!Array.isArray(data.command_buttons) || data.command_buttons.length > MAX_COMMAND_BUTTONS) {
      throw new ExportFileError("invalidSection");
    }
    parsed.commandButtons = data.command_buttons;
  }
  if (DATA_SECTIONS.every((section) => !hasSection(parsed, section))) {
    throw new ExportFileError("empty");
  }
  return parsed;
}

export function hasSection(file: ParsedExportFile, section: DataSection): boolean {
  return file[section] !== undefined;
}

export interface ExportResult {
  /** Database counts; both zero when the database was not exported. */
  folders: number;
  sessions: number;
  /** Null when command buttons were not exported. */
  commandButtons: number | null;
}

/**
 * Ask where to save, then write the selected sections there. The backend
 * shows the save dialog itself (titled `dialogTitle`), so the file written
 * is always one the user picked. Resolves to null when they cancel.
 */
export async function exportData(
  selection: SectionSelection,
  dialogTitle: string,
): Promise<ExportResult | null> {
  const commandButtons = selection.commandButtons ? getExportableCommandButtons() : null;
  const counts = await invoke<{ folders: number; sessions: number } | null>("data_export_file", {
    title: dialogTitle,
    includeDatabase: selection.database,
    settings: selection.settings ? getExportableSettings() : null,
    commandButtons,
  });
  if (counts === null) return null;
  return { ...counts, commandButtons: commandButtons?.length ?? null };
}

/** An error as text: an Error's message, or what was rejected with (Tauri rejects with strings). */
export function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

export type SectionImportResult =
  | { section: "database"; ok: true; summary: string }
  | { section: "settings"; ok: true }
  | { section: "commandButtons"; ok: true; added: number; skipped: number }
  | { section: DataSection; ok: false; error: string };

/**
 * Import the selected sections of a parsed file. Each section is imported
 * on its own, so a failure in one is reported without stopping the others.
 * Selected sections the file does not hold are left out of the result.
 */
export async function importData(
  file: ParsedExportFile,
  selection: SectionSelection,
): Promise<SectionImportResult[]> {
  const results: SectionImportResult[] = [];

  if (selection.database && file.database) {
    try {
      const summary = await invoke<string>("db_import", { data: file.database });
      results.push({ section: "database", ok: true, summary });
    } catch (e) {
      results.push({ section: "database", ok: false, error: errorText(e) });
    }
    // Reload even after a failure: db_import is not transactional, so a
    // failed import may still have added rows before it stopped.
    const reloads = await Promise.allSettled([
      useSessionStore.getState().loadAll(),
      useHighlightStore.getState().loadProfiles(),
      useLoginSequenceStore.getState().loadAll(),
    ]);
    for (const reload of reloads) {
      if (reload.status === "rejected") {
        // eslint-disable-next-line no-console -- the import itself succeeded; detail for devtools
        console.warn("reload after import failed:", reload.reason);
      }
    }
  }

  if (selection.settings && file.settings) {
    try {
      await importSettings(file.settings);
      results.push({ section: "settings", ok: true });
    } catch (e) {
      results.push({ section: "settings", ok: false, error: errorText(e) });
    }
  }

  if (selection.commandButtons && file.commandButtons) {
    try {
      const { added, skipped } = importCommandButtons(file.commandButtons);
      results.push({ section: "commandButtons", ok: true, added, skipped });
    } catch (e) {
      results.push({ section: "commandButtons", ok: false, error: errorText(e) });
    }
  }

  return results;
}
