import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { open } from "@tauri-apps/plugin-dialog";
import { DialogKeys } from "./DialogKeys";
import {
  useCredentialProfilesStore,
  type CredentialProfile,
} from "../stores/credentialProfilesStore";
import { useToastLayer } from "../hooks/useToastLayer";
import { ConfirmDialog } from "./ConfirmDialog";
import { ToastLayer } from "./ToastContainer";
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
  const { profiles, updating, loadAll, createProfile, updateProfile, deleteProfile, getSecret } =
    useCredentialProfilesStore();
  const [edit, setEdit] = useState<EditState | null>(null);
  const [error, setError] = useState("");
  const [confirmDelete, setConfirmDelete] = useState<CredentialProfile | null>(null);
  // The save of the form that is open is under way. The ref says so at once:
  // a second submit, as from a double click, comes before the state is
  // rendered. Both are the open form's and go back when it is left: a save
  // that is still under way then is not the next form's, and does not hold
  // that form's own up.
  const [saving, setSaving] = useState(false);
  const savingRef = useRef(false);
  // What had the focus when a save was sent. A save disables the form's
  // fields and its Save button, and the one of them that had the focus loses
  // it. After a save that failed it is given back.
  const focusBefore = useRef<HTMLElement | null>(null);
  // A stored entry whose change is under way (`updating`, which the store
  // keeps) is not opened for another change, nor deleted, before that change
  // has its answer: of two saves of one entry the later could land first, and
  // the earlier then put back what it held, an old password included.
  // Which form is open, counted up when one is opened and when one is left,
  // by Cancel, by Escape or by its save. What a save, a file dialog or the
  // read of a stored password brings back is for the form it was asked from,
  // and for no other that is open by then.
  const formRef = useRef(0);
  // What this dialog reports while it is open is shown in a strip on top of it.
  const { layer, addToast } = useToastLayer();

  /** A form is opened or left: the one that is open from now on has no save under way. */
  const nextForm = useCallback(() => {
    formRef.current += 1;
    savingRef.current = false;
    setSaving(false);
    focusBefore.current = null;
  }, []);

  const leaveForm = useCallback(() => {
    nextForm();
    setEdit(null);
  }, [nextForm]);

  useEffect(() => {
    if (saving) return;
    const element = focusBefore.current;
    focusBefore.current = null;
    if (!element?.isConnected) return;
    // Not into a dialog that another one lies over, which takes no focus.
    if (element.closest("[inert]")) return;
    // Not away from where the user has gone meanwhile.
    const active = document.activeElement;
    if (active === null || active === document.body || active === element) element.focus();
  }, [saving]);

  /** An error for a form: in it while it is open, with the dialog once it is left. */
  const reportFor = (form: number, err: unknown): void => {
    const message = err instanceof Error ? err.message : String(err);
    if (formRef.current === form) setError(message);
    else addToast(message);
  };

  // ESC closes the inline edit form if open, otherwise closes the whole
  // manager. ConfirmDialog handles its own ESC via the shared stack.
  const handleEscape = useCallback(() => {
    if (confirmDelete !== null) return;
    if (edit !== null) {
      leaveForm();
      return;
    }
    onClose();
  }, [edit, confirmDelete, onClose, leaveForm]);

  useEffect(() => {
    loadAll().catch((err: unknown) => {
      const msg = err instanceof Error ? err.message : String(err);
      addToast(msg);
    });
  }, [loadAll, addToast]);

  const startCreate = (): void => {
    nextForm();
    setError("");
    setEdit(blankEdit());
  };

  const startEdit = (profile: CredentialProfile): void => {
    nextForm();
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
    // Prefill the secret for password profiles so the user can see/edit it:
    // in the form it was read for, and not over a password that was typed
    // before it came, nor while the profile is switched to another type.
    if (profile.auth_type === "password") {
      const form = formRef.current;
      getSecret(profile.id)
        .then((secret) => {
          if (formRef.current !== form) return;
          setEdit((prev) =>
            prev?.id === profile.id && prev.authType === "password" && prev.secret === ""
              ? { ...prev, secret }
              : prev,
          );
        })
        .catch(noop);
    }
  };

  const handleSave = (e: React.SyntheticEvent): void => {
    e.preventDefault();
    if (!edit || savingRef.current) return;
    if (!edit.name.trim()) {
      setError(t("credentialProfiles.nameRequired"));
      return;
    }

    const form = formRef.current;
    // A password is a password profile's. The one a form still holds after
    // its profile was switched to another type, read or typed, is not sent
    // along with that profile.
    const secret = edit.authType === "password" ? edit.secret : "";
    const doSave = async (): Promise<void> => {
      if (edit.mode === "create") {
        await createProfile({
          name: edit.name.trim(),
          authType: edit.authType,
          username: edit.username.trim(),
          keyPath: edit.keyPath,
          secret,
        });
      } else if (edit.id) {
        await updateProfile(edit.id, {
          name: edit.name.trim(),
          authType: edit.authType,
          username: edit.username.trim(),
          keyPath: edit.keyPath,
          // Only send a secret if the user typed one — an empty string means
          // "leave the existing keychain entry alone".
          secret: secret !== "" ? secret : undefined,
        });
      }
      // The form that was saved closes. One that was opened since stays.
      if (formRef.current === form) leaveForm();
    };

    const active = document.activeElement;
    focusBefore.current =
      active instanceof HTMLElement && e.currentTarget.contains(active) ? active : null;
    setError("");
    savingRef.current = true;
    setSaving(true);
    doSave()
      .catch((err: unknown) => {
        reportFor(form, err);
      })
      .finally(() => {
        // The form's own, if it is still the one that is open: one that was
        // left has given them back, and they may be the next form's by now.
        if (formRef.current !== form) return;
        savingRef.current = false;
        setSaving(false);
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
    <div className="dialog-overlay dialog-overlay-messages" role="presentation">
      <DialogKeys onEscape={handleEscape} />
      <ToastLayer layer={layer} />
      <div
        className="dialog dialog-wide"
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
                      disabled={updating.includes(p.id)}
                      onClick={() => {
                        startEdit(p);
                      }}
                    >
                      {t("common.edit")}
                    </button>
                    <button
                      type="button"
                      className="dialog-btn dialog-btn-cancel"
                      disabled={updating.includes(p.id)}
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
            {/* While its save is under way a form takes nothing: what was
                typed then would be no part of the save, and would go with
                the form when it closes. Cancel is left, to leave by. */}
            <fieldset className="dialog-fieldset" disabled={saving}>
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
                {/* Its list is outside the fields that the form disables, so it
                    is told itself, and closes a list that is open. */}
                <CustomSelect
                  id="cm-authtype"
                  disabled={saving}
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
                        const form = formRef.current;
                        open({
                          title: t("credentialProfiles.keyPathLabel"),
                          defaultPath: edit.keyPath || undefined,
                          multiple: false,
                          directory: false,
                        })
                          .then((path) => {
                            // Only into the form it was asked from, if that is
                            // still open, and beside what was typed since. A
                            // failure of the dialog that was shown goes with it.
                            if (!path || formRef.current !== form) return;
                            setError("");
                            setEdit((prev) => (prev ? { ...prev, keyPath: path } : prev));
                          })
                          .catch((err: unknown) => {
                            reportFor(form, err);
                          });
                      }}
                    >
                      {t("credentialProfiles.keyPathBrowse")}
                    </button>
                  </div>
                </div>
              )}
            </fieldset>
            {error && <p className="dialog-error">{error}</p>}
            <div className="dialog-actions">
              <button type="button" className="dialog-btn dialog-btn-cancel" onClick={leaveForm}>
                {t("dialog.cancel")}
              </button>
              <button type="submit" className="dialog-btn dialog-btn-primary" disabled={saving}>
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
