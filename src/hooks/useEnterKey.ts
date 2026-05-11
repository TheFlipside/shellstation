import { useEffect } from "react";

// Shared stack so that when multiple dialogs are open, Enter only commits the
// topmost one. The listener is registered in the capture phase and stops
// propagation, preventing the key from reaching React-tree handlers below
// (e.g. the session sidebar's onKeyDown, which would otherwise interpret
// Enter as a second double-click on the selected session and open it twice).
const enterStack: (() => void)[] = [];
let listenerAttached = false;

function handleGlobalKeyDown(e: KeyboardEvent): void {
  if (e.key !== "Enter" || enterStack.length === 0) return;
  // Ignore IME-composing keypresses so the user can still confirm input.
  if (e.isComposing) return;
  // Multi-line inputs treat Enter as a newline; let them handle it.
  if (e.target instanceof HTMLTextAreaElement) return;
  e.preventDefault();
  e.stopPropagation();
  const top = enterStack[enterStack.length - 1];
  top();
}

/** Calls the given callback when the Enter key is pressed (outside textareas).
 *
 * When several dialogs are mounted at once, only the most recently mounted
 * one reacts to Enter. The event is consumed before reaching React-tree
 * handlers, so background lists/trees do not also act on the keypress.
 */
export function useEnterKey(callback: () => void): void {
  useEffect(() => {
    enterStack.push(callback);
    if (!listenerAttached) {
      document.addEventListener("keydown", handleGlobalKeyDown, true);
      listenerAttached = true;
    }
    return () => {
      const idx = enterStack.lastIndexOf(callback);
      if (idx !== -1) enterStack.splice(idx, 1);
      if (enterStack.length === 0) {
        document.removeEventListener("keydown", handleGlobalKeyDown, true);
        listenerAttached = false;
      }
    };
  }, [callback]);
}
