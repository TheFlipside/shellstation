import React from "react";
import { useTranslation } from "react-i18next";
import { useEscapeKey } from "../hooks/useEscapeKey";
import { useSettingsStore } from "../stores/settingsStore";

const AVAILABLE_LANGUAGES = [
  { code: "en", label: "English" },
  { code: "de", label: "Deutsch" },
];

interface SettingsDialogProps {
  onClose: () => void;
}

export function SettingsDialog({ onClose }: SettingsDialogProps): React.JSX.Element {
  const { t, i18n } = useTranslation();
  const {
    language,
    setLanguage,
    closeOnDisconnect,
    setCloseOnDisconnect,
    openLocalOnStartup,
    setOpenLocalOnStartup,
    confirmOnQuit,
    setConfirmOnQuit,
  } = useSettingsStore();

  const currentLang = language !== "" ? language : (i18n.resolvedLanguage ?? "en");

  useEscapeKey(onClose);

  return (
    <div className="dialog-overlay" onClick={onClose} role="presentation">
      <div
        className="dialog dialog-wide"
        onClick={(e) => {
          e.stopPropagation();
        }}
        role="dialog"
        aria-modal="true"
        aria-labelledby="settings-title"
      >
        <h3 className="dialog-title" id="settings-title">
          {t("settings.title")}
        </h3>
        <h4 className="settings-section-title">{t("settings.general")}</h4>
        <div className="dialog-field">
          <label htmlFor="settings-language">{t("settings.languageLabel")}</label>
          <select
            id="settings-language"
            value={currentLang}
            onChange={(e) => {
              setLanguage(e.target.value);
            }}
          >
            {AVAILABLE_LANGUAGES.map((lang) => (
              <option key={lang.code} value={lang.code}>
                {lang.label}
              </option>
            ))}
          </select>
        </div>
        <h4 className="settings-section-title">{t("settings.sessions")}</h4>
        <div className="dialog-field dialog-field-row">
          <input
            type="checkbox"
            id="settings-close-on-disconnect"
            checked={closeOnDisconnect}
            onChange={(e) => {
              setCloseOnDisconnect(e.target.checked);
            }}
          />
          <label htmlFor="settings-close-on-disconnect">
            {t("settings.closeOnDisconnectLabel")}
          </label>
          <span className="settings-help" title={t("settings.closeOnDisconnectHint")}>
            ?
          </span>
        </div>
        <div className="dialog-field dialog-field-row">
          <input
            type="checkbox"
            id="settings-open-local-on-startup"
            checked={openLocalOnStartup}
            onChange={(e) => {
              setOpenLocalOnStartup(e.target.checked);
            }}
          />
          <label htmlFor="settings-open-local-on-startup">
            {t("settings.openLocalOnStartupLabel")}
          </label>
          <span className="settings-help" title={t("settings.openLocalOnStartupHint")}>
            ?
          </span>
        </div>
        <div className="dialog-field dialog-field-row">
          <input
            type="checkbox"
            id="settings-confirm-on-quit"
            checked={confirmOnQuit}
            onChange={(e) => {
              setConfirmOnQuit(e.target.checked);
            }}
          />
          <label htmlFor="settings-confirm-on-quit">{t("settings.confirmOnQuitLabel")}</label>
          <span className="settings-help" title={t("settings.confirmOnQuitHint")}>
            ?
          </span>
        </div>
        <div className="dialog-actions">
          <button type="button" className="dialog-btn dialog-btn-primary" onClick={onClose}>
            {t("settings.close")}
          </button>
        </div>
      </div>
    </div>
  );
}
