/**
 * Export / import section picker.
 *
 * The import dialog must only offer what the file actually holds: a section
 * missing from the file is shown, but unchecked and disabled, so it can
 * never be sent to the import. Buttons a file would add are listed with
 * their commands before anything is imported, since one click runs them.
 */
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import type { TFunction } from "i18next";
import {
  DataExportDialog,
  DataImportDialog,
  exportResultLines,
  importResultLine,
} from "./DataTransferDialog";
import type { ParsedExportFile } from "../dataTransfer";

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

const BUTTON = {
  id: "9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d",
  name: "Uptime",
  command: "uptime\\r",
  color: "#89b4fa",
};

const BUTTONS_ONLY: ParsedExportFile = {
  legacy: false,
  appVersion: "0.30.0",
  exportedAt: "2026-10-27T12:00:00+00:00",
  commandButtons: [],
};

/** Echoes the key and its options, so tests can see what was interpolated. */
const t = ((key: string, options?: Record<string, unknown>) =>
  options ? `${key} ${JSON.stringify(options)}` : key) as unknown as TFunction;

function checkbox(label: string): HTMLInputElement {
  return screen.getByLabelText(label);
}

function pressKey(key: string): void {
  fireEvent.keyDown(document, { key });
}

describe("DataImportDialog", () => {
  it("disables and unchecks the sections the file does not hold", () => {
    render(<DataImportDialog file={BUTTONS_ONLY} onConfirm={vi.fn()} onCancel={vi.fn()} />);
    expect(checkbox("settings.dataSectionDatabase")).toBeDisabled();
    expect(checkbox("settings.dataSectionDatabase")).not.toBeChecked();
    expect(checkbox("settings.dataSectionSettings")).toBeDisabled();
    expect(checkbox("settings.dataSectionSettings")).not.toBeChecked();
    expect(checkbox("settings.dataSectionCommandButtons")).toBeEnabled();
    expect(checkbox("settings.dataSectionCommandButtons")).toBeChecked();
  });

  it("imports the checked sections", () => {
    const onConfirm = vi.fn();
    render(<DataImportDialog file={BUTTONS_ONLY} onConfirm={onConfirm} onCancel={vi.fn()} />);
    fireEvent.click(screen.getByText("settings.dataImportAction"));
    expect(onConfirm).toHaveBeenCalledWith({
      database: false,
      settings: false,
      commandButtons: true,
    });
  });

  it("cannot be confirmed with nothing checked", () => {
    const onConfirm = vi.fn();
    render(<DataImportDialog file={BUTTONS_ONLY} onConfirm={onConfirm} onCancel={vi.fn()} />);
    fireEvent.click(checkbox("settings.dataSectionCommandButtons"));
    const importButton = screen.getByText("settings.dataImportAction");
    expect(importButton).toBeDisabled();
    pressKey("Enter");
    expect(onConfirm).not.toHaveBeenCalled();
  });

  it("lists the buttons the file would add, with their commands", () => {
    const hidden = { ...BUTTON, id: "1b9d6bcd-bbfd-4b2d-9b5d-ab8dfbbd4bed", name: "a\u202eb" };
    const file = { ...BUTTONS_ONLY, commandButtons: [BUTTON, hidden, "junk"] };
    render(<DataImportDialog file={file} onConfirm={vi.fn()} onCancel={vi.fn()} />);
    const list = screen.getByRole("list", { name: "settings.dataSectionCommandButtons" });
    const items = within(list).getAllByRole("listitem");
    expect(items).toHaveLength(1);
    expect(items[0]).toHaveTextContent("Uptime");
    expect(items[0]).toHaveTextContent("uptime\\r");
    // The count matches the list, not the entries the file happens to hold.
    expect(screen.getByText(/settings\.dataImportCommandButtonsDetail/)).toHaveTextContent(
      '{"count":1}',
    );
    // It scrolls, so the keyboard must be able to reach it.
    expect(list).toHaveAttribute("tabindex", "0");
  });

  it("does not import on Enter unless the Import button has focus", () => {
    const onConfirm = vi.fn();
    render(<DataImportDialog file={BUTTONS_ONLY} onConfirm={onConfirm} onCancel={vi.fn()} />);
    expect(checkbox("settings.dataSectionCommandButtons")).toHaveFocus();
    pressKey("Enter");
    expect(onConfirm).not.toHaveBeenCalled();

    screen.getByText("settings.dataImportAction").focus();
    pressKey("Enter");
    expect(onConfirm).toHaveBeenCalledOnce();
  });

  it("shows no button list when the file holds no importable buttons", () => {
    render(<DataImportDialog file={BUTTONS_ONLY} onConfirm={vi.fn()} onCancel={vi.fn()} />);
    expect(screen.queryByRole("list")).toBeNull();
  });

  it("says when a file comes from before the export format", () => {
    const legacy: ParsedExportFile = {
      legacy: true,
      appVersion: null,
      exportedAt: null,
      database: { folders: [], sessions: [] },
    };
    render(<DataImportDialog file={legacy} onConfirm={vi.fn()} onCancel={vi.fn()} />);
    expect(screen.getByText("settings.dataImportLegacyFile")).toBeInTheDocument();
  });

  it("says which version wrote the file", () => {
    render(<DataImportDialog file={BUTTONS_ONLY} onConfirm={vi.fn()} onCancel={vi.fn()} />);
    expect(screen.getByText(/settings\.dataImportFileInfo/)).toHaveTextContent(
      '"version":"0.30.0"',
    );
  });

  it("leaves out the origin line when the file does not say", () => {
    const file = { ...BUTTONS_ONLY, appVersion: null };
    render(<DataImportDialog file={file} onConfirm={vi.fn()} onCancel={vi.fn()} />);
    expect(screen.queryByText(/settings\.dataImportFileInfo/)).toBeNull();
    expect(screen.queryByText("settings.dataImportLegacyFile")).toBeNull();
  });
});

