import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { DialogKeys } from "./DialogKeys";
import {
  useLoginSequenceStore,
  type LoginSequence,
  type LoginSequenceStep,
} from "../stores/loginSequenceStore";
import { useToastLayer } from "../hooks/useToastLayer";
import { ConfirmDialog } from "./ConfirmDialog";
import { ToastLayer } from "./ToastContainer";

interface LoginSequenceManagerProps {
  onClose: () => void;
}

interface EditState {
  mode: "create" | "edit";
  id?: string;
  name: string;
  sendInitialCr: boolean;
  steps: LoginSequenceStep[];
}

function blankStep(): LoginSequenceStep {
  return { pattern: "", response: "", append_cr: true };
}

function blankEdit(): EditState {
  return {
    mode: "create",
    name: "",
    sendInitialCr: true,
    steps: [blankStep()],
  };
}

export function LoginSequenceManager({ onClose }: LoginSequenceManagerProps): React.JSX.Element {
  const { t } = useTranslation();
  const { sequences, updating, loadAll, createSequence, updateSequence, deleteSequence } =
    useLoginSequenceStore();
  const [edit, setEdit] = useState<EditState | null>(null);
  const [error, setError] = useState("");
  const [confirmDelete, setConfirmDelete] = useState<LoginSequence | null>(null);
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
  // A stored entry whose change or deletion is under way (`updating`, which
  // the store keeps) is not opened for another change, nor deleted, before
  // that has its answer: of two saves of one entry the later could land
  // first, and the earlier then put back what it held. Its row says that it
  // is busy, to a screen reader and by its look.
  // Which form is open, counted up when one is opened and when one is left,
  // by Cancel, by Escape or by its save. What a save brings back is for the
  // form it was sent from, and for no other that is open by then.
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

  const startEdit = (seq: LoginSequence): void => {
    nextForm();
    setError("");
    setEdit({
      mode: "edit",
      id: seq.id,
      name: seq.name,
      sendInitialCr: seq.send_initial_cr,
      steps: seq.steps.length > 0 ? [...seq.steps] : [blankStep()],
    });
  };

  const handleSave = (e: React.SyntheticEvent): void => {
    e.preventDefault();
    if (!edit || savingRef.current) return;
    if (!edit.name.trim()) {
      setError(t("loginSequences.nameRequired"));
      return;
    }
    const validSteps = edit.steps.filter((s) => s.pattern.trim() !== "");
    if (validSteps.length === 0) {
      setError(t("loginSequences.needOneStep"));
      return;
    }

    const form = formRef.current;
    const doSave = async (): Promise<void> => {
      if (edit.mode === "create") {
        await createSequence({
          name: edit.name.trim(),
          sendInitialCr: edit.sendInitialCr,
          steps: validSteps,
        });
      } else if (edit.id) {
        await updateSequence(edit.id, {
          name: edit.name.trim(),
          sendInitialCr: edit.sendInitialCr,
          steps: validSteps,
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

  const handleDelete = (seq: LoginSequence): void => {
    // Once: a second confirmation can come before the first is rendered.
    if (useLoginSequenceStore.getState().updating.includes(seq.id)) return;
    deleteSequence(seq.id)
      .then(() => {
        setConfirmDelete(null);
      })
      .catch((err: unknown) => {
        addToast(err instanceof Error ? err.message : String(err));
        setConfirmDelete(null);
      });
  };

  const updateStep = (idx: number, patch: Partial<LoginSequenceStep>): void => {
    if (!edit) return;
    setEdit({
      ...edit,
      steps: edit.steps.map((s, i) => (i === idx ? { ...s, ...patch } : s)),
    });
  };

  const removeStep = (idx: number): void => {
    if (!edit || edit.steps.length <= 1) return;
    setEdit({ ...edit, steps: edit.steps.filter((_, i) => i !== idx) });
  };

  const moveStep = (idx: number, direction: -1 | 1): void => {
    if (!edit) return;
    const target = idx + direction;
    if (target < 0 || target >= edit.steps.length) return;
    const next = [...edit.steps];
    [next[idx], next[target]] = [next[target], next[idx]];
    setEdit({ ...edit, steps: next });
  };

  const addStep = (): void => {
    if (!edit) return;
    setEdit({ ...edit, steps: [...edit.steps, blankStep()] });
  };

  return (
    <div className="dialog-overlay dialog-overlay-messages" role="presentation">
      <DialogKeys onEscape={handleEscape} />
      <ToastLayer layer={layer} />
      <div
        className="dialog dialog-wide"
        role="dialog"
        aria-modal="true"
        aria-labelledby="lsm-title"
      >
        <h3 className="dialog-title" id="lsm-title">
          {t("loginSequences.title")}
        </h3>
        <p className="dialog-field-note">{t("loginSequences.info")}</p>

        {!edit && (
          <>
            <div className="credential-profile-list">
              {sequences.length === 0 && (
                <p className="dialog-info">{t("loginSequences.noneYet")}</p>
              )}
              {sequences.map((seq) => (
                <div
                  key={seq.id}
                  className={
                    updating.includes(seq.id)
                      ? "credential-profile-row credential-profile-row-busy"
                      : "credential-profile-row"
                  }
                  aria-busy={updating.includes(seq.id)}
                >
                  <div className="credential-profile-meta">
                    <strong>{seq.name}</strong>
                    <span className="credential-profile-divider">|</span>
                    <span className="credential-profile-sub">
                      {t("loginSequences.stepCount", { count: seq.steps.length })}
                    </span>
                  </div>
                  <div className="credential-profile-actions">
                    <button
                      type="button"
                      className="dialog-btn"
                      disabled={updating.includes(seq.id)}
                      onClick={() => {
                        startEdit(seq);
                      }}
                    >
                      {t("common.edit")}
                    </button>
                    <button
                      type="button"
                      className="dialog-btn dialog-btn-cancel"
                      disabled={updating.includes(seq.id)}
                      onClick={() => {
                        setConfirmDelete(seq);
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
                {t("loginSequences.newSequence")}
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
                <label htmlFor="lsm-name">{t("loginSequences.nameLabel")}</label>
                <input
                  id="lsm-name"
                  type="text"
                  value={edit.name}
                  onChange={(e) => {
                    setEdit({ ...edit, name: e.target.value });
                  }}
                  placeholder={t("loginSequences.namePlaceholder")}
                  autoFocus
                />
              </div>

              <div className="dialog-field">
                <label
                  className="dialog-checkbox-label"
                  title={t("loginSequences.sendInitialCrHint")}
                >
                  <input
                    type="checkbox"
                    checked={edit.sendInitialCr}
                    onChange={(e) => {
                      setEdit({ ...edit, sendInitialCr: e.target.checked });
                    }}
                  />
                  {t("loginSequences.sendInitialCr")}
                </label>
              </div>

              <div className="dialog-field" role="group" aria-labelledby="lsm-steps-label">
                <span className="dialog-field-heading" id="lsm-steps-label">
                  {t("loginSequences.stepsLabel")}
                </span>
                {edit.steps.map((step, idx) => (
                  <div key={idx} className="login-sequence-step">
                    <div className="login-sequence-step-header">
                      <span className="login-sequence-step-number">
                        {t("loginSequences.stepNumber", { n: idx + 1 })}
                      </span>
                      <div className="login-sequence-step-actions">
                        <button
                          type="button"
                          className="dialog-btn dialog-btn-small"
                          disabled={idx === 0}
                          onClick={() => {
                            moveStep(idx, -1);
                          }}
                          title={t("loginSequences.moveUp")}
                        >
                          &#x25B2;
                        </button>
                        <button
                          type="button"
                          className="dialog-btn dialog-btn-small"
                          disabled={idx === edit.steps.length - 1}
                          onClick={() => {
                            moveStep(idx, 1);
                          }}
                          title={t("loginSequences.moveDown")}
                        >
                          &#x25BC;
                        </button>
                        <button
                          type="button"
                          className="dialog-btn dialog-btn-small dialog-btn-cancel"
                          disabled={edit.steps.length <= 1}
                          onClick={() => {
                            removeStep(idx);
                          }}
                          title={t("common.delete")}
                        >
                          &#x2715;
                        </button>
                      </div>
                    </div>
                    <div className="dialog-field">
                      <label htmlFor={`lsm-pattern-${String(idx)}`}>
                        {t("loginSequences.patternLabel")}
                      </label>
                      <input
                        id={`lsm-pattern-${String(idx)}`}
                        type="text"
                        value={step.pattern}
                        onChange={(e) => {
                          updateStep(idx, { pattern: e.target.value });
                        }}
                        placeholder={t("loginSequences.patternPlaceholder")}
                        className="monospace-input"
                      />
                    </div>
                    <div className="dialog-field">
                      <label htmlFor={`lsm-response-${String(idx)}`}>
                        {t("loginSequences.responseLabel")}
                      </label>
                      <input
                        id={`lsm-response-${String(idx)}`}
                        type="text"
                        value={step.response}
                        onChange={(e) => {
                          updateStep(idx, { response: e.target.value });
                        }}
                        placeholder={t("loginSequences.responsePlaceholder")}
                        className="monospace-input"
                      />
                    </div>
                    <label className="dialog-checkbox-label">
                      <input
                        type="checkbox"
                        checked={step.append_cr}
                        onChange={(e) => {
                          updateStep(idx, { append_cr: e.target.checked });
                        }}
                      />
                      {t("loginSequences.appendCr")}
                    </label>
                  </div>
                ))}
                <button type="button" className="dialog-btn" onClick={addStep}>
                  {t("loginSequences.addStep")}
                </button>
              </div>
            </fieldset>

            <div className="escape-help">
              <p className="dialog-field-note">{t("loginSequences.escapeTitle")}</p>
              <div className="escape-help-grid">
                <span className="escape-code">\s</span>
                <span>{t("loginSequences.esc_s")}</span>
                <span className="escape-code">\w</span>
                <span>{t("loginSequences.esc_w")}</span>
                <span className="escape-code">\r</span>
                <span>{t("loginSequences.esc_r")}</span>
                <span className="escape-code">\n</span>
                <span>{t("loginSequences.esc_n")}</span>
                <span className="escape-code">\t</span>
                <span>{t("loginSequences.esc_t")}</span>
                <span className="escape-code">\b</span>
                <span>{t("loginSequences.esc_b")}</span>
                <span className="escape-code">\e</span>
                <span>{t("loginSequences.esc_e")}</span>
                <span className="escape-code">\\</span>
                <span>{t("loginSequences.esc_backslash")}</span>
                <span className="escape-code">\p</span>
                <span>{t("loginSequences.esc_p")}</span>
              </div>
            </div>

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
            message={t("loginSequences.deleteConfirm", { name: confirmDelete.name })}
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
