import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { useEscapeKey } from "../hooks/useEscapeKey";
import type { Folder, Session } from "../stores/sessionStore";
import { useHighlightStore } from "../stores/highlightStore";
import { useCredentialProfilesStore } from "../stores/credentialProfilesStore";
import { SESSION_ICON_KEYS, SessionIconComponent } from "./SessionIcons";
import { CustomSelect } from "./CustomSelect";

export interface SessionFormData {
  folderId: string;
  name: string;
  hostname: string;
  port: number;
  protocol: string;
  tags: string;
  icon: string;
  jumpHostId: string | null;
  highlightProfileId: string | null;
  credentialProfileId: string | null;
}

interface SessionDialogProps {
  title: string;
  folders: Folder[];
  sessions: Session[];
  defaultFolderId: string;
  initial?: Partial<SessionFormData>;
  onSubmit: (data: SessionFormData) => void;
  onCancel: () => void;
  onManageCredentials: () => void;
}

export function SessionDialog({
  title,
  folders,
  sessions,
  defaultFolderId,
  initial,
  onSubmit,
  onCancel,
  onManageCredentials,
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
  const [tags, setTags] = useState(initial?.tags ?? "");
  const [icon, setIcon] = useState(initial?.icon ?? "desktop");
  const [jumpHostId, setJumpHostId] = useState(initial?.jumpHostId ?? "");
  const [highlightProfileId, setHighlightProfileId] = useState(initial?.highlightProfileId ?? "");
  const [credentialProfileId, setCredentialProfileId] = useState(
    initial?.credentialProfileId ?? "",
  );
  const highlightProfiles = useHighlightStore((s) => s.profiles);
  const credentialProfiles = useCredentialProfilesStore((s) => s.profiles);
  const [error, setError] = useState("");

  const handleSubmit = (e: React.SyntheticEvent): void => {
    e.preventDefault();
    setError("");
    if (!name.trim() || !hostname.trim()) return;
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
      tags,
      icon,
      jumpHostId: protocol === "telnet" ? null : jumpHostId || null,
      highlightProfileId: highlightProfileId || null,
      credentialProfileId: protocol === "telnet" ? null : credentialProfileId || null,
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
            <CustomSelect
              id="sd-folder"
              value={folderId}
              onChange={setFolderId}
              options={folders.map((f) => ({ value: f.id, label: f.name }))}
            />
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
            <CustomSelect
              id="sd-protocol"
              value={protocol}
              onChange={(v) => {
                setProtocol(v);
                if (v === "telnet") {
                  setPort((prev) => (prev === "22" ? "23" : prev));
                } else {
                  setPort((prev) => (prev === "23" ? "22" : prev));
                }
              }}
              options={[
                { value: "ssh", label: "SSH" },
                { value: "telnet", label: "Telnet" },
              ]}
            />
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
          {protocol === "ssh" && (
            <>
              <div className="dialog-field">
                <label htmlFor="sd-credprofile">{t("session.credentialProfileLabel")}</label>
                <div className="dialog-row">
                  <div className="dialog-field-grow">
                    <CustomSelect
                      id="sd-credprofile"
                      value={credentialProfileId}
                      onChange={setCredentialProfileId}
                      options={[
                        { value: "", label: t("session.credentialProfileNone") },
                        ...credentialProfiles.map((p) => ({
                          value: p.id,
                          label: p.username ? `${p.name} (${p.username})` : p.name,
                        })),
                      ]}
                    />
                  </div>
                  <button type="button" className="dialog-btn" onClick={onManageCredentials}>
                    {t("credentialProfiles.manageLink")}
                  </button>
                </div>
              </div>
              <div className="dialog-field">
                <label htmlFor="sd-jump">{t("session.jumpHostLabel")}</label>
                <CustomSelect
                  id="sd-jump"
                  value={jumpHostId}
                  onChange={setJumpHostId}
                  options={[
                    { value: "", label: t("session.jumpHostNone") },
                    ...sessions.map((s) => ({
                      value: s.id,
                      label: `${s.name} (${s.hostname})`,
                    })),
                  ]}
                />
              </div>
            </>
          )}
          <div className="dialog-field">
            <label htmlFor="sd-highlight">{t("session.highlightProfileLabel")}</label>
            <CustomSelect
              id="sd-highlight"
              value={highlightProfileId}
              onChange={setHighlightProfileId}
              options={[
                { value: "", label: t("session.highlightProfileNone") },
                ...highlightProfiles.map((p) => ({
                  value: p.id,
                  label: p.name,
                })),
              ]}
            />
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
