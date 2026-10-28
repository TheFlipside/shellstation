/**
 * TerminalTabs tests.
 *
 * Focus of this suite: reconnecting an exited tab. Each session_connect opens
 * a real backend connection, so a reconnect that no tab ends up owning is a
 * connection nobody can see or close. Also covers keeping the active tab
 * scrolled into view in the tab strip. Terminal and CommandBar are stubbed out;
 * the real Zustand stores are used so the tests exercise the same tab state
 * the app does.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { useSessionStore, type Session } from "../stores/sessionStore";
import { useTerminalStore } from "../stores/terminalStore";
import { useToastStore } from "../stores/toastStore";
import { TerminalTabs } from "./TerminalTabs";

interface StubTerminalProps {
  sessionId: string;
  onReconnect?: () => void;
  onClose?: () => void;
}

// The stub stands in for pressing Enter (reconnect) or Ctrl+W (close) on an
// exited terminal: one button per callback. Built with createElement inside
// an async factory because vi.mock is hoisted above the module's JSX runtime
// import.
vi.mock("./Terminal", async () => {
  const { createElement, Fragment } = await import("react");
  return {
    Terminal: ({ sessionId, onReconnect, onClose }: StubTerminalProps) =>
      createElement(
        Fragment,
        null,
        createElement(
          "button",
          { type: "button", onClick: () => onReconnect?.() },
          `reconnect ${sessionId}`,
        ),
        createElement(
          "button",
          { type: "button", onClick: () => onClose?.() },
          `close ${sessionId}`,
        ),
      ),
  };
});
vi.mock("./CommandBar", () => ({ CommandBar: () => null }));

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve()) }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(vi.fn())),
}));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
  Trans: () => null,
}));
// The stores import the i18n singleton, which would otherwise pull in the
// real react-i18next initialization that the mock above replaces.
vi.mock("../i18n", () => ({
  default: { t: (key: string) => key, changeLanguage: vi.fn() },
}));

const OLD_CONN = "conn-old";
const NEW_CONN = "conn-new";

// Only the fields TerminalTabs reads; the cast stands in for the rest.
const SESSION = {
  id: "db-1",
  name: "web",
  hostname: "web.example",
  port: 22,
  protocol: "ssh",
  username: "admin",
} as Session;

interface PendingCall {
  cmd: string;
  resolve: (value: unknown) => void;
  reject: (err: unknown) => void;
}

/** IPC calls still waiting on the backend, oldest first. */
let pending: PendingCall[] = [];
/** Commands whose calls stay pending until a test settles them. */
let deferred = new Set<string>();

beforeEach(() => {
  pending = [];
  deferred = new Set(["session_connect"]);
  vi.mocked(invoke).mockImplementation((cmd: string) =>
    deferred.has(cmd)
      ? new Promise((resolve, reject) => {
          pending.push({ cmd, resolve, reject });
        })
      : Promise.resolve(undefined as never),
  );
  useSessionStore.setState({ sessions: [SESSION] });
  useTerminalStore.setState({
    tabs: [{ id: OLD_CONN, title: "web", type: "ssh", sessionDbId: SESSION.id, exited: true }],
    activeTabId: OLD_CONN,
  });
});

afterEach(() => {
  // clearMocks keeps implementations, so put the default back.
  vi.mocked(invoke).mockImplementation(() => Promise.resolve(undefined as never));
  useSessionStore.setState({ sessions: [] });
  useTerminalStore.setState({ tabs: [], activeTabId: null });
  useToastStore.setState({ toasts: [] });
});

function connectCalls(): number {
  return vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === "session_connect").length;
}

function sshDisconnectsOf(id: string): number {
  return vi
    .mocked(invoke)
    .mock.calls.filter(
      ([cmd, args]) => cmd === "ssh_disconnect" && (args as { id?: string } | undefined)?.id === id,
    ).length;
}

function tabIds(): string[] {
  return useTerminalStore.getState().tabs.map((tb) => tb.id);
}

function pressReconnect(): void {
  fireEvent.click(screen.getByText(`reconnect ${OLD_CONN}`));
}

