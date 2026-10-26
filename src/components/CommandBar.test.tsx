/**
 * CommandBar tests.
 *
 * Focus of this suite: input sent from the command bar reaches the backend
 * *and* scrolls the target terminal back to the live prompt, the way typed
 * input does. Without the scroll, a user reading scrollback sees nothing
 * happen when they press a command button.
 */
import { afterEach, beforeEach, describe, expect, it, vi, type Mock } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { readText } from "@tauri-apps/plugin-clipboard-manager";
import type { Terminal as XTerm } from "@xterm/xterm";
import { useSettingsStore } from "../stores/settingsStore";
import { useTerminalStore } from "../stores/terminalStore";
import { SESSION_CLOSED_ERROR, registerTerminal, unregisterTerminal } from "../terminalRegistry";
import { CommandBar } from "./CommandBar";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve()) }));
vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({
  readText: vi.fn(() => Promise.resolve("")),
}));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));
// settingsStore imports the i18n singleton, which would otherwise pull in the
// real react-i18next initialization that the mock above replaces.
vi.mock("../i18n", () => ({
  default: { t: (key: string) => key, changeLanguage: vi.fn() },
}));

/**
 * A stand-in for a live xterm.js instance in the registry. The spy is handed
 * back separately so assertions never reference it as an unbound method.
 */
function fakeTerm(): { term: XTerm; scrollToBottom: Mock } {
  const scrollToBottom = vi.fn();
  return { term: { scrollToBottom } as unknown as XTerm, scrollToBottom };
}

// addCommandButton validates the id against a UUID pattern and drops anything
// else, so the fixture needs a real v4 UUID.
const BUTTON_ID = "9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d";
const registered: { id: string; term: XTerm }[] = [];

/** Add a tab to the terminal store with a terminal registered for it. */
function addSession(id: string, onSessionClosed?: () => void): Mock {
  useTerminalStore.getState().addTab(id, id, "ssh");
  const { term, scrollToBottom } = fakeTerm();
  registerTerminal(id, term, onSessionClosed);
  registered.push({ id, term });
  return scrollToBottom;
}

beforeEach(() => {
  useSettingsStore.getState().addCommandButton({
    id: BUTTON_ID,
    name: "uptime-btn",
    command: "uptime\\n",
    color: "#89b4fa",
  });
});

afterEach(() => {
  useSettingsStore.getState().removeCommandButton(BUTTON_ID);
  for (const { id, term } of registered.splice(0)) {
    unregisterTerminal(id, term);
    useTerminalStore.getState().removeTab(id);
  }
});

describe("CommandBar scroll-to-prompt", () => {
  it("scrolls the active terminal when a command button is pressed", () => {
    const scrollToBottom = addSession("tab-1");
    render(<CommandBar uiScale={100} />);

    fireEvent.click(screen.getByText("uptime-btn"));

    expect(scrollToBottom).toHaveBeenCalledTimes(1);
  });

  it("scrolls the active terminal when freeform input is sent", () => {
    const scrollToBottom = addSession("tab-1");
    render(<CommandBar uiScale={100} />);

    fireEvent.change(screen.getByPlaceholderText("commandBar.freeformPlaceholder"), {
      target: { value: "uptime" },
    });
    fireEvent.click(screen.getByText("commandBar.sendToActiveShort"));

    expect(scrollToBottom).toHaveBeenCalledTimes(1);
  });

  it("scrolls every target terminal on send-to-all, not just the active one", () => {
    const firstScroll = addSession("tab-1");
    const secondScroll = addSession("tab-2");
    render(<CommandBar uiScale={100} />);

    fireEvent.change(screen.getByPlaceholderText("commandBar.freeformPlaceholder"), {
      target: { value: "uptime" },
    });
    fireEvent.click(screen.getByText("commandBar.sendToAllShort"));

    expect(firstScroll).toHaveBeenCalledTimes(1);
    expect(secondScroll).toHaveBeenCalledTimes(1);
  });

  it("does nothing when the active session has already exited", () => {
    const scrollToBottom = addSession("tab-1");
    useTerminalStore.getState().markTabExited("tab-1");
    render(<CommandBar uiScale={100} />);

    fireEvent.click(screen.getByText("uptime-btn"));

    expect(scrollToBottom).not.toHaveBeenCalled();
  });
});

describe("CommandBar closed-session reporting", () => {
  afterEach(() => {
    // clearMocks keeps implementations, so put the default back.
    vi.mocked(invoke).mockImplementation(() => Promise.resolve(undefined as never));
  });

  /**
   * Let the rejected send propagate through executeCommand. It is a pure
   * microtask chain, which drains before the next macrotask, so this does
   * not depend on how many awaits the chain has.
   */
  async function settle(): Promise<void> {
    await act(async () => {
      await new Promise<void>((resolve) => {
        setTimeout(resolve, 0);
      });
    });
  }

  it("tells the terminal when a button press finds its session closed", async () => {
    // The tab still looks alive (its exit event was missed), so the button
    // is enabled and the rejected send is the first sign the session is gone.
    vi.mocked(invoke).mockRejectedValue(SESSION_CLOSED_ERROR);
    const onSessionClosed = vi.fn();
    addSession("tab-1", onSessionClosed);
    render(<CommandBar uiScale={100} />);

    fireEvent.click(screen.getByText("uptime-btn"));
    await settle();

    expect(invoke).toHaveBeenCalledWith("ssh_write", { id: "tab-1", data: "uptime\n" });
    expect(onSessionClosed).toHaveBeenCalledTimes(1);
  });

  it("tells the terminal when a clipboard paste finds its session closed", async () => {
    vi.mocked(readText).mockResolvedValueOnce("pasted");
    vi.mocked(invoke).mockRejectedValue(SESSION_CLOSED_ERROR);
    useSettingsStore.getState().updateCommandButton(BUTTON_ID, { command: "\\v" });
    const onSessionClosed = vi.fn();
    addSession("tab-1", onSessionClosed);
    render(<CommandBar uiScale={100} />);

    fireEvent.click(screen.getByText("uptime-btn"));
    await settle();

    expect(invoke).toHaveBeenCalledWith("ssh_write", { id: "tab-1", data: "pasted" });
    expect(onSessionClosed).toHaveBeenCalledTimes(1);
  });

  it("does not report other send failures as a closed session", async () => {
    vi.mocked(invoke).mockRejectedValue("Too many pending writes for this session");
    const onSessionClosed = vi.fn();
    addSession("tab-1", onSessionClosed);
    render(<CommandBar uiScale={100} />);

    fireEvent.click(screen.getByText("uptime-btn"));
    await settle();

    expect(invoke).toHaveBeenCalledWith("ssh_write", { id: "tab-1", data: "uptime\n" });
    expect(onSessionClosed).not.toHaveBeenCalled();
  });
});
