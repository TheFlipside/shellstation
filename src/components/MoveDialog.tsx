import React, { useRef } from "react";
import { useTranslation } from "react-i18next";
import { useEscapeKey } from "../hooks/useEscapeKey";
import type { Folder } from "../stores/sessionStore";

interface MoveDialogProps {
  folders: Folder[];
  excludeId: string;
  showRoot: boolean;
  onSubmit: (targetFolderId: string) => void;
  onCancel: () => void;
}

export function MoveDialog({
  folders,
  excludeId,
  showRoot,
  onSubmit,
  onCancel,
}: MoveDialogProps): React.JSX.Element {
  const { t } = useTranslation();
  useEscapeKey(onCancel);
  const selectRef = useRef<HTMLSelectElement>(null);

  return (
    <div className="dialog-overlay" onClick={onCancel} role="presentation">
      <div
        className="dialog"
        onClick={(e) => {
          e.stopPropagation();
        }}
        role="dialog"
        aria-modal="true"
        aria-labelledby="mv-title"
      >
        <h3 className="dialog-title" id="mv-title">
          {t("dialog.moveTo")}
        </h3>
        <div className="dialog-field">
          <label htmlFor="mv-folder">{t("dialog.targetFolder")}</label>
          <select id="mv-folder" ref={selectRef} defaultValue="">
            {showRoot && <option value="__root__">{t("dialog.root")}</option>}
            {folders
              .filter((f) => f.id !== excludeId)
              .map((f) => (
                <option key={f.id} value={f.id}>
                  {f.name}
                </option>
              ))}
          </select>
        </div>
        <div className="dialog-actions">
          <button type="button" className="dialog-btn dialog-btn-cancel" onClick={onCancel}>
            {t("dialog.cancel")}
          </button>
          <button
            type="button"
            className="dialog-btn dialog-btn-primary"
            onClick={() => {
              const val = selectRef.current?.value;
              if (val) onSubmit(val);
            }}
          >
            {t("dialog.move")}
          </button>
        </div>
      </div>
    </div>
  );
}
