import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { useEscapeKey } from "../hooks/useEscapeKey";
import { useLoginSequenceStore } from "../stores/loginSequenceStore";
import { CustomSelect } from "./CustomSelect";

interface FolderLoginSequenceDialogProps {
  folderName: string;
  onSubmit: (sequenceId: string | null) => void;
  onCancel: () => void;
  onManageSequences: () => void;
}

export function FolderLoginSequenceDialog({
  folderName,
  onSubmit,
  onCancel,
  onManageSequences,
}: FolderLoginSequenceDialogProps): React.JSX.Element {
  const { t } = useTranslation();
  useEscapeKey(onCancel);
  const [sequenceId, setSequenceId] = useState("");
  const sequences = useLoginSequenceStore((s) => s.sequences);

  const handleSubmit = (e: React.SyntheticEvent): void => {
    e.preventDefault();
    onSubmit(sequenceId || null);
  };

  return (
    <div className="dialog-overlay" role="presentation">
      <div
        className="dialog dialog-wide"
        role="dialog"
        aria-modal="true"
        aria-labelledby="flsd-title"
      >
        <h3 className="dialog-title" id="flsd-title">
          {t("folderLoginSequence.title", { name: folderName })}
        </h3>
        <p className="dialog-info">{t("folderLoginSequence.info")}</p>
        <form onSubmit={handleSubmit}>
          <div className="dialog-field">
            <label htmlFor="flsd-sequence">{t("folderLoginSequence.sequenceLabel")}</label>
            <div className="dialog-row">
              <div className="dialog-field-grow">
                <CustomSelect
                  id="flsd-sequence"
                  value={sequenceId}
                  onChange={setSequenceId}
                  options={[
                    { value: "", label: t("folderLoginSequence.sequenceNone") },
                    ...sequences.map((s) => ({
                      value: s.id,
                      label: s.name,
                    })),
                  ]}
                />
              </div>
              <button type="button" className="dialog-btn" onClick={onManageSequences}>
                {t("loginSequences.manageLink")}
              </button>
            </div>
          </div>
          <div className="dialog-actions">
            <button type="button" className="dialog-btn dialog-btn-cancel" onClick={onCancel}>
              {t("dialog.cancel")}
            </button>
            <button type="submit" className="dialog-btn dialog-btn-primary">
              {t("dialog.save")}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
