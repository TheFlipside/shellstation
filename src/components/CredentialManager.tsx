import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { open } from "@tauri-apps/plugin-dialog";
import { useEscapeKey } from "../hooks/useEscapeKey";
import {
  useCredentialProfilesStore,
  type CredentialProfile,
} from "../stores/credentialProfilesStore";
import { useToastStore } from "../stores/toastStore";
import { ConfirmDialog } from "./ConfirmDialog";
import { CustomSelect } from "./CustomSelect";

// eslint-disable-next-line @typescript-eslint/no-empty-function
const noop = (): void => {};

interface CredentialManagerProps {
  onClose: () => void;
}

interface EditState {
  mode: "create" | "edit";
  id?: string;
  name: string;
  authType: string;
  username: string;
  keyPath: string;
  secret: string;
}

function blankEdit(): EditState {
  return {
    mode: "create",
    name: "",
    authType: "password",
    username: "",
    keyPath: "",
    secret: "",
  };
}

export function CredentialManager({ onClose }: CredentialManagerProps): React.JSX.Element {
  const { t } = useTranslation();
  const { profiles, loadAll, createProfile, updateProfile, deleteProfile, getSecret } =
    useCredentialProfilesStore();
  const [edit, setEdit] = useState<EditState | null>(null);
  const [error, setError] = useState("");
  const [confirmDelete, setConfirmDelete] = useState<CredentialProfile | null>(null);
  const addToast = useToastStore((s) => s.addToast);

  // ESC closes the inline edit form if open, otherwise closes the whole
  // manager. ConfirmDialog handles its own ESC via the shared stack.
  const handleEscape = useCallback(() => {
    if (confirmDelete !== null) return;
    if (edit !== null) {
      setEdit(null);
      return;
    }
    onClose();
  }, [edit, confirmDelete, onClose]);
  useEscapeKey(handleEscape);

  useEffect(() => {
    loadAll().catch((err: unknown) => {
      const msg = err instanceof Error ? err.message : String(err);
      addToast(msg);
    });
  }, [loadAll, addToast]);

  const startCreate = (): void => {
    setError("");
    setEdit(blankEdit());
  };

  const startEdit = (profile: CredentialProfile): void => {
    setError("");
    setEdit({
      mode: "edit",
      id: profile.id,
      name: profile.name,
      authType: profile.auth_type,
      username: profile.username,
      keyPath: profile.key_path,
      secret: "",
    });
    // Prefill the secret for password profiles so the user can see/edit it.
    if (profile.auth_type === "password") {
      getSecret(profile.id)
        .then((secret) => {
          setEdit((prev) => (prev?.id === profile.id ? { ...prev, secret } : prev));
        })
        .catch(noop);
    }
  };

  const handleSave = (e: React.SyntheticEvent): void => {
    e.preventDefault();
    if (!edit) return;
    if (!edit.name.trim()) {
      setError(t("credentialProfiles.nameRequired"));
      return;
    }

    const doSave = async (): Promise<void> => {
      if (edit.mode === "create") {
        await createProfile({
          name: edit.name.trim(),
          authType: edit.authType,
          username: edit.username.trim(),
          keyPath: edit.keyPath,
          secret: edit.secret,
        });
      } else if (edit.id) {
        await updateProfile(edit.id, {
          name: edit.name.trim(),
          authType: edit.authType,
          username: edit.username.trim(),
          keyPath: edit.keyPath,
          // Only send a secret if the user typed one — an empty string means
          // "leave the existing keychain entry alone".
          secret: edit.secret !== "" ? edit.secret : undefined,
        });
      }
      setEdit(null);
    };

    doSave().catch((err: unknown) => {
      setError(err instanceof Error ? err.message : String(err));
    });
  };

  const handleDelete = (profile: CredentialProfile): void => {
    deleteProfile(profile.id)
      .then(() => {
        setConfirmDelete(null);
      })
      .catch((err: unknown) => {
        addToast(err instanceof Error ? err.message : String(err));
        setConfirmDelete(null);
      });
  };

  const needsKeyPath = edit?.authType === "key";
  const needsPassword = edit?.authType === "password";

  return (
    <div className="dialog-overlay" onClick={onClose} role="presentation">
      <div
        className="dialog dialog-wide"
        onClick={(e) => {
          e.stopPropagation();
        }}
        role="dialog"
        aria-modal="true"
        aria-labelledby="cm-title"
      >
        <h3 className="dialog-title" id="cm-title">
          {t("credentialProfiles.title")}
        </h3>
        <p className="dialog-info">{t("credentialProfiles.info")}</p>

        {!edit && (
          <>
            <div className="credential-profile-list">
              {profiles.length === 0 && (
                <p className="dialog-info">{t("credentialProfiles.noneYet")}</p>
              )}
              {profiles.map((p) => (
                <div key={p.id} className="credential-profile-row">
                  <div className="credential-profile-meta">
                    <strong>{p.name}</strong>
                    <span className="credential-profile-divider">|</span>
                    <span className="credential-profile-sub">
                      {p.username ? `${p.username} · ` : ""}
                      {p.auth_type}
                    </span>
                  </div>
                  <div className="credential-profile-actions">
                    <button
                      type="button"
                      className="dialog-btn"
                      onClick={() => {
                        startEdit(p);
                      }}
                    >
                      {t("common.edit")}
                    </button>
                    <button
                      type="button"
                      className="dialog-btn dialog-btn-cancel"
                      onClick={() => {
                        setConfirmDelete(p);
                      }}
                    >
                      {t("common.delete")}
                    </button>
                  </div>
                </div>
              ))}
            </div>
            <div className="dialog-actions">
              <button type="button" className="dialog-btn dialog-btn-cancel" onClick={onClose}>
                {t("settings.close")}
              </button>
              <button type="button" className="dialog-btn dialog-btn-primary" onClick={startCreate}>
                {t("credentialProfiles.newProfile")}
              </button>
            </div>
          </>
        )}

        {edit && (
          <form onSubmit={handleSave}>
            <div className="dialog-field">
              <label htmlFor="cm-name">{t("credentialProfiles.nameLabel")}</label>
              <input
                id="cm-name"
                type="text"
                value={edit.name}
                onChange={(e) => {
                  setEdit({ ...edit, name: e.target.value });
                }}
                placeholder={t("credentialProfiles.namePlaceholder")}
                autoFocus
              />
            </div>
            <div className="dialog-field">
              <label htmlFor="cm-authtype">{t("credentialProfiles.authTypeLabel")}</label>
              <CustomSelect
                id="cm-authtype"
                value={edit.authType}
                onChange={(v) => {
                  setEdit({ ...edit, authType: v });
                }}
                options={[
                  { value: "password", label: t("credentialProfiles.authPassword") },
                  { value: "key", label: t("credentialProfiles.authKey") },
                  {
                    value: "keyboard-interactive",
                    label: t("credentialProfiles.authKeyboardInteractive"),
                  },
                ]}
              />
            </div>
            <div className="dialog-field">
              <label htmlFor="cm-username">{t("credentialProfiles.usernameLabel")}</label>
              <input
                id="cm-username"
                type="text"
                value={edit.username}
                onChange={(e) => {
                  setEdit({ ...edit, username: e.target.value });
                }}
                placeholder={t("credentialProfiles.usernamePlaceholder")}
              />
            </div>
            {needsPassword && (
              <div className="dialog-field">
                <label htmlFor="cm-password">{t("credentialProfiles.passwordLabel")}</label>
                <input
                  id="cm-password"
                  type="password"
                  autoComplete="off"
                  value={edit.secret}
                  onChange={(e) => {
                    setEdit({ ...edit, secret: e.target.value });
                  }}
                  placeholder={t("credentialProfiles.passwordPlaceholder")}
                />
                {edit.mode === "edit" && (
                  <p className="dialog-info">{t("credentialProfiles.passwordHint")}</p>
                )}
              </div>
            )}
            {needsKeyPath && (
              <div className="dialog-field">
                <label htmlFor="cm-keypath">{t("credentialProfiles.keyPathLabel")}</label>
                <div className="dialog-row">
                  <div className="dialog-field-grow">
                    <input
                      id="cm-keypath"
                      type="text"
                      value={edit.keyPath}
                      onChange={(e) => {
                        setEdit({ ...edit, keyPath: e.target.value });
                      }}
                      placeholder={t("credentialProfiles.keyPathPlaceholder")}
                    />
                  </div>
                  <button
                    type="button"
                    className="dialog-btn"
                    onClick={() => {
                      void (async () => {
                        const path = await open({
                          title: t("credentialProfiles.keyPathLabel"),
                          defaultPath: edit.keyPath || undefined,
                          multiple: false,
                          directory: false,
                        });
                        if (path) {
                          setEdit({ ...edit, keyPath: path });
                        }
                      })();
                    }}
                  >
                    {t("credentialProfiles.keyPathBrowse")}
                  </button>
                </div>
              </div>
            )}
            {error && <p className="dialog-error">{error}</p>}
            <div className="dialog-actions">
              <button
                type="button"
                className="dialog-btn dialog-btn-cancel"
                onClick={() => {
                  setEdit(null);
                }}
              >
                {t("dialog.cancel")}
              </button>
              <button type="submit" className="dialog-btn dialog-btn-primary">
                {t("dialog.save")}
              </button>
            </div>
          </form>
        )}

        {confirmDelete && (
          <ConfirmDialog
            message={t("credentialProfiles.deleteConfirm", { name: confirmDelete.name })}
            onConfirm={() => {
              handleDelete(confirmDelete);
            }}
            onCancel={() => {
              setConfirmDelete(null);
            }}
          />
        )}
      </div>
    </div>
  );
}
