/**
 * The user's own login for another user's shared session.
 *
 * What is picked here decides which of the user's secrets a connection to
 * that session sends, so the dialog must show the session's host as its owner
 * wrote it, list every jump host the user needs a profile for, write only the
 * user's own changes, and never clear a mapping it has not read. And since
 * the owner can change the session afterwards, a save confirms what was
 * shown before it assigns anything, and the dialog says where something
 * assigned waits to be confirmed.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { SessionLoginDialog } from "./SessionLoginDialog";
import type { Session } from "../stores/sessionStore";
import { useCredentialProfilesStore } from "../stores/credentialProfilesStore";
import { useLoginSequenceStore } from "../stores/loginSequenceStore";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
// Echoes the key and its options, so tests can see what was interpolated.
vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, options?: Record<string, unknown>) =>
      options ? `${key} ${JSON.stringify(options)}` : key,
    i18n: { language: "en" },
  }),
}));

const TARGET = "00000000-0000-4000-8000-0000000000a0";
const HOP = "00000000-0000-4000-8000-0000000000a1";
const PROFILE_A = "00000000-0000-4000-8000-0000000000b1";
const PROFILE_B = "00000000-0000-4000-8000-0000000000b2";
const SEQUENCE = "00000000-0000-4000-8000-0000000000c1";

function session(overrides: Partial<Session> = {}): Session {
  return {
    id: TARGET,
    folder_id: "00000000-0000-4000-8000-000000000002",
    name: "core-switch",
    hostname: "switch.example",
    port: 2222,
    protocol: "ssh",
    username: "",
    auth_method: "profile",
    jump_host_id: null,
    tags: "[]",
    icon: "desktop",
    sort_order: 0,
    highlight_profile_id: null,
    credential_profile_id: null,
    login_sequence_id: null,
    legacy_algorithms: false,
    owner: "bob",
    visibility: "shared",
    ...overrides,
  };
}

const WITH_HOP = session({ jump_host_id: HOP });
const BASTION = session({ id: HOP, name: "bastion", hostname: "bastion.example", port: 22 });

type Args = Record<string, string | null>;

/**
 * What was confirmed of a session, as the backend answers: all of it for the
 * session a dialog is for, and for a jump host the host, the port and the
 * jump host it goes on to, with the rest `null`.
 */
interface Pin {
  protocol: string | null;
  hostname: string;
  port: number;
  username: string | null;
  legacy_algorithms: boolean | null;
  jump_host_id: string | null;
}

/** What is confirmed of a session as the one a dialog is for. */
function pinOf(s: Session): Pin {
  return {
    protocol: s.protocol,
    hostname: s.hostname,
    port: s.port,
    username: s.username,
    legacy_algorithms: s.legacy_algorithms,
    jump_host_id: s.jump_host_id,
  };
}

/** What the dialog sends of a session when it confirms it. */
function shownOf(s: Session): { id: string } & Pin {
  return { id: s.id, ...pinOf(s) };
}

/** What is confirmed of a session as a jump host. */
function hopPinOf(s: Session): Pin {
  return { ...pinOf(s), protocol: null, username: null, legacy_algorithms: null };
}

/**
 * Answers the mapping commands from `stored`, keyed by command and session,
 * and writes what is set back into it. `fail` names a command that rejects.
 * `pins` is what has been confirmed, by session, and takes what is confirmed.
 */
function backend(
  stored: Record<string, string>,
  fail?: string,
  pins: Record<string, Pin> = {},
): void {
  vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
    const a = (args ?? {}) as Args;
    if (cmd === fail) return Promise.reject(new Error(`${cmd} failed`));
    if (cmd === "session_login_confirmed") {
      const ids = (args as { sessionIds: string[] }).sessionIds;
      return Promise.resolve(ids.map((id) => pins[id] ?? null));
    }
    if (cmd === "session_login_confirm") {
      // The first as the session the dialog is for, the others as jump hosts.
      (args as { sessions: ({ id: string } & Pin)[] }).sessions.forEach(({ id, ...pin }, i) => {
        pins[id] =
          i === 0 ? pin : { ...pin, protocol: null, username: null, legacy_algorithms: null };
      });
      return Promise.resolve(null);
    }
    const sessionId = a.sessionId ?? "";
    if (cmd === "get_session_credential") {
      return Promise.resolve(stored[`profile:${sessionId}`] ?? null);
    }
    if (cmd === "get_session_login_sequence") {
      return Promise.resolve(stored[`sequence:${sessionId}`] ?? null);
    }
    if (cmd === "set_session_credential") {
      stored[`profile:${sessionId}`] = a.credentialProfileId ?? "";
      return Promise.resolve(null);
    }
    if (cmd === "set_session_login_sequence") {
      stored[`sequence:${sessionId}`] = a.loginSequenceId ?? "";
      return Promise.resolve(null);
    }
    return Promise.reject(new Error(`unexpected command ${cmd}`));
  });
}

