/**
 * QuittingOverlay: shown while the backend closes the sessions before the
 * app exits. It must say so, offer nothing to click, and keep the keyboard
 * from a terminal, whose session is closing, and from the app's shortcuts.
 */
import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { QuittingOverlay } from "./QuittingOverlay";

vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));

describe("QuittingOverlay", () => {
  it("says that the sessions are closing, as an alert with no buttons, over everything", () => {
    const { container } = render(<QuittingOverlay />);
    const dialog = screen.getByRole("alertdialog", { name: "settings.quittingTitle" });
    expect(dialog).toHaveAccessibleDescription("settings.quittingMessage");
    expect(screen.getByRole("progressbar")).toBeInTheDocument();
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
    // In a portal of its own, under the class that puts it above the toasts.
    expect(container).toBeEmptyDOMElement();
    expect(dialog.closest(".dialog-overlay-quitting")).not.toBeNull();
  });

  it("takes the focus from a terminal into itself", () => {
    const terminal = document.createElement("textarea");
    document.body.appendChild(terminal);
    try {
      terminal.focus();
      render(<QuittingOverlay />);
      expect(screen.getByRole("alertdialog")).toHaveFocus();
    } finally {
      terminal.remove();
    }
  });

  it.each(["keydown", "keyup", "keypress"])(
    "keeps every %s from the app while it shows, and gives them back after",
    (type) => {
      const handler = vi.fn();
      document.addEventListener(type, handler, true);
      try {
        const { unmount } = render(<QuittingOverlay />);
        const key = new KeyboardEvent(type, { key: "t", ctrlKey: true, cancelable: true });
        document.dispatchEvent(key);
        expect(handler).not.toHaveBeenCalled();
        expect(key.defaultPrevented).toBe(true);

        unmount();
        document.dispatchEvent(new KeyboardEvent(type, { key: "t", ctrlKey: true }));
        expect(handler).toHaveBeenCalledTimes(1);
      } finally {
        document.removeEventListener(type, handler, true);
      }
    },
  );
});
