import React, { useEffect, useRef } from "react";
import ReactDOM from "react-dom";
import { useTranslation } from "react-i18next";
import { useEscapeKey } from "../hooks/useEscapeKey";

interface PasteConfirmDialogProps {
  content: string;
  onConfirm: () => void;
  onCancel: () => void;
}

const PREVIEW_MAX_CHARS = 16384;

export function PasteConfirmDialog({
  content,
  onConfirm,
  onCancel,
}: PasteConfirmDialogProps): React.JSX.Element {
  const { t } = useTranslation();
  useEscapeKey(onCancel);

  const cancelRef = useRef<HTMLButtonElement>(null);

  // Default focus to Cancel — Enter is intentionally not bound, so a stray
  // keypress cannot auto-execute the paste.
  useEffect(() => {
    cancelRef.current?.focus();
  }, []);

  const lineCount = content.split(/\r\n|\r|\n/).length;
  const truncated = content.length > PREVIEW_MAX_CHARS;
  const preview = truncated ? content.slice(0, PREVIEW_MAX_CHARS) : content;

  return ReactDOM.createPortal(
    <div className="dialog-overlay" onClick={onCancel}>
      <div
        className="dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="paste-confirm-title"
        onClick={(e) => {
          e.stopPropagation();
        }}
      >
        <h3 id="paste-confirm-title" className="dialog-title">
          {t("pasteConfirm.title")}
        </h3>
        <p className="dialog-text">{t("pasteConfirm.message", { count: lineCount })}</p>
        <pre className="dialog-paste-preview">{preview}</pre>
        {truncated ? (
          <p className="dialog-text dialog-paste-truncated">{t("pasteConfirm.truncated")}</p>
        ) : null}
        <div className="dialog-actions">
          <button
            ref={cancelRef}
            type="button"
            className="dialog-btn dialog-btn-cancel"
            onClick={onCancel}
          >
            {t("dialog.cancel")}
          </button>
          <button type="button" className="dialog-btn dialog-btn-primary" onClick={onConfirm}>
            {t("pasteConfirm.confirm")}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
