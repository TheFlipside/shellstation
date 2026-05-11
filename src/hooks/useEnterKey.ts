import { useEffect, useLayoutEffect, useRef } from "react";

// Each hook instance registers a ref, not the raw callback. The ref is
// updated in a layout-time effect so the latest closure always fires, while
// the stack itself only mutates on mount / unmount. This decouples the
// listener lifecycle from callback identity:
//   - StrictMode's intentional double-invoke (mount → unmount → mount) ends
//     with exactly one stack entry instead of two duplicates.
//   - Unstable callers (inline arrow functions) do not churn the stack on
//     every render.
//   - The handler always sees the latest closure via ref.current.
interface CallbackRef {
  current: () => void;
}
const enterStack: CallbackRef[] = [];
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
  top.current();
}

/** Calls the given callback when the Enter key is pressed (outside textareas).
 *
 * Registered in the capture phase so the keypress is consumed before any
 * background React handler (e.g. a list/tree onKeyDown) can also act on it.
 *
 * When several dialogs are mounted at once, only the most recently mounted
 * one reacts to Enter — the others remain on the stack but inactive.
 */
export function useEnterKey(callback: () => void): void {
  const ref = useRef(callback);
  // Update the ref in a layout effect rather than during render. Mutating
  // refs at render time is unsafe in concurrent mode: a render can be
  // discarded or replayed before it commits, leaving the ref pointing at a
  // closure from an abandoned render. useLayoutEffect runs synchronously
  // after commit, so ref.current is always the closure of the rendered tree.
  useLayoutEffect(() => {
    ref.current = callback;
  }, [callback]);

  useEffect(() => {
    enterStack.push(ref);
    if (!listenerAttached) {
      document.addEventListener("keydown", handleGlobalKeyDown, true);
      listenerAttached = true;
    }
    return () => {
      const idx = enterStack.lastIndexOf(ref);
      if (idx !== -1) enterStack.splice(idx, 1);
      if (enterStack.length === 0) {
        document.removeEventListener("keydown", handleGlobalKeyDown, true);
        listenerAttached = false;
      }
    };
  }, []);
}
