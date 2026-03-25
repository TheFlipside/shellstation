import React from "react";
import ReactDOM from "react-dom/client";
import "@xterm/xterm/css/xterm.css";
import "./i18n";
import App from "./App";

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
