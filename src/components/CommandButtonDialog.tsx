import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { useEnterKey } from "../hooks/useEnterKey";
import { useEscapeKey } from "../hooks/useEscapeKey";

export interface CommandButtonFormData {
  name: string;
  command: string;
  color: string;
}

export const MAX_NAME_LENGTH = 32;
const MAX_COMMAND_LENGTH = 1024;

/** Preset colors for command button color picker. */
const COLOR_PRESETS = [
  "#89b4fa", // blue
  "#a6e3a1", // green
  "#f9e2af", // yellow
  "#fab387", // peach
  "#f38ba8", // red
  "#f5c2e7", // pink
  "#94e2d5", // teal
  "#74c7ec", // sapphire
  "#cba6f7", // lavender
  "#585b70", // gray
];

interface CommandButtonDialogProps {
  initial?: CommandButtonFormData;
  onSave: (data: CommandButtonFormData) => void;
  onCancel: () => void;
}

export function CommandButtonDialog({
  initial,
  onSave,
  onCancel,
}: CommandButtonDialogProps): React.JSX.Element {
  const { t } = useTranslation();
  useEscapeKey(onCancel);
  const [name, setName] = useState(initial?.name ?? "");
  const [command, setCommand] = useState(initial?.command ?? "");
  const [color, setColor] = useState(initial?.color ?? COLOR_PRESETS[0]);

  const handleClear = (): void => {
    setName("");
    setCommand("");
    setColor(COLOR_PRESETS[0]);
  };

  const handleSave = (): void => {
    if (!name.trim() || !command) return;
    onSave({ name: name.trim(), command, color });
  };

  useEnterKey(handleSave);

  return (
    <div
      className="dialog-overlay"
      onClick={(e) => {
        if (e.target === e.currentTarget) onCancel();
      }}
      role="presentation"
    >
      <div className="dialog" style={{ minWidth: 400 }}>
        <div className="dialog-title">{t("commandBar.dialogTitle")}</div>

        <label className="dialog-field">
          <span>{t("commandBar.nameLabel")}</span>
          <input
            type="text"
            value={name}
            onChange={(e) => {
              setName(e.target.value);
            }}
            placeholder={t("commandBar.namePlaceholder")}
            maxLength={MAX_NAME_LENGTH}
            autoFocus
          />
        </label>

        <label className="dialog-field">
          <span>{t("commandBar.commandLabel")}</span>
          <textarea
            className="command-textarea"
            value={command}
            onChange={(e) => {
              setCommand(e.target.value);
            }}
            placeholder={t("commandBar.commandPlaceholder")}
            maxLength={MAX_COMMAND_LENGTH}
            rows={3}
          />
        </label>

        <div className="dialog-field escape-hint-grid">
          <span>{t("commandBar.escapeHintTitle")}</span>
          <div className="escape-hint-columns">
            {(t("commandBar.escapeHintItems", { returnObjects: true }) as string[]).map((item) => (
              <span key={item}>{item}</span>
            ))}
          </div>
        </div>

        <div className="dialog-field">
          <span>{t("commandBar.colorLabel")}</span>
          <div className="color-preset-row">
            {COLOR_PRESETS.map((c) => (
              <button
                key={c}
                type="button"
                className={`color-preset-swatch${c === color ? " color-preset-selected" : ""}`}
                style={{ backgroundColor: c }}
                onClick={() => {
                  setColor(c);
                }}
                title={c}
              />
            ))}
          </div>
        </div>

        <div className="dialog-actions">
          <button type="button" className="dialog-btn dialog-btn-cancel" onClick={handleClear}>
            {t("commandBar.clear")}
          </button>
          <div style={{ flex: 1 }} />
          <button type="button" className="dialog-btn dialog-btn-cancel" onClick={onCancel}>
            {t("dialog.cancel")}
          </button>
          <button
            type="button"
            className="dialog-btn dialog-btn-primary"
            onClick={handleSave}
            disabled={!name.trim() || !command}
          >
            {t("commandBar.ok")}
          </button>
        </div>
      </div>
    </div>
  );
}
