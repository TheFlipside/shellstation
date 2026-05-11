import { useEffect, useLayoutEffect, useRef } from "react";

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
 * background React handler can also act on it. The handler returns early
 * when no dialog is mounted (empty stack), so background Escape handlers
 * — e.g. ContextMenu, CustomSelect dropdown — keep working when no dialog
 * is open. While a dialog IS open, however, this hook calls
 * `stopPropagation()` and the event will NOT reach those background
 * handlers: the topmost dialog "owns" Escape exclusively. That is the
 * intended modal behavior; if you want Escape to do something other than
 * dismiss the dialog while it is open, handle it inside the dialog itself.
 *
 * When several dialogs are mounted at once, only the most recently mounted
 * one reacts to Escape — matching typical modal dismissal behavior.
 */
export function useEscapeKey(callback: () => void): void {
  const ref = useRef(callback);
  // Update the ref in a layout effect rather than during render. Mutating
  // refs at render time is unsafe in concurrent mode (see useEnterKey.ts).
  useLayoutEffect(() => {
    ref.current = callback;
  }, [callback]);

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
