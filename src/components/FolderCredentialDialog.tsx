import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { useEscapeKey } from "../hooks/useEscapeKey";
import { useCredentialProfilesStore } from "../stores/credentialProfilesStore";
import { CustomSelect } from "./CustomSelect";

interface FolderCredentialDialogProps {
  folderName: string;
  onSubmit: (profileId: string | null) => void;
  onCancel: () => void;
  onManageCredentials: () => void;
}

export function FolderCredentialDialog({
  folderName,
  onSubmit,
  onCancel,
  onManageCredentials,
}: FolderCredentialDialogProps): React.JSX.Element {
  const { t } = useTranslation();
  useEscapeKey(onCancel);
  const [profileId, setProfileId] = useState("");
  const credentialProfiles = useCredentialProfilesStore((s) => s.profiles);

  const handleSubmit = (e: React.SyntheticEvent): void => {
    e.preventDefault();
    onSubmit(profileId || null);
  };

  return (
    <div className="dialog-overlay" role="presentation">
      <div
        className="dialog dialog-wide"
        role="dialog"
        aria-modal="true"
        aria-labelledby="fcd-title"
      >
        <h3 className="dialog-title" id="fcd-title">
          {t("folderCredential.title", { name: folderName })}
        </h3>
        <p className="dialog-info">{t("folderCredential.info")}</p>
        <form onSubmit={handleSubmit}>
          <div className="dialog-field">
            <label htmlFor="fcd-profile">{t("folderCredential.profileLabel")}</label>
            <div className="dialog-row">
              <div className="dialog-field-grow">
                <CustomSelect
                  id="fcd-profile"
                  value={profileId}
                  onChange={setProfileId}
                  options={[
                    { value: "", label: t("folderCredential.profileNone") },
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
