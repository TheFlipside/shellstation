/**
 * Terminal component tests.
 *
 * Focus of this suite: the multiline-paste confirmation flow and the focus
 * restoration that follows it. xterm.js, the Tauri IPC layer, and i18n are
 * mocked; the real Zustand settings store is used so the tests exercise the
 * same `confirmMultilinePaste` path the app does.
 */
import { beforeEach, describe, expect, it, vi, type Mock } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { useSettingsStore } from "../stores/settingsStore";
import { scrollTerminalToBottom } from "../terminalRegistry";
import { Terminal as XTerm } from "@xterm/xterm";
import { Terminal } from "./Terminal";

/** The subset of the xterm.js API that Terminal.tsx actually touches. */
interface FakeTerminal {
  open: Mock;
  loadAddon: Mock;
  onData: Mock;
  onResize: Mock;
  onSelectionChange: Mock;
  attachCustomKeyEventHandler: Mock;
  getSelection: Mock;
  paste: Mock;
  focus: Mock;
  scrollToBottom: Mock;
  write: Mock;
  dispose: Mock;
  options: Record<string, unknown>;
}

// Every fake terminal the component constructs, in creation order.
const { fakeTerminals } = vi.hoisted(() => ({ fakeTerminals: [] as FakeTerminal[] }));

// The component calls `new XTerm(...)` / `new FitAddon()`, so every mocked
// constructor has to be a function expression — arrow functions are not
// constructible and fail with "is not a constructor".
vi.mock("@xterm/xterm", () => {
  const disposable = { dispose: vi.fn() };
  return {
    Terminal: vi.fn(function (): FakeTerminal {
      const term: FakeTerminal = {
        open: vi.fn(),
        loadAddon: vi.fn(),
        onData: vi.fn(() => disposable),
        onResize: vi.fn(() => disposable),
        onSelectionChange: vi.fn(() => disposable),
        attachCustomKeyEventHandler: vi.fn(),
        getSelection: vi.fn(() => ""),
        paste: vi.fn(),
        focus: vi.fn(),
        scrollToBottom: vi.fn(),
        write: vi.fn(),
        dispose: vi.fn(),
        options: {},
      };
      fakeTerminals.push(term);
      return term;
    }),
  };
});

vi.mock("@xterm/addon-fit", () => ({
  FitAddon: vi.fn(function () {
    return { fit: vi.fn() };
  }),
}));
vi.mock("@xterm/addon-search", () => ({
  SearchAddon: vi.fn(function () {
    return {};
  }),
}));
vi.mock("@xterm/addon-webgl", () => ({
  WebglAddon: vi.fn(function () {
    return { onContextLoss: vi.fn(), dispose: vi.fn() };
  }),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve()) }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(vi.fn())),
}));
vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({
  readText: vi.fn(() => Promise.resolve("")),
  writeText: vi.fn(() => Promise.resolve()),
}));

// Translations resolve to their keys, so assertions can match on key names.
vi.mock("../i18n", () => ({ default: { t: (key: string) => key } }));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));

/** The most recently constructed fake terminal. */
function latestTerminal(): FakeTerminal {
  // Not `.at(-1)` — the app's tsconfig targets ES2020, which predates it.
  const term = fakeTerminals[fakeTerminals.length - 1] as FakeTerminal | undefined;
  if (!term) {
    throw new Error("Terminal did not construct an xterm.js instance");
  }
  return term;
}

function renderTerminal(visible = true): ReturnType<typeof render> {
  return render(<Terminal sessionId="session-1" sessionType="local" visible={visible} />);
}

/**
 * Dispatch a native paste on the terminal container. jsdom has no usable
 * ClipboardEvent constructor, so clipboardData is attached to a plain Event —
 * the component only ever calls `getData("text/plain")` on it.
 */
