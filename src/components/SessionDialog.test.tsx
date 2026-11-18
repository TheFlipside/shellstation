/**
 * SessionDialog: a session's name and host are required fields. The webview
 * does not send the form with one of them empty and says so at the field; a
 * field of spaces alone is emptied, and the webview asked to say the same.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import i18n from "../i18n";
import type { Folder } from "../stores/sessionStore";
import { SessionDialog, type SessionFormData } from "./SessionDialog";

// The stores the dialog reads import invoke; nothing here may reach a backend.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const FOLDER: Folder = {
  id: "00000000-0000-4000-8000-0000000000f1",
  name: "Lab",
  parent_id: null,
  sort_order: 0,
  owner: "local",
  visibility: "personal",
};

beforeEach(async () => {
  await i18n.changeLanguage("en");
  vi.mocked(invoke).mockReset();
});

function open(onSubmit: (data: SessionFormData) => void = vi.fn()): void {
  render(
    <SessionDialog
      title="New Session"
      folders={[FOLDER]}
      sessions={[]}
      defaultFolderId={FOLDER.id}
      onSubmit={onSubmit}
      onCancel={vi.fn()}
      onManageCredentials={vi.fn()}
      onManageLoginSequences={vi.fn()}
    />,
  );
}

function field(id: string): HTMLInputElement {
  const element = document.getElementById(id);
  if (!(element instanceof HTMLInputElement)) throw new Error(`no field ${id}`);
  return element;
}

function type(id: string, value: string): void {
  fireEvent.change(field(id), { target: { value } });
}

function form(): HTMLFormElement {
  const element = document.querySelector("form");
  if (!element) throw new Error("no form");
  return element;
}

describe("the name and the host of a session", () => {
  it("are required fields", () => {
    open();
    expect(field("sd-name").required).toBe(true);
    expect(field("sd-host").required).toBe(true);
    // The webview does not send a form with one of them empty.
    expect(form().checkValidity()).toBe(false);
    type("sd-name", "core switch");
    expect(form().checkValidity()).toBe(false);
    type("sd-host", "switch.example");
    expect(form().checkValidity()).toBe(true);
  });

  it.each([
    ["the name", "   ", "switch.example", "sd-name"],
    ["the host", "core switch", " \t", "sd-host"],
  ] as const)("are empty with spaces alone: %s", (_what, name, host, emptied) => {
    const onSubmit = vi.fn();
    // Whether the form was valid at the moment the webview was asked about it.
    const validWhenAsked: boolean[] = [];
    const said = vi.spyOn(HTMLFormElement.prototype, "reportValidity").mockImplementation(function (
      this: HTMLFormElement,
    ) {
      validWhenAsked.push(this.checkValidity());
      return false;
    });
    try {
      open(onSubmit);
      type("sd-name", name);
      type("sd-host", host);
      // Spaces alone get past a webview's check of a required field.
      expect(form().checkValidity()).toBe(true);

      // `fireEvent.submit` sends the event without that check, as a submit
      // that got past it.
      fireEvent.submit(form());
      expect(onSubmit).not.toHaveBeenCalled();
      // The field is emptied, and the webview asked once it is: asked before,
      // it would have found nothing to say.
      expect(field(emptied).value).toBe("");
      expect(validWhenAsked).toEqual([false]);
    } finally {
      said.mockRestore();
    }
  });

  it("are emptied both where both hold spaces alone, and nothing is sent", () => {
    const onSubmit = vi.fn();
    const said = vi.spyOn(HTMLFormElement.prototype, "reportValidity").mockReturnValue(false);
    try {
      open(onSubmit);
      type("sd-name", "  ");
      type("sd-host", " ");
      fireEvent.submit(form());
      expect(field("sd-name").value).toBe("");
      expect(field("sd-host").value).toBe("");
      expect(said).toHaveBeenCalledTimes(1);
      expect(onSubmit).not.toHaveBeenCalled();
      // The dialog asks the backend nothing for this.
      expect(invoke).not.toHaveBeenCalled();
    } finally {
      said.mockRestore();
    }
  });

  it("are saved without the spaces around them", () => {
    const onSubmit = vi.fn<(data: SessionFormData) => void>();
    open(onSubmit);
    type("sd-name", "  core switch ");
    type("sd-host", " switch.example  ");
    fireEvent.submit(form());
    expect(onSubmit).toHaveBeenCalledTimes(1);
    expect(onSubmit.mock.calls[0][0]).toMatchObject({
      folderId: FOLDER.id,
      name: "core switch",
      hostname: "switch.example",
      port: 22,
    });
  });
});
