import { useEffect, useRef } from "react";

// Each hook instance registers a ref, not the raw callback. The ref is
// updated each render so the latest closure always fires, while the stack
// only mutates on mount / unmount. This decouples the listener lifecycle
// from callback identity (see the matching note in useEnterKey.ts).
//
// Note: unlike useEnterKey, this hook does NOT call preventDefault. Escape
// has no default browser action that needs blocking, and intercepting it
// would interfere with future native dialog/IME behavior.
interface CallbackRef {
  current: () => void;
}
const escapeStack: CallbackRef[] = [];
let listenerAttached = false;

function handleGlobalKeyDown(e: KeyboardEvent): void {
  if (e.key !== "Escape" || escapeStack.length === 0) return;
  e.stopPropagation();
  const top = escapeStack[escapeStack.length - 1];
  top.current();
}

/** Calls the given callback when the Escape key is pressed.
 *
 * Registered in the capture phase so the keypress is consumed before any
 * background React handler can also act on it.
 *
 * When several dialogs are mounted at once, only the most recently mounted
 * one reacts to Escape — matching typical modal dismissal behavior.
 */
export function useEscapeKey(callback: () => void): void {
  const ref = useRef(callback);
  // Keep the ref pointed at the latest callback every render.
  ref.current = callback;

  useEffect(() => {
    escapeStack.push(ref);
    if (!listenerAttached) {
      document.addEventListener("keydown", handleGlobalKeyDown, true);
      listenerAttached = true;
    }
    return () => {
      const idx = escapeStack.lastIndexOf(ref);
      if (idx !== -1) escapeStack.splice(idx, 1);
      if (escapeStack.length === 0) {
        document.removeEventListener("keydown", handleGlobalKeyDown, true);
        listenerAttached = false;
      }
    };
  }, []);
}
