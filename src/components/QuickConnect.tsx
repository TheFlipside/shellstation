import React, { useState } from "react";
import { useTranslation } from "react-i18next";

export interface QuickConnectParams {
  host: string;
  port: number;
  username: string;
  authMethod: "password" | "publickey";
  authCredential: string;
}

interface QuickConnectProps {
  onConnect: (params: QuickConnectParams) => void;
  onCancel: () => void;
}

export function QuickConnect({ onConnect, onCancel }: QuickConnectProps): React.JSX.Element {
  const { t } = useTranslation();
  const [host, setHost] = useState("");
  const [port, setPort] = useState("22");
  const [username, setUsername] = useState("");
  const [authMethod, setAuthMethod] = useState<"password" | "publickey">("password");
  const [credential, setCredential] = useState("");

  const handleSubmit = (e: React.SyntheticEvent): void => {
    e.preventDefault();
    if (!host.trim() || !username.trim()) {
      return;
    }
    onConnect({
      host: host.trim(),
      port: Number(port) || 22,
      username: username.trim(),
      authMethod,
      authCredential: credential,
    });
  };

  return (
    <div className="dialog-overlay" onClick={onCancel} role="presentation">
      <div
        className="dialog"
        onClick={(e) => {
          e.stopPropagation();
        }}
        role="dialog"
        aria-modal="true"
        aria-labelledby="qc-title"
      >
        <h3 className="dialog-title" id="qc-title">
          {t("quickConnect.title")}
        </h3>
        <form onSubmit={handleSubmit}>
          <div className="dialog-field">
            <label htmlFor="qc-host">{t("quickConnect.hostLabel")}</label>
            <input
              id="qc-host"
              type="text"
              value={host}
              onChange={(e) => {
                setHost(e.target.value);
              }}
              placeholder={t("session.hostnamePlaceholder")}
              autoFocus
            />
          </div>
          <div className="dialog-field">
            <label htmlFor="qc-port">{t("session.portLabel")}</label>
            <input
              id="qc-port"
              type="number"
              value={port}
              onChange={(e) => {
                setPort(e.target.value);
              }}
              min={1}
              max={65535}
            />
          </div>
          <div className="dialog-field">
            <label htmlFor="qc-username">{t("session.usernameLabel")}</label>
            <input
              id="qc-username"
              type="text"
              value={username}
              onChange={(e) => {
                setUsername(e.target.value);
              }}
              placeholder={t("session.usernamePlaceholder")}
            />
          </div>
          <div className="dialog-field">
            <label htmlFor="qc-auth">{t("session.authMethodLabel")}</label>
            <select
              id="qc-auth"
              value={authMethod}
              onChange={(e) => {
                setAuthMethod(e.target.value as "password" | "publickey");
              }}
            >
              <option value="password">{t("session.authPassword")}</option>
              <option value="publickey">{t("session.authPublicKey")}</option>
            </select>
          </div>
          <div className="dialog-field">
            <label htmlFor="qc-credential">
              {authMethod === "password" ? t("session.passwordLabel") : t("session.keyPathLabel")}
            </label>
            <input
              id="qc-credential"
              type={authMethod === "password" ? "password" : "text"}
              value={credential}
              onChange={(e) => {
                setCredential(e.target.value);
              }}
              placeholder={
                authMethod === "password"
                  ? t("session.passwordPlaceholder")
                  : t("session.keyPathPlaceholder")
              }
            />
          </div>
          <div className="dialog-actions">
            <button type="button" className="dialog-btn dialog-btn-cancel" onClick={onCancel}>
              {t("dialog.cancel")}
            </button>
            <button type="submit" className="dialog-btn dialog-btn-primary">
              {t("quickConnect.connect")}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
