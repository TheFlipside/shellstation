import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { open } from "@tauri-apps/plugin-dialog";
import { useEscapeKey } from "../hooks/useEscapeKey";
import { CustomSelect } from "./CustomSelect";

export interface QuickConnectParams {
  protocol: "ssh" | "telnet";
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
  useEscapeKey(onCancel);
  const [protocol, setProtocol] = useState<"ssh" | "telnet">("ssh");
  const [host, setHost] = useState("");
  const [port, setPort] = useState("22");
  const [username, setUsername] = useState("");
  const [authMethod, setAuthMethod] = useState<"password" | "publickey">("password");
  const [credential, setCredential] = useState("");

  // Clear credential from state when the dialog unmounts.
  useEffect(() => {
    return () => {
      setCredential("");
    };
  }, []);

  const handleSubmit = (e: React.SyntheticEvent): void => {
    e.preventDefault();
    if (!host.trim()) return;
    if (protocol === "ssh" && !username.trim()) return;
    const defaultPort = protocol === "telnet" ? 23 : 22;
    onConnect({
      protocol,
      host: host.trim(),
      port: Number(port) || defaultPort,
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
            <label htmlFor="qc-protocol">{t("session.protocolLabel")}</label>
            <CustomSelect
              id="qc-protocol"
              value={protocol}
              onChange={(v) => {
                const p = v as "ssh" | "telnet";
                setProtocol(p);
                setPort((prev) =>
                  (prev === "22" && p === "telnet") || (prev === "23" && p === "ssh")
                    ? p === "telnet"
                      ? "23"
                      : "22"
                    : prev,
                );
              }}
              options={[
                { value: "ssh", label: "SSH" },
                { value: "telnet", label: "Telnet" },
              ]}
            />
          </div>
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
          {protocol === "ssh" && (
            <>
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
                <CustomSelect
                  id="qc-auth"
                  value={authMethod}
                  onChange={(v) => {
                    setAuthMethod(v as "password" | "publickey");
                  }}
                  options={[
                    { value: "password", label: t("session.authPassword") },
                    { value: "publickey", label: t("session.authPublicKey") },
                  ]}
                />
              </div>
              <div className="dialog-field">
                <label htmlFor="qc-credential">
                  {authMethod === "password"
                    ? t("session.passwordLabel")
                    : t("session.keyPathLabel")}
                </label>
                {authMethod === "password" ? (
                  <input
                    id="qc-credential"
                    type="password"
                    value={credential}
                    onChange={(e) => {
                      setCredential(e.target.value);
                    }}
                    placeholder={t("session.passwordPlaceholder")}
                  />
                ) : (
                  <div className="dialog-row">
                    <div className="dialog-field-grow">
                      <input
                        id="qc-credential"
                        type="text"
                        value={credential}
                        onChange={(e) => {
                          setCredential(e.target.value);
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
                            defaultPath: credential || undefined,
                            multiple: false,
                            directory: false,
                          });
                          if (path) {
                            setCredential(path);
                          }
                        })();
                      }}
                    >
                      {t("session.keyPathBrowse")}
                    </button>
                  </div>
                )}
              </div>
            </>
          )}
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
