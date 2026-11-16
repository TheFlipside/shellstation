import React, { useEffect, useState } from "react";
import ReactDOM from "react-dom";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { DialogKeys } from "./DialogKeys";
import { closeSessionsForUpdate, updateInstallFailed } from "../page";
import { useSettingsStore } from "../stores/settingsStore";
import { useToastStore } from "../stores/toastStore";

type Phase = "idle" | "checking" | "available" | "downloading" | "ready" | "error";

interface PendingUpdate {
  update: Update;
  current: string;
  notes: string;
}

/**
 * Owns the entire app-update flow: auto-checks on mount (if the user
 * hasn't opted out), listens for manual checks from the native Help menu,
 * shows a dialog when an update is available, drives the download, has the
 * backend close every session, installs, and triggers a relaunch.
 *
 * Mounted once at the App root; no props.
 */
export function UpdateDialog(): React.JSX.Element | null {
  const { t } = useTranslation();
  const { autoCheckForUpdates } = useSettingsStore();
  const { addToast } = useToastStore();

  const [phase, setPhase] = useState<Phase>("idle");
  const [pending, setPending] = useState<PendingUpdate | null>(null);
  const [progress, setProgress] = useState<{ downloaded: number; total: number | null }>({
    downloaded: 0,
    total: null,
  });

  // Set from the click on "Install and restart" until the install has
  // failed. A ref, since it is read by handlers that outlive a render.
  const installing = React.useRef(false);

  // Manual check: invoked from the Help menu via a backend-emitted event,
  // and on mount if the user has auto-check enabled. `manual` controls the
  // up-to-date UX — silent for background checks, toast for menu-triggered.
  const runCheck = React.useCallback(
    async (manual: boolean): Promise<void> => {
      // Not while an update installs: the check would take the dialog away,
      // with the sessions closing behind it, and offer the update again.
      if (installing.current) return;
      setPhase("checking");
      try {
        const update = await check();
        if (update) {
          setPending({
            update,
            current: update.currentVersion,
            notes: update.body ?? "",
          });
          setPhase("available");
        } else {
          setPhase("idle");
          if (manual) {
            addToast(t("updates.upToDateToast"), "info");
          }
        }
      } catch (e) {
        // Stash the raw error in a ref/console for devs but never show
        // it in the dialog — plugin error messages can leak the endpoint
        // URL or hostnames. The user sees a generic "Update failed" string.
        // eslint-disable-next-line no-console -- diagnostic detail goes to devtools, not the UI
        console.warn("update check failed:", e);
        if (manual) {
          setPhase("error");
          addToast(t("updates.checkFailedToast"), "error");
        } else {
          // Background checks fail silently — offline, transient DNS, etc.
          setPhase("idle");
        }
      }
    },
    [addToast, t],
  );

  // Auto-check on mount when the setting is on. Run once per app launch.
  const autoCheckRef = React.useRef(false);
  useEffect(() => {
    if (autoCheckRef.current) return;
    autoCheckRef.current = true;
    if (autoCheckForUpdates) {
      void runCheck(false);
    }
  }, [autoCheckForUpdates, runCheck]);

  // Menu-driven manual check from the Rust side.
  useEffect(() => {
    const unlistenPromise = listen("updater:check-requested", () => {
      void runCheck(true);
    });
    return () => {
      void unlistenPromise.then((unlisten) => {
        unlisten();
      });
    };
  }, [runCheck]);

  const dismiss = (): void => {
    setPhase("idle");
    setPending(null);
    setProgress({ downloaded: 0, total: null });
  };

  const install = async (): Promise<void> => {
    if (!pending || installing.current) return;
    installing.current = true;
    const { update } = pending;
    setPhase("downloading");
    // Whether the backend was asked to close the sessions, after which this
    // page opens none until it is told that the install failed.
    let sessionsClosed = false;
    try {
      await update.download((event) => {
        if (event.event === "Started") {
          setProgress({ downloaded: 0, total: event.data.contentLength ?? null });
        } else if (event.event === "Progress") {
          setProgress((p) => ({
            downloaded: p.downloaded + event.data.chunkLength,
            total: p.total,
          }));
        }
      });
      setPhase("ready");
      // Installing ends the app with every session in it: on Windows the
      // plugin starts the installer and exits the process, and elsewhere
      // relaunch() restarts it. So the sessions are closed first, the way a
      // quit closes them, and only now: the download is complete and its
      // signature checked, so a download that fails costs no session.
      sessionsClosed = true;
      try {
        await closeSessionsForUpdate();
      } catch (e) {
        // Not a reason to stay on this version: install all the same, and
        // leave the sessions to the operating system, as before.
        // eslint-disable-next-line no-console -- diagnostic detail goes to devtools, not the UI
        console.warn("closing the sessions before the update failed:", e);
      }
      await update.install();
      // On macOS + Linux the plugin doesn't auto-relaunch; the NSIS passive
      // installer on Windows hands control back, then exits. relaunch()
      // works on all three.
      await relaunch();
    } catch (e) {
      // Same generic-error policy as the check path: detail to console only.
      // eslint-disable-next-line no-console -- diagnostic detail goes to devtools, not the UI
      console.warn("update install failed:", e);
      if (sessionsClosed) {
        // The app stays, so it has to be able to connect again, and by the
        // time the dialog says that the update failed. Should the backend
        // not hear of it, a reload of the window lets the page connect.
        try {
          await updateInstallFailed();
        } catch (failed) {
          // eslint-disable-next-line no-console -- diagnostic detail goes to devtools, not the UI
          console.warn("update_install_failed failed:", failed);
        }
      }
      // Give back the downloaded update, which install() keeps on failure.
      update.close().catch(() => undefined);
      installing.current = false;
      setPhase("error");
    }
  };

  // Nothing to show until a check finds an update. Escape is claimed by the
  // DialogKeys element below, which only exists in this branch — an idle
  // updater sitting at the app root cannot hold the key away from terminals.
  if (phase === "idle" || phase === "checking") return null;

  return ReactDOM.createPortal(
    <div className="dialog-overlay" role="presentation">
      <DialogKeys
        onEscape={() => {
          // Not while it downloads, nor while the sessions close and the
          // update installs: the dialog would go and leave an app that
          // refuses to connect and restarts a moment later.
          if (phase === "available" || phase === "error") dismiss();
        }}
      />
      <div className="dialog dialog-update" role="dialog" aria-modal="true">
        {phase === "available" && pending && (
          <>
            <h3 className="dialog-title">{t("updates.availableTitle")}</h3>
            <p className="dialog-text">
              {t("updates.availableMessage", {
                current: pending.current,
                next: pending.update.version,
              })}
            </p>
            {pending.notes && (
              <div className="dialog-update-notes">
                <h4>{t("updates.releaseNotes")}</h4>
                <pre>{pending.notes}</pre>
              </div>
            )}
            <div className="dialog-actions">
              <button type="button" className="dialog-btn dialog-btn-cancel" onClick={dismiss}>
                {t("updates.later")}
              </button>
              <button
                type="button"
                className="dialog-btn dialog-btn-primary"
                onClick={() => {
                  void install();
                }}
              >
                {t("updates.installAndRestart")}
              </button>
            </div>
          </>
        )}
        {phase === "downloading" && (
          <>
            <h3 className="dialog-title">{t("updates.downloadingTitle")}</h3>
            <p className="dialog-text">
              {progress.total !== null
                ? t("updates.downloadingProgress", {
                    downloaded: formatBytes(progress.downloaded),
                    total: formatBytes(progress.total),
                  })
                : t("updates.downloadingIndeterminate")}
            </p>
            <div className="dialog-update-progress">
              <div
                className="dialog-update-progress-bar"
                style={{
                  width:
                    progress.total !== null && progress.total > 0
                      ? `${String(Math.min(100, (progress.downloaded / progress.total) * 100))}%`
                      : "0%",
                }}
              />
            </div>
          </>
        )}
        {phase === "ready" && (
          <>
            <h3 className="dialog-title">{t("updates.readyTitle")}</h3>
            <p className="dialog-text">{t("updates.readyMessage")}</p>
          </>
        )}
        {phase === "error" && (
          <>
            <h3 className="dialog-title">{t("updates.errorTitle")}</h3>
            {/* Raw plugin error messages can leak the update endpoint URL,
                resolver hostnames, or TLS details. Show only the generic
                fixed string; the detail is in `error` and goes to the
                console for diagnostics. */}
            <p className="dialog-text">{t("updates.errorUnknown")}</p>
            <div className="dialog-actions">
              <button type="button" className="dialog-btn dialog-btn-cancel" onClick={dismiss}>
                {t("dialog.close")}
              </button>
            </div>
          </>
        )}
      </div>
    </div>,
    document.body,
  );
}

function formatBytes(n: number): string {
  if (n < 1024) return `${String(n)} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}
