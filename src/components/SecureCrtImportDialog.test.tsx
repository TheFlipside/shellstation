/**
 * SecureCRT import picker.
 *
 * Like the import of a ShellStation export, it may only offer what the file
 * holds and what can be imported from it, and it lists every button with its
 * command before anything is imported, since one click runs a button.
 */
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { SecureCrtImportDialog } from "./SecureCrtImportDialog";
import type { SecureCrtButtons, SecureCrtFile } from "../secureCrtImport";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve()) }));
// Echoes the key and its options, so tests can see what was interpolated.
vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, options?: Record<string, unknown>) =>
      options ? `${key} ${JSON.stringify(options)}` : key,
    i18n: { language: "en" },
  }),
}));
vi.mock("../i18n", () => ({
  default: { t: (key: string) => key, changeLanguage: vi.fn() },
}));

function buttons(names: string[], notImportable = 0): SecureCrtButtons {
  return {
    buttons: names.map((name, i) => ({
      id: `9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6${String(i)}`,
      name,
      command: `${name}\\r`,
      color: "#f38ba8",
    })),
    notImportable,
    warnings: [],
  };
}

const FULL: SecureCrtFile = {
  xml: "<VanDyke/>",
  sessions: { folders: 1, sessions: 3 },
  buttonBars: buttons(["sh ver", "sh run"]),
  commandManager: buttons(["sh vers"], 2),
};

// By role: a block's button list carries the same name as its checkbox.
function checkbox(label: string): HTMLInputElement {
  return screen.getByRole("checkbox", { name: label });
}

describe("SecureCrtImportDialog", () => {
  it("offers every part the file has, all checked, and imports the chosen ones", () => {
    const onConfirm = vi.fn();
    render(<SecureCrtImportDialog file={FULL} onConfirm={onConfirm} onCancel={vi.fn()} />);
    for (const label of [
      "settings.secureCrtSectionSessions",
      "settings.secureCrtSectionButtonBars",
      "settings.secureCrtSectionCommandManager",
    ]) {
      expect(checkbox(label)).toBeEnabled();
      expect(checkbox(label)).toBeChecked();
    }
    expect(screen.getByText(/settings\.secureCrtSessionsDetail/)).toHaveTextContent(
      '{"folders":1,"sessions":3}',
    );
    fireEvent.click(checkbox("settings.secureCrtSectionCommandManager"));
    fireEvent.click(screen.getByText("settings.dataImportAction"));
    expect(onConfirm).toHaveBeenCalledWith({
      sessions: true,
      buttonBars: true,
      commandManager: false,
    });
  });

  it("lists the buttons of each block with their commands", () => {
    render(<SecureCrtImportDialog file={FULL} onConfirm={vi.fn()} onCancel={vi.fn()} />);
    const bar = screen.getByRole("list", { name: "settings.secureCrtSectionButtonBars" });
    const items = within(bar).getAllByRole("listitem");
    expect(items).toHaveLength(2);
    expect(items[0]).toHaveTextContent("sh ver");
    expect(items[0]).toHaveTextContent("sh ver\\r");
    const commands = screen.getByRole("list", { name: "settings.secureCrtSectionCommandManager" });
    expect(within(commands).getAllByRole("listitem")).toHaveLength(1);
  });

  it("says how many entries of a block cannot be imported", () => {
    render(<SecureCrtImportDialog file={FULL} onConfirm={vi.fn()} onCancel={vi.fn()} />);
    expect(document.getElementById("securecrt-import-commandManager-detail")).toHaveTextContent(
      'settings.dataImportCommandButtonsDetail {"count":1}',
    );
    // A sentence of its own, not glued to the detail, and read out with it.
    const note = document.getElementById("securecrt-import-commandManager-note");
    expect(note).toHaveTextContent('settings.secureCrtButtonsLeftOut {"count":2}');
    expect(checkbox("settings.secureCrtSectionCommandManager")).toHaveAttribute(
      "aria-describedby",
      "securecrt-import-commandManager-detail securecrt-import-commandManager-note",
    );
    expect(document.getElementById("securecrt-import-buttonBars-note")).toBeNull();
  });

  it("treats a block without entries like one the file does not have", () => {
    const empty = { buttons: [], notImportable: 0, warnings: [] };
    const file: SecureCrtFile = { ...FULL, commandManager: empty };
    render(<SecureCrtImportDialog file={file} onConfirm={vi.fn()} onCancel={vi.fn()} />);
    expect(checkbox("settings.secureCrtSectionCommandManager")).toBeDisabled();
    expect(document.getElementById("securecrt-import-commandManager-detail")).toHaveTextContent(
      "settings.dataImportNotInFile",
    );
  });

  it("disables what the file lacks or holds nothing importable from", () => {
    const file: SecureCrtFile = {
      xml: "<VanDyke/>",
      // Only SecureCRT's built-in templates, which are not imported.
      sessions: { folders: 0, sessions: 0 },
      buttonBars: buttons(["sh ver"]),
      // Only menu functions and scripts.
      commandManager: buttons([], 3),
    };
    const onConfirm = vi.fn();
    render(<SecureCrtImportDialog file={file} onConfirm={onConfirm} onCancel={vi.fn()} />);
    expect(checkbox("settings.secureCrtSectionSessions")).toBeDisabled();
    expect(checkbox("settings.secureCrtSectionSessions")).not.toBeChecked();
    expect(screen.getByText("settings.secureCrtSessionsNone")).toBeInTheDocument();
    expect(checkbox("settings.secureCrtSectionCommandManager")).toBeDisabled();
    expect(document.getElementById("securecrt-import-commandManager-detail")).toHaveTextContent(
      "settings.secureCrtButtonsNone",
    );
    expect(document.getElementById("securecrt-import-commandManager-note")).toHaveTextContent(
      'settings.secureCrtButtonsLeftOut {"count":3}',
    );
    // No list for a block nothing can be imported from.
    expect(screen.getAllByRole("list")).toHaveLength(1);
    fireEvent.click(screen.getByText("settings.dataImportAction"));
    expect(onConfirm).toHaveBeenCalledWith({
      sessions: false,
      buttonBars: true,
      commandManager: false,
    });
  });

  it("marks a block the file does not have as not included", () => {
    const file: SecureCrtFile = { ...FULL, buttonBars: null, commandManager: null };
    render(<SecureCrtImportDialog file={file} onConfirm={vi.fn()} onCancel={vi.fn()} />);
    expect(checkbox("settings.secureCrtSectionButtonBars")).toBeDisabled();
    expect(screen.getAllByText("settings.dataImportNotInFile")).toHaveLength(2);
  });

  it("does not import on Enter unless the Import button has focus", () => {
    const onConfirm = vi.fn();
    render(<SecureCrtImportDialog file={FULL} onConfirm={onConfirm} onCancel={vi.fn()} />);
    expect(checkbox("settings.secureCrtSectionSessions")).toHaveFocus();
    fireEvent.keyDown(document, { key: "Enter" });
    expect(onConfirm).not.toHaveBeenCalled();

    screen.getByText("settings.dataImportAction").focus();
    fireEvent.keyDown(document, { key: "Enter" });
    expect(onConfirm).toHaveBeenCalledOnce();
  });
});
