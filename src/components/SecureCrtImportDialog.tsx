import React from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { ButtonPreview, SectionPickerDialog, type SectionOption } from "./DataTransferDialog";
import {
  SECURECRT_SECTIONS,
  type SecureCrtButtons,
  type SecureCrtFile,
  type SecureCrtSection,
  type SecureCrtSelection,
} from "../secureCrtImport";

interface SecureCrtImportDialogProps {
  file: SecureCrtFile;
  onConfirm: (selection: SecureCrtSelection) => void;
  onCancel: () => void;
}

function sectionLabels(t: TFunction): Record<SecureCrtSection, string> {
  return {
    sessions: t("settings.secureCrtSectionSessions"),
    buttonBars: t("settings.secureCrtSectionButtonBars"),
    commandManager: t("settings.secureCrtSectionCommandManager"),
  };
}

/**
 * Choose what to take from a SecureCRT export: its sessions, the buttons of
 * its button bars, and those of its Command Manager. Like the import of a
 * ShellStation export, a part the file lacks is shown but cannot be
 * checked, and the buttons are listed with their commands before anything
 * is imported.
 */
export function SecureCrtImportDialog({
  file,
  onConfirm,
  onCancel,
}: SecureCrtImportDialogProps): React.JSX.Element {
  const { t } = useTranslation();
  const labels = sectionLabels(t);
  const notInFile = t("settings.dataImportNotInFile");

  const sessions: SectionOption =
    file.sessions === null
      ? { detail: notInFile, available: false }
      : file.sessions.sessions === 0
        ? { detail: t("settings.secureCrtSessionsNone"), available: false }
        : {
            detail: t("settings.secureCrtSessionsDetail", {
              folders: file.sessions.folders,
              sessions: file.sessions.sessions,
            }),
            available: true,
          };

  const buttons = (section: SecureCrtSection, list: SecureCrtButtons | null): SectionOption => {
    // A block with no entries at all, such as an unused Command Manager,
    // holds as little as one the file does not have.
    if (list === null || (list.buttons.length === 0 && list.notImportable === 0)) {
      return { detail: notInFile, available: false };
    }
    const note =
      list.notImportable > 0
        ? t("settings.secureCrtButtonsLeftOut", { count: list.notImportable })
        : undefined;
    if (list.buttons.length === 0) {
      return { detail: t("settings.secureCrtButtonsNone"), note, available: false };
    }
    return {
      detail: t("settings.dataImportCommandButtonsDetail", { count: list.buttons.length }),
      note,
      available: true,
      preview: <ButtonPreview buttons={list.buttons} label={labels[section]} />,
    };
  };

  return (
    <SectionPickerDialog
      idPrefix="securecrt-import"
      title={t("settings.secureCrtImportTitle")}
      info={t("settings.secureCrtImportInfo")}
      sections={SECURECRT_SECTIONS}
      labels={labels}
      options={{
        sessions,
        buttonBars: buttons("buttonBars", file.buttonBars),
        commandManager: buttons("commandManager", file.commandManager),
      }}
      confirmLabel={t("settings.dataImportAction")}
      // As with a ShellStation export: the button lists are there to be read
      // first, so only a click on Import imports.
      confirmOnEnter={false}
      onConfirm={onConfirm}
      onCancel={onCancel}
    />
  );
}