function writes(): [string, Args][] {
  return vi
    .mocked(invoke)
    .mock.calls.filter(([cmd]) => cmd.startsWith("set_"))
    .map(([cmd, args]) => [cmd, args as Args]);
}

/** The notice that something assigned waits to be confirmed, if it is shown. */
function notice(): HTMLElement | null {
  return document.getElementById("sld-to-confirm");
}

/** The names of the commands called so far, in order. */
function commands(): string[] {
  return vi.mocked(invoke).mock.calls.map(([cmd]) => cmd);
}

/** What each call to confirm passed as the sessions shown. */
function confirmations(): unknown[] {
  return vi
    .mocked(invoke)
    .mock.calls.filter(([cmd]) => cmd === "session_login_confirm")
    .map(([, args]) => (args as { sessions: unknown }).sessions);
}

function select(selectId: string): HTMLElement {
  const trigger = document.getElementById(selectId);
  if (!trigger) throw new Error(`no select ${selectId}`);
  return trigger;
}

function pick(selectId: string, optionLabel: string): void {
  fireEvent.click(select(selectId));
  fireEvent.click(screen.getByRole("option", { name: optionLabel }));
}

function saveButton(): HTMLButtonElement {
  return screen.getByRole("button", { name: "dialog.save" });
}

function cancelButton(): HTMLButtonElement {
  return screen.getByRole("button", { name: "dialog.cancel" });
}

function form(): HTMLFormElement {
  const el = screen.getByRole("dialog").querySelector("form");
  if (!el) throw new Error("no form");
  return el;
}

/** A promise that the test settles when it chooses to. */
function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolve: (value: T) => void = () => undefined;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

function open(target: Session, sessions: Session[], onClose = vi.fn()): typeof onClose {
  render(
    <SessionLoginDialog
      session={target}
      sessions={sessions}
      onClose={onClose}
      onManageCredentials={vi.fn()}
      onManageLoginSequences={vi.fn()}
    />,
  );
  return onClose;
}

beforeEach(() => {
  useCredentialProfilesStore.setState({
    profiles: [
      {
        id: PROFILE_A,
        name: "radius",
        auth_type: "password",
        username: "alice",
        keychain_ref: "",
        key_path: "",
        sort_order: 0,
      },
      {
        id: PROFILE_B,
        name: "bastion-key",
        auth_type: "key",
        username: "",
        keychain_ref: "",
        key_path: "",
        sort_order: 1,
      },
    ],
  });
  useLoginSequenceStore.setState({
    sequences: [{ id: SEQUENCE, name: "enable", send_initial_cr: false, steps: [], sort_order: 0 }],
  });
});

