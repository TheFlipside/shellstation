/**
 * The forms of the two managers and of Quick Connect: a save is sent once,
 * however often it is asked for while it is under way; what a save, a file
 * dialog or the read of a stored password brings back is for the form it was
 * asked from; a file dialog that fails says so; Quick Connect passes on no
 * port outside 1 to 65535; and the steps of a login sequence are a group
 * with a name.
 */
import React from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import i18n from "../i18n";
import {
  useCredentialProfilesStore,
  type CredentialProfile,
} from "../stores/credentialProfilesStore";
import { useLoginSequenceStore, type LoginSequence } from "../stores/loginSequenceStore";
import { CredentialManager } from "./CredentialManager";
import { LoginSequenceManager } from "./LoginSequenceManager";
import { QuickConnect, type QuickConnectParams } from "./QuickConnect";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));

/** A promise settled from outside, for an answer that is still to come. */
interface Pending {
  promise: Promise<unknown>;
  resolve: (value: unknown) => void;
  reject: (reason: unknown) => void;
}

function pending(): Pending {
  let resolve: Pending["resolve"] = () => undefined;
  let reject: Pending["reject"] = () => undefined;
  const promise = new Promise<unknown>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

/** Answers of the backend by command; a list for whatever is not named. */
function backend(answers: Record<string, () => Promise<unknown>>): void {
  vi.mocked(invoke).mockImplementation((command: string) =>
    command in answers ? answers[command]() : Promise.resolve([]),
  );
}

/** How often a command was sent. */
function sent(command: string): number {
  return vi.mocked(invoke).mock.calls.filter(([name]) => name === command).length;
}

/** Let what has settled reach the component, through every step of a chain. */
async function settle(): Promise<void> {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

async function renderOpen(dialog: React.JSX.Element): Promise<void> {
  render(dialog);
  await settle();
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

function button(key: string): HTMLButtonElement {
  return screen.getByRole<HTMLButtonElement>("button", { name: i18n.t(key) });
}

/** Pick the option with this text in one of the app's selects. */
function choose(selectId: string, label: string): void {
  const select = document.getElementById(selectId);
  if (!select) throw new Error(`no select ${selectId}`);
  fireEvent.click(select);
  fireEvent.click(screen.getByRole("option", { name: label }));
}

/**
 * Two submits with nothing rendered between them, as from a double click:
 * the second one meets the form as the first one found it.
 */
function submitTwiceAtOnce(): void {
  act(() => {
    fireEvent.submit(form());
    fireEvent.submit(form());
  });
}

/** The message a manager shows with itself, in the strip of its own toasts. */
function shownWithDialog(message: string): boolean {
  return screen.getByText(message).closest(".toast-strip") !== null;
}

const SEQUENCE: LoginSequence = {
  id: "00000000-0000-4000-8000-0000000000c1",
  name: "cisco enable",
  send_initial_cr: true,
  steps: [{ pattern: "Password:", response: "", append_cr: true }],
  sort_order: 0,
};

const PROFILE: CredentialProfile = {
  id: "00000000-0000-4000-8000-0000000000c2",
  name: "lab admin",
  auth_type: "password",
  username: "",
  keychain_ref: "",
  key_path: "",
  sort_order: 0,
};

beforeEach(async () => {
  await i18n.changeLanguage("en");
  vi.mocked(invoke).mockReset();
  vi.mocked(open).mockReset();
  backend({});
  useLoginSequenceStore.setState({ sequences: [] });
  useCredentialProfilesStore.setState({ profiles: [] });
});

/** What the tests of a save need to know of a manager. */
interface Manager {
  name: string;
  render: () => React.JSX.Element;
  /** The button that opens the form for a new entry. */
  newEntry: string;
  nameField: string;
  nameRequired: string;
  /** Fill in what a new entry needs beside its name. */
  fill: () => void;
  list: string;
  create: string;
  update: string;
  /** An entry as the backend has it. */
  stored: { name: string };
}

const MANAGERS: Manager[] = [
  {
    name: "the login sequence manager",
    render: () => <LoginSequenceManager onClose={vi.fn()} />,
    newEntry: "loginSequences.newSequence",
    nameField: "lsm-name",
    nameRequired: "loginSequences.nameRequired",
    fill: () => {
      type("lsm-pattern-0", "Password:");
    },
    list: "login_sequence_list",
    create: "login_sequence_create",
    update: "login_sequence_update",
    stored: SEQUENCE,
  },
  {
    name: "the credential manager",
    render: () => <CredentialManager onClose={vi.fn()} />,
    newEntry: "credentialProfiles.newProfile",
    nameField: "cm-name",
    nameRequired: "credentialProfiles.nameRequired",
    fill: () => undefined,
    list: "credential_profile_list",
    create: "credential_profile_create",
    update: "credential_profile_update",
    stored: PROFILE,
  },
];

describe.each(MANAGERS)("a save in the form of $name", (manager) => {
  /** Open the form for a new entry and write one of this name into it. */
  function write(name: string): void {
    fireEvent.click(button(manager.newEntry));
    type(manager.nameField, name);
    manager.fill();
  }

  it("creates an entry once for a double click on Save", async () => {
    const save = pending();
    backend({ [manager.create]: () => save.promise });
    await renderOpen(manager.render());
    write(manager.stored.name);

    submitTwiceAtOnce();
    expect(sent(manager.create)).toBe(1);
    // One that comes later meets a button that says so.
    expect(button("dialog.save")).toBeDisabled();
    fireEvent.submit(form());
    expect(sent(manager.create)).toBe(1);

    save.resolve(manager.stored);
    await settle();
    // The form has closed, and the list has the one entry.
    expect(document.querySelector("form")).toBeNull();
    expect(screen.getAllByText(manager.stored.name)).toHaveLength(1);
  });

  it("changes an entry once for a double click on Save", async () => {
    const save = pending();
    backend({
      [manager.list]: () => Promise.resolve([manager.stored]),
      [manager.update]: () => save.promise,
      credential_profile_get_secret: () => Promise.resolve(""),
    });
    await renderOpen(manager.render());
    fireEvent.click(button("common.edit"));
    type(manager.nameField, "renamed");

    submitTwiceAtOnce();
    expect(button("dialog.save")).toBeDisabled();
    fireEvent.submit(form());
    expect(sent(manager.update)).toBe(1);

    save.resolve(null);
    await settle();
    expect(document.querySelector("form")).toBeNull();
    expect(screen.getAllByText("renamed")).toHaveLength(1);
  });

  it("can be asked for again after one that failed", async () => {
    const first = pending();
    const answers = [first.promise, Promise.resolve(manager.stored)];
    backend({ [manager.create]: () => answers.shift() ?? Promise.resolve(null) });
    await renderOpen(manager.render());
    write(manager.stored.name);

    fireEvent.submit(form());
    first.reject(new Error("database is locked"));
    await settle();
    // The form is still there, with what was typed and with the reason.
    expect(screen.getByText("database is locked").closest("form")).toBe(form());
    expect(field(manager.nameField).value).toBe(manager.stored.name);
    expect(button("dialog.save")).toBeEnabled();

    // The next try takes the reason away, and goes through.
    fireEvent.submit(form());
    expect(sent(manager.create)).toBe(2);
    expect(screen.queryByText("database is locked")).toBeNull();
    await settle();
    expect(document.querySelector("form")).toBeNull();
  });

  it("can be sent for another entry after one that went through", async () => {
    backend({ [manager.create]: () => Promise.resolve(manager.stored) });
    await renderOpen(manager.render());
    write(manager.stored.name);
    fireEvent.submit(form());
    await settle();
    expect(document.querySelector("form")).toBeNull();

    write("another entry");
    expect(button("dialog.save")).toBeEnabled();
    fireEvent.submit(form());
    expect(sent(manager.create)).toBe(2);
    await settle();
  });

  it("is not sent for a form that lacks a name, and is not held up by it", async () => {
    const save = pending();
    backend({ [manager.create]: () => save.promise });
    await renderOpen(manager.render());
    fireEvent.click(button(manager.newEntry));
    manager.fill();

    fireEvent.submit(form());
    expect(sent(manager.create)).toBe(0);
    expect(screen.getByText(i18n.t(manager.nameRequired))).toBeInTheDocument();
    expect(button("dialog.save")).toBeEnabled();

    type(manager.nameField, manager.stored.name);
    fireEvent.submit(form());
    expect(sent(manager.create)).toBe(1);
  });

  it("leaves a form alone that was opened while it was under way", async () => {
    const save = pending();
    backend({ [manager.create]: () => save.promise });
    await renderOpen(manager.render());
    write(manager.stored.name);
    fireEvent.submit(form());

    // The form is left and another one opened, before the save has its answer.
    fireEvent.click(button("dialog.cancel"));
    write("written since");
    // A manager sends one save at a time: this form's has to wait.
    expect(button("dialog.save")).toBeDisabled();
    fireEvent.submit(form());
    expect(sent(manager.create)).toBe(1);

    save.resolve(manager.stored);
    await settle();
    expect(field(manager.nameField).value).toBe("written since");
    expect(button("dialog.save")).toBeEnabled();
  });

  it("says with the manager that it failed, once its form was left for another", async () => {
    const save = pending();
    backend({ [manager.create]: () => save.promise });
    await renderOpen(manager.render());
    write(manager.stored.name);
    fireEvent.submit(form());

    fireEvent.click(button("dialog.cancel"));
    write("written since");
    save.reject(new Error("database is locked"));
    await settle();

    // Not in the form that is open, which it is not about.
    expect(field(manager.nameField).value).toBe("written since");
    expect(form().querySelector(".dialog-error")).toBeNull();
    expect(shownWithDialog("database is locked")).toBe(true);
    expect(button("dialog.save")).toBeEnabled();
  });

  it("says with the manager that it failed, once its form was left for the list", async () => {
    const save = pending();
    backend({ [manager.create]: () => save.promise });
    await renderOpen(manager.render());
    write(manager.stored.name);
    fireEvent.submit(form());

    fireEvent.keyDown(document.body, { key: "Escape" });
    expect(document.querySelector("form")).toBeNull();
    save.reject(new Error("database is locked"));
    await settle();
    expect(shownWithDialog("database is locked")).toBe(true);
  });
});

describe("the stored password of a profile", () => {
  /** The manager with one password profile, and the reads of its password. */
  async function editing(reads: Pending[]): Promise<void> {
    backend({
      credential_profile_list: () => Promise.resolve([PROFILE]),
      credential_profile_get_secret: () => reads.shift()?.promise ?? Promise.resolve(""),
    });
    await renderOpen(<CredentialManager onClose={vi.fn()} />);
    fireEvent.click(button("common.edit"));
  }

  it("is read into the form of its profile", async () => {
    const read = pending();
    await editing([read]);
    read.resolve("stored password");
    await settle();
    expect(field("cm-password").value).toBe("stored password");
    // In a field that does not show it.
    expect(field("cm-password").type).toBe("password");
  });

  it("is not put over a password that was typed before it came", async () => {
    const read = pending();
    await editing([read]);
    type("cm-password", "new password");
    read.resolve("stored password");
    await settle();
    expect(field("cm-password").value).toBe("new password");
  });

  it("is not put into a profile that was switched to a key before it came", async () => {
    const read = pending();
    await editing([read]);
    choose("cm-authtype", i18n.t("credentialProfiles.authKey"));
    read.resolve("stored password");
    await settle();
    // What the form holds for a password shows once it is one again.
    choose("cm-authtype", i18n.t("credentialProfiles.authPassword"));
    expect(field("cm-password").value).toBe("");
  });

  it("is for the form it was read for, not for one opened for the profile since", async () => {
    const first = pending();
    const second = pending();
    await editing([first, second]);
    fireEvent.click(button("dialog.cancel"));
    fireEvent.click(button("common.edit"));

    first.resolve("read for the form that was left");
    await settle();
    expect(field("cm-password").value).toBe("");
    second.resolve("stored password");
    await settle();
    expect(field("cm-password").value).toBe("stored password");
  });
});

describe("the file dialog for a key", () => {
  async function profileWithKey(): Promise<void> {
    await renderOpen(<CredentialManager onClose={vi.fn()} />);
    fireEvent.click(button("credentialProfiles.newProfile"));
    type("cm-name", "lab key");
    choose("cm-authtype", i18n.t("credentialProfiles.authKey"));
  }

  it("puts the chosen file into a credential profile, with what was typed since", async () => {
    const choice = pending();
    vi.mocked(open).mockReturnValue(choice.promise);
    await profileWithKey();

    fireEvent.click(button("credentialProfiles.keyPathBrowse"));
    type("cm-username", "admin");
    choice.resolve("C:\\keys\\lab");
    await settle();
    expect(field("cm-keypath").value).toBe("C:\\keys\\lab");
    expect(field("cm-username").value).toBe("admin");
  });

  it("does not open a credential profile's form again that was left meanwhile", async () => {
    const choice = pending();
    vi.mocked(open).mockReturnValue(choice.promise);
    await profileWithKey();

    fireEvent.click(button("credentialProfiles.keyPathBrowse"));
    fireEvent.click(button("dialog.cancel"));
    choice.resolve("C:\\keys\\lab");
    await settle();
    expect(document.querySelector("form")).toBeNull();
  });

  it("does not put it into another form that was opened meanwhile", async () => {
    const choice = pending();
    vi.mocked(open).mockReturnValue(choice.promise);
    await profileWithKey();

    fireEvent.click(button("credentialProfiles.keyPathBrowse"));
    fireEvent.click(button("dialog.cancel"));
    fireEvent.click(button("credentialProfiles.newProfile"));
    choose("cm-authtype", i18n.t("credentialProfiles.authKey"));
    choice.resolve("C:\\keys\\lab");
    await settle();
    expect(field("cm-keypath").value).toBe("");
  });

  it("says in a credential profile's form that it failed", async () => {
    vi.mocked(open).mockRejectedValue("the dialog could not be opened");
    await profileWithKey();

    fireEvent.click(button("credentialProfiles.keyPathBrowse"));
    await settle();
    expect(screen.getByText("the dialog could not be opened").closest("form")).toBe(form());
    expect(field("cm-keypath").value).toBe("");
  });

  it("says with the manager that it failed, once the form was saved", async () => {
    const choice = pending();
    vi.mocked(open).mockReturnValue(choice.promise);
    backend({ credential_profile_create: () => Promise.resolve({ ...PROFILE, name: "lab key" }) });
    await profileWithKey();

    fireEvent.click(button("credentialProfiles.keyPathBrowse"));
    fireEvent.submit(form());
    await settle();
    expect(document.querySelector("form")).toBeNull();

    choice.reject("the dialog could not be opened");
    await settle();
    expect(shownWithDialog("the dialog could not be opened")).toBe(true);
  });

  it("leaves the reason of a failed save in the form until a file is chosen", async () => {
    const choice = pending();
    vi.mocked(open).mockReturnValue(choice.promise);
    backend({ credential_profile_create: () => Promise.reject(new Error("keychain is locked")) });
    await profileWithKey();
    fireEvent.submit(form());
    await settle();
    expect(screen.getByText("keychain is locked")).toBeInTheDocument();

    fireEvent.click(button("credentialProfiles.keyPathBrowse"));
    expect(screen.getByText("keychain is locked")).toBeInTheDocument();
    choice.resolve("C:\\keys\\lab");
    await settle();
    expect(screen.queryByText("keychain is locked")).toBeNull();
    expect(field("cm-keypath").value).toBe("C:\\keys\\lab");
  });

  it("says in Quick Connect that it failed, until it is asked for again", async () => {
    vi.mocked(open).mockRejectedValueOnce(new Error("the dialog could not be opened"));
    vi.mocked(open).mockResolvedValueOnce("/home/lab/.ssh/id_ed25519");
    await renderOpen(<QuickConnect onConnect={vi.fn()} onCancel={vi.fn()} />);
    choose("qc-auth", i18n.t("session.authPublicKey"));

    fireEvent.click(button("session.keyPathBrowse"));
    await settle();
    expect(screen.getByText("the dialog could not be opened")).toBeInTheDocument();
    expect(field("qc-credential").value).toBe("");

    fireEvent.click(button("session.keyPathBrowse"));
    expect(screen.queryByText("the dialog could not be opened")).toBeNull();
    await settle();
    expect(field("qc-credential").value).toBe("/home/lab/.ssh/id_ed25519");
  });
});

describe("the port of Quick Connect", () => {
  /** What a submit passed on, with this in the port field. */
  async function connectWith(port: string): Promise<QuickConnectParams | null> {
    const onConnect = vi.fn<(params: QuickConnectParams) => void>();
    await renderOpen(<QuickConnect onConnect={onConnect} onCancel={vi.fn()} />);
    type("qc-host", "switch.example");
    type("qc-username", "admin");
    type("qc-port", port);
    // `fireEvent.submit` sends the event without the check a browser makes of
    // a field's `min` and `max` first, so what is tried is the form's own.
    fireEvent.submit(form());
    return onConnect.mock.calls.length > 0 ? onConnect.mock.calls[0][0] : null;
  }

  it.each(["0", "65536", "70000", "-1", "22.5"])("is not passed on as %s", async (port) => {
    expect(await connectWith(port)).toBeNull();
    expect(screen.getByText(i18n.t("session.portRange"))).toBeInTheDocument();
  });

  it.each([
    ["1", 1],
    ["2222", 2222],
    ["65535", 65535],
    // An empty field is the protocol's own port.
    ["", 22],
  ] as const)("is passed on for %j as %d", async (port, expected) => {
    expect((await connectWith(port))?.port).toBe(expected);
    expect(screen.queryByText(i18n.t("session.portRange"))).toBeNull();
  });

  it("is Telnet's own for an empty field of a Telnet connection", async () => {
    const onConnect = vi.fn<(params: QuickConnectParams) => void>();
    await renderOpen(<QuickConnect onConnect={onConnect} onCancel={vi.fn()} />);
    // The protocols are named as they are in every language.
    choose("qc-protocol", "Telnet");
    type("qc-host", "switch.example");
    type("qc-port", "");
    fireEvent.submit(form());
    expect(onConnect).toHaveBeenCalledTimes(1);
    expect(onConnect.mock.calls[0][0]).toMatchObject({ protocol: "telnet", port: 23 });
  });

  it("is no longer said to be wrong once it is changed, or the protocol is", async () => {
    expect(await connectWith("70000")).toBeNull();
    type("qc-port", "2222");
    expect(screen.queryByText(i18n.t("session.portRange"))).toBeNull();

    type("qc-port", "70000");
    fireEvent.submit(form());
    expect(screen.getByText(i18n.t("session.portRange"))).toBeInTheDocument();
    choose("qc-protocol", "Telnet");
    expect(screen.queryByText(i18n.t("session.portRange"))).toBeNull();
  });
});

describe("the steps of a login sequence", () => {
  it("are a group named by their heading, which is no field's label", async () => {
    await renderOpen(<LoginSequenceManager onClose={vi.fn()} />);
    fireEvent.click(button("loginSequences.newSequence"));

    const group = screen.getByRole("group", { name: i18n.t("loginSequences.stepsLabel") });
    expect(group).toContainElement(field("lsm-pattern-0"));
    const heading = screen.getByText(i18n.t("loginSequences.stepsLabel"));
    expect(heading.tagName).not.toBe("LABEL");
    // Every label in the form is some field's.
    const labels = Array.from(form().querySelectorAll("label"));
    expect(labels.length).toBeGreaterThan(0);
    for (const label of labels) expect(label.control).not.toBeNull();
  });
});
