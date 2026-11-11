/**
 * ToastContainer: toasts close on their own after the configured time,
 * except sticky ones, which stay until the user dismisses them. The sticky
 * toasts are a group of their own, above the others: each group has a button
 * that closes its toasts once there is more than one, and only the list of
 * the others scrolls to a new toast.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import i18n from "../i18n";
import { useSettingsStore } from "../stores/settingsStore";
import { useToastStore } from "../stores/toastStore";
import { ToastContainer } from "./ToastContainer";

// The settings store imports invoke; nothing here may reach a backend.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

beforeEach(async () => {
  // The real English text, so the placeholder is checked as well.
  await i18n.changeLanguage("en");
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

  it("shows the sticky toasts above the others, whatever came when", () => {
    const { container } = render(<ToastContainer />);
    show("passing");
    show("staying", true);
    show("another");
    show("staying too", true);
    const groups = Array.from(container.querySelectorAll(".toast-container > .toast-group"));
    expect(groups.map((g) => g.className)).toEqual([
      "toast-group toast-group-sticky",
      "toast-group toast-group-passing",
    ]);
    const messages = (group: Element): (string | null)[] =>
      Array.from(group.querySelectorAll(".toast-list .toast-message")).map((m) => m.textContent);
    expect(messages(groups[0])).toEqual(["staying", "staying too"]);
    expect(messages(groups[1])).toEqual(["passing", "another"]);
  });

  it("offers to close a group's toasts only when it has more than one", () => {
    render(<ToastContainer />);
    show("staying", true);
    show("passing");
    expect(screen.queryByRole("button", { name: /Close all/ })).not.toBeInTheDocument();

    show("another");
    expect(screen.getAllByRole("button", { name: /Close all/ })).toHaveLength(1);
    show("staying too", true);
    expect(screen.getAllByRole("button", { name: "Close all (2)" })).toHaveLength(2);
  });

  it("closes the other toasts without the sticky ones, and the sticky ones alone", () => {
    const { container } = render(<ToastContainer />);
    const closeAll = (group: string): Element => {
      const button = container.querySelector(`.${group} > .toast-close-all`);
      if (button === null) throw new Error(`no button in ${group}`);
      return button;
    };
    show("staying", true);
    show("staying too", true);
    show("passing");
    show("another");
    show("a third");
    // The real English text, with the number of toasts in the group.
    expect(closeAll("toast-group-passing")).toHaveTextContent("Close all (3)");
    // The button is above the list, not in it, so it does not scroll away.
    expect(container.querySelector(".toast-list .toast-close-all")).toBeNull();

    fireEvent.click(closeAll("toast-group-passing"));
    expect(useToastStore.getState().toasts.map((t) => t.message)).toEqual([
      "staying",
      "staying too",
    ]);
    expect(screen.getByText("staying")).toBeInTheDocument();
    expect(container.querySelector(".toast-group-passing")).toBeNull();

    show("passing");
    fireEvent.click(closeAll("toast-group-sticky"));
    expect(useToastStore.getState().toasts.map((t) => t.message)).toEqual(["passing"]);
  });

  it("neither counts nor closes a toast that is closed alone", () => {
    render(<ToastContainer />);
    act(() => {
      useToastStore
        .getState()
        .addToast("check is off", "error", { sticky: true, closeAlone: true });
    });
    show("staying", true);
    // Two sticky toasts, but only one that the button would close.
    expect(screen.queryByRole("button", { name: /Close all/ })).not.toBeInTheDocument();

    show("staying too", true);
    fireEvent.click(screen.getByRole("button", { name: "Close all (2)" }));
    expect(useToastStore.getState().toasts.map((t) => t.message)).toEqual(["check is off"]);

    // Its own button still closes it.
    fireEvent.click(screen.getByRole("button", { name: "×" }));
    expect(screen.queryByText("check is off")).not.toBeInTheDocument();
  });

  /** Give a list a height, as jsdom lays nothing out, and record its scrolling. */
  function layOut(list: Element, scrollHeight: number, clientHeight: number): number[] {
    const scrolledTo: number[] = [];
    Object.defineProperty(list, "scrollHeight", { configurable: true, get: () => scrollHeight });
    Object.defineProperty(list, "clientHeight", { configurable: true, get: () => clientHeight });
    Object.defineProperty(list, "scrollTop", {
      configurable: true,
      get: () => scrolledTo[scrolledTo.length - 1] ?? 0,
      set: (top: number) => {
        scrolledTo.push(top);
      },
    });
    return scrolledTo;
  }

  function listOf(container: HTMLElement, group: string): Element {
    const list = container.querySelector(`.${group} > .toast-list`);
    if (list === null) throw new Error(`no list in ${group}`);
    return list;
  }

  it("scrolls the list to a new toast, and leaves it alone when one goes", () => {
    const { container } = render(<ToastContainer />);
    show("first");
    const scrolledTo = layOut(listOf(container, "toast-group-passing"), 900, 300);

    show("second");
    expect(scrolledTo).toEqual([900]);

    // The newest toast goes: the list stays where it is.
    act(() => {
      useToastStore.getState().removeToast(useToastStore.getState().toasts[1].id);
    });
    expect(screen.queryByText("second")).not.toBeInTheDocument();
    expect(scrolledTo).toEqual([900]);

    show("third");
    expect(scrolledTo).toEqual([900, 900]);
  });

  it("never scrolls the sticky toasts, nor any list for a sticky toast", () => {
    const { container } = render(<ToastContainer />);
    show("staying", true);
    show("passing");
    const sticky = layOut(listOf(container, "toast-group-sticky"), 900, 300);
    const passing = layOut(listOf(container, "toast-group-passing"), 900, 300);

    show("staying too", true);
    // The same sticky toast again is no new toast.
    show("staying", true);
    expect(sticky).toEqual([]);
    expect(passing).toEqual([]);

    // A new toast of the other kind scrolls its own list alone.
    show("another");
    expect(sticky).toEqual([]);
    expect(passing).toEqual([900]);
  });

  it("lets a list take the pointer only while it is too tall for its room", () => {
    const { container } = render(<ToastContainer />);
    show("staying", true);
    const list = listOf(container, "toast-group-sticky");
    expect(list).not.toHaveClass("toast-list-scrolls");

    // Measured again when the toasts change.
    layOut(list, 900, 300);
    show("staying too", true);
    expect(list).toHaveClass("toast-list-scrolls");

    // And when the window does: now there is room for all of it.
    layOut(list, 900, 900);
    act(() => {
      window.dispatchEvent(new Event("resize"));
    });
    expect(list).not.toHaveClass("toast-list-scrolls");
  });
});