describe("SessionLoginDialog", () => {
  it("names the owner, and shows protocol, host and port each by itself", async () => {
    backend({});
    open(session(), [session()]);
    expect(screen.getByRole("dialog")).toHaveTextContent('sessionLogin.info {"owner":"bob"}');
    expect(document.getElementById("sld-protocol")).toHaveTextContent(/^ssh$/);
    expect(document.getElementById("sld-host")).toHaveTextContent(/^switch\.example$/);
    expect(document.getElementById("sld-port")).toHaveTextContent(/^2222$/);
    expect(document.getElementById("sld-username")?.textContent).toBe("");
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
  });

  it("shows the session's own user name, with a backslash of its own doubled", async () => {
    backend({});
    // A domain's user name, and one that reads like a character written out.
    const domain = session({ username: "CORP\\admin" });
    const { unmount } = render(
      <SessionLoginDialog
        session={domain}
        sessions={[domain]}
        onClose={vi.fn()}
        onManageCredentials={vi.fn()}
        onManageLoginSequences={vi.fn()}
      />,
    );
    expect(document.getElementById("sld-username")?.textContent).toBe("CORP\\\\admin");
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    unmount();
    const odd = session({ username: "\\u{0430}dmin \u0430" });
    open(odd, [odd]);
    expect(document.getElementById("sld-username")?.textContent).toBe("\\\\u{0430}dmin \\u{0430}");
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
  });

  it("takes whatever is not Telnet for SSH, as a connect does", async () => {
    // A protocol the app would not have saved. The backend connects to it
    // over SSH, with the profile assigned to it and through its jump host.
    const odd = session({ protocol: "ssh2", jump_host_id: HOP });
    backend({ [`profile:${TARGET}`]: PROFILE_A });
    open(odd, [odd, BASTION]);
    await waitFor(() => {
      expect(document.getElementById("sld-profile")).toHaveTextContent("radius (alice)");
    });
    expect(document.getElementById("sld-protocol")?.textContent).toBe("ssh2");
    expect(document.getElementById("sld-hop-host-0")).toHaveTextContent(/^bastion\.example$/);
    expect(screen.getByRole("checkbox")).toBeInTheDocument();
    // What is assigned to it shows as waiting to be confirmed.
    expect(notice()).toHaveTextContent("sessionLogin.notConfirmed");
  });

  it("writes out what would change how a name, host or owner displays", async () => {
    backend({});
    // A right-to-left override in front of a reversed host name, a zero-width
    // space in the name, and a line break in the owner.
    const spoofed = session({
      name: "core\u200bswitch",
      hostname: "\u202emoc.elpmaxe.live",
      owner: "bob\nYou",
      jump_host_id: HOP,
    });
    const hop = session({ id: HOP, name: "bas\u2066tion", hostname: "b\u202e.example", port: 22 });
    open(spoofed, [spoofed, { ...hop, owner: spoofed.owner }]);
    const dialog = screen.getByRole("dialog");
    expect(document.getElementById("sld-host")?.textContent).toBe("\\u{202E}moc.elpmaxe.live");
    expect(document.getElementById("sld-hop-host-0")?.textContent).toBe("b\\u{202E}.example");
    expect(document.getElementById("sld-hop-port-0")?.textContent).toBe("22");
    expect(dialog).toHaveTextContent('sessionLogin.title {"name":"core\\\\u{200B}switch"}');
    expect(dialog).toHaveTextContent('sessionLogin.info {"owner":"bob\\\\u{000A}You"}');
    expect(dialog).toHaveTextContent(
      'sessionLogin.jumpHostProfileLabel {"name":"bas\\\\u{2066}tion"}',
    );
    // None of them is left in the dialog as a character.
    expect(dialog.textContent).not.toMatch(/[\u202e\u200b\u2066\n]/);
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
  });

  it("shows a host in ASCII, and nothing longer than it may be", async () => {
    backend({});
    // A Cyrillic a in the host, and a name, host and owner as only a
    // statement past the app's own checks could write them.
    const lookalike = session({ hostname: "b\u0430stion.corp", jump_host_id: HOP });
    const long = session({
      id: HOP,
      name: "n".repeat(300),
      hostname: "h".repeat(300),
      owner: lookalike.owner,
    });
    open({ ...lookalike, name: "t".repeat(300), owner: "o".repeat(300) }, [
      { ...lookalike, owner: "o".repeat(300) },
      { ...long, owner: "o".repeat(300) },
    ]);
    const dialog = screen.getByRole("dialog");
    expect(document.getElementById("sld-host")?.textContent).toBe("b\\u{0430}stion.corp");
    expect(document.getElementById("sld-hop-host-0")?.textContent).toBe(`${"h".repeat(255)}…`);
    expect(dialog).toHaveTextContent(`sessionLogin.title {"name":"${"t".repeat(64)}…"}`);
    expect(dialog).toHaveTextContent(`sessionLogin.info {"owner":"${"o".repeat(64)}…"}`);
    expect(dialog).toHaveTextContent(
      `sessionLogin.jumpHostProfileLabel {"name":"${"n".repeat(64)}…"}`,
    );
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
  });

  it("takes no choice before what is assigned has been read", async () => {
    const read = deferred<string | null>();
    backend({});
    vi.mocked(invoke).mockImplementationOnce(() => read.promise);
    open(session(), [session()]);
    for (const id of ["sld-profile", "sld-loginseq"]) {
      expect(document.getElementById(id)).toBeDisabled();
    }
    expect(saveButton()).toBeDisabled();
    // A click opens nothing, so nothing can be picked that the read would
    // then replace.
    fireEvent.click(select("sld-loginseq"));
    expect(screen.queryByRole("option")).not.toBeInTheDocument();
    read.resolve(PROFILE_A);
    await waitFor(() => {
      expect(document.getElementById("sld-profile")).toHaveTextContent("radius (alice)");
    });
    expect(document.getElementById("sld-profile")).toBeEnabled();
    expect(document.getElementById("sld-loginseq")).toBeEnabled();
  });

  it("shows what is assigned, and saves nothing that was not changed", async () => {
    const stored = { [`profile:${TARGET}`]: PROFILE_A, [`sequence:${TARGET}`]: SEQUENCE };
    backend(stored);
    const onClose = open(session(), [session()]);
    expect(saveButton()).toBeDisabled();
    await waitFor(() => {
      expect(document.getElementById("sld-profile")).toHaveTextContent("radius (alice)");
    });
    expect(document.getElementById("sld-loginseq")).toHaveTextContent("enable");
    fireEvent.click(saveButton());
    await waitFor(() => {
      expect(onClose).toHaveBeenCalledTimes(1);
    });
    expect(writes()).toEqual([]);
    // Saving is also how the user confirms what the session names.
    expect(confirmations()).toEqual([[shownOf(session())]]);
  });

  it("confirms what it shows before it assigns anything", async () => {
    backend({});
    const onClose = open(WITH_HOP, [WITH_HOP, BASTION]);
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    vi.mocked(invoke).mockClear();
    pick("sld-profile", "radius (alice)");
    pick("sld-hop-0", "bastion-key");
    fireEvent.click(saveButton());
    await waitFor(() => {
      expect(onClose).toHaveBeenCalledTimes(1);
    });
    expect(commands()).toEqual([
      "session_login_confirm",
      "set_session_credential",
      "set_session_credential",
    ]);
    // The session and its jump host, each as the dialog showed it.
    expect(confirmations()).toEqual([
      [
        {
          id: TARGET,
          protocol: "ssh",
          hostname: "switch.example",
          port: 2222,
          username: "",
          legacy_algorithms: false,
          jump_host_id: HOP,
        },
        // The jump host is sent whole; the backend takes what a jump host is
        // held to.
        {
          id: HOP,
          protocol: "ssh",
          hostname: "bastion.example",
          port: 22,
          username: "",
          legacy_algorithms: false,
          jump_host_id: null,
        },
      ],
    ]);
  });

  it("assigns nothing to a session that is no longer what it showed", async () => {
    // The backend reads the session again and refuses to confirm it.
    const stored: Record<string, string> = {};
    backend(stored, "session_login_confirm");
    const onClose = open(session(), [session()]);
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    pick("sld-profile", "radius (alice)");
    pick("sld-loginseq", "enable");
    fireEvent.click(saveButton());
    expect(await screen.findByRole("alert")).toHaveTextContent("session_login_confirm failed");
    expect(writes()).toEqual([]);
    expect(stored).toEqual({});
    expect(onClose).not.toHaveBeenCalled();
  });

  it("says so where something assigned waits to be confirmed", async () => {
    // A profile assigned before sessions were confirmed, or on another machine.
    backend({ [`profile:${TARGET}`]: PROFILE_A });
    open(session(), [session()]);
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    expect(notice()).toHaveTextContent("sessionLogin.notConfirmed");
    // Nothing was confirmed before, so nothing is crossed out.
    expect(screen.getByRole("dialog").querySelector("del")).toBeNull();
  });

  it("shows what was confirmed before, crossed out, where the session names something else", async () => {
    const before = pinOf(
      session({ protocol: "ssh", hostname: "old.example", port: 22, jump_host_id: HOP }),
    );
    const hopBefore = hopPinOf(session({ hostname: "old-bastion.example", port: 2022 }));
    backend({ [`profile:${TARGET}`]: PROFILE_A, [`profile:${HOP}`]: PROFILE_B }, undefined, {
      [TARGET]: before,
      [HOP]: hopBefore,
    });
    open(WITH_HOP, [WITH_HOP, BASTION]);
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    expect(notice()).toHaveTextContent("sessionLogin.notConfirmed");
    const crossedOut = Array.from(screen.getByRole("dialog").querySelectorAll("del")).map(
      (el) => el.textContent,
    );
    // The session's host and port, then the jump host's; the protocol is the same.
    expect(crossedOut).toEqual(["old.example", "22", "old-bastion.example", "2022"]);
    expect(document.getElementById("sld-host")).toHaveTextContent(/^switch\.example$/);
    // What has changed is marked, and what has not is not.
    const marked = Array.from(document.querySelectorAll(".dialog-given-changed")).map(
      (el) => el.id,
    );
    expect(marked).toEqual(["sld-host", "sld-port", "sld-hop-host-0", "sld-hop-port-0"]);
  });

  it("marks a changed user name and switch, and shows the jump host that was", async () => {
    // Confirmed with another user name, with the switch off, and through
    // another jump host. That one's owner has had it name another host
    // since, the same as the one the session goes through now.
    const was = session({
      id: "00000000-0000-4000-8000-0000000000a2",
      name: "old-bastion",
      hostname: "bastion.example",
      port: 22,
    });
    const now = session({ username: "root", legacy_algorithms: true, jump_host_id: HOP });
    backend({ [`profile:${TARGET}`]: PROFILE_A }, undefined, {
      [TARGET]: {
        ...pinOf(now),
        username: "admin",
        legacy_algorithms: false,
        jump_host_id: was.id,
      },
      [HOP]: hopPinOf(BASTION),
      [was.id]: hopPinOf({ ...was, hostname: "confirmed.example", port: 2022 }),
    });
    open(now, [now, BASTION, was]);
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    expect(notice()).toHaveTextContent("sessionLogin.notConfirmed");
    expect(document.getElementById("sld-username")).toHaveClass("dialog-given-changed");
    expect(document.getElementById("sld-username")?.nextElementSibling?.textContent).toBe("admin");
    expect(screen.getByRole("checkbox").closest("label")).toHaveClass("dialog-label-changed");
    // The jump host it goes on to now is marked, with the one it went on to
    // before crossed out: by the host and port the user confirmed of it, not
    // by what its row names now.
    expect(document.querySelector('label[for="sld-hop-0"]')).toHaveClass("dialog-label-changed");
    expect(document.getElementById("sld-was-jump-0")?.textContent).toBe("confirmed.example  2022");
    // The jump host itself is as it was confirmed.
    expect(document.getElementById("sld-hop-host-0")).not.toHaveClass("dialog-given-changed");
  });

  it("shows the jump host that was where the session goes through none now", async () => {
    backend({ [`sequence:${TARGET}`]: SEQUENCE }, undefined, {
      // One the user confirmed nothing of is shown by its ID.
      [TARGET]: { ...pinOf(session()), jump_host_id: HOP },
    });
    open(session(), [session()]);
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    expect(notice()).toHaveTextContent("sessionLogin.notConfirmed");
    expect(document.getElementById("sld-was-jump-0")?.textContent).toBe(HOP);
  });

  it("no longer says that something waits once a save has confirmed it", async () => {
    // Confirming works and assigning then fails: what is assigned by now is
    // confirmed, so the notice is gone while the error shows.
    const stored: Record<string, string> = { [`profile:${TARGET}`]: PROFILE_A };
    backend(stored, "set_session_login_sequence");
    const onClose = open(session(), [session()]);
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    expect(notice()).toHaveTextContent("sessionLogin.notConfirmed");
    pick("sld-loginseq", "enable");
    fireEvent.click(saveButton());
    expect(await screen.findByRole("alert")).toHaveTextContent("set_session_login_sequence failed");
    expect(onClose).not.toHaveBeenCalled();
    expect(notice()).toBeNull();
  });

  it("says nothing where the session names what was confirmed", async () => {
    backend({ [`profile:${TARGET}`]: PROFILE_A, [`profile:${HOP}`]: PROFILE_B }, undefined, {
      [TARGET]: pinOf(WITH_HOP),
      // Of a jump host only its host, its port and where it goes on are
      // confirmed: its own protocol, user name and switch are not what it is
      // connected to with.
      [HOP]: hopPinOf({ ...BASTION, protocol: "telnet", username: "x", legacy_algorithms: true }),
    });
    open(WITH_HOP, [WITH_HOP, BASTION]);
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    expect(notice()).toBeNull();
    expect(screen.getByRole("dialog").querySelector("del")).toBeNull();
  });

  it.each([
    ["the host", { hostname: "old.example" }],
    ["the port", { port: 22 }],
    ["the user name", { username: "root" }],
    ["the switch for legacy algorithms", { legacy_algorithms: true }],
    ["the protocol", { protocol: "telnet" }],
    ["the jump host", { jump_host_id: HOP }],
  ])("asks again where %s is another than was confirmed", async (_what, change) => {
    backend({ [`sequence:${TARGET}`]: SEQUENCE }, undefined, {
      [TARGET]: { ...pinOf(session()), ...change },
    });
    open(session(), [session()]);
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    expect(notice()).toHaveTextContent("sessionLogin.notConfirmed");
  });

  it("holds a session to what was confirmed where only its jump host has a profile", async () => {
    // Nothing is assigned to the session itself. The profile for its jump
    // host is used with the session's switch for legacy algorithms, so the
    // session waits to be confirmed as well: here the jump host is confirmed
    // and the session never was.
    backend({ [`profile:${HOP}`]: PROFILE_B }, undefined, { [HOP]: hopPinOf(BASTION) });
    open(WITH_HOP, [WITH_HOP, BASTION]);
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    expect(notice()).toHaveTextContent("sessionLogin.notConfirmed");
  });

  it.each([
    ["was never confirmed", {}],
    ["names another host", { [HOP]: hopPinOf({ ...BASTION, hostname: "old.example" }) }],
    ["goes on to another jump host", { [HOP]: hopPinOf({ ...BASTION, jump_host_id: TARGET }) }],
  ])("asks where a jump host with a profile %s", async (_what, hopPins) => {
    backend({ [`profile:${HOP}`]: PROFILE_B }, undefined, {
      [TARGET]: pinOf(WITH_HOP),
      ...hopPins,
    });
    open(WITH_HOP, [WITH_HOP, BASTION]);
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    expect(notice()).toHaveTextContent("sessionLogin.notConfirmed");
  });

  it("says nothing where the session and its jump host with a profile are confirmed", async () => {
    backend({ [`profile:${HOP}`]: PROFILE_B }, undefined, {
      [TARGET]: pinOf(WITH_HOP),
      [HOP]: hopPinOf(BASTION),
    });
    open(WITH_HOP, [WITH_HOP, BASTION]);
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    expect(notice()).toBeNull();
  });

  it("marks each jump host of a chain for what has changed of it", async () => {
    // The session goes through `near`, which goes on to `far`. When the user
    // confirmed them, `near` went on to another jump host and `far` had
    // another port.
    const FAR = "00000000-0000-4000-8000-0000000000a3";
    const GONE = "00000000-0000-4000-8000-0000000000a4";
    const far = session({ id: FAR, name: "far", hostname: "far.example", port: 22 });
    const near = session({ id: HOP, name: "near", hostname: "near.example", jump_host_id: FAR });
    backend(
      {
        [`profile:${TARGET}`]: PROFILE_A,
        [`profile:${HOP}`]: PROFILE_B,
        [`profile:${FAR}`]: PROFILE_B,
      },
      undefined,
      {
        [TARGET]: pinOf(WITH_HOP),
        [HOP]: hopPinOf({ ...near, jump_host_id: GONE }),
        [FAR]: hopPinOf({ ...far, port: 2022 }),
        [GONE]: hopPinOf(session({ hostname: "gone.example", port: 22 })),
      },
    );
    open(WITH_HOP, [WITH_HOP, near, far]);
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    expect(notice()).toHaveTextContent("sessionLogin.notConfirmed");
    const label = (i: number): Element | null =>
      document.querySelector(`label[for="sld-hop-${String(i)}"]`);
    // The first jump host is as it was confirmed, and the session still goes
    // on to it.
    expect(label(0)).not.toHaveClass("dialog-label-changed");
    expect(document.getElementById("sld-was-jump-0")).toBeNull();
    expect(document.getElementById("sld-hop-host-0")).not.toHaveClass("dialog-given-changed");
    // It goes on to the second where it went on to another before: that one
    // is marked, with the one that was crossed out in its place.
    expect(label(1)).toHaveClass("dialog-label-changed");
    expect(document.getElementById("sld-was-jump-1")?.textContent).toBe("gone.example  22");
    // And the second's own port is another.
    expect(document.getElementById("sld-hop-port-1")).toHaveClass("dialog-given-changed");
    expect(document.getElementById("sld-hop-port-1")?.nextElementSibling?.textContent).toBe("2022");
    expect(document.getElementById("sld-hop-host-1")).not.toHaveClass("dialog-given-changed");
    // Nothing went on from the last one, then or now.
    expect(document.getElementById("sld-was-jump-2")).toBeNull();
  });

  it("no longer says that a jump host waits once a save has confirmed it", async () => {
    const stored: Record<string, string> = { [`profile:${HOP}`]: PROFILE_B };
    backend(stored, "set_session_login_sequence");
    const onClose = open(WITH_HOP, [WITH_HOP, BASTION]);
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    expect(notice()).toHaveTextContent("sessionLogin.notConfirmed");
    pick("sld-loginseq", "enable");
    fireEvent.click(saveButton());
    expect(await screen.findByRole("alert")).toHaveTextContent("set_session_login_sequence failed");
    expect(onClose).not.toHaveBeenCalled();
    expect(notice()).toBeNull();
    expect(screen.getByRole("dialog").querySelector("del")).toBeNull();
  });

  it("does not ask again for the switch or the jump host of a Telnet session", async () => {
    const telnet = session({ protocol: "telnet" });
    backend({ [`sequence:${TARGET}`]: SEQUENCE }, undefined, {
      [TARGET]: { ...pinOf(telnet), legacy_algorithms: true, jump_host_id: HOP },
    });
    open(telnet, [telnet]);
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    expect(notice()).toBeNull();
    expect(screen.getByRole("dialog").querySelector("del")).toBeNull();
  });

  it("asks for nothing where nothing is assigned", async () => {
    // Never confirmed, and nothing of the user's that could go anywhere.
    backend({});
    open(WITH_HOP, [WITH_HOP, BASTION]);
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    expect(notice()).toBeNull();
  });

  it("shows the owner's switch for legacy algorithms, for SSH", async () => {
    backend({});
    const legacy = session({ legacy_algorithms: true });
    open(legacy, [legacy]);
    const box = screen.getByRole("checkbox", { name: "session.legacyAlgorithmsLabel" });
    expect(box).toBeChecked();
    expect(box).toBeDisabled();
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
  });

  it("writes the user's choice for the session and for each jump host", async () => {
    const stored: Record<string, string> = { [`profile:${TARGET}`]: PROFILE_A };
    backend(stored);
    const onClose = open(WITH_HOP, [WITH_HOP, BASTION]);
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    // The jump host is listed with its own host, and needs a profile.
    expect(screen.getByRole("dialog")).toHaveTextContent(
      'sessionLogin.jumpHostProfileLabel {"name":"bastion"}',
    );
    // Host and port apart, so that a host cannot bring a port of its own.
    expect(document.getElementById("sld-hop-host-0")).toHaveTextContent(/^bastion\.example$/);
    expect(document.getElementById("sld-hop-port-0")).toHaveTextContent(/^22$/);
    expect(screen.getByText("sessionLogin.jumpHostNeedsProfile")).toBeInTheDocument();
    expect(screen.queryByText("sessionLogin.jumpHostUnavailable")).not.toBeInTheDocument();

    pick("sld-hop-0", "bastion-key");
    expect(screen.queryByText("sessionLogin.jumpHostNeedsProfile")).not.toBeInTheDocument();
    pick("sld-loginseq", "enable");
    fireEvent.click(saveButton());
    await waitFor(() => {
      expect(onClose).toHaveBeenCalledTimes(1);
    });
    // The session's own profile was not changed, so it is not written.
    expect(writes()).toEqual([
      ["set_session_credential", { sessionId: HOP, credentialProfileId: PROFILE_B }],
      ["set_session_login_sequence", { sessionId: TARGET, loginSequenceId: SEQUENCE }],
    ]);
  });

  it("clears an assignment with null", async () => {
    backend({ [`profile:${TARGET}`]: PROFILE_A });
    const onClose = open(session(), [session()]);
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    pick("sld-profile", "session.credentialProfileNone");
    fireEvent.click(saveButton());
    await waitFor(() => {
      expect(onClose).toHaveBeenCalledTimes(1);
    });
    expect(writes()).toEqual([
      ["set_session_credential", { sessionId: TARGET, credentialProfileId: null }],
    ]);
    // Nothing is assigned any more, so there is nothing to confirm.
    expect(confirmations()).toEqual([]);
  });

  it("says so where a jump host of the session cannot be used", async () => {
    backend({});
    // The bastion is not among the sessions the user can see.
    open(WITH_HOP, [WITH_HOP]);
    expect(screen.getByText("sessionLogin.jumpHostUnavailable")).toBeInTheDocument();
    expect(document.getElementById("sld-hop-0")).toBeNull();
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
  });

  it("offers a Telnet session a login sequence and no credential profile", async () => {
    const telnet = session({ protocol: "telnet", jump_host_id: HOP });
    backend({});
    const onClose = open(telnet, [telnet, BASTION]);
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    expect(document.getElementById("sld-profile")).toBeNull();
    expect(document.getElementById("sld-hop-0")).toBeNull();
    expect(commands()).toEqual(["get_session_login_sequence", "session_login_confirmed"]);
    expect(screen.queryByRole("checkbox")).not.toBeInTheDocument();
    pick("sld-loginseq", "enable");
    fireEvent.click(saveButton());
    await waitFor(() => {
      expect(onClose).toHaveBeenCalledTimes(1);
    });
    expect(writes()).toEqual([
      ["set_session_login_sequence", { sessionId: TARGET, loginSequenceId: SEQUENCE }],
    ]);
  });

  it("saves nothing when what is assigned could not be read", async () => {
    backend({}, "get_session_credential");
    const onClose = open(session(), [session()]);
    expect(await screen.findByRole("alert")).toHaveTextContent("get_session_credential failed");
    expect(saveButton()).toBeDisabled();
    // Also not by submitting the form past its button.
    fireEvent.submit(form());
    expect(writes()).toEqual([]);
    expect(onClose).not.toHaveBeenCalled();
  });

  it("takes no input and does not close while a save is under way", async () => {
    const write = deferred<null>();
    backend({});
    const onClose = open(session(), [session()]);
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    pick("sld-profile", "radius (alice)");
    vi.mocked(invoke).mockImplementationOnce(() => write.promise);
    fireEvent.click(saveButton());
    await waitFor(() => {
      expect(cancelButton()).toBeDisabled();
    });
    expect(saveButton()).toBeDisabled();
    expect(document.getElementById("sld-profile")).toBeDisabled();
    expect(document.getElementById("sld-loginseq")).toBeDisabled();
    // Escape is taken and leaves the dialog open; a second submit starts
    // nothing more. The save is at its first step, which confirms.
    fireEvent.keyDown(document, { key: "Escape" });
    fireEvent.submit(form());
    expect(onClose).not.toHaveBeenCalled();
    expect(confirmations()).toHaveLength(1);
    expect(writes()).toHaveLength(0);
    write.resolve(null);
    await waitFor(() => {
      expect(onClose).toHaveBeenCalledTimes(1);
    });
    expect(confirmations()).toHaveLength(1);
    expect(writes()).toHaveLength(1);
  });

  it("closes on Escape while nothing is being saved", async () => {
    backend({});
    const onClose = open(session(), [session()]);
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(writes()).toEqual([]);
  });

  it("keeps the session it was opened for", async () => {
    backend({});
    const { rerender } = render(
      <SessionLoginDialog
        session={session()}
        sessions={[session()]}
        onClose={vi.fn()}
        onManageCredentials={vi.fn()}
        onManageLoginSequences={vi.fn()}
      />,
    );
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    // The owner changes the host and adds a jump host while it is open.
    const changed = session({ hostname: "elsewhere.example", jump_host_id: HOP });
    rerender(
      <SessionLoginDialog
        session={changed}
        sessions={[changed, BASTION]}
        onClose={vi.fn()}
        onManageCredentials={vi.fn()}
        onManageLoginSequences={vi.fn()}
      />,
    );
    expect(document.getElementById("sld-host")).toHaveTextContent(/^switch\.example$/);
    expect(document.getElementById("sld-hop-0")).toBeNull();
  });

  it("stays open when a save fails, and then writes only what is left", async () => {
    const stored: Record<string, string> = {};
    backend(stored, "set_session_login_sequence");
    const onClose = open(WITH_HOP, [WITH_HOP, BASTION]);
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    pick("sld-profile", "radius (alice)");
    pick("sld-hop-0", "bastion-key");
    pick("sld-loginseq", "enable");
    fireEvent.click(saveButton());
    expect(await screen.findByRole("alert")).toHaveTextContent("set_session_login_sequence failed");
    expect(onClose).not.toHaveBeenCalled();
    // Saving took the focus from its button; it is back where trying again is.
    await waitFor(() => {
      expect(saveButton()).toHaveFocus();
    });
    expect(stored[`profile:${TARGET}`]).toBe(PROFILE_A);
    expect(stored[`profile:${HOP}`]).toBe(PROFILE_B);

    vi.mocked(invoke).mockClear();
    backend(stored);
    await waitFor(() => {
      expect(saveButton()).toBeEnabled();
    });
    fireEvent.click(saveButton());
    await waitFor(() => {
      expect(onClose).toHaveBeenCalledTimes(1);
    });
    expect(writes()).toEqual([
      ["set_session_login_sequence", { sessionId: TARGET, loginSequenceId: SEQUENCE }],
    ]);
  });
});
