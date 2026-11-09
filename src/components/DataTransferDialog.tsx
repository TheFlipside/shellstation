import React, { useRef, useState } from "react";
import ReactDOM from "react-dom";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { DialogKeys } from "./DialogKeys";
import {
  DATA_SECTIONS,
  ExportFileError,
  errorText,
  hasSection,
  type DataSection,
  type ExportFileErrorCode,
  type ExportResult,
  type ParsedExportFile,
  type SectionImportResult,
  type SectionSelection,
} from "../dataTransfer";
import { isImportableButton, useSettingsStore, type CommandButton } from "../stores/settingsStore";

export interface SectionOption {
  detail: string;
  /**
   * A further sentence, shown as its own line after the detail rather than
   * joined to it, so each language can word and punctuate both freely.
   */
  note?: string;
  available: boolean;
  /** Shown under the detail line, e.g. the buttons a file would add. */
  preview?: React.ReactNode;
}

interface SectionPickerDialogProps<S extends string> {
  idPrefix: string;
  title: string;
  info: string | null;
  /** The sections, in the order they are shown. */
  sections: readonly S[];
  labels: Record<S, string>;
  options: Record<S, SectionOption>;
  confirmLabel: string;
  /** Whether Enter outside a button confirms; otherwise only a click does. */
  confirmOnEnter: boolean;
  onConfirm: (selection: Record<S, boolean>) => void;
  onCancel: () => void;
}

const SECTION_LABEL_KEYS: Record<DataSection, string> = {
  database: "settings.dataSectionDatabase",
  settings: "settings.dataSectionSettings",
  commandButtons: "settings.dataSectionCommandButtons",
};

function sectionLabel(t: TFunction, section: DataSection): string {
  return t(SECTION_LABEL_KEYS[section]);
}

function sectionLabels(t: TFunction): Record<DataSection, string> {
  return {
    database: sectionLabel(t, "database"),
    settings: sectionLabel(t, "settings"),
    commandButtons: sectionLabel(t, "commandButtons"),
  };
}

/** One line of the outcome shown under the export / import buttons. */
export interface DataResultLine {
  ok: boolean;
  message: string;
}

export function exportResultLines(
  t: TFunction,
  selection: SectionSelection,
  result: ExportResult,
): DataResultLine[] {
  const lines: DataResultLine[] = [];
  if (selection.database) {
    lines.push({
      ok: true,
      message: t("settings.dataExportedDatabase", {
        folders: result.folders,
        sessions: result.sessions,
      }),
    });
  }
  if (selection.settings) {
    lines.push({ ok: true, message: t("settings.dataExportedSettings") });
  }
  if (result.commandButtons !== null) {
    lines.push({
      ok: true,
      message: t("settings.dataExportedCommandButtons", { count: result.commandButtons }),
    });
  }
  return lines;
}

export function importResultLine(t: TFunction, result: SectionImportResult): DataResultLine {
  if (!result.ok) {
    return {
      ok: false,
      message: t("settings.dataSectionFailed", {
        section: sectionLabel(t, result.section),
        error: result.error,
      }),
    };
  }
  switch (result.section) {
    case "database":
      // db_import composes its own summary; shown as it was before.
      return { ok: true, message: result.summary };
    case "settings":
      return { ok: true, message: t("settings.dataImportedSettings") };
    case "commandButtons":
      return {
        ok: true,
        message: t("settings.dataImportedCommandButtons", {
          added: result.added,
          skipped: result.skipped,
        }),
      };
  }
}

const FILE_ERROR_KEYS: Record<ExportFileErrorCode, string> = {
  invalidJson: "settings.dataImportInvalidJson",
  unknownFormat: "settings.dataImportUnknownFormat",
  newerVersion: "settings.dataImportNewerVersion",
  invalidSection: "settings.dataImportInvalidSection",
  empty: "settings.dataImportEmpty",
};

/** The message for a file that could not be opened for import. */
export function importFileErrorMessage(t: TFunction, error: unknown): string {
  if (error instanceof ExportFileError) return t(FILE_ERROR_KEYS[error.code]);
  return t("settings.dataImportFailed", { error: errorText(error) });
}

