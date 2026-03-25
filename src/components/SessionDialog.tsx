import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { useEscapeKey } from "../hooks/useEscapeKey";
import type { Folder, Session } from "../stores/sessionStore";

export type SessionIcon =
  | "desktop"
  | "linux"
  | "windows"
  | "apple"
  | "network"
  | "firewall"
  | "database"
  | "web"
  | "cloud"
  | "container"
  | "printer"
  | "lock";

export const SESSION_ICONS: { key: SessionIcon; emoji: string }[] = [
  { key: "desktop", emoji: "\uD83D\uDDA5\uFE0F" },
  { key: "linux", emoji: "\uD83D\uDC27" },
  { key: "windows", emoji: "\uD83E\uDE9F" },
  { key: "apple", emoji: "\uD83C\uDF4E" },
  { key: "network", emoji: "\uD83D\uDD00" },
  { key: "firewall", emoji: "\uD83D\uDEE1\uFE0F" },
  { key: "database", emoji: "\uD83D\uDDC4\uFE0F" },
  { key: "web", emoji: "\uD83C\uDF10" },
  { key: "cloud", emoji: "\u2601\uFE0F" },
  { key: "container", emoji: "\uD83D\uDC33" },
  { key: "printer", emoji: "\uD83D\uDDA8\uFE0F" },
  { key: "lock", emoji: "\uD83D\uDD12" },
];

export function iconEmoji(key: string): string {
  return SESSION_ICONS.find((i) => i.key === key)?.emoji ?? "\uD83D\uDDA5\uFE0F";
}

export interface SessionFormData {
  folderId: string;
  name: string;
  hostname: string;
  port: number;
  username: string;
  authMethod: string;
  tags: string;
  icon: string;
  jumpHostId: string | null;
  password: string;
  keyPath: string;
}

interface SessionDialogProps {
  title: string;
  folders: Folder[];
  sessions: Session[];
  defaultFolderId: string;
  initial?: Partial<SessionFormData>;
  onSubmit: (data: SessionFormData) => void;
  onCancel: () => void;
}

