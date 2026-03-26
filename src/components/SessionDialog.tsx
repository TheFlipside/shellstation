import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { open } from "@tauri-apps/plugin-dialog";
import { useEscapeKey } from "../hooks/useEscapeKey";
import type { Folder, Session } from "../stores/sessionStore";
import { SESSION_ICON_KEYS, SessionIconComponent } from "./SessionIcons";

export interface SessionFormData {
  folderId: string;
  name: string;
  hostname: string;
  port: number;
  protocol: string;
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
  const [protocol, setProtocol] = useState(initial?.protocol ?? "ssh");
  const [port, setPort] = useState(
    String(initial?.port ?? (initial?.protocol === "telnet" ? 23 : 22)),
  );
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
    if (!name.trim() || !hostname.trim()) return;
    if (protocol === "ssh" && !username.trim()) return;
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
      protocol,
      username: username.trim(),
      authMethod: protocol === "telnet" ? "none" : authMethod,
      tags,
      icon,
      jumpHostId: protocol === "telnet" ? null : jumpHostId || null,
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
          </div>
          <div className="dialog-field">
            <label htmlFor="sd-protocol">{t("session.protocolLabel")}</label>
            <select
              id="sd-protocol"
              value={protocol}
              onChange={(e) => {
                setProtocol(e.target.value);
                if (e.target.value === "telnet") {
                  setPort((prev) => (prev === "22" ? "23" : prev));
                  setAuthMethod("none");
                } else {
                  setPort((prev) => (prev === "23" ? "22" : prev));
                  if (authMethod === "none") setAuthMethod("password");
                }
              }}
            >
              <option value="ssh">SSH</option>
              <option value="telnet">Telnet</option>
            </select>
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
          {protocol === "ssh" && (
            <>
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
                  {authMethod === "password"
                    ? t("session.passwordLabel")
                    : t("session.keyPathLabel")}
                </label>
                {authMethod === "password" ? (
                  <input
                    id="sd-credential"
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
                        id="sd-credential"
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
            </>
          )}
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