function firePaste(container: HTMLElement, text: string): void {
  const target = container.querySelector(".terminal-instance");
  if (!target) {
    throw new Error("terminal container was not rendered");
  }
  const event = new Event("paste", { bubbles: true, cancelable: true });
  Object.defineProperty(event, "clipboardData", {
    value: { getData: () => text },
  });
  fireEvent(target, event);
}

const MULTILINE = "uptime\nreboot\n";

beforeEach(() => {
  fakeTerminals.length = 0;
  // The store is a module singleton shared across tests in this file.
  useSettingsStore.getState().setConfirmMultilinePaste(true);
  // Freeze the 50 ms re-fit/focus timer so it cannot fire mid-assertion and
  // inflate a focus call count; the test that cares about it advances the
  // clock explicitly. A future test that awaits a real promise chain (the
  // clipboard read path, say) must opt out with vi.useRealTimers().
  vi.useFakeTimers();
  return () => {
    vi.useRealTimers();
  };
});

describe("Terminal selection colors", () => {
  /** The theme the component handed to the xterm.js constructor. */
  function constructorTheme(): Record<string, string> {
    const call = vi.mocked(XTerm).mock.calls[0] as [{ theme: Record<string, string> }] | undefined;
    if (!call) {
      throw new Error("Terminal did not construct an xterm.js instance");
    }
    return call[0].theme;
  }

  beforeEach(() => {
    // Pin the palette so the assertions do not depend on the OS dark-mode
    // preference, and start from "no override" in this shared store. Every
    // value touched here is restored afterwards: the store is a module
    // singleton, so anything left behind leaks into the other suites below.
    const previousThemeMode = useSettingsStore.getState().themeMode;
    useSettingsStore.getState().setThemeMode("dark");
    useSettingsStore.getState().setTerminalBackgroundColor(null);
    useSettingsStore.getState().setTerminalForegroundColor(null);
    return () => {
      useSettingsStore.getState().setThemeMode(previousThemeMode);
      useSettingsStore.getState().setTerminalBackgroundColor(null);
      useSettingsStore.getState().setTerminalForegroundColor(null);
    };
  });

  it("renders selection as reverse video of the theme palette", () => {
    renderTerminal();

    const theme = constructorTheme();
    expect(theme.selectionBackground).toBe(theme.foreground);
    expect(theme.selectionForeground).toBe(theme.background);
  });

  it("keeps the unfocused selection readable rather than dimming it", () => {
    // selectionForeground applies regardless of focus, so a dimmed inactive
    // background would leave forced-color text on a washed-out backdrop.
    renderTerminal();

    expect(constructorTheme().selectionInactiveBackground).toBe(constructorTheme().foreground);
  });

  it("keeps selection colors fully opaque", () => {
    // An alpha suffix here is what made the old selection wash out against
    // bright output, so the 6-digit form is part of the contract.
    renderTerminal();

    const theme = constructorTheme();
    expect(theme.selectionBackground).toMatch(/^#[0-9a-f]{6}$/i);
    expect(theme.selectionForeground).toMatch(/^#[0-9a-f]{6}$/i);
  });

  it("inverts the user's color overrides, not the theme defaults", () => {
    useSettingsStore.getState().setTerminalBackgroundColor("#101020");
    useSettingsStore.getState().setTerminalForegroundColor("#f0f0ff");

    renderTerminal();

    const theme = constructorTheme();
    expect(theme.selectionBackground).toBe("#f0f0ff");
    expect(theme.selectionForeground).toBe("#101020");
    expect(theme.selectionInactiveBackground).toBe("#f0f0ff");
  });

  it("re-derives selection colors when an override changes on a live terminal", () => {
    renderTerminal();

    act(() => {
      useSettingsStore.getState().setTerminalForegroundColor("#00ff00");
    });

    const theme = latestTerminal().options.theme as Record<string, string>;
    expect(theme.selectionBackground).toBe("#00ff00");
    expect(theme.selectionForeground).toBe(theme.background);
  });
});

describe("Terminal paste confirmation", () => {
  it("opens the confirmation dialog instead of pasting control characters", () => {
    const { container } = renderTerminal();

    firePaste(container, MULTILINE);

    expect(screen.getByText("pasteConfirm.title")).toBeInTheDocument();
    expect(latestTerminal().paste).not.toHaveBeenCalled();
  });

  it("pastes single-line content directly, without a dialog", () => {
    const { container } = renderTerminal();

    firePaste(container, "uptime");

    expect(screen.queryByText("pasteConfirm.title")).not.toBeInTheDocument();
    expect(latestTerminal().paste).toHaveBeenCalledWith("uptime");
  });

  it("pastes directly when the confirmation setting is off", () => {
    useSettingsStore.getState().setConfirmMultilinePaste(false);
    const { container } = renderTerminal();

    firePaste(container, MULTILINE);

    expect(screen.queryByText("pasteConfirm.title")).not.toBeInTheDocument();
    expect(latestTerminal().paste).toHaveBeenCalledWith(MULTILINE);
  });

  it("ignores a second paste while the dialog is open", () => {
    const { container } = renderTerminal();

    firePaste(container, MULTILINE);
    firePaste(container, "sudo rm -rf /\n");

    // The preview still shows the first payload — it cannot be swapped
    // underneath the user after they have started reading it. Read the
    // element's raw textContent: getByText collapses the newlines that are
    // the whole point of this assertion.
    const preview = document.querySelector(".dialog-paste-preview");
    expect(preview?.textContent).toBe(MULTILINE);
  });
});

describe("Terminal registry lifecycle", () => {
  it("registers its terminal so external senders can scroll it", () => {
    renderTerminal();

    scrollTerminalToBottom("session-1");

    expect(latestTerminal().scrollToBottom).toHaveBeenCalledTimes(1);
  });

  it("unregisters its terminal on unmount", () => {
    const { unmount } = renderTerminal();
    const term = latestTerminal();

    unmount();
    scrollTerminalToBottom("session-1");

    expect(term.scrollToBottom).not.toHaveBeenCalled();
  });
});

describe("Terminal focus restoration", () => {
  it("pastes and returns focus to the terminal on confirm", () => {
    const { container } = renderTerminal();
    firePaste(container, MULTILINE);
    const term = latestTerminal();
    term.focus.mockClear();

    fireEvent.click(screen.getByText("pasteConfirm.confirm"));

    expect(term.paste).toHaveBeenCalledWith(MULTILINE);
    expect(term.focus).toHaveBeenCalledTimes(1);
    expect(screen.queryByText("pasteConfirm.title")).not.toBeInTheDocument();
  });

  it("returns focus without pasting on cancel", () => {
    const { container } = renderTerminal();
    firePaste(container, MULTILINE);
    const term = latestTerminal();
    term.focus.mockClear();

    fireEvent.click(screen.getByText("dialog.cancel"));

    expect(term.paste).not.toHaveBeenCalled();
    expect(term.focus).toHaveBeenCalledTimes(1);
    expect(screen.queryByText("pasteConfirm.title")).not.toBeInTheDocument();
  });

  it("does not steal focus for a terminal in a background tab", () => {
    const { container } = renderTerminal(false);
    firePaste(container, MULTILINE);
    const term = latestTerminal();
    term.focus.mockClear();

    fireEvent.click(screen.getByText("pasteConfirm.confirm"));

    expect(term.paste).toHaveBeenCalledWith(MULTILINE);
    expect(term.focus).not.toHaveBeenCalled();
  });

  it("focuses on becoming visible, but not while hidden", () => {
    const { rerender } = renderTerminal(false);
    const term = latestTerminal();
    term.focus.mockClear();

    vi.advanceTimersByTime(100);
    expect(term.focus).not.toHaveBeenCalled();

    rerender(<Terminal sessionId="session-1" sessionType="local" visible={true} />);
    vi.advanceTimersByTime(100);

    expect(term.focus).toHaveBeenCalledTimes(1);
  });
});
