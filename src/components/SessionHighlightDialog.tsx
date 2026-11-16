import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { DialogKeys } from "./DialogKeys";
import { useHighlightStore } from "../stores/highlightStore";
import { CustomSelect } from "./CustomSelect";
import { shownAsWritten } from "../displayText";
import type { Session } from "../stores/sessionStore";

/** A session name is shown up to this many characters, as in **My Login**. */
const MAX_NAME_CHARS = 64;

interface SessionHighlightDialogProps {
  /** Another user's shared session. */
  session: Session;
  onSubmit: (profileId: string | null) => void;
  onCancel: () => void;
}

/**
 * Which of the user's own highlight profiles another user's shared session
 * is shown with (PostgreSQL mode).
 *
 * Such a session is not the user's to edit, but how its output looks in
 * their own window is theirs: highlight profiles are each user's own, and so
 * is the choice of one for a session. Nothing here is sent anywhere, so
 * unlike **My Login** this confirms nothing about the session. Its name is
 * still another user's text, and is shown written out.
 */
export function SessionHighlightDialog({
  session,
  onSubmit,
  onCancel,
}: SessionHighlightDialogProps): React.JSX.Element {
  const { t } = useTranslation();
  const profiles = useHighlightStore((s) => s.profiles);
  // What the user picked here, if anything yet. Until then the dialog shows
  // what is chosen, going by the profiles as they are now and not as they
  // were when it opened: a save must not clear a choice only because the
  // profiles had not been loaded yet. A profile that is gone shows as none,
  // which is what the terminal does.
  const [picked, setPicked] = useState<string | null>(null);
  const chosen = session.highlight_profile_id ?? "";
  const profileId = picked ?? (profiles.some((p) => p.id === chosen) ? chosen : "");

  const handleSubmit = (e: React.SyntheticEvent): void => {
    e.preventDefault();
    onSubmit(profileId || null);
  };

  return (
    <div className="dialog-overlay" role="presentation">
      <DialogKeys onEscape={onCancel} />
      <div
        className="dialog dialog-wide"
        role="dialog"
        aria-modal="true"
        aria-labelledby="shd-title"
      >
        <h3 className="dialog-title dialog-quoted-text" id="shd-title">
          {t("session.myHighlightingTitle", {
            name: shownAsWritten(session.name, MAX_NAME_CHARS),
          })}
        </h3>
        <p className="dialog-info">{t("session.myHighlightingInfo")}</p>
        <form onSubmit={handleSubmit}>
          <div className="dialog-field">
            <label htmlFor="shd-profile">{t("session.highlightProfileLabel")}</label>
            <CustomSelect
              id="shd-profile"
              value={profileId}
              onChange={setPicked}
              options={[
                { value: "", label: t("session.highlightProfileNone") },
                ...profiles.map((p) => ({ value: p.id, label: p.name })),
              ]}
            />
          </div>
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
