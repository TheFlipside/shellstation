/**
 * The sidebar's search field and its menu.
 *
 * The webview's context menu is suppressed for the whole window, so a
 * right-click on the field pasted nothing. The field has a menu of the app's
 * own instead, with Copy and Paste. The contract pinned here: Copy takes the
 * selection and nothing else, Paste goes where the caret is, as one line and
 * no longer than the field takes, and the clipboard is read only on a click on
 * Paste and never typed in behind a dialog.
 */
import React, { useState } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import { DialogKeys } from "./DialogKeys";
import { MAX_QUERY_LENGTH, SearchField, singleLine } from "./SearchField";

// The settings store imports invoke; nothing here may reach a backend.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({
  readText: vi.fn(() => Promise.resolve("")),
  writeText: vi.fn(() => Promise.resolve()),
}));
// Translations resolve to their keys, so assertions can match on key names.
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));
vi.mock("../i18n", () => ({
  default: { t: (key: string) => key, changeLanguage: vi.fn() },
}));

const onEscape = vi.fn();

/** The field as the sidebar holds it: its text is the parent's. */
function Sidebar({
  initial = "",
  dialog = false,
}: {
  initial?: string;
  dialog?: boolean;
}): React.JSX.Element {
  const [value, setValue] = useState(initial);
  return (
    <>
      <SearchField value={value} placeholder="Search" onChange={setValue} onEscape={onEscape} />
      {dialog && (
        <div className="dialog-overlay">
          <DialogKeys onEscape={vi.fn()} />
          <div className="dialog" />
        </div>
      )}
    </>
  );
}

function field(): HTMLInputElement {
  return screen.getByPlaceholderText<HTMLInputElement>("Search");
}

function copyItem(): HTMLElement {
  return screen.getByRole("button", { name: "contextMenu.copy" });
}

function pasteItem(): HTMLElement {
  return screen.getByRole("button", { name: "contextMenu.paste" });
}

/** Click Paste and let the clipboard answer. */
async function clickPaste(): Promise<void> {
  await act(async () => {
    fireEvent.click(pasteItem());
    await Promise.resolve();
  });
}

beforeEach(() => {
  vi.mocked(readText).mockResolvedValue("");
});

describe("singleLine", () => {
  it("drops the line breaks around the text and joins the lines inside", () => {
    expect(singleLine("router-1\r\n")).toBe("router-1");
    expect(singleLine("\n\nrouter-1\n")).toBe("router-1");
    expect(singleLine("core\r\n\r\nedge\nlab")).toBe("core edge lab");
    expect(singleLine("\r\n")).toBe("");
    expect(singleLine(" a b ")).toBe(" a b ");
  });

  it("takes a tab and the other control characters for line breaks", () => {
    expect(singleLine("core\tedge")).toBe("core edge");
    expect(singleLine("\u0000core\u0000\u001b\u007f\u0085edge\u009f")).toBe("core edge");
    // Half of a character of two units, without its other half.
    expect(singleLine("core\ud800edge")).toBe("core edge");
    expect(singleLine("\udc00core\ud800")).toBe("core");
    expect(singleLine("a\ud83d😀\ude00b")).toBe("a \u{1F600} b");
    // Not what a name can have: letters of any script, and a space.
    expect(singleLine("Zürich 東京  ")).toBe("Zürich 東京  ");
  });

  it("takes a long run of line breaks in its stride", () => {
    // A pattern anchored at the end would start over at each of them.
    const run = "\n".repeat(200_000);
    expect(singleLine(`${run}core${run}edge${run}`)).toBe("core edge");
    expect(singleLine(`${run}core${run}edge${run}`, 6)).toBe("core e");
  });

  it("cuts the text to the room there is", () => {
    expect(singleLine("router", 10)).toBe("router");
    expect(singleLine("router", 6)).toBe("router");
    expect(singleLine("router", 3)).toBe("rou");
    expect(singleLine("router", 0)).toBe("");
    expect(singleLine("router", -4)).toBe("");
    expect(singleLine("\n\ncore\nedge\n", 7)).toBe("core ed");
  });

  it("ends neither in a space for a line that did not fit nor in half a character", () => {
    expect(singleLine("core\nedge", 4)).toBe("core");
    expect(singleLine("core\nedge", 5)).toBe("core");
    expect(singleLine("core\nedge", 6)).toBe("core e");
    // A space of the text's own stays, also as the last that fits.
    expect(singleLine("ab \nc", 3)).toBe("ab ");
    expect(singleLine("core\n\u{1F600}", 6)).toBe("core");
    // U+1F600 takes two units.
    expect(singleLine("ab\u{1F600}", 3)).toBe("ab");
    expect(singleLine("ab\u{1F600}", 4)).toBe("ab\u{1F600}");
    expect(singleLine("\u{1F600}", 1)).toBe("");
    expect(singleLine("a\n\u{1F600}", 3)).toBe("a");
  });
});

