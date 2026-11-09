import React, { useEffect, useRef } from "react";
import ReactDOM from "react-dom";
import { useTranslation } from "react-i18next";

/**
 * Shown while the app closes its sessions before it quits: the backend tells
 * every SSH server that the session is ending and waits for it, which can
 * take a few seconds. It has no buttons, since the quit cannot be called
 * off, and covers everything else, toasts included.
 */
export function QuittingOverlay(): React.JSX.Element {
  const { t } = useTranslation();
  const dialogRef = useRef<HTMLDivElement>(null);

  // Take the focus from the terminal, whose session is closing, and keep
  // every key from the app while this shows: a key caught at the window
  // before anything else sees it reaches no terminal and no shortcut.
  useEffect(() => {
    dialogRef.current?.focus();
    const swallow = (e: KeyboardEvent): void => {
      e.preventDefault();
      e.stopImmediatePropagation();
    };
    window.addEventListener("keydown", swallow, true);
    window.addEventListener("keyup", swallow, true);
    window.addEventListener("keypress", swallow, true);
    return () => {
      window.removeEventListener("keydown", swallow, true);
      window.removeEventListener("keyup", swallow, true);
      window.removeEventListener("keypress", swallow, true);
    };
  }, []);

  return ReactDOM.createPortal(
    <div className="dialog-overlay dialog-overlay-quitting" role="presentation">
      <div
        ref={dialogRef}
        className="dialog"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="quitting-title"
        aria-describedby="quitting-text"
        tabIndex={-1}
      >
        <h3 id="quitting-title" className="dialog-title">
          {t("settings.quittingTitle")}
        </h3>
        <p id="quitting-text" className="dialog-text">
          {t("settings.quittingMessage")}
        </p>
        <div
          className="dialog-update-progress dialog-progress-indeterminate"
          role="progressbar"
          aria-labelledby="quitting-title"
        >
          <div className="dialog-update-progress-bar" />
        </div>
      </div>
    </div>,
    document.body,
  );
}