function pressClose(): void {
  fireEvent.click(screen.getByText(`close ${OLD_CONN}`));
}

/**
 * Settle the oldest pending call to `cmd`, then let everything it triggers
 * run. The follow-up work is a pure microtask chain, which drains before the
 * next macrotask, so this does not depend on how many steps the chain has.
 */
async function finish(cmd: string, outcome: { value: unknown } | { error: string }): Promise<void> {
  const index = pending.findIndex((call) => call.cmd === cmd);
  if (index === -1) {
    throw new Error(`no ${cmd} call is pending`);
  }
  const [call] = pending.splice(index, 1);
  await act(async () => {
    if ("value" in outcome) {
      call.resolve(outcome.value);
    } else {
      call.reject(outcome.error);
    }
    await new Promise<void>((resolve) => {
      setTimeout(resolve, 0);
    });
  });
}

/** Let the work already triggered run, e.g. a reconnect's first teardown. */
async function settle(): Promise<void> {
  await act(async () => {
    await new Promise<void>((resolve) => {
      setTimeout(resolve, 0);
    });
  });
}

describe("TerminalTabs reconnect", () => {
  it("closes the old session before connecting again", async () => {
    // The backend holds an ended session until it is told to close it, and a
    // device that counts sessions must see it end before the new one arrives.
    deferred.add("ssh_disconnect");
    render(<TerminalTabs uiScale={100} />);

    pressReconnect();
    expect(sshDisconnectsOf(OLD_CONN)).toBe(1);
    expect(connectCalls()).toBe(0);

    await finish("ssh_disconnect", { value: undefined });
    expect(connectCalls()).toBe(1);
    await finish("session_connect", { value: NEW_CONN });
    expect(tabIds()).toEqual([NEW_CONN]);
  });

  it("ignores a second reconnect while the first is still connecting", async () => {
    render(<TerminalTabs uiScale={100} />);

    pressReconnect();
    pressReconnect();
    await settle();

    expect(sshDisconnectsOf(OLD_CONN)).toBe(1);
    expect(connectCalls()).toBe(1);
    await finish("session_connect", { value: NEW_CONN });
    expect(tabIds()).toEqual([NEW_CONN]);
  });

  it("allows another attempt once a reconnect has failed", async () => {
    render(<TerminalTabs uiScale={100} />);

    pressReconnect();
    await settle();
    await finish("session_connect", { error: "connection refused" });

    expect(useToastStore.getState().toasts.map((toast) => toast.message)).toEqual([
      "terminal.connectionFailed",
    ]);
    expect(tabIds()).toEqual([OLD_CONN]);

    pressReconnect();
    await settle();
    expect(connectCalls()).toBe(2);
    await finish("session_connect", { value: NEW_CONN });
    expect(tabIds()).toEqual([NEW_CONN]);
  });

  it("does not connect for a tab closed while its old session is torn down", async () => {
    deferred.add("ssh_disconnect");
    render(<TerminalTabs uiScale={100} />);

    pressReconnect();
    pressClose();
    await finish("ssh_disconnect", { value: undefined });
    await finish("ssh_disconnect", { value: undefined });

    expect(connectCalls()).toBe(0);
    expect(tabIds()).toEqual([]);
  });

  it("does not reconnect a tab that is already being closed", async () => {
    deferred.add("ssh_disconnect");
    render(<TerminalTabs uiScale={100} />);

    pressClose();
    pressReconnect();
    await finish("ssh_disconnect", { value: undefined });

    // Only the close's own teardown, and no connect.
    expect(sshDisconnectsOf(OLD_CONN)).toBe(1);
    expect(connectCalls()).toBe(0);
    expect(tabIds()).toEqual([]);
  });

  it("still reconnects when tearing down the old session fails", async () => {
    // The old session may be gone from the backend already, e.g. after a
    // failed earlier attempt, and its teardown then reports an error.
    deferred.add("ssh_disconnect");
    render(<TerminalTabs uiScale={100} />);

    pressReconnect();
    await finish("ssh_disconnect", { error: "SSH session conn-old not found" });

    expect(connectCalls()).toBe(1);
    await finish("session_connect", { value: NEW_CONN });
    expect(tabIds()).toEqual([NEW_CONN]);
  });

  it("disconnects a reconnected session whose tab was closed meanwhile", async () => {
    render(<TerminalTabs uiScale={100} />);

    pressReconnect();
    await settle();
    act(() => {
      useTerminalStore.getState().removeTab(OLD_CONN);
    });
    await finish("session_connect", { value: NEW_CONN });

    expect(invoke).toHaveBeenCalledWith("ssh_disconnect", { id: NEW_CONN });
    expect(tabIds()).toEqual([]);
  });

  it("disconnects a reconnected session whose tab is mid-close", async () => {
    // destroyTab awaits the old session's teardown before removing the tab.
    // A reconnect landing in that window must not keep the tab alive on the
    // new session.
    deferred.add("ssh_disconnect");
    render(<TerminalTabs uiScale={100} />);

    pressReconnect();
    await finish("ssh_disconnect", { value: undefined });
    pressClose();
    await finish("session_connect", { value: NEW_CONN });
    // The close's teardown of the old session, then the orphan's.
    await finish("ssh_disconnect", { value: undefined });
    await finish("ssh_disconnect", { value: undefined });

    expect(sshDisconnectsOf(NEW_CONN)).toBe(1);
    expect(tabIds()).toEqual([]);
    expect(pending).toEqual([]);
  });

  it("tears a closing tab down only once when it is closed again", async () => {
    deferred.add("ssh_disconnect");
    render(<TerminalTabs uiScale={100} />);

    pressClose();
    pressClose();
    await finish("ssh_disconnect", { value: undefined });

    const teardowns = vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === "ssh_disconnect");
    expect(teardowns).toEqual([["ssh_disconnect", { id: OLD_CONN }]]);
    expect(tabIds()).toEqual([]);
  });

  it("tears down an orphaned reconnect by the saved session's protocol", async () => {
    // session_connect connects with the protocol stored on the session, which
    // may have been edited since the tab was opened.
    useSessionStore.setState({ sessions: [{ ...SESSION, protocol: "telnet" }] });
    render(<TerminalTabs uiScale={100} />);

    pressReconnect();
    await settle();
    act(() => {
      useTerminalStore.getState().removeTab(OLD_CONN);
    });
    await finish("session_connect", { value: NEW_CONN });

    expect(invoke).toHaveBeenCalledWith("telnet_disconnect", { id: NEW_CONN });
    expect(invoke).not.toHaveBeenCalledWith("ssh_disconnect", { id: NEW_CONN });
  });

  it("gives the tab the protocol of the session it reconnected", async () => {
    // Writes and the eventual close go by the tab's type. Keeping the old
    // one would send them to the wrong backend, and closing the tab would
    // never close the new session.
    useSessionStore.setState({ sessions: [{ ...SESSION, protocol: "telnet" }] });
    render(<TerminalTabs uiScale={100} />);

    pressReconnect();
    await settle();
    await finish("session_connect", { value: NEW_CONN });

    expect(useTerminalStore.getState().tabs).toMatchObject([
      { id: NEW_CONN, type: "telnet", meta: { host: SESSION.hostname, port: SESSION.port } },
    ]);
  });
});

describe("TerminalTabs tab strip", () => {
  it("scrolls a newly opened tab into view", () => {
    // New tabs are appended at the end of the strip. Once it overflows, that
    // is past the right edge, so the active tab would sit out of sight.
    render(<TerminalTabs uiScale={100} />);
    const scrollIntoView = vi.spyOn(Element.prototype, "scrollIntoView");
    scrollIntoView.mockClear();

    act(() => {
      useTerminalStore.getState().addTab(NEW_CONN, "db-server", "ssh");
    });

    expect(scrollIntoView).toHaveBeenCalledTimes(1);
    expect(scrollIntoView).toHaveBeenCalledWith({ block: "nearest", inline: "nearest" });
    expect(scrollIntoView.mock.contexts[0]).toBe(screen.getByTitle("db-server"));
  });
});
