/**
 * ToastContainer: toasts close on their own after the configured time,
 * except sticky ones, which stay until the user dismisses them. The sticky
 * toasts are a group of their own, above the others: each group has a button
 * that closes its toasts once there is more than one, and only the list of
 * the others scrolls to a new toast.
 *
 * Every dialog lies above the toasts. Under one, a stack takes no input and
 * no toast's time runs. What a dialog raises itself is shown with it, in a
 * strip on top of its box, until it is closed or the dialog is.
 */
import React from "react";
import ReactDOM from "react-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { useToastLayer, type ToastLayerHandle } from "../hooks/useToastLayer";
import i18n from "../i18n";
import { useSettingsStore } from "../stores/settingsStore";
import { useToastStore } from "../stores/toastStore";
import { DialogKeys } from "./DialogKeys";
import { SETTLE_MS, ToastContainer, ToastLayer } from "./ToastContainer";

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

/** A dialog that raises no toasts, as most do not. */
function Prompt({ name }: { name: string }): React.JSX.Element {
  return (
    <div className="dialog-overlay" data-testid={name}>
      <DialogKeys />
      <div className="dialog" />
    </div>
  );
}

/** What the dialog below raises its toasts with. */
let raise: ToastLayerHandle["addToast"];

/** A dialog that shows its own toasts, as Settings does. */
function Manager({
  onOverlayClick,
  onSave,
  children,
}: {
  /**
   * A click that gets as far as the overlay. No dialog's overlay listens
   * for one; this is to see whether a click the box refuses is passed on.
   */
  onOverlayClick?: () => void;
  /** A click on the button in the dialog's box. */
  onSave?: () => void;
  children?: React.ReactNode;
}): React.JSX.Element {
  const { layer, addToast } = useToastLayer();
  raise = addToast;
  return (
    <div
      className="dialog-overlay dialog-overlay-messages"
      data-testid="manager"
      onClick={onOverlayClick}
      role="presentation"
    >
      <DialogKeys />
      <ToastLayer layer={layer} />
      <div className="dialog">
        <button type="button" onClick={onSave}>
          Save
        </button>
      </div>
      {children}
    </div>
  );
}

function raiseOwn(message: string): void {
  act(() => {
    raise(message);
  });
}

/** The stack a toast is shown in. */
function stackOf(message: string): HTMLElement {
  const stack = screen.getByText(message).closest<HTMLElement>(".toast-stack");
  if (stack === null) throw new Error("the toast is in no stack");
  return stack;
}

