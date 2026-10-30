import i18n from "./i18n";
import { useSettingsStore } from "./stores/settingsStore";
import { useToastStore } from "./stores/toastStore";

/**
 * Say that the private-address check is off although the previous version
 * had it on (see `startUpConnectionSettings`). The notice stays until it is
 * dismissed, or until the check is switched on, when it would no longer be
 * true. Returns a function that takes the notice down. A second call while
 * the notice is up shows the same toast, which either function takes down.
 */
export function showPrivateIpCheckOffNotice(): () => void {
  if (useSettingsStore.getState().restrictPrivateIps) return () => undefined;
  const { addToast, removeToast } = useToastStore.getState();
  const id = addToast(
    i18n.t("settings.restrictPrivateIpsNotHandedOver", {
      setting: i18n.t("settings.restrictPrivateIpsLabel"),
      section: i18n.t("settings.security"),
    }),
    "error",
    { sticky: true },
  );
  const unsubscribers: (() => void)[] = [];
  const stopWatching = (): void => {
    for (const unsubscribe of unsubscribers) unsubscribe();
  };
  const dismiss = (): void => {
    stopWatching();
    removeToast(id);
  };
  unsubscribers.push(
    useSettingsStore.subscribe((state) => {
      if (state.restrictPrivateIps) dismiss();
    }),
    // Once the user has dismissed it, there is nothing left to watch for.
    useToastStore.subscribe((state) => {
      if (!state.toasts.some((toast) => toast.id === id)) stopWatching();
    }),
  );
  return dismiss;
}
