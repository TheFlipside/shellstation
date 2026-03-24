import React, { useState } from "react";
import type { Folder, Session } from "../stores/sessionStore";

export interface SessionFormData {
  folderId: string;
  name: string;
  hostname: string;
  port: number;
  username: string;
  authMethod: string;
  tags: string;
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
  const [folderId, setFolderId] = useState(initial?.folderId ?? defaultFolderId);
  const [name, setName] = useState(initial?.name ?? "");
  const [hostname, setHostname] = useState(initial?.hostname ?? "");
  const [port, setPort] = useState(String(initial?.port ?? 22));
  const [username, setUsername] = useState(initial?.username ?? "");
  const [authMethod, setAuthMethod] = useState(initial?.authMethod ?? "password");
  const [password, setPassword] = useState(initial?.password ?? "");
  const [keyPath, setKeyPath] = useState(initial?.keyPath ?? "");
  const [tags, setTags] = useState(initial?.tags ?? "");
  const [jumpHostId, setJumpHostId] = useState(initial?.jumpHostId ?? "");

  const handleSubmit = (e: React.SyntheticEvent): void => {
    e.preventDefault();
    if (!name.trim() || !hostname.trim() || !username.trim()) return;
    onSubmit({
      folderId,
      name: name.trim(),
      hostname: hostname.trim(),
      port: Number(port) || 22,
      username: username.trim(),
      authMethod,
      tags,
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
            <label htmlFor="sd-folder">Folder</label>
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
            <label htmlFor="sd-name">Name</label>
            <input
              id="sd-name"
              type="text"
              value={name}
              onChange={(e) => {
                setName(e.target.value);
              }}
              placeholder="My Server"
              autoFocus
            />
          </div>
          <div className="dialog-row">
            <div className="dialog-field dialog-field-grow">
              <label htmlFor="sd-host">Hostname</label>
              <input
                id="sd-host"
                type="text"
                value={hostname}
                onChange={(e) => {
                  setHostname(e.target.value);
                }}
                placeholder="hostname or IP"
              />
            </div>
            <div className="dialog-field dialog-field-small">
              <label htmlFor="sd-port">Port</label>
              <input
                id="sd-port"
                type="number"
                value={port}
                onChange={(e) => {
                  setPort(e.target.value);
                }}
                min={1}
                max={65535}
              />
            </div>
          </div>
          <div className="dialog-field">
            <label htmlFor="sd-user">Username</label>
            <input
              id="sd-user"
              type="text"
              value={username}
              onChange={(e) => {
                setUsername(e.target.value);
              }}
              placeholder="user"
            />
          </div>
          <div className="dialog-field">
            <label htmlFor="sd-auth">Auth Method</label>
            <select
              id="sd-auth"
              value={authMethod}
              onChange={(e) => {
                setAuthMethod(e.target.value);
              }}
            >
              <option value="password">Password</option>
              <option value="publickey">Public Key</option>
            </select>
          </div>
          <div className="dialog-field">
            <label htmlFor="sd-credential">
              {authMethod === "password" ? "Password" : "Key Path"}
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
              placeholder={authMethod === "password" ? "password" : "~/.ssh/id_ed25519"}
            />
          </div>
          <div className="dialog-field">
            <label htmlFor="sd-jump">Jump Host</label>
            <select
              id="sd-jump"
              value={jumpHostId}
              onChange={(e) => {
                setJumpHostId(e.target.value);
              }}
            >
              <option value="">None</option>
              {sessions.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name} ({s.hostname})
                </option>
              ))}
            </select>
          </div>
          <div className="dialog-field">
            <label htmlFor="sd-tags">Tags</label>
            <input
              id="sd-tags"
              type="text"
              value={tags}
              onChange={(e) => {
                setTags(e.target.value);
              }}
              placeholder="prod, eu, web (comma-separated)"
            />
          </div>
          <div className="dialog-actions">
            <button type="button" className="dialog-btn dialog-btn-cancel" onClick={onCancel}>
              Cancel
            </button>
            <button type="submit" className="dialog-btn dialog-btn-primary">
              Save
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
