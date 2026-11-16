/**
 * A menu is painted above the dialogs, so one that is open when a dialog
 * appears would lie over it: the host-key prompt comes when the host answers,
 * whatever is open then. A context menu and the list of a select close when a
 * dialog opens.
 */
import React from "react";
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { ContextMenu } from "./ContextMenu";
import { CustomSelect } from "./CustomSelect";
import { DialogKeys } from "./DialogKeys";

// The settings store imports invoke; nothing here may reach a backend.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

function Prompt(): React.JSX.Element {
  return (
    <div className="dialog-overlay" data-testid="prompt">
      <DialogKeys onEscape={vi.fn()} />
      <div className="dialog" />
    </div>
  );
}

describe("ContextMenu", () => {
  const items = [{ label: "Disconnect", onClick: vi.fn() }];

  it("closes when a dialog opens", () => {
    const onClose = vi.fn();
    const { rerender } = render(<ContextMenu x={10} y={10} items={items} onClose={onClose} />);
    expect(screen.getByRole("button", { name: "Disconnect" })).toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();

    rerender(
      <>
        <ContextMenu x={10} y={10} items={items} onClose={onClose} />
        <Prompt />
      </>,
    );
    expect(onClose).toHaveBeenCalled();
  });

  it("closes when a dialog opens together with it", () => {
    // The dialog comes first in the tree, and has said that it is there
    // before the menu listens.
    const onClose = vi.fn();
    render(
      <>
        <Prompt />
        <ContextMenu x={10} y={10} items={items} onClose={onClose} />
      </>,
    );
    expect(onClose).toHaveBeenCalled();
  });

  it("is not asked for from behind a dialog, where the keyboard still reaches", () => {
    // The menu key on what kept the focus when a dialog appeared.
    const behind = vi.fn();
    const inside = vi.fn();
    function Scenario({ prompt }: { prompt: boolean }): React.JSX.Element {
      return (
        <>
          <div data-testid="tab" onContextMenu={behind} />
          {prompt && (
            <div className="dialog-overlay">
              <DialogKeys onEscape={vi.fn()} />
              <div className="dialog" data-testid="field" onContextMenu={inside} />
            </div>
          )}
        </>
      );
    }

    const { rerender } = render(<Scenario prompt={true} />);
    fireEvent.contextMenu(screen.getByTestId("tab"));
    expect(behind).not.toHaveBeenCalled();
    // What is in the dialog on top is asked as before.
    fireEvent.contextMenu(screen.getByTestId("field"));
    expect(inside).toHaveBeenCalledTimes(1);

    rerender(<Scenario prompt={false} />);
    fireEvent.contextMenu(screen.getByTestId("tab"));
    expect(behind).toHaveBeenCalledTimes(1);
  });

  it("stays open in a dialog that was there before it, and when a dialog closes", () => {
    const onClose = vi.fn();
    const { rerender } = render(
      <>
        <Prompt />
        <span />
      </>,
    );
    rerender(
      <>
        <Prompt />
        <ContextMenu x={10} y={10} items={items} onClose={onClose} />
      </>,
    );
    rerender(
      <>
        <span />
        <ContextMenu x={10} y={10} items={items} onClose={onClose} />
      </>,
    );
    expect(screen.queryByTestId("prompt")).not.toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();
  });
});

describe("CustomSelect", () => {
  const options = [
    { value: "ssh", label: "SSH" },
    { value: "telnet", label: "Telnet" },
  ];

  it("closes its list when a dialog opens", () => {
    const select = <CustomSelect value="ssh" options={options} onChange={vi.fn()} />;
    const { rerender } = render(select);
    fireEvent.click(screen.getByRole("button", { name: /SSH/ }));
    expect(screen.getByRole("listbox")).toBeInTheDocument();

    rerender(
      <>
        {select}
        <Prompt />
      </>,
    );
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
  });

  it("keeps its list open in a dialog that renders again", () => {
    function Dialog({ note }: { note: string }): React.JSX.Element {
      return (
        <div className="dialog-overlay">
          <DialogKeys onEscape={vi.fn()} />
          <div className="dialog">
            <p>{note}</p>
            <CustomSelect value="ssh" options={options} onChange={vi.fn()} />
          </div>
        </div>
      );
    }
    const { rerender } = render(<Dialog note="first" />);
    fireEvent.click(screen.getByRole("button", { name: /SSH/ }));
    expect(screen.getByRole("listbox")).toBeInTheDocument();

    rerender(<Dialog note="second" />);
    expect(screen.getByText("second")).toBeInTheDocument();
    expect(screen.getByRole("listbox")).toBeInTheDocument();
  });
});
