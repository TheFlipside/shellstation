import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { open } from "@tauri-apps/plugin-dialog";
import { useEscapeKey } from "../hooks/useEscapeKey";
import type { Session } from "../stores/sessionStore";

interface FolderCredentialDialogProps {
  folderId: string;
  folderName: string;
  sessions: Session[];
  onSubmit: (
    username: string,
    authMethod: string,
    credential: string,
    jumpHostId: string | null,
  ) => void;
  onCancel: () => void;
}

export function FolderCredentialDialog({
  folderId,
  folderName,
  sessions,
  onSubmit,
  onCancel,
}: FolderCredentialDialogProps): React.JSX.Element {
  const { t } = useTranslation();
  useEscapeKey(onCancel);
  const [username, setUsername] = useState("");
  const [authMethod, setAuthMethod] = useState("password");
  const [password, setPassword] = useState("");
  const [keyPath, setKeyPath] = useState("");
  const [jumpHostId, setJumpHostId] = useState("");

  // Clear credentials from state when the dialog unmounts.
  useEffect(() => {
    return () => {
      setPassword("");
      setKeyPath("");
    };
  }, []);

  // Filter jump host candidates: only SSH sessions not in the target folder.
  const jumpHostCandidates = sessions.filter(
    (s) => s.protocol === "ssh" && s.folder_id !== folderId,
  );

  const handleSubmit = (e: React.SyntheticEvent): void => {
    e.preventDefault();
    if (!username.trim()) return;
    const credential = authMethod === "password" ? password : keyPath;
    if (!credential.trim()) return;
    onSubmit(username.trim(), authMethod, credential, jumpHostId || null);
  };

  return (
    <div className="dialog-overlay" onClick={onCancel} role="presentation">
      <div
        className="dialog dialog-wide"
        onClick={(e) => {
          e.stopPropagation();
        }}
        role="dialog"
        aria-modal="true"
        aria-labelledby="fcd-title"
      >
        <h3 className="dialog-title" id="fcd-title">
          {t("folderCredential.title", { name: folderName })}
        </h3>
        <p className="dialog-info">{t("folderCredential.info")}</p>
        <form onSubmit={handleSubmit}>
          <div className="dialog-field">
            <label htmlFor="fcd-username">{t("session.usernameLabel")}</label>
            <input
              id="fcd-username"
              type="text"
              value={username}
              onChange={(e) => {
                setUsername(e.target.value);
              }}
              placeholder={t("session.usernamePlaceholder")}
              autoFocus
            />
          </div>
          <div className="dialog-field">
            <label htmlFor="fcd-auth">{t("session.authMethodLabel")}</label>
            <select
              id="fcd-auth"
              value={authMethod}
              onChange={(e) => {
                setAuthMethod(e.target.value);
              }}
            >
              <option value="password">{t("session.authPassword")}</option>
              <option value="publickey">{t("session.authPublicKey")}</option>
            </select>
          </div>
          <div className="dialog-field">
            <label htmlFor="fcd-credential">
              {authMethod === "password" ? t("session.passwordLabel") : t("session.keyPathLabel")}
            </label>
            {authMethod === "password" ? (
              <input
                id="fcd-credential"
                type="password"
                value={password}
                onChange={(e) => {
                  setPassword(e.target.value);
                }}
                placeholder={t("session.passwordPlaceholder")}
              />
            ) : (
              <div className="dialog-row">
                <div className="dialog-field-grow">
                  <input
                    id="fcd-credential"
                    type="text"
                    value={keyPath}
                    onChange={(e) => {
                      setKeyPath(e.target.value);
                    }}
                    placeholder={t("session.keyPathPlaceholder")}
                  />
                </div>
                <button
                  type="button"
                  className="dialog-btn"
                  onClick={() => {
                    void (async () => {
                      const path = await open({
                        title: t("session.keyPathLabel"),
                        defaultPath: keyPath || undefined,
                        multiple: false,
                        directory: false,
                      });
                      if (path) {
                        setKeyPath(path);
                      }
                    })();
                  }}
                >
                  {t("session.keyPathBrowse")}
                </button>
              </div>
            )}
          </div>
          <div className="dialog-field">
            <label htmlFor="fcd-jump">{t("folderCredential.jumpHostLabel")}</label>
            <select
              id="fcd-jump"
              value={jumpHostId}
              onChange={(e) => {
                setJumpHostId(e.target.value);
              }}
            >
              <option value="">{t("session.jumpHostNone")}</option>
              {jumpHostCandidates.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name} ({s.hostname})
                </option>
              ))}
            </select>
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