describe("ToastContainer and dialogs", () => {
  it("lets no time run under a dialog, and all of it once the dialog is gone", () => {
    const { rerender } = render(
      <>
        <ToastContainer />
      </>,
    );
    show("passing");
    act(() => {
      vi.advanceTimersByTime(3000);
    });

    rerender(
      <>
        <ToastContainer />
        <Prompt name="host-key" />
      </>,
    );
    expect(stackOf("passing")).toHaveAttribute("inert");
    act(() => {
      vi.advanceTimersByTime(60_000);
    });
    expect(screen.getByText("passing")).toBeInTheDocument();

    rerender(
      <>
        <ToastContainer />
      </>,
    );
    expect(stackOf("passing")).not.toHaveAttribute("inert");
    act(() => {
      vi.advanceTimersByTime(4999);
    });
    expect(screen.getByText("passing")).toBeInTheDocument();
    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(screen.queryByText("passing")).not.toBeInTheDocument();
  });

  it("puts a toast that arrives under a dialog into a stack that waits", () => {
    render(
      <>
        <ToastContainer />
        <Prompt name="host-key" />
      </>,
    );
    show("import warning");
    expect(stackOf("import warning")).toHaveAttribute("inert");
    expect(within(screen.getByTestId("host-key")).queryByText("import warning")).toBeNull();
    act(() => {
      vi.advanceTimersByTime(60_000);
    });
    expect(screen.getByText("import warning")).toBeInTheDocument();
  });

  it("waits under a dialog that is rendered into the body, as a confirmation is", () => {
    const { rerender } = render(
      <>
        <ToastContainer />
        <Manager />
      </>,
    );
    show("connection failed");
    raiseOwn("delete failed");
    expect(stackOf("delete failed")).not.toHaveAttribute("inert");

    rerender(
      <>
        <ToastContainer />
        <Manager>{ReactDOM.createPortal(<Prompt name="confirm" />, document.body)}</Manager>
      </>,
    );
    expect(stackOf("connection failed")).toHaveAttribute("inert");
    expect(stackOf("delete failed")).toHaveAttribute("inert");

    rerender(
      <>
        <ToastContainer />
        <Manager />
      </>,
    );
    expect(stackOf("delete failed")).not.toHaveAttribute("inert");
  });

  it("shows a dialog's toasts over it in StrictMode, which mounts the dialog twice", () => {
    render(
      <React.StrictMode>
        <ToastContainer />
        <Manager />
      </React.StrictMode>,
    );
    raiseOwn("save failed");
    expect(within(screen.getByTestId("manager")).getByText("save failed")).toBeInTheDocument();
    expect(useToastStore.getState().layers).toHaveLength(1);
  });

  it("shows what a dialog raises with that dialog, and nothing else there", () => {
    render(
      <>
        <ToastContainer />
        <Manager />
      </>,
    );
    raiseOwn("save failed");
    show("connection failed");

    const manager = screen.getByTestId("manager");
    expect(within(manager).getByText("save failed")).toBeInTheDocument();
    expect(within(manager).queryByText("connection failed")).toBeNull();
    // Its own dialog does not lie over it: it can be used.
    expect(stackOf("save failed")).not.toHaveAttribute("inert");
    expect(stackOf("connection failed")).toHaveAttribute("inert");
    // It is a strip of the dialog, before its box, and not the stack that
    // floats at the window's edge.
    expect(stackOf("save failed")).toHaveClass("toast-strip");
    expect(stackOf("save failed")).not.toHaveClass("toast-container");
    expect(stackOf("save failed").nextElementSibling).toHaveClass("dialog");
    expect(stackOf("connection failed")).toHaveClass("toast-container");

    // The window's toast waits under the dialog for as long as it is open.
    act(() => {
      vi.advanceTimersByTime(60_000);
    });
    expect(screen.getByText("connection failed")).toBeInTheDocument();
  });

  it("takes no click in a dialog's box right after its strip has changed", () => {
    // The box moves when a toast comes or goes, and a control of the dialog
    // comes to lie where the pointer is.
    const onSave = vi.fn();
    const onOverlayClick = vi.fn();
    render(<Manager onSave={onSave} onOverlayClick={onOverlayClick} />);
    const save = screen.getByRole("button", { name: "Save" });
    const settle = (): void => {
      act(() => {
        vi.advanceTimersByTime(SETTLE_MS);
      });
    };

    // A click the box takes is passed on, and would be heard at the overlay.
    fireEvent.click(save);
    expect(onSave).toHaveBeenCalledTimes(1);
    expect(onOverlayClick).toHaveBeenCalledTimes(1);
    onOverlayClick.mockClear();

    // A toast comes: the click that was on its way is refused at the box. It
    // does not reach the button and is not passed on.
    raiseOwn("save failed");
    expect(fireEvent.mouseDown(save)).toBe(false);
    expect(fireEvent.click(save)).toBe(false);
    expect(onSave).toHaveBeenCalledTimes(1);
    expect(onOverlayClick).not.toHaveBeenCalled();
    act(() => {
      vi.advanceTimersByTime(SETTLE_MS - 1);
    });
    fireEvent.click(save);
    expect(onSave).toHaveBeenCalledTimes(1);
    act(() => {
      vi.advanceTimersByTime(1);
    });
    fireEvent.click(save);
    expect(onSave).toHaveBeenCalledTimes(2);

    // The last toast is closed: the second click of a double click on its
    // button is on whatever has moved there.
    fireEvent.click(screen.getByRole("button", { name: "×" }));
    fireEvent.click(save);
    expect(onSave).toHaveBeenCalledTimes(2);
    settle();
    fireEvent.click(save);
    expect(onSave).toHaveBeenCalledTimes(3);

    // A toast in the window's stack moves nothing in the dialog.
    show("connection failed");
    fireEvent.click(save);
    expect(onSave).toHaveBeenCalledTimes(4);
  });

  it("keeps what a dialog raises until it is closed, or the dialog is", () => {
    // A toast that went by itself would move what the dialog holds.
    const { rerender } = render(
      <>
        <ToastContainer />
        <Manager />
      </>,
    );
    raiseOwn("save failed");
    raiseOwn("imported");
    act(() => {
      vi.advanceTimersByTime(600_000);
    });
    expect(screen.getByText("save failed")).toBeInTheDocument();

    // Its own button closes it.
    fireEvent.click(within(stackOf("save failed")).getAllByRole("button", { name: "×" })[0]);
    expect(screen.queryByText("save failed")).not.toBeInTheDocument();
    expect(screen.getByText("imported")).toBeInTheDocument();

    // In the window's stack it has its time, like any other.
    rerender(
      <>
        <ToastContainer />
      </>,
    );
    expect(stackOf("imported")).toHaveClass("toast-container");
    act(() => {
      vi.advanceTimersByTime(5000);
    });
    expect(screen.queryByText("imported")).not.toBeInTheDocument();
  });

  it("covers a dialog's toasts with a dialog on top of it, not with one under it", () => {
    const { rerender } = render(
      <>
        <ToastContainer />
        <Prompt name="under" />
        <Manager />
      </>,
    );
    raiseOwn("save failed");
    expect(stackOf("save failed")).not.toHaveAttribute("inert");

    // A host key prompt arrives on top.
    rerender(
      <>
        <ToastContainer />
        <Prompt name="under" />
        <Manager />
        <Prompt name="host-key" />
      </>,
    );
    expect(stackOf("save failed")).toHaveAttribute("inert");
    expect(within(screen.getByTestId("host-key")).queryByText("save failed")).toBeNull();
    act(() => {
      vi.advanceTimersByTime(60_000);
    });
    expect(screen.getByText("save failed")).toBeInTheDocument();
  });

  it("covers a dialog's toasts with a dialog opened inside its overlay", () => {
    const { rerender } = render(<Manager />);
    raiseOwn("save failed");
    rerender(
      <Manager>
        <Prompt name="nested" />
      </Manager>,
    );
    expect(stackOf("save failed")).toHaveAttribute("inert");
  });

  it("hands a closing dialog's toasts to the window's stack, with their time in full", () => {
    const { rerender, container } = render(
      <>
        <ToastContainer />
        <Manager />
      </>,
    );
    raiseOwn("imported");
    act(() => {
      vi.advanceTimersByTime(4000);
    });

    rerender(
      <>
        <ToastContainer />
      </>,
    );
    expect(container.querySelectorAll(".toast-container")).toHaveLength(1);
    expect(stackOf("imported")).not.toHaveAttribute("inert");
    act(() => {
      vi.advanceTimersByTime(4999);
    });
    expect(screen.getByText("imported")).toBeInTheDocument();
    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(screen.queryByText("imported")).not.toBeInTheDocument();
  });

  it("shows what a dialog reports after it has closed in the window's stack", () => {
    const { rerender } = render(
      <>
        <ToastContainer />
        <Manager />
      </>,
    );
    rerender(
      <>
        <ToastContainer />
      </>,
    );
    raiseOwn("import done");
    expect(useToastStore.getState().toasts.map((t) => t.layer)).toEqual([null]);
    expect(screen.getByText("import done")).toBeInTheDocument();
  });

  it("closes the toasts of one stack with that stack's button", () => {
    render(
      <>
        <ToastContainer />
        <Manager />
      </>,
    );
    raiseOwn("warning a");
    raiseOwn("warning b");
    show("connection failed");
    show("connection failed again");

    const manager = screen.getByTestId("manager");
    fireEvent.click(within(manager).getByRole("button", { name: "Close all (2)" }));
    expect(useToastStore.getState().toasts.map((t) => t.message)).toEqual([
      "connection failed",
      "connection failed again",
    ]);
  });
});
