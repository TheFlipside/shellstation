import React, { useCallback, useLayoutEffect, useRef } from "react";
import { Trans, useTranslation } from "react-i18next";
import { DialogKeys } from "./DialogKeys";

/** What known_hosts has on record for the host (`HostKeyStatus` in Rust). */
export type HostKeyStatus = "new" | "changed" | "otherType" | "unchecked";

export interface KnownKey {
  keyType: string;
  fingerprint: string;
}

export interface HostVerifyRequest {
  sessionId: string;
  host: string;
  port: number;
  fingerprint: string;
  keyType: string;
  status: HostKeyStatus;
  /** The keys on record: of the offered type when changed, of other types when otherType. */
  knownKeys: KnownKey[];
}

interface HostVerifyDialogProps {
  request: HostVerifyRequest;
  onRespond: (sessionId: string, accept: boolean) => void;
}

/** Title, explanation and closing question for each status but "new". */
const WARNINGS = {
  changed: {
    title: "hostVerify.titleChanged",
    text: "hostVerify.changed",
    confirm: "hostVerify.confirmChanged",
  },
  otherType: {
    title: "hostVerify.titleOtherType",
    text: "hostVerify.otherType",
    confirm: "hostVerify.confirmOtherType",
  },
  unchecked: {
    title: "hostVerify.titleUnchecked",
    text: "hostVerify.unchecked",
    confirm: "hostVerify.confirmUnchecked",
  },
} as const;

export function HostVerifyDialog({ request, onRespond }: HostVerifyDialogProps): React.JSX.Element {
  const { t } = useTranslation();
  const dialogRef = useRef<HTMLDivElement>(null);
  const handleReject = useCallback(() => {
    onRespond(request.sessionId, false);
  }, [onRespond, request.sessionId]);
  const handleAccept = useCallback(() => {
    onRespond(request.sessionId, true);
  }, [onRespond, request.sessionId]);
  const warning = request.status === "new" ? null : WARNINGS[request.status];
  // DialogKeys takes Enter before a focused button sees it, so a focused
  // button in this dialog gets the click. Otherwise Enter accepts only a
  // first connection's key: any other takes a click on Accept, so that a
  // keystroke meant for something else cannot trust it.
  const handleEnter = useCallback(() => {
    const focused = document.activeElement;
    if (focused instanceof HTMLButtonElement && dialogRef.current?.contains(focused)) {
      focused.click();
    } else if (warning === null) {
      handleAccept();
    }
  }, [handleAccept, warning]);
  // A button focused for the request before must not stay focused for this
  // one, where Enter would click it: say Accept, answered for a new host,
  // then Accept Anyway for a changed key in its place.
  useLayoutEffect(() => {
    const focused = document.activeElement;
    if (focused instanceof HTMLElement && dialogRef.current?.contains(focused)) {
      focused.blur();
    }
  }, [request.sessionId]);

  return (
    <div className="dialog-overlay" role="presentation">
      <DialogKeys onEscape={handleReject} onEnter={handleEnter} />
      <div
        ref={dialogRef}
        className="dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="hv-title"
        aria-describedby="hv-text"
      >
        <h3 className={warning ? "dialog-title dialog-warning" : "dialog-title"} id="hv-title">
          {t(warning ? warning.title : "hostVerify.title")}
        </h3>
        <p className="dialog-text" id="hv-text">
          {/* Trans puts the values into the translation before it reads
              the <1> markup, and the app turns escaping off: escaped here
              and unescaped in the text, a host name cannot add markup. */}
          <Trans
            i18nKey={warning ? warning.text : "hostVerify.authenticity"}
            values={{
              host: request.host,
              port: request.port,
              keyType: request.keyType,
              knownTypes: [...new Set(request.knownKeys.map((key) => key.keyType))].join(", "),
            }}
            components={{ 1: <strong /> }}
            tOptions={{ interpolation: { escapeValue: true } }}
            shouldUnescape
          />
        </p>
        {request.knownKeys.map((key) => (
          <React.Fragment key={`${key.keyType} ${key.fingerprint}`}>
            <p className="dialog-text">{t("hostVerify.recordedKey", { keyType: key.keyType })}</p>
            <code className="dialog-fingerprint dialog-fingerprint-recorded">
              {key.fingerprint}
            </code>
          </React.Fragment>
        ))}
        <p className="dialog-text">
          {t(warning ? "hostVerify.offeredKey" : "hostVerify.fingerprint", {
            keyType: request.keyType,
          })}
        </p>
        <code
          className={
            warning ? "dialog-fingerprint dialog-fingerprint-suspect" : "dialog-fingerprint"
          }
        >
          {request.fingerprint}
        </code>
        <p className="dialog-text">{t(warning ? warning.confirm : "hostVerify.confirmConnect")}</p>
        <div className="dialog-actions">
          <button type="button" className="dialog-btn dialog-btn-cancel" onClick={handleReject}>
            {t("hostVerify.reject")}
          </button>
          <button
            type="button"
            className={warning ? "dialog-btn dialog-btn-danger" : "dialog-btn dialog-btn-primary"}
            onClick={handleAccept}
          >
            {t(warning ? "hostVerify.acceptAnyway" : "hostVerify.accept")}
          </button>
        </div>
      </div>
    </div>
  );
}