describe("SearchField", () => {
  it("opens a menu with Copy and Paste on a right-click, and nothing else", () => {
    render(<Sidebar />);
    expect(screen.queryByRole("button")).not.toBeInTheDocument();

    // Refused, so that no menu of the webview's comes with it.
    expect(fireEvent.contextMenu(field())).toBe(false);
    expect(screen.getAllByRole("button").map((button) => button.textContent)).toEqual([
      "contextMenu.copy",
      "contextMenu.paste",
    ]);
    // Opening the menu reads nothing.
    expect(readText).not.toHaveBeenCalled();
  });

  it("copies the selection, and offers no Copy without one", () => {
    render(<Sidebar initial="core-router-1" />);
    fireEvent.contextMenu(field());
    expect(copyItem()).toBeDisabled();
    fireEvent.keyDown(document, { key: "Escape" });

    field().setSelectionRange(5, 11);
    fireEvent.contextMenu(field());
    expect(copyItem()).toBeEnabled();
    fireEvent.click(copyItem());

    expect(writeText).toHaveBeenCalledTimes(1);
    expect(writeText).toHaveBeenCalledWith("router");
    expect(field()).toHaveValue("core-router-1");
    expect(field()).toHaveFocus();
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it("pastes at the caret and puts the caret behind what was pasted", async () => {
    vi.mocked(readText).mockResolvedValue("router-");
    render(<Sidebar initial="core-1" />);
    field().setSelectionRange(5, 5);
    fireEvent.contextMenu(field());
    await clickPaste();

    expect(field()).toHaveValue("core-router-1");
    expect(field().selectionStart).toBe(12);
    expect(field().selectionEnd).toBe(12);
    expect(field()).toHaveFocus();
    expect(writeText).not.toHaveBeenCalled();
  });

  it("pastes over the selection", async () => {
    vi.mocked(readText).mockResolvedValue("\r\nedge\r\n");
    render(<Sidebar initial="core-router-1" />);
    field().setSelectionRange(0, 4);
    fireEvent.contextMenu(field());
    await clickPaste();

    expect(field()).toHaveValue("edge-router-1");
    expect(field().selectionStart).toBe(4);
  });

  it("pastes into an empty field, as one line", async () => {
    vi.mocked(readText).mockResolvedValue("10.0.0.1\r\n");
    render(<Sidebar />);
    fireEvent.contextMenu(field());
    await clickPaste();
    expect(field()).toHaveValue("10.0.0.1");
  });

  it("leaves the field alone when the clipboard holds no text or cannot be read", async () => {
    render(<Sidebar initial="core" />);
    vi.mocked(readText).mockResolvedValue("\n");
    fireEvent.contextMenu(field());
    await clickPaste();
    expect(field()).toHaveValue("core");

    vi.mocked(readText).mockRejectedValue(new Error("no clipboard"));
    fireEvent.contextMenu(field());
    await clickPaste();
    expect(field()).toHaveValue("core");
  });

  it("pastes nothing when a dialog has opened before the clipboard answers", async () => {
    let answer: ((text: string) => void) | undefined;
    vi.mocked(readText).mockReturnValue(
      new Promise<string>((resolve) => {
        answer = resolve;
      }),
    );
    const { rerender } = render(<Sidebar initial="core" />);
    fireEvent.contextMenu(field());
    fireEvent.click(pasteItem());

    expect(readText).toHaveBeenCalledTimes(1);

    rerender(<Sidebar initial="core" dialog={true} />);
    await act(async () => {
      answer?.("-router");
      await Promise.resolve();
    });
    expect(field()).toHaveValue("core");
  });

  it("pastes nothing into a field that is gone when the clipboard answers", async () => {
    let answer: ((text: string) => void) | undefined;
    vi.mocked(readText).mockReturnValue(
      new Promise<string>((resolve) => {
        answer = resolve;
      }),
    );
    const onChange = vi.fn();
    const { unmount } = render(
      <SearchField value="core" placeholder="Search" onChange={onChange} onEscape={onEscape} />,
    );
    fireEvent.contextMenu(field());
    fireEvent.click(pasteItem());
    unmount();
    await act(async () => {
      answer?.("-router");
      await Promise.resolve();
    });
    expect(onChange).not.toHaveBeenCalled();
  });

  it("takes no more text than a search goes by, typed or pasted", async () => {
    const almost = "a".repeat(MAX_QUERY_LENGTH - 2);
    vi.mocked(readText).mockResolvedValue("router");
    render(<Sidebar initial={almost} />);
    // The limit for what is typed, and for a paste from the keyboard.
    expect(field()).toHaveAttribute("maxlength", String(MAX_QUERY_LENGTH));

    field().setSelectionRange(almost.length, almost.length);
    fireEvent.contextMenu(field());
    await clickPaste();
    expect(field()).toHaveValue(`${almost}ro`);

    // Full: nothing more goes in at the caret, but a selection makes room.
    fireEvent.contextMenu(field());
    await clickPaste();
    expect(field()).toHaveValue(`${almost}ro`);

    field().setSelectionRange(0, 4);
    fireEvent.contextMenu(field());
    await clickPaste();
    expect(field()).toHaveValue(`rout${almost.slice(4)}ro`);
    expect(field().selectionStart).toBe(4);
  });

  it("pastes no half of a character into the last place", async () => {
    // U+1F600 takes two units, and one is left.
    const almost = "a".repeat(MAX_QUERY_LENGTH - 1);
    vi.mocked(readText).mockResolvedValue("\u{1F600}");
    render(<Sidebar initial={almost} />);
    field().setSelectionRange(almost.length, almost.length);
    fireEvent.contextMenu(field());
    await clickPaste();
    expect(field()).toHaveValue(almost);
  });

  it("leaves the field as it is when the clipboard cannot be written", async () => {
    vi.mocked(writeText).mockRejectedValueOnce(new Error("no clipboard"));
    render(<Sidebar initial="core" />);
    field().setSelectionRange(0, 4);
    fireEvent.contextMenu(field());
    await act(async () => {
      fireEvent.click(copyItem());
      await Promise.resolve();
    });
    expect(writeText).toHaveBeenCalledWith("core");
    expect(field()).toHaveValue("core");
  });

  it("closes its menu when a dialog opens", () => {
    const { rerender } = render(<Sidebar />);
    fireEvent.contextMenu(field());
    expect(pasteItem()).toBeInTheDocument();
    rerender(<Sidebar dialog={true} />);
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it("opens no menu from behind a dialog, where the keyboard still reaches", () => {
    render(<Sidebar dialog={true} />);
    expect(fireEvent.contextMenu(field())).toBe(false);
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it("gives Escape to the menu first, and then to the search", () => {
    render(<Sidebar initial="core" />);
    fireEvent.contextMenu(field());
    fireEvent.keyDown(field(), { key: "Escape" });
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
    expect(onEscape).not.toHaveBeenCalled();

    fireEvent.keyDown(field(), { key: "Escape" });
    expect(onEscape).toHaveBeenCalledTimes(1);
  });

  it("closes its menu when the field is typed in", () => {
    render(<Sidebar initial="core" />);
    fireEvent.contextMenu(field());
    fireEvent.keyDown(field(), { key: "ArrowLeft", shiftKey: true });
    expect(screen.queryByRole("button")).not.toBeInTheDocument();

    fireEvent.contextMenu(field());
    fireEvent.change(field(), { target: { value: "cores" } });
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
    expect(field()).toHaveValue("cores");
  });
});
