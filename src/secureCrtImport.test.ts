/**
 * SecureCRT import: turning the buttons the backend read into command bar
 * buttons, and adding the ones the user picked.
 *
 * The buttons come from someone else's file and one click sends their
 * command to a live session, so these tests pin that they pass the same
 * check as buttons from a ShellStation export, and that importing them adds
 * to the command bar without changing or duplicating what is there.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import {
  hasSecureCrtContent,
  importSecureCrtButtons,
  readSecureCrtFile,
  toCommandButtons,
  type SecureCrtFile,
} from "./secureCrtImport";
import { DEFAULT_BUTTON_COLOR, useSettingsStore } from "./stores/settingsStore";

vi.mock("./i18n", () => ({
  default: { t: (key: string) => key, changeLanguage: vi.fn() },
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const UUID_RE = /^[\da-f]{8}-[\da-f]{4}-[\da-f]{4}-[\da-f]{4}-[\da-f]{12}$/i;

function list(buttons: { name: string; command: string }[], skipped = 0, warnings: string[] = []) {
  return { buttons, skipped, warnings };
}

function file(parts: Partial<SecureCrtFile>): SecureCrtFile {
  return { xml: "<VanDyke/>", sessions: null, buttonBars: null, commandManager: null, ...parts };
}

beforeEach(() => {
  useSettingsStore.setState(useSettingsStore.getInitialState(), true);
  vi.mocked(invoke).mockReset();
});

describe("toCommandButtons", () => {
  it("gives each button a new id and the default color", () => {
    const { buttons, notImportable } = toCommandButtons(
      list([
        { name: "sh ver", command: "show version \\r" },
        { name: "sh ver", command: "show version \\r" },
      ]),
    );
    expect(buttons.map(({ name, command, color }) => ({ name, command, color }))).toEqual([
      { name: "sh ver", command: "show version \\r", color: DEFAULT_BUTTON_COLOR },
      { name: "sh ver", command: "show version \\r", color: DEFAULT_BUTTON_COLOR },
    ]);
    expect(buttons[0].id).toMatch(UUID_RE);
    expect(buttons[0].id).not.toBe(buttons[1].id);
    expect(notImportable).toBe(0);
  });

  it("leaves out buttons a ShellStation export file could not hold", () => {
    const { buttons, notImportable, warnings } = toCommandButtons(
      list(
        [
          { name: "ok", command: "uptime\\r" },
          // A text-direction override makes the name read as something else.
          { name: `rm${String.fromCodePoint(0x202e)}`, command: "ls\\r" },
          // A zero-width space hides part of the command.
          { name: "ls", command: `ls${String.fromCodePoint(0x200b)}; rm -rf ~\\r` },
          { name: "", command: "x" },
        ],
        2,
        ['Skipped "menu" in button bar "Default": …'],
      ),
    );
    expect(buttons.map((b) => b.name)).toEqual(["ok"]);
    // The two the backend skipped and the three left out here.
    expect(notImportable).toBe(5);
    expect(warnings).toEqual(['Skipped "menu" in button bar "Default": …']);
  });
});

describe("readSecureCrtFile", () => {
  it("reads the file in the backend and keeps it for the session import", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      sessions: { folders: 1, sessions: 3 },
      button_bars: list([{ name: "sh ver", command: "show version \\r" }]),
      command_manager: null,
    });
    const read = await readSecureCrtFile("<xml/>");
    expect(invoke).toHaveBeenCalledWith("inspect_securecrt", { xml: "<xml/>" });
    expect(read.xml).toBe("<xml/>");
    expect(read.sessions).toEqual({ folders: 1, sessions: 3 });
    expect(read.buttonBars?.buttons.map((b) => b.name)).toEqual(["sh ver"]);
    expect(read.commandManager).toBeNull();
    expect(hasSecureCrtContent(read)).toBe(true);
  });

  it("says when a file holds nothing for the dialog to show", () => {
    expect(hasSecureCrtContent(file({}))).toBe(false);
    // An unused Command Manager has nothing to offer or to explain.
    expect(hasSecureCrtContent(file({ commandManager: toCommandButtons(list([])) }))).toBe(false);
    // Buttons that cannot be imported are still worth saying why.
    expect(hasSecureCrtContent(file({ commandManager: toCommandButtons(list([], 2)) }))).toBe(true);
    // Sessions that cannot be imported, likewise.
    expect(hasSecureCrtContent(file({ sessions: { folders: 0, sessions: 0 } }))).toBe(true);
  });
});

describe("importSecureCrtButtons", () => {
  const bars = toCommandButtons(
    list(
      [
        { name: "sh ver", command: "show version \\r" },
        { name: "sh run", command: "show running-config \\r" },
      ],
      1,
      ["bar warning"],
    ),
  );
  const commands = toCommandButtons(
    list(
      [
        // Also on a button bar: VanDyke's own way to the Command Manager
        // copies the bar's buttons there.
        { name: "sh run", command: "show running-config \\r" },
        { name: "sh vers", command: "show version \\r" },
      ],
      0,
      ["command warning"],
    ),
  );
  const both = file({ buttonBars: bars, commandManager: commands });

  it("adds the selected blocks after the existing buttons, each button once", () => {
    const existing = {
      id: "9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d",
      name: "Uptime",
      command: "uptime\\r",
      color: "#89b4fa",
    };
    useSettingsStore.setState({ commandButtons: [existing] });
    const result = importSecureCrtButtons(both, {
      sessions: false,
      buttonBars: true,
      commandManager: true,
    });
    expect(useSettingsStore.getState().commandButtons.map((b) => b.name)).toEqual([
      "Uptime",
      "sh ver",
      "sh run",
      "sh vers",
    ]);
    expect(useSettingsStore.getState().commandButtons[0]).toEqual(existing);
    // The duplicate "sh run" and the entry the backend could not read.
    expect(result).toEqual({ added: 3, skipped: 2, warnings: ["bar warning", "command warning"] });
  });

  it("skips a button already in the command bar, leaving that one as it is", () => {
    const existing = {
      id: "9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d",
      name: "sh ver",
      command: "show version \\r",
      color: "#89b4fa",
    };
    useSettingsStore.setState({ commandButtons: [existing] });
    const result = importSecureCrtButtons(file({ buttonBars: bars }), {
      sessions: false,
      buttonBars: true,
      commandManager: false,
    });
    expect(useSettingsStore.getState().commandButtons).toEqual([
      existing,
      expect.objectContaining({ name: "sh run" }),
    ]);
    expect(result).toEqual({ added: 1, skipped: 2, warnings: ["bar warning"] });
  });

  it("takes only the blocks the user picked", () => {
    const result = importSecureCrtButtons(both, {
      sessions: true,
      buttonBars: false,
      commandManager: true,
    });
    expect(useSettingsStore.getState().commandButtons.map((b) => b.name)).toEqual([
      "sh run",
      "sh vers",
    ]);
    expect(result).toEqual({ added: 2, skipped: 0, warnings: ["command warning"] });
  });

  it("does nothing when no block of buttons was picked", () => {
    expect(
      importSecureCrtButtons(both, { sessions: true, buttonBars: false, commandManager: false }),
    ).toBeNull();
    // A picked block the file lacks is no block either.
    expect(
      importSecureCrtButtons(file({ buttonBars: bars }), {
        sessions: false,
        buttonBars: false,
        commandManager: true,
      }),
    ).toBeNull();
    expect(useSettingsStore.getState().commandButtons).toEqual([]);
  });
});
