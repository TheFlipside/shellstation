import React from "react";
import ReactDOM from "react-dom";
import { useTranslation } from "react-i18next";
import { DialogKeys } from "./DialogKeys";

interface ConfirmDialogProps {
  message: string;
  confirmLabel?: string;
  onConfirm: () => void;
  onCancel: () => void;
}

export function ConfirmDialog({
  message,
  confirmLabel,
  onConfirm,
  onCancel,
}: ConfirmDialogProps): React.JSX.Element {
  const { t } = useTranslation();

  return ReactDOM.createPortal(
    <div className="dialog-overlay" role="presentation">
      <DialogKeys onEscape={onCancel} onEnter={onConfirm} />
      <div className="dialog" role="dialog" aria-modal="true">
        <p className="dialog-text">{message}</p>
        <div className="dialog-actions">
          <button type="button" className="dialog-btn dialog-btn-cancel" onClick={onCancel}>
            {t("dialog.cancel")}
          </button>
          <button type="button" className="dialog-btn dialog-btn-danger" onClick={onConfirm}>
            {confirmLabel ?? t("contextMenu.delete")}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
