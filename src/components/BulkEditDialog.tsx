import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { useEscapeKey } from "../hooks/useEscapeKey";
import type { Session, BulkSessionEdit } from "../stores/sessionStore";
import { useHighlightStore } from "../stores/highlightStore";
import { SESSION_ICON_KEYS, SessionIconComponent } from "./SessionIcons";
import { CustomSelect } from "./CustomSelect";

interface BulkEditDialogProps {
  folderName: string;
  jumpHostCandidates: Session[];
  onSubmit: (edit: BulkSessionEdit) => void;
  onCancel: () => void;
}

export function BulkEditDialog({
  folderName,
  jumpHostCandidates,
  onSubmit,
  onCancel,
}: BulkEditDialogProps): React.JSX.Element {
  const { t } = useTranslation();
  useEscapeKey(onCancel);
  const highlightProfiles = useHighlightStore((s) => s.profiles);

  const [setJumpHost, setSetJumpHost] = useState(false);
  const [jumpHostId, setJumpHostId] = useState("");
  const [setHighlight, setSetHighlight] = useState(false);
  const [highlightProfileId, setHighlightProfileId] = useState("");
  const [setIconFlag, setSetIconFlag] = useState(false);
  const [icon, setIcon] = useState("desktop");

  const handleSubmit = (e: React.SyntheticEvent): void => {
    e.preventDefault();
    const edit: BulkSessionEdit = {};
    if (setJumpHost) edit.jumpHostId = jumpHostId || null;
    if (setHighlight) edit.highlightProfileId = highlightProfileId || null;
    if (setIconFlag) edit.icon = icon;
    onSubmit(edit);
  };

  const anyFieldSelected = setJumpHost || setHighlight || setIconFlag;

  return (
    <div className="dialog-overlay" role="presentation">
      <div
        className="dialog dialog-wide"
        role="dialog"
        aria-modal="true"
        aria-labelledby="bed-title"
      >
        <h3 className="dialog-title" id="bed-title">
          {t("bulkEdit.title", { name: folderName })}
        </h3>
        <p className="dialog-info">{t("bulkEdit.info")}</p>
        <form onSubmit={handleSubmit}>
          <div className="dialog-field">
            <label className="bulk-edit-toggle">
              <input
                type="checkbox"
                checked={setJumpHost}
                onChange={(e) => {
                  setSetJumpHost(e.target.checked);
                }}
              />
              {t("bulkEdit.setJumpHost")}
            </label>
            {setJumpHost && (
              <CustomSelect
                id="bed-jump"
                value={jumpHostId}
                onChange={setJumpHostId}
                options={[
                  { value: "", label: t("bulkEdit.clearValue") },
                  ...jumpHostCandidates
                    .filter((s) => {
                      try {
                        const parsed: unknown = JSON.parse(s.tags || "[]");
                        return (
                          Array.isArray(parsed) &&
                          parsed.some(
                            (tag) => typeof tag === "string" && tag.toLowerCase() === "jumphost",
                          )
                        );
                      } catch {
                        return false;
                      }
                    })
                    .map((s) => ({
                      value: s.id,
                      label: `${s.name} (${s.hostname})`,
                    })),
                ]}
              />
            )}
          </div>

          <div className="dialog-field">
            <label className="bulk-edit-toggle">
              <input
                type="checkbox"
                checked={setHighlight}
                onChange={(e) => {
                  setSetHighlight(e.target.checked);
                }}
              />
              {t("bulkEdit.setHighlightProfile")}
            </label>
            {setHighlight && (
              <CustomSelect
                id="bed-highlight"
                value={highlightProfileId}
                onChange={setHighlightProfileId}
                options={[
                  { value: "", label: t("bulkEdit.clearValue") },
                  ...highlightProfiles.map((p) => ({
                    value: p.id,
                    label: p.name,
                  })),
                ]}
              />
            )}
          </div>

          <div className="dialog-field">
            <label className="bulk-edit-toggle">
              <input
                type="checkbox"
                checked={setIconFlag}
                onChange={(e) => {
                  setSetIconFlag(e.target.checked);
                }}
              />
              {t("bulkEdit.setIcon")}
            </label>
            {setIconFlag && (
              <div className="icon-picker">
                {SESSION_ICON_KEYS.map((key) => (
                  <button
                    key={key}
                    type="button"
                    className={`icon-picker-btn${icon === key ? " icon-picker-btn-active" : ""}`}
                    onClick={() => {
                      setIcon(key);
                    }}
                    title={t(`session.icon_${key}`)}
                  >
                    <SessionIconComponent iconKey={key} />
                  </button>
                ))}
              </div>
            )}
          </div>

          <div className="dialog-actions">
            <button type="button" className="dialog-btn dialog-btn-cancel" onClick={onCancel}>
              {t("dialog.cancel")}
            </button>
            <button
              type="submit"
              className="dialog-btn dialog-btn-primary"
              disabled={!anyFieldSelected}
            >
              {t("dialog.save")}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
