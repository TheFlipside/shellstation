/**
 * useAppQuit: every way of quitting the webview sees goes through the
 * backend's `app_quit`, which closes the sessions before the app exits. The
 * window's close button must never be left to close the window itself,
 * which would quit with the sessions still open, and no failure may leave
 * an app that ignores every further quit.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, renderHook } from "@testing-library/react";
import { QUIT_WATCHDOG_MS, useAppQuit } from "./useAppQuit";
import { useSettingsStore } from "../stores/settingsStore";
import { useTerminalStore } from "../stores/terminalStore";

const invoke = vi.fn<(cmd: string) => Promise<unknown>>();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string) => invoke(cmd),
}));

/** Handlers registered with `listen`, by event name. */
const listeners = new Map<string, () => void>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: (event: string, handler: () => void) => {
    listeners.set(event, handler);
    return Promise.resolve(() => {
      listeners.delete(event);
    });
  },
}));

let closeRequested: ((event: { preventDefault: () => void }) => void) | undefined;
const destroy = vi.fn<() => Promise<void>>();
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    onCloseRequested: (handler: (event: { preventDefault: () => void }) => void) => {
      closeRequested = handler;
      return Promise.resolve(() => {
        closeRequested = undefined;
      });
    },
    destroy,
  }),
}));

function openTabs(count: number, exited = false): void {
  useTerminalStore.setState({
    tabs: Array.from({ length: count }, (_, i) => ({
      id: `t${String(i)}`,
      title: `tab ${String(i)}`,
      type: "ssh" as const,
      exited,
    })),
  });
}

/** Click the window's close button; returns whether the close was held. */
function clickClose(): boolean {
  const preventDefault = vi.fn();
  act(() => {
    closeRequested?.({ preventDefault });
  });
  return preventDefault.mock.calls.length > 0;
}

function pressQuitKey(modifier: "ctrlKey" | "metaKey" = "ctrlKey"): void {
  act(() => {
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "q", [modifier]: true }));
  });
}

function emit(event: string): void {
  act(() => {
    listeners.get(event)?.();
  });
}

/** Let pending promise callbacks run. */
async function settle(): Promise<void> {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

beforeEach(() => {
  invoke.mockReset();
  invoke.mockResolvedValue(undefined);
  destroy.mockReset();
  destroy.mockResolvedValue(undefined);
  listeners.clear();
  closeRequested = undefined;
  useTerminalStore.setState({ tabs: [] });
  useSettingsStore.setState({ confirmOnQuit: true });
  vi.spyOn(console, "warn").mockImplementation(() => undefined);
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("useAppQuit", () => {
  it("quits through the backend when the window is closed with no sessions open", () => {
    renderHook(() => useAppQuit());
    expect(clickClose()).toBe(true);
    expect(invoke).toHaveBeenCalledExactlyOnceWith("app_quit");
    expect(destroy).not.toHaveBeenCalled();
  });

  it("does not ask about tabs whose session has ended", () => {
    openTabs(2, true);
    const { result } = renderHook(() => useAppQuit());
    pressQuitKey();
    expect(result.current.confirming).toBe(false);
    expect(invoke).toHaveBeenCalledExactlyOnceWith("app_quit");
  });

  it("holds the window's close for the confirmation, then quits through the backend", () => {
    openTabs(2);
    const { result } = renderHook(() => useAppQuit());
    expect(clickClose()).toBe(true);
    expect(result.current.confirming).toBe(true);
    expect(invoke).not.toHaveBeenCalled();

    act(() => {
      result.current.confirmQuit();
    });
    expect(result.current.confirming).toBe(false);
    expect(invoke).toHaveBeenCalledExactlyOnceWith("app_quit");
  });

  it("keeps the app open when the confirmation is cancelled", () => {
    openTabs(1);
    const { result } = renderHook(() => useAppQuit());
    pressQuitKey("metaKey");
    expect(result.current.confirming).toBe(true);
    act(() => {
      result.current.cancelQuit();
    });
    expect(result.current.confirming).toBe(false);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("quits without asking when the setting is off", () => {
    openTabs(1);
    useSettingsStore.setState({ confirmOnQuit: false });
    const { result } = renderHook(() => useAppQuit());
    pressQuitKey();
    expect(result.current.confirming).toBe(false);
    expect(invoke).toHaveBeenCalledExactlyOnceWith("app_quit");
  });

  it("asks nothing more once the quit is confirmed", () => {
    openTabs(1);
    const { result } = renderHook(() => useAppQuit());
    pressQuitKey();
    act(() => {
      result.current.confirmQuit();
    });
    pressQuitKey();
    expect(clickClose()).toBe(true);
    expect(result.current.confirming).toBe(false);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(destroy).not.toHaveBeenCalled();
  });

  it("shows that it is quitting once the backend says so, in place of the confirmation", () => {
    openTabs(1);
    const { result } = renderHook(() => useAppQuit());
    pressQuitKey();
    expect(result.current.confirming).toBe(true);
    expect(result.current.quitting).toBe(false);

    emit("app:quitting");
    expect(result.current.quitting).toBe(true);
    expect(result.current.confirming).toBe(false);
  });

  it("asks nothing more while a quit the backend began is running", () => {
    // The menu's quit item goes to the backend directly.
    openTabs(1);
    const { result } = renderHook(() => useAppQuit());
    emit("app:quitting");
    pressQuitKey();
    clickClose();
    expect(result.current.confirming).toBe(false);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("closes the window when the backend cannot be asked to quit", async () => {
    invoke.mockRejectedValue(new Error("ipc down"));
    renderHook(() => useAppQuit());
    clickClose();
    await vi.waitFor(() => {
      expect(destroy).toHaveBeenCalledTimes(1);
    });
  });

  it("lets the user try again when neither the backend nor the window will close", async () => {
    invoke.mockRejectedValueOnce(new Error("ipc down"));
    destroy.mockRejectedValueOnce(new Error("window busy"));
    renderHook(() => useAppQuit());
    clickClose();
    await vi.waitFor(() => {
      expect(destroy).toHaveBeenCalledTimes(1);
    });
    await settle();

    clickClose();
    expect(invoke).toHaveBeenCalledTimes(2);
  });

  it("closes the window itself when the app is still running long after the quit", async () => {
    vi.useFakeTimers();
    renderHook(() => useAppQuit());
    clickClose();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(QUIT_WATCHDOG_MS - 1);
    });
    expect(destroy).not.toHaveBeenCalled();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1);
    });
    expect(destroy).toHaveBeenCalledTimes(1);
  });

  it("lets go of the window, the keyboard and the backend's events when unmounted", async () => {
    const { unmount } = renderHook(() => useAppQuit());
    expect(closeRequested).toBeDefined();
    expect(listeners.has("app:quitting")).toBe(true);

    unmount();
    await settle();
    expect(closeRequested).toBeUndefined();
    expect(listeners.size).toBe(0);
    pressQuitKey();
    expect(invoke).not.toHaveBeenCalled();
  });

  it("leaves the window alone after unmounting, even with a quit under way", async () => {
    vi.useFakeTimers();
    const { unmount } = renderHook(() => useAppQuit());
    emit("app:quitting");
    unmount();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(QUIT_WATCHDOG_MS);
    });
    expect(destroy).not.toHaveBeenCalled();
  });

  it("closes the window itself when a quit the backend began does not end", async () => {
    vi.useFakeTimers();
    renderHook(() => useAppQuit());
    emit("app:quitting");
    await act(async () => {
      await vi.advanceTimersByTimeAsync(QUIT_WATCHDOG_MS);
    });
    expect(destroy).toHaveBeenCalledTimes(1);
  });
});