describe("SectionPickerDialog keys", () => {
  it("cancels on Escape", () => {
    const onCancel = vi.fn();
    render(<DataExportDialog onConfirm={vi.fn()} onCancel={onCancel} />);
    pressKey("Escape");
    expect(onCancel).toHaveBeenCalledOnce();
  });

  it("exports on Enter, once however often it is pressed", () => {
    const onConfirm = vi.fn();
    render(<DataExportDialog onConfirm={onConfirm} onCancel={vi.fn()} />);
    pressKey("Enter");
    pressKey("Enter");
    expect(onConfirm).toHaveBeenCalledOnce();
  });

  it("cancels on Enter while the Cancel button has focus", () => {
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    render(<DataExportDialog onConfirm={onConfirm} onCancel={onCancel} />);
    screen.getByText("dialog.cancel").focus();
    pressKey("Enter");
    expect(onCancel).toHaveBeenCalledOnce();
    expect(onConfirm).not.toHaveBeenCalled();
  });

  it("starts with focus on the first section", () => {
    render(<DataImportDialog file={BUTTONS_ONLY} onConfirm={vi.fn()} onCancel={vi.fn()} />);
    expect(checkbox("settings.dataSectionCommandButtons")).toHaveFocus();
  });
});

describe("DataExportDialog", () => {
  it("offers every section, all checked, and exports the chosen ones", () => {
    const onConfirm = vi.fn();
    render(<DataExportDialog onConfirm={onConfirm} onCancel={vi.fn()} />);
    fireEvent.click(checkbox("settings.dataSectionDatabase"));
    fireEvent.click(screen.getByText("settings.dataExportAction"));
    expect(onConfirm).toHaveBeenCalledWith({
      database: false,
      settings: true,
      commandButtons: true,
    });
  });
});

describe("result lines", () => {
  it("reports only the exported sections", () => {
    const lines = exportResultLines(
      t,
      { database: true, settings: false, commandButtons: false },
      { folders: 1, sessions: 2, commandButtons: null },
    );
    expect(lines).toEqual([
      { ok: true, message: 'settings.dataExportedDatabase {"folders":1,"sessions":2}' },
    ]);
  });

  it("reports a section that failed to import with its error", () => {
    const line = importResultLine(t, { section: "settings", ok: false, error: "boom" });
    expect(line.ok).toBe(false);
    expect(line.message).toBe(
      'settings.dataSectionFailed {"section":"settings.dataSectionSettings","error":"boom"}',
    );
  });

  it("shows the database summary as the backend wrote it", () => {
    const line = importResultLine(t, { section: "database", ok: true, summary: "Imported 3" });
    expect(line).toEqual({ ok: true, message: "Imported 3" });
  });

  it("counts added and skipped buttons", () => {
    const line = importResultLine(t, {
      section: "commandButtons",
      ok: true,
      added: 2,
      skipped: 1,
    });
    expect(line.message).toBe('settings.dataImportedCommandButtons {"added":2,"skipped":1}');
  });
});
