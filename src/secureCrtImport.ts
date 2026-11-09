import { invoke } from "@tauri-apps/api/core";
import {
  DEFAULT_BUTTON_COLOR,
  importCommandButtons,
  isImportableButton,
  type CommandButton,
} from "./stores/settingsStore";

/**
 * Importing from a SecureCRT export. The backend reads the file
 * (`inspect_securecrt`, see src-tauri/src/import/mod.rs) and the user picks
 * what to take from it: sessions go through `import_securecrt`, while
 * buttons are added here, because the command bar lives in this webview's
 * localStorage.
 */

/** A SecureCRT button that sends a string, as the backend returns it. */
interface RawButton {
  name: string;
  command: string;
}

/** The buttons of one block of the file, as the backend returns them. */
interface RawButtonList {
  buttons: RawButton[];
  /** Entries the backend could not turn into a button. */
  skipped: number;
  /** Why, already sanitized and capped by the backend. */
  warnings: string[];
}

/** What `inspect_securecrt` returns. A part is null when the file lacks it. */
interface SecureCrtContents {
  sessions: { folders: number; sessions: number } | null;
  button_bars: RawButtonList | null;
  command_manager: RawButtonList | null;
}

export type SecureCrtSection = "sessions" | "buttonBars" | "commandManager";
export const SECURECRT_SECTIONS: readonly SecureCrtSection[] = [
  "sessions",
  "buttonBars",
  "commandManager",
];
export type SecureCrtSelection = Record<SecureCrtSection, boolean>;

/** The buttons of one block, ready for the command bar. */
export interface SecureCrtButtons {
  buttons: CommandButton[];
  /** Entries in the file that cannot become a button. */
  notImportable: number;
  warnings: string[];
}

/** A SecureCRT export, read and ready to import from. */
export interface SecureCrtFile {
  /** Kept to hand to `import_securecrt` if the user picks the sessions. */
  xml: string;
  sessions: { folders: number; sessions: number } | null;
  /** From the button bars. */
  buttonBars: SecureCrtButtons | null;
  /** From the Command Manager. */
  commandManager: SecureCrtButtons | null;
}

/**
 * Turn the backend's buttons into command bar buttons. Each gets a new id
 * and the default color, as a button made in the app does, and goes through
 * the same check as a button from a ShellStation export file: one whose
 * name or command holds hidden characters is left out.
 */
export function toCommandButtons(list: RawButtonList): SecureCrtButtons {
  const buttons: CommandButton[] = [];
  for (const raw of list.buttons) {
    const button = {
      id: crypto.randomUUID(),
      name: raw.name,
      command: raw.command,
      color: DEFAULT_BUTTON_COLOR,
    };
    // `importCommandButtons` checks again when they are added. This check is
    // what keeps a button out of the list the user reviews; both stay.
    if (isImportableButton(button)) buttons.push(button);
  }
  return {
    buttons,
    notImportable: list.skipped + list.buttons.length - buttons.length,
    warnings: list.warnings,
  };
}

/** Read a SecureCRT export, without importing anything yet. */
export async function readSecureCrtFile(xml: string): Promise<SecureCrtFile> {
  const contents = await invoke<SecureCrtContents>("inspect_securecrt", { xml });
  return {
    xml,
    sessions: contents.sessions,
    buttonBars: contents.button_bars && toCommandButtons(contents.button_bars),
    commandManager: contents.command_manager && toCommandButtons(contents.command_manager),
  };
}

/**
 * Whether the file has anything for the import dialog to show: a sessions
 * block, or a block of buttons with at least one entry, importable or not.
 * A sessions block counts even with no importable sessions in it, so the
 * dialog can say so.
 */
export function hasSecureCrtContent(file: SecureCrtFile): boolean {
  const hasEntries = (list: SecureCrtButtons | null): boolean =>
    list !== null && (list.buttons.length > 0 || list.notImportable > 0);
  return file.sessions !== null || hasEntries(file.buttonBars) || hasEntries(file.commandManager);
}

export interface SecureCrtButtonImportResult {
  added: number;
  /** Buttons already there, past the command bar's limit, or not importable. */
  skipped: number;
  /** Why buttons were left out of the selected blocks. */
  warnings: string[];
}

/**
 * Add the buttons of the selected blocks to the command bar, button bars
 * first. A button already in the bar, by id or by name and command, is
 * skipped, so one that is in both blocks is added once. Null when no block
 * of buttons was selected.
 */
export function importSecureCrtButtons(
  file: SecureCrtFile,
  selection: SecureCrtSelection,
): SecureCrtButtonImportResult | null {
  const lists = [
    selection.buttonBars ? file.buttonBars : null,
    selection.commandManager ? file.commandManager : null,
  ].filter((list): list is SecureCrtButtons => list !== null);
  if (lists.length === 0) return null;
  const { added, skipped } = importCommandButtons(lists.flatMap((list) => list.buttons));
  return {
    added,
    skipped: skipped + lists.reduce((sum, list) => sum + list.notImportable, 0),
    warnings: lists.flatMap((list) => list.warnings),
  };
}