/**
 * One checkbox per section. Every available section starts checked, since
 * taking everything is the common case; unavailable ones stay unchecked and
 * disabled, with the reason in their detail line.
 */
export function SectionPickerDialog<S extends string>({
  idPrefix,
  title,
  info,
  sections,
  labels,
  options,
  confirmLabel,
  confirmOnEnter,
  onConfirm,
  onCancel,
}: SectionPickerDialogProps<S>): React.JSX.Element {
  const { t } = useTranslation();
  // Complete only because every caller lists all of its section keys in
  // `sections`; a key left out would read as unselected but be missing
  // from what `onConfirm` receives.
  const [selection, setSelection] = useState(
    () =>
      Object.fromEntries(
        sections.map((section) => [section, options[section].available]),
      ) as Record<S, boolean>,
  );
  const anySelected = sections.some((section) => selection[section]);
  // The parent closes the dialog asynchronously; a repeated Enter before
  // that must not start a second export or import.
  const confirmed = useRef(false);
  const confirm = (): void => {
    if (!anySelected || confirmed.current) return;
    confirmed.current = true;
    onConfirm(selection);
  };
  // DialogKeys takes Enter before a focused button sees it. Pressing Enter
  // on Cancel must cancel, not import, so a focused button gets the click.
  const handleEnter = (): void => {
    const focused = document.activeElement;
    if (focused instanceof HTMLButtonElement) {
      focused.click();
    } else if (confirmOnEnter) {
      confirm();
    }
  };
  const firstAvailable = sections.find((section) => options[section].available);

  return ReactDOM.createPortal(
    <div className="dialog-overlay" role="presentation">
      <DialogKeys onEscape={onCancel} onEnter={handleEnter} />
      <div
        className="dialog dialog-wide"
        role="dialog"
        aria-modal="true"
        aria-labelledby={`${idPrefix}-title`}
      >
        <h3 className="dialog-title" id={`${idPrefix}-title`}>
          {title}
        </h3>
        {info !== null && <p className="dialog-text">{info}</p>}
        {sections.map((section) => {
          const option = options[section];
          const inputId = `${idPrefix}-${section}`;
          return (
            <div
              key={section}
              className={
                option.available
                  ? "data-section-option"
                  : "data-section-option data-section-option-unavailable"
              }
            >
              <div className="dialog-field dialog-field-row">
                <input
                  type="checkbox"
                  id={inputId}
                  checked={selection[section]}
                  disabled={!option.available}
                  autoFocus={section === firstAvailable}
                  aria-describedby={
                    option.note === undefined
                      ? `${inputId}-detail`
                      : `${inputId}-detail ${inputId}-note`
                  }
                  onChange={(e) => {
                    const checked = e.target.checked;
                    setSelection((prev) => ({ ...prev, [section]: checked }));
                  }}
                />
                <label htmlFor={inputId}>{labels[section]}</label>
              </div>
              <p className="data-section-detail" id={`${inputId}-detail`}>
                {option.detail}
              </p>
              {option.note !== undefined && (
                <p className="data-section-detail" id={`${inputId}-note`}>
                  {option.note}
                </p>
              )}
              {option.available && option.preview}
            </div>
          );
        })}
        <div className="dialog-actions">
          <button type="button" className="dialog-btn dialog-btn-cancel" onClick={onCancel}>
            {t("dialog.cancel")}
          </button>
          <button
            type="button"
            className="dialog-btn dialog-btn-primary"
            disabled={!anySelected}
            onClick={confirm}
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}

interface DataExportDialogProps {
  onConfirm: (selection: SectionSelection) => void;
  onCancel: () => void;
}

export function DataExportDialog({
  onConfirm,
  onCancel,
}: DataExportDialogProps): React.JSX.Element {
  const { t } = useTranslation();
  const buttonCount = useSettingsStore((s) => s.commandButtons.length);
  return (
    <SectionPickerDialog
      idPrefix="data-export"
      title={t("settings.dbExport")}
      info={t("settings.dataExportInfo")}
      sections={DATA_SECTIONS}
      labels={sectionLabels(t)}
      options={{
        database: { detail: t("settings.dataExportDatabaseDetail"), available: true },
        settings: { detail: t("settings.dataExportSettingsDetail"), available: true },
        commandButtons: {
          detail: t("settings.dataExportCommandButtonsDetail", { count: buttonCount }),
          available: true,
        },
      }}
      confirmLabel={t("settings.dataExportAction")}
      confirmOnEnter
      onConfirm={onConfirm}
      onCancel={onCancel}
    />
  );
}

interface DataImportDialogProps {
  file: ParsedExportFile;
  onConfirm: (selection: SectionSelection) => void;
  onCancel: () => void;
}

/** Where the file came from, or null when it does not say. */
function fileInfo(t: TFunction, language: string, file: ParsedExportFile): string | null {
  if (file.legacy) return t("settings.dataImportLegacyFile");
  if (file.appVersion === null || file.exportedAt === null) return null;
  const date = new Date(file.exportedAt);
  if (Number.isNaN(date.getTime())) return null;
  let formatted: string;
  try {
    formatted = date.toLocaleString(language);
  } catch {
    formatted = date.toISOString();
  }
  return t("settings.dataImportFileInfo", { version: file.appVersion, date: formatted });
}

/**
 * Every button the file would add, name and command as written. A button
 * runs its command in a live session on one click, so what a file adds is
 * shown before anything is imported, not only how many there are. The list
 * scrolls, so it takes focus to let the keyboard scroll it too.
 */
export function ButtonPreview({
  buttons,
  label,
}: {
  buttons: readonly CommandButton[];
  label: string;
}): React.JSX.Element | null {
  if (buttons.length === 0) return null;
  return (
    <ul className="data-button-preview" aria-label={label} tabIndex={0}>
      {buttons.map((button, i) => (
        <li key={i}>
          <span
            className="data-button-preview-swatch"
            style={{ backgroundColor: button.color }}
            aria-hidden="true"
          />
          {/* The name may be cut short with an ellipsis; the title shows it whole. */}
          <span className="data-button-preview-name" title={button.name}>
            {button.name}
          </span>
          <code className="data-button-preview-command">{button.command}</code>
        </li>
      ))}
    </ul>
  );
}

export function DataImportDialog({
  file,
  onConfirm,
  onCancel,
}: DataImportDialogProps): React.JSX.Element {
  const { t, i18n } = useTranslation();
  const notInFile = t("settings.dataImportNotInFile");
  const option = (section: DataSection, detail: () => string): SectionOption =>
    hasSection(file, section)
      ? { detail: detail(), available: true }
      : { detail: notInFile, available: false };
  // Counted and listed alike, so the number matches the list the user checks.
  const importableButtons = (file.commandButtons ?? []).filter(isImportableButton);

  return (
    <SectionPickerDialog
      idPrefix="data-import"
      title={t("settings.dbImport")}
      info={fileInfo(t, i18n.language, file)}
      sections={DATA_SECTIONS}
      labels={sectionLabels(t)}
      options={{
        database: option("database", () =>
          t("settings.dataImportDatabaseDetail", {
            folders: file.database?.folders.length ?? 0,
            sessions: file.database?.sessions.length ?? 0,
          }),
        ),
        settings: option("settings", () => t("settings.dataImportSettingsDetail")),
        commandButtons: {
          ...option("commandButtons", () =>
            t("settings.dataImportCommandButtonsDetail", { count: importableButtons.length }),
          ),
          preview: (
            <ButtonPreview
              buttons={importableButtons}
              label={t("settings.dataSectionCommandButtons")}
            />
          ),
        },
      }}
      confirmLabel={t("settings.dataImportAction")}
      // Importing takes a click on the Import button, never a stray Enter:
      // the button list is there to be read first.
      confirmOnEnter={false}
      onConfirm={onConfirm}
      onCancel={onCancel}
    />
  );
}
