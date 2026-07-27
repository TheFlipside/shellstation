/**
 * Registry unit tests. The registry is what lets input sent from outside a
 * terminal (command buttons, the command bar) scroll that terminal back to
 * the live prompt, so its lifecycle correctness is what keeps that working.
 */
import { describe, expect, it, vi, type Mock } from "vitest";
import type { Terminal as XTerm } from "@xterm/xterm";
import { registerTerminal, scrollTerminalToBottom, unregisterTerminal } from "./terminalRegistry";

/**
 * A stand-in exposing only the method the registry calls. The spy is returned
 * alongside the terminal rather than asserted through `term.scrollToBottom`,
 * which would read as an unbound method reference.
 */
function fakeTerm(): { term: XTerm; scrollToBottom: Mock } {
  const scrollToBottom = vi.fn();
  return { term: { scrollToBottom } as unknown as XTerm, scrollToBottom };
}

describe("terminalRegistry", () => {
  it("scrolls the terminal registered for a session", () => {
    const { term, scrollToBottom } = fakeTerm();
    registerTerminal("session-a", term);

    scrollTerminalToBottom("session-a");

    expect(scrollToBottom).toHaveBeenCalledTimes(1);
    unregisterTerminal("session-a", term);
  });

  it("ignores sessions with no registered terminal", () => {
    expect(() => {
      scrollTerminalToBottom("never-registered");
    }).not.toThrow();
  });

  it("stops scrolling a terminal after it is unregistered", () => {
    const { term, scrollToBottom } = fakeTerm();
    registerTerminal("session-b", term);
    unregisterTerminal("session-b", term);

    scrollTerminalToBottom("session-b");

    expect(scrollToBottom).not.toHaveBeenCalled();
  });

  it("does not let a stale teardown evict the current terminal", () => {
    // Guards the case where a replacement claims the session id before the
    // previous instance tears down. React's effect ordering prevents this
    // today; the test pins the defensive behavior in place.
    const previous = fakeTerm();
    const current = fakeTerm();
    registerTerminal("session-c", previous.term);
    registerTerminal("session-c", current.term);

    unregisterTerminal("session-c", previous.term);
    scrollTerminalToBottom("session-c");

    expect(current.scrollToBottom).toHaveBeenCalledTimes(1);
    expect(previous.scrollToBottom).not.toHaveBeenCalled();
    unregisterTerminal("session-c", current.term);
  });
});
