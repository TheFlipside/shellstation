import i18n from "./i18n";
import { useToastStore } from "./stores/toastStore";

/**
 * What the backend's check at startup found of the row-level security setup
 * in the PostgreSQL database (`RlsSetupStatus` in `commands/database.rs`).
 */
export type RlsSetupStatus =
  | { state: "inPlace" }
  | { state: "incomplete"; tables: string[]; functions: boolean }
  | { state: "unchecked" };

/**
 * Say that the database is not protected the way this version sets it up, or
 * that this could not be checked. Only the role that owns the tables can
 * apply the setup, so for everyone else the notice is something to tell
 * their administrator; until now it was a line in the application log.
 *
 * The notice stays until it is dismissed and comes back at the next start
 * for as long as it is true. It is closed alone: the button that closes all
 * sticky toasts, which is there for a pile of notices of another kind,
 * leaves this one up. A second call with the same status shows the same
 * toast.
 *
 * The table names are the backend's own, not the database's.
 */
export function showRlsSetupNotice(status: RlsSetupStatus | null | undefined): void {
  if (status === null || status === undefined) return;
  const { addToast } = useToastStore.getState();
  const stays = { sticky: true, closeAlone: true };
  if (status.state === "unchecked") {
    addToast(i18n.t("settings.rlsSetupUnchecked"), "warning", stays);
  } else if (status.state === "incomplete") {
    const parts = [...status.tables];
    if (status.functions) parts.push(i18n.t("settings.rlsSetupFunctions"));
    addToast(i18n.t("settings.rlsSetupIncomplete", { parts: parts.join(", ") }), "error", stays);
  }
}
