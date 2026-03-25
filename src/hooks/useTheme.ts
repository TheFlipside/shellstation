import { useEffect, useSyncExternalStore } from "react";
import { useSettingsStore, type ThemeMode } from "../stores/settingsStore";

const MEDIA_QUERY = "(prefers-color-scheme: dark)";

/** Subscribe to OS dark-mode preference changes. */
function subscribeToSystemTheme(callback: () => void): () => void {
  const mql = window.matchMedia(MEDIA_QUERY);
  mql.addEventListener("change", callback);
  return () => {
    mql.removeEventListener("change", callback);
  };
}

function getSystemIsDark(): boolean {
  return window.matchMedia(MEDIA_QUERY).matches;
}

export type ResolvedTheme = "dark" | "light";

/**
 * Returns the effective theme ("dark" | "light") after resolving "system"
 * against the OS preference. Also applies the `data-theme` attribute on
 * `<html>` so CSS variables activate.
 */
export function useTheme(): ResolvedTheme {
  const themeMode: ThemeMode = useSettingsStore((s) => s.themeMode);

  const systemIsDark = useSyncExternalStore(subscribeToSystemTheme, getSystemIsDark);

  const resolved: ResolvedTheme =
    themeMode === "system" ? (systemIsDark ? "dark" : "light") : themeMode;

  useEffect(() => {
    document.documentElement.setAttribute("data-theme", resolved);
  }, [resolved]);

  return resolved;
}
