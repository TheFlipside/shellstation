import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { useAppStore } from "../stores/appStore";

interface UserIdentDialogProps {
  onDone: () => void;
}

export function UserIdentDialog({ onDone }: UserIdentDialogProps): React.JSX.Element {
  const { t } = useTranslation();
  const [ident, setIdent] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    invoke<string>("get_os_username")
      .then((name) => {
        setIdent(name);
      })
      .catch(() => {
        // Ignore — user can type manually
      });
  }, []);

  const handleSubmit = (e: React.SyntheticEvent): void => {
    e.preventDefault();
    const trimmed = ident.trim();
    if (!trimmed) return;
    setLoading(true);
    setError(null);
    useAppStore
      .getState()
      .setUserIdent(trimmed)
      .then(() => {
        onDone();
      })
      .catch((err: unknown) => {
        setError(String(err));
      })
      .finally(() => {
        setLoading(false);
      });
  };

  return (
    <div className="dialog-overlay" role="presentation">
      <div
        className="dialog"
        onClick={(e) => {
          e.stopPropagation();
        }}
        role="dialog"
        aria-modal="true"
        aria-labelledby="uid-title"
      >
        <h3 className="dialog-title" id="uid-title">
          {t("userIdent.promptTitle")}
        </h3>
        <p className="dialog-text">{t("userIdent.promptMessage")}</p>
        <form onSubmit={handleSubmit}>
          <div className="dialog-field">
            <label htmlFor="uid-input">{t("userIdent.promptHint")}</label>
            <input
              id="uid-input"
              type="text"
              value={ident}
              onChange={(e) => {
                setIdent(e.target.value);
              }}
              autoFocus
              maxLength={128}
            />
          </div>
          {error !== null && <p className="dialog-error">{error}</p>}
          <div className="dialog-actions">
            <button
              type="submit"
              className="dialog-btn dialog-btn-primary"
              disabled={loading || !ident.trim()}
            >
              {t("dialog.save")}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
