import React from "react";
import { Trans, useTranslation } from "react-i18next";

export interface HostVerifyRequest {
  sessionId: string;
  host: string;
  port: number;
  fingerprint: string;
  keyType: string;
}

interface HostVerifyDialogProps {
  request: HostVerifyRequest;
  onRespond: (sessionId: string, accept: boolean) => void;
}

export function HostVerifyDialog({ request, onRespond }: HostVerifyDialogProps): React.JSX.Element {
  const { t } = useTranslation();

  return (
    <div className="dialog-overlay" role="presentation">
      <div className="dialog" role="dialog" aria-modal="true" aria-labelledby="hv-title">
        <h3 className="dialog-title" id="hv-title">
          {t("hostVerify.title")}
        </h3>
        <p className="dialog-text">
          <Trans
            i18nKey="hostVerify.authenticity"
            values={{ host: request.host, port: request.port }}
            components={{ 1: <strong /> }}
          />
        </p>
        <p className="dialog-text">{t("hostVerify.fingerprint", { keyType: request.keyType })}</p>
        <code className="dialog-fingerprint">{request.fingerprint}</code>
        <p className="dialog-text">{t("hostVerify.confirmConnect")}</p>
        <div className="dialog-actions">
          <button
            type="button"
            className="dialog-btn dialog-btn-cancel"
            onClick={() => {
              onRespond(request.sessionId, false);
            }}
          >
            {t("hostVerify.reject")}
          </button>
          <button
            type="button"
            className="dialog-btn dialog-btn-primary"
            onClick={() => {
              onRespond(request.sessionId, true);
            }}
          >
            {t("hostVerify.accept")}
          </button>
        </div>
      </div>
    </div>
  );
}
