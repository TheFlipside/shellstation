import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { useEscapeKey } from "../hooks/useEscapeKey";
import type { Folder, Session } from "../stores/sessionStore";
import { sessionHasTag } from "../stores/sessionStore";
import { useAppStore } from "../stores/appStore";
import { useHighlightStore } from "../stores/highlightStore";
import { useCredentialProfilesStore } from "../stores/credentialProfilesStore";
import { useLoginSequenceStore } from "../stores/loginSequenceStore";
import { SESSION_ICON_KEYS, SessionIconComponent } from "./SessionIcons";
import { CustomSelect } from "./CustomSelect";

export interface SessionFormData {
  folderId: string;
  name: string;
  hostname: string;
  port: number;
  protocol: string;
  username: string;
  tags: string;
  icon: string;
  jumpHostId: string | null;
  highlightProfileId: string | null;
  credentialProfileId: string | null;
  loginSequenceId: string | null;
  legacyAlgorithms: boolean;
}

interface SessionDialogProps {
  title: string;
  folders: Folder[];
  sessions: Session[];
  defaultFolderId: string;
  sessionId?: string;
  initial?: Partial<SessionFormData>;
  onSubmit: (data: SessionFormData) => void;
  onCancel: () => void;
  onManageCredentials: () => void;
  onManageLoginSequences: () => void;
}

export function SessionDialog({
  title,
  folders,
  sessions,
  defaultFolderId,
  sessionId,
  initial,
  onSubmit,
  onCancel,
  onManageCredentials,
  onManageLoginSequences,
}: SessionDialogProps): React.JSX.Element {
  const { t } = useTranslation();
  useEscapeKey(onCancel);
  const isPg = useAppStore((s) => s.dbBackend) === "postgres";
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
  const [username, setUsername] = useState(initial?.username ?? "");
  const [credentialProfileId, setCredentialProfileId] = useState(
    initial?.credentialProfileId ?? "",
  );
  const [loginSequenceId, setLoginSequenceId] = useState(initial?.loginSequenceId ?? "");
  const [legacyAlgorithms, setLegacyAlgorithms] = useState(initial?.legacyAlgorithms ?? false);

  // In PG mode, load the per-user credential mapping for this session.
  useEffect(() => {
    if (!isPg || !sessionId) return;
    invoke<string | null>("get_session_credential", { sessionId })
      .then((profileId) => {
        if (profileId !== null) {
          setCredentialProfileId(profileId);
        }
      })
      .catch(() => {
        // Ignore — fall back to session's own credential_profile_id
      });
  }, [isPg, sessionId]);

  useEffect(() => {
    if (!isPg || !sessionId) return;
    invoke<string | null>("get_session_login_sequence", { sessionId })
      .then((seqId) => {
        if (seqId !== null) {
          setLoginSequenceId(seqId);
        }
      })
      .catch(() => undefined);
  }, [isPg, sessionId]);

  const highlightProfiles = useHighlightStore((s) => s.profiles);
  const credentialProfiles = useCredentialProfilesStore((s) => s.profiles);
  const loginSequences = useLoginSequenceStore((s) => s.sequences);
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

    // In PG mode, save credential mapping per-user and don't overwrite the
    // shared session's credential_profile_id column.
    if (isPg && sessionId && protocol !== "telnet") {
      invoke("set_session_credential", {
        sessionId,
        credentialProfileId: credentialProfileId || null,
      }).catch(() => undefined);
    }
    if (isPg && sessionId) {
      invoke("set_session_login_sequence", {
        sessionId,
        loginSequenceId: loginSequenceId || null,
      }).catch(() => undefined);
    }

    onSubmit({
      folderId,
      name: name.trim(),
      hostname: hostname.trim(),
      port: portNum,
      protocol,
      username: protocol === "ssh" && !credentialProfileId ? username.trim() : "",
      tags,
      icon,
      jumpHostId: protocol === "telnet" ? null : jumpHostId || null,
      highlightProfileId: highlightProfileId || null,
      // In PG mode, don't write credentialProfileId to the shared session row
      credentialProfileId: isPg || protocol === "telnet" ? null : credentialProfileId || null,
      loginSequenceId: isPg ? null : loginSequenceId || null,
      legacyAlgorithms: protocol === "ssh" ? legacyAlgorithms : false,
    });
  };

  return (
    <div className="dialog-overlay" role="presentation">
      <div
        className="dialog dialog-wide"
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
              {!credentialProfileId && (
                <div className="dialog-field">
                  <label htmlFor="sd-username">{t("session.usernameLabel")}</label>
                  <input
                    id="sd-username"
                    type="text"
                    value={username}
                    onChange={(e) => {
                      setUsername(e.target.value);
                    }}
                    placeholder={t("session.usernamePlaceholder")}
                  />
                  <span className="dialog-hint">{t("session.usernameHintKbd")}</span>
                </div>
              )}
              <div className="dialog-field">
                <label htmlFor="sd-jump">{t("session.jumpHostLabel")}</label>
                <CustomSelect
                  id="sd-jump"
                  value={jumpHostId}
                  onChange={setJumpHostId}
                  options={[
                    { value: "", label: t("session.jumpHostNone") },
                    ...sessions
                      .filter((s) => {
                        if (s.protocol !== "ssh") return false;
                        if (s.id === sessionId) return false;
                        // Keep the currently assigned jump host visible even if its tag was removed.
                        if (s.id === jumpHostId) return true;
                        return sessionHasTag(s, "jumphost");
                      })
                      .map((s) => ({
                        value: s.id,
                        label: `${s.name} (${s.hostname})`,
                      })),
                  ]}
                />
              </div>
              <div className="dialog-field">
                <label
                  htmlFor="sd-legacy-algos"
                  className="dialog-checkbox-label"
                  title={t("session.legacyAlgorithmsHint")}
                >
                  <input
                    id="sd-legacy-algos"
                    type="checkbox"
                    checked={legacyAlgorithms}
                    onChange={(e) => {
                      setLegacyAlgorithms(e.target.checked);
                    }}
                  />
                  {t("session.legacyAlgorithmsLabel")}
                </label>
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
            <label htmlFor="sd-loginseq">{t("session.loginSequenceLabel")}</label>
            <div className="dialog-row">
              <div className="dialog-field-grow">
                <CustomSelect
                  id="sd-loginseq"
                  value={loginSequenceId}
                  onChange={setLoginSequenceId}
                  options={[
                    { value: "", label: t("session.loginSequenceNone") },
                    ...loginSequences.map((s) => ({
                      value: s.id,
                      label: s.name,
                    })),
                  ]}
                />
              </div>
              <button type="button" className="dialog-btn" onClick={onManageLoginSequences}>
                {t("loginSequences.manageLink")}
              </button>
            </div>
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
            <p className="dialog-field-note">{t("session.tagsJumpHostNote")}</p>
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
