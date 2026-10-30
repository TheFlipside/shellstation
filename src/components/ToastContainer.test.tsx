/**
 * ToastContainer: toasts close on their own after the configured time,
 * except sticky ones, which stay until the user dismisses them.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { useSettingsStore } from "../stores/settingsStore";
import { useToastStore } from "../stores/toastStore";
import { ToastContainer } from "./ToastContainer";

// The settings store imports invoke; nothing here may reach a backend.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

beforeEach(() => {
  vi.useFakeTimers();
  useToastStore.setState(useToastStore.getInitialState(), true);
  useSettingsStore.setState({ toastAutoDismiss: true, toastDismissSeconds: 5 });
});

afterEach(() => {
  vi.useRealTimers();
});

function show(message: string, sticky = false): void {
  act(() => {
    useToastStore.getState().addToast(message, "error", { sticky });
  });
}

describe("ToastContainer", () => {
  it("closes a toast once its time is up", () => {
    render(<ToastContainer />);
    show("passing");
    act(() => {
      vi.advanceTimersByTime(4999);
    });
    expect(screen.getByText("passing")).toBeInTheDocument();
    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(screen.queryByText("passing")).not.toBeInTheDocument();
  });

  it("keeps a sticky toast until it is dismissed", () => {
    render(<ToastContainer />);
    show("staying", true);
    act(() => {
      vi.advanceTimersByTime(60_000);
    });
    expect(screen.getByText("staying")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "×" }));
    expect(screen.queryByText("staying")).not.toBeInTheDocument();
  });
});
