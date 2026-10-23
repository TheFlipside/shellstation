/**
 * Settles the pick that is currently waiting on the native picker, if any.
 * Held so the next pick can clean up one that never heard back.
 */
let settlePending: (() => void) | null = null;

/**
 * Show a native file picker and resolve with the selected file (or null if
 * the user cancelled).
 *
 * Only the input's own `change` and `cancel` events settle the pick. Window
 * focus is deliberately not treated as a cancel signal: in WebView2 the window
 * regains focus as the native dialog closes, but the chosen file reaches the
 * page over a separate IPC hop and `change` routinely lands after `focus` —
 * often more than a frame later. Reading "focus without files" as a cancel
 * silently discarded every pick, so the imports behind this helper did nothing
 * at all.
 *
 * A webview that never dispatches `cancel` (WebKit before Safari 16.4) leaves
 * a cancelled pick pending with its hidden <input> attached. The native picker
 * is modal, so a pick still pending when the next one starts can only be such
 * a silent cancel: it is settled with null and its input removed at that
 * point, which bounds the leftover to a single hidden element. The flip side
 * for callers: on such a webview the promise may stay pending after a cancel,
 * so do not hold UI state (a busy flag, a disabled button) across the await —
 * set it only once a file has actually been returned.
 */
export function pickFile(accept: string): Promise<File | null> {
  settlePending?.();
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = accept;
    input.style.display = "none";
    let resolved = false;
    const finish = (value: File | null): void => {
      if (resolved) return;
      resolved = true;
      if (settlePending === cancel) settlePending = null;
      input.remove();
      resolve(value);
    };
    const cancel = (): void => {
      finish(null);
    };
    input.addEventListener("change", () => {
      finish(input.files?.[0] ?? null);
    });
    input.addEventListener("cancel", cancel);
    settlePending = cancel;
    document.body.appendChild(input);
    input.click();
  });
}
