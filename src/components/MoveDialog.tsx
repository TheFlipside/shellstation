import React, { useCallback, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useEnterKey } from "../hooks/useEnterKey";
import { useEscapeKey } from "../hooks/useEscapeKey";
import type { Folder } from "../stores/sessionStore";
import { CustomSelect } from "./CustomSelect";

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
  const options = useMemo(() => {
    const opts = folders
      .filter((f) => f.id !== excludeId)
      .map((f) => ({ value: f.id, label: f.name }));
    if (showRoot) opts.unshift({ value: "__root__", label: t("dialog.root") });
    return opts;
  }, [folders, excludeId, showRoot, t]);
  const [selected, setSelected] = useState(options[0]?.value ?? "");
  const handleSubmit = useCallback(() => {
    if (selected) onSubmit(selected);
  }, [onSubmit, selected]);
  useEnterKey(handleSubmit);

  return (
    <div className="dialog-overlay" role="presentation">
      <div className="dialog" role="dialog" aria-modal="true" aria-labelledby="mv-title">
        <h3 className="dialog-title" id="mv-title">
          {t("dialog.moveTo")}
        </h3>
        <div className="dialog-field">
          <label htmlFor="mv-folder">{t("dialog.targetFolder")}</label>
          <CustomSelect id="mv-folder" value={selected} onChange={setSelected} options={options} />
        </div>
        <div className="dialog-actions">
          <button type="button" className="dialog-btn dialog-btn-cancel" onClick={onCancel}>
            {t("dialog.cancel")}
          </button>
          <button type="button" className="dialog-btn dialog-btn-primary" onClick={handleSubmit}>
            {t("dialog.move")}
          </button>
        </div>
      </div>
    </div>
  );
}
