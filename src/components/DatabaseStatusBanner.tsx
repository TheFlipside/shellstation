import React from "react";
import { useTranslation } from "react-i18next";

interface DatabaseStatusBannerProps {
  error: string;
  onOpenSettings: () => void;
}

export function DatabaseStatusBanner({
  error,
  onOpenSettings,
}: DatabaseStatusBannerProps): React.JSX.Element {
  const { t } = useTranslation();

  return (
    <div className="db-status-banner">
      <span>{t("settings.dbFallbackBanner", { message: error })}</span>
      <button type="button" onClick={onOpenSettings}>
        {t("settings.dbOpenSettings")}
      </button>
    </div>
  );
}
