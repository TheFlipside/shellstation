/**
 * Registry of live xterm.js instances, keyed by session id.
 *
 * Input typed into a terminal is handled by xterm itself, which scrolls the
 * viewport back to the prompt (`scrollOnUserInput`). Input sent from outside
 * the terminal — command buttons, the freeform command bar — goes straight to
 * the backend over IPC and never passes through xterm, so those senders need a
 * way to reach the instance and apply the same scroll behavior.
 *
 * Keeping the map in a plain module (rather than a Zustand store) keeps
 * non-serializable terminal objects out of application state.
 */
import type { Terminal as XTerm } from "@xterm/xterm";

const terminals = new Map<string, XTerm>();

/** Register a terminal as the live instance for `sessionId`. */
export function registerTerminal(sessionId: string, term: XTerm): void {
  terminals.set(sessionId, term);
}

/**
 * Remove a terminal from the registry. The instance is passed so a stale
 * teardown cannot evict a newer terminal that has already claimed the same
 * session id. React runs effect cleanups before the replacement's effects, so
 * this interleaving does not occur today — the identity check is defensive,
 * kept so that a future change introducing overlapping instances degrades
 * into a no-op rather than a silently dead registry entry.
 */
export function unregisterTerminal(sessionId: string, term: XTerm): void {
  if (terminals.get(sessionId) === term) {
    terminals.delete(sessionId);
  }
}

/**
 * Scroll a session's terminal to the live prompt. No-op when the session has
 * no terminal — a disconnected or already-closed tab is not an error here.
 */
export function scrollTerminalToBottom(sessionId: string): void {
  terminals.get(sessionId)?.scrollToBottom();
}
