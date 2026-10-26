/**
 * Registry unit tests. The registry is what lets input sent from outside a
 * terminal (command buttons, the command bar) scroll that terminal back to
 * the live prompt, so its lifecycle correctness is what keeps that working.
 * The same senders use it to report a session the backend says has closed.
 */
import { afterEach, describe, expect, it, vi, type Mock } from "vitest";
import type { Terminal as XTerm } from "@xterm/xterm";
import sshModuleSource from "../src-tauri/src/ssh/mod.rs?raw";
import {
  SESSION_CLOSED_ERROR,
  isSessionClosedError,
  registerTerminal,
  reportSessionClosed,
  scrollTerminalToBottom,
  unregisterTerminal,
} from "./terminalRegistry";

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

describe("terminalRegistry session-closed reporting", () => {
  // The registry is a module singleton. Unregister in afterEach, not at the
  // end of each test, so a failed assertion cannot leak an entry onward.
  const registered: { id: string; term: XTerm }[] = [];

  function register(id: string, onSessionClosed?: () => void): XTerm {
    const { term } = fakeTerm();
    registerTerminal(id, term, onSessionClosed);
    registered.push({ id, term });
    return term;
  }

  afterEach(() => {
    for (const { id, term } of registered.splice(0)) {
      unregisterTerminal(id, term);
    }
  });

  it("runs the closed handler registered for a session", () => {
    const onSessionClosed = vi.fn();
    register("session-d", onSessionClosed);

    reportSessionClosed("session-d");

    expect(onSessionClosed).toHaveBeenCalledTimes(1);
  });

  it("ignores reports for a session with no registered terminal", () => {
    expect(() => {
      reportSessionClosed("never-registered");
    }).not.toThrow();
  });

  it("ignores reports for a terminal registered without a handler", () => {
    register("session-e");

    expect(() => {
      reportSessionClosed("session-e");
    }).not.toThrow();
  });

  it("stops reporting to a terminal after it is unregistered", () => {
    const onSessionClosed = vi.fn();
    const term = register("session-f", onSessionClosed);
    unregisterTerminal("session-f", term);

    reportSessionClosed("session-f");

    expect(onSessionClosed).not.toHaveBeenCalled();
  });

  it("drops the previous handler when a session is re-registered without one", () => {
    const previous = vi.fn();
    register("session-g", previous);
    register("session-g");

    reportSessionClosed("session-g");

    expect(previous).not.toHaveBeenCalled();
  });

  it("does not let a stale teardown evict the current terminal's handler", () => {
    const previous = vi.fn();
    const current = vi.fn();
    const previousTerm = register("session-h", previous);
    register("session-h", current);

    unregisterTerminal("session-h", previousTerm);
    reportSessionClosed("session-h");

    expect(current).toHaveBeenCalledTimes(1);
    expect(previous).not.toHaveBeenCalled();
  });

  it("recognizes only the backend's closed-session error", () => {
    expect(isSessionClosedError(SESSION_CLOSED_ERROR)).toBe(true);
    expect(isSessionClosedError("Too many pending writes for this session")).toBe(false);
    expect(isSessionClosedError("SSH session abc not found")).toBe(false);
    expect(isSessionClosedError(new Error(SESSION_CLOSED_ERROR))).toBe(false);
  });

  it("matches the error string the Rust backend returns", () => {
    // The two definitions are kept in sync by hand. This fails if either one
    // changes alone, which would otherwise silently disable the frontend's
    // closed-session handling.
    const match = /pub const SESSION_CLOSED_ERROR: &str = "([^"]*)";/.exec(sshModuleSource);

    expect(match?.[1]).toBe(SESSION_CLOSED_ERROR);
  });
});