export function SessionDialog({
  title,
  folders,
  sessions,
  defaultFolderId,
  initial,
  onSubmit,
  onCancel,
}: SessionDialogProps): React.JSX.Element {
  const { t } = useTranslation();
  useEscapeKey(onCancel);
  const [folderId, setFolderId] = useState(initial?.folderId ?? defaultFolderId);
  const [name, setName] = useState(initial?.name ?? "");
  const [hostname, setHostname] = useState(initial?.hostname ?? "");
  const [port, setPort] = useState(String(initial?.port ?? 22));
  const [username, setUsername] = useState(initial?.username ?? "");
  const [authMethod, setAuthMethod] = useState(initial?.authMethod ?? "password");
  const [password, setPassword] = useState(initial?.password ?? "");
  const [keyPath, setKeyPath] = useState(initial?.keyPath ?? "");
  const [tags, setTags] = useState(initial?.tags ?? "");
  const [icon, setIcon] = useState(initial?.icon ?? "desktop");
  const [jumpHostId, setJumpHostId] = useState(initial?.jumpHostId ?? "");
  const [error, setError] = useState("");

  const handleSubmit = (e: React.SyntheticEvent): void => {
    e.preventDefault();
    setError("");
    if (!name.trim() || !hostname.trim() || !username.trim()) return;
    const portNum = Number(port);
    if (!Number.isInteger(portNum) || portNum < 1 || portNum > 65535) {
      setError(t("session.portRange"));
      return;
    }
    onSubmit({
      folderId,
      name: name.trim(),
      hostname: hostname.trim(),
      port: portNum,
      username: username.trim(),
      authMethod,
      tags,
      icon,
      jumpHostId: jumpHostId || null,
      password,
      keyPath,
    });
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
        aria-labelledby="sd-title"
      >
        <h3 className="dialog-title" id="sd-title">
          {title}
        </h3>
        <form onSubmit={handleSubmit}>
          <div className="dialog-field">
            <label htmlFor="sd-folder">{t("session.folderLabel")}</label>
            <select
              id="sd-folder"
              value={folderId}
              onChange={(e) => {
                setFolderId(e.target.value);
              }}
            >
              {folders.map((f) => (
                <option key={f.id} value={f.id}>
                  {f.name}
                </option>
              ))}
            </select>
          </div>
          <div className="dialog-field">
            <label htmlFor="sd-name">{t("session.nameLabel")}</label>
            <input
              id="sd-name"
              type="text"
              value={name}
              onChange={(e) => {
                setName(e.target.value);
              }}
              placeholder={t("session.namePlaceholder")}
              autoFocus
            />
          </div>
          <div className="dialog-field">
            <label>{t("session.iconLabel")}</label>
            <div className="icon-picker">
              {SESSION_ICONS.map((ic) => (
                <button
                  key={ic.key}
                  type="button"
                  className={`icon-picker-btn${icon === ic.key ? " icon-picker-btn-active" : ""}`}
                  onClick={() => {
                    setIcon(ic.key);
                  }}
                  title={t(`session.icon_${ic.key}`)}
                >
                  {ic.emoji}
                </button>
              ))}
            </div>
          </div>
          <div className="dialog-row">
            <div className="dialog-field dialog-field-grow">
              <label htmlFor="sd-host">{t("session.hostnameLabel")}</label>
              <input
                id="sd-host"
                type="text"
                value={hostname}
                onChange={(e) => {
                  setHostname(e.target.value);
                }}
                placeholder={t("session.hostnamePlaceholder")}
              />
            </div>
            <div className="dialog-field dialog-field-small">
              <label htmlFor="sd-port">{t("session.portLabel")}</label>
              <input
                id="sd-port"
                type="number"
                value={port}
                onChange={(e) => {
                  setPort(e.target.value);
                  setError("");
                }}
              />
            </div>
          </div>
          <div className="dialog-field">
            <label htmlFor="sd-user">{t("session.usernameLabel")}</label>
            <input
              id="sd-user"
              type="text"
              value={username}
              onChange={(e) => {
                setUsername(e.target.value);
              }}
              placeholder={t("session.usernamePlaceholder")}
            />
          </div>
          <div className="dialog-field">
            <label htmlFor="sd-auth">{t("session.authMethodLabel")}</label>
            <select
              id="sd-auth"
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
            <label htmlFor="sd-credential">
              {authMethod === "password" ? t("session.passwordLabel") : t("session.keyPathLabel")}
            </label>
            <input
              id="sd-credential"
              type={authMethod === "password" ? "password" : "text"}
              value={authMethod === "password" ? password : keyPath}
              onChange={(e) => {
                if (authMethod === "password") {
                  setPassword(e.target.value);
                } else {
                  setKeyPath(e.target.value);
                }
              }}
              placeholder={
                authMethod === "password"
                  ? t("session.passwordPlaceholder")
                  : t("session.keyPathPlaceholder")
              }
            />
          </div>
          <div className="dialog-field">
            <label htmlFor="sd-jump">{t("session.jumpHostLabel")}</label>
            <select
              id="sd-jump"
              value={jumpHostId}
              onChange={(e) => {
                setJumpHostId(e.target.value);
              }}
            >
              <option value="">{t("session.jumpHostNone")}</option>
              {sessions.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name} ({s.hostname})
                </option>
              ))}
            </select>
          </div>
          <div className="dialog-field">
            <label htmlFor="sd-tags">{t("session.tagsLabel")}</label>
            <input
              id="sd-tags"
              type="text"
              value={tags}
              onChange={(e) => {
                setTags(e.target.value);
              }}
              placeholder={t("session.tagsPlaceholder")}
            />
          </div>
          {error && <p className="dialog-error">{error}</p>}
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
