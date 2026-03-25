import React from "react";
import ReactDOM from "react-dom/client";
import "@xterm/xterm/css/xterm.css";
import "./i18n";
import App from "./App";
import type { ThemeMode } from "./stores/settingsStore";

// Resolve the initial theme before React renders to prevent a flash of
// unstyled content. The Zustand persisted store writes to localStorage
// under "shellstation-settings".
function resolveInitialTheme(): "dark" | "light" {
  try {
    const raw = localStorage.getItem("shellstation-settings");
    if (raw) {
      const parsed = JSON.parse(raw) as { state?: { themeMode?: ThemeMode } };
      const mode = parsed.state?.themeMode;
      if (mode === "light") return "light";
      if (mode === "system") {
        return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
      }
    }
  } catch {
    // Ignore parse errors — fall through to dark default.
  }
  return "dark";
}
document.documentElement.setAttribute("data-theme", resolveInitialTheme());

// Suppress the native browser/webview context menu globally.
// Individual components (e.g. Terminal) handle right-click behavior themselves.
document.addEventListener("contextmenu", (e) => {
  e.preventDefault();
});

// eslint-disable-next-line @typescript-eslint/no-non-null-assertion
ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
