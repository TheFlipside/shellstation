import { useEffect } from "react";

// Shared stack so that when multiple dialogs are open, ESC only dismisses
// the topmost one. Each active hook instance pushes its callback; the last
// one registered wins and consumes the event.
const escapeStack: (() => void)[] = [];
let listenerAttached = false;

function handleGlobalKeyDown(e: KeyboardEvent): void {
  if (e.key !== "Escape" || escapeStack.length === 0) return;
  e.stopPropagation();
  const top = escapeStack[escapeStack.length - 1];
  top();
}

/** Calls the given callback when the Escape key is pressed.
 *
 * When several dialogs are mounted at once, only the most recently mounted
 * one reacts to Escape — matching typical modal dismissal behavior.
 */
export function useEscapeKey(callback: () => void): void {
  useEffect(() => {
    escapeStack.push(callback);
    if (!listenerAttached) {
      document.addEventListener("keydown", handleGlobalKeyDown);
      listenerAttached = true;
    }
    return () => {
      const idx = escapeStack.lastIndexOf(callback);
      if (idx !== -1) escapeStack.splice(idx, 1);
      if (escapeStack.length === 0) {
        document.removeEventListener("keydown", handleGlobalKeyDown);
        listenerAttached = false;
      }
    };
  }, [callback]);
}
