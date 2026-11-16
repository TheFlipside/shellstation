/**
 * SessionHighlightDialog: in PostgreSQL mode, which of the user's own
 * highlight profiles another user's shared session is shown with. It offers
 * the user's profiles and none, starts from what is chosen, and shows the
 * session's name, which is another user's text, written out.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { SessionHighlightDialog } from "./SessionHighlightDialog";
import { useHighlightStore } from "../stores/highlightStore";
import { useSessionStore, type Session } from "../stores/sessionStore";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
// Echoes the key and its options, so tests can see what was interpolated.
vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, options?: Record<string, unknown>) =>
      options ? `${key} ${JSON.stringify(options)}` : key,
  }),
}));

const SESSION = "00000000-0000-4000-8000-0000000000a0";
const CISCO = "00000000-0000-4000-8000-0000000000b1";
const LINUX = "00000000-0000-4000-8000-0000000000b2";
const GONE = "00000000-0000-4000-8000-0000000000b3";

function session(overrides: Partial<Session> = {}): Session {
  return {
    id: SESSION,
    folder_id: "00000000-0000-4000-8000-000000000002",
    name: "core-switch",
    hostname: "switch.example",
    port: 22,
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

function trigger(): HTMLElement {
  const element = document.getElementById("shd-profile");
  if (!element) throw new Error("no select");
  return element;
}

function pick(optionLabel: string): void {
  fireEvent.click(trigger());
  fireEvent.click(screen.getByRole("option", { name: optionLabel }));
}

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  useHighlightStore.setState({
    profiles: [
      { id: CISCO, name: "Cisco", rules: [], sort_order: 0 },
      { id: LINUX, name: "Linux", rules: [], sort_order: 1 },
    ],
  });
});

describe("SessionHighlightDialog", () => {
  it("offers the user's own profiles and none, starting from none", () => {
    render(<SessionHighlightDialog session={session()} onSubmit={vi.fn()} onCancel={vi.fn()} />);
    expect(screen.getByRole("dialog")).toHaveAccessibleName(
      'session.myHighlightingTitle {"name":"core-switch"}',
    );
    expect(trigger()).toHaveTextContent("session.highlightProfileNone");
    fireEvent.click(trigger());
    expect(screen.getAllByRole("option").map((o) => o.textContent)).toEqual([
      "session.highlightProfileNone",
      "Cisco",
      "Linux",
    ]);
  });

  it("starts from the profile that is chosen and saves another", () => {
    const onSubmit = vi.fn();
    render(
      <SessionHighlightDialog
        session={session({ highlight_profile_id: CISCO })}
        onSubmit={onSubmit}
        onCancel={vi.fn()}
      />,
    );
    expect(trigger()).toHaveTextContent("Cisco");
    pick("Linux");
    fireEvent.click(screen.getByRole("button", { name: "dialog.save" }));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith(LINUX);
  });

  it("saves none as no profile", () => {
    const onSubmit = vi.fn();
    render(
      <SessionHighlightDialog
        session={session({ highlight_profile_id: CISCO })}
        onSubmit={onSubmit}
        onCancel={vi.fn()}
      />,
    );
    pick("session.highlightProfileNone");
    fireEvent.click(screen.getByRole("button", { name: "dialog.save" }));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith(null);
  });

  it("shows a profile that is gone as none, and saves that", () => {
    const onSubmit = vi.fn();
    render(
      <SessionHighlightDialog
        session={session({ highlight_profile_id: GONE })}
        onSubmit={onSubmit}
        onCancel={vi.fn()}
      />,
    );
    expect(trigger()).toHaveTextContent("session.highlightProfileNone");
    fireEvent.click(screen.getByRole("button", { name: "dialog.save" }));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith(null);
  });

  it("shows what is chosen once the profiles are there, and does not clear it before", () => {
    // The dialog can open before the profiles have been loaded.
    useHighlightStore.setState({ profiles: [] });
    const onSubmit = vi.fn();
    render(
      <SessionHighlightDialog
        session={session({ highlight_profile_id: CISCO })}
        onSubmit={onSubmit}
        onCancel={vi.fn()}
      />,
    );
    expect(trigger()).toHaveTextContent("session.highlightProfileNone");
    act(() => {
      useHighlightStore.setState({
        profiles: [{ id: CISCO, name: "Cisco", rules: [], sort_order: 0 }],
      });
    });
    expect(trigger()).toHaveTextContent("Cisco");
    fireEvent.click(screen.getByRole("button", { name: "dialog.save" }));
    expect(onSubmit).toHaveBeenCalledExactlyOnceWith(CISCO);
  });

  it("writes out what another user put into the session's name, and cuts it", () => {
    const override = String.fromCodePoint(0x202e);
    render(
      <SessionHighlightDialog
        session={session({ name: `core${override}gnp.exe${"x".repeat(100)}` })}
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
      />,
    );
    const title = screen.getByRole("dialog").getAttribute("aria-labelledby") ?? "";
    const text = document.getElementById(title)?.textContent ?? "";
    expect(text).not.toContain(override);
    expect(text).toContain("202E");
    expect(text).toContain("…");
    expect(text).not.toContain("x".repeat(65));
  });

  it("cancels on the button and on Escape, without saving", () => {
    const onSubmit = vi.fn();
    const onCancel = vi.fn();
    render(<SessionHighlightDialog session={session()} onSubmit={onSubmit} onCancel={onCancel} />);
    fireEvent.click(screen.getByRole("button", { name: "dialog.cancel" }));
    expect(onCancel).toHaveBeenCalledTimes(1);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onCancel).toHaveBeenCalledTimes(2);
    expect(onSubmit).not.toHaveBeenCalled();
  });
});

describe("setSessionHighlightProfile", () => {
  it("sends the session and the profile, then loads the sessions again", async () => {
    const chosen = session({ highlight_profile_id: LINUX });
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === "session_set_highlight_profile") return Promise.resolve(null);
      if (cmd === "folder_list") return Promise.resolve([]);
      if (cmd === "session_list_all") return Promise.resolve([chosen]);
      return Promise.reject(new Error(`unexpected command ${cmd}`));
    });
    useSessionStore.setState({ sessions: [session()], searchQuery: "" });

    await useSessionStore.getState().setSessionHighlightProfile(SESSION, LINUX);
    expect(vi.mocked(invoke).mock.calls[0]).toEqual([
      "session_set_highlight_profile",
      { sessionId: SESSION, highlightProfileId: LINUX },
    ]);
    // What a terminal of that session reads its profile from.
    expect(useSessionStore.getState().sessions[0]?.highlight_profile_id).toBe(LINUX);

    await useSessionStore.getState().setSessionHighlightProfile(SESSION, null);
    expect(vi.mocked(invoke)).toHaveBeenCalledWith("session_set_highlight_profile", {
      sessionId: SESSION,
      highlightProfileId: null,
    });
  });

  it("leaves the sessions as they are when the backend refuses", async () => {
    vi.mocked(invoke).mockRejectedValue("Highlight profile not found");
    useSessionStore.setState({ sessions: [session()], searchQuery: "" });
    await expect(useSessionStore.getState().setSessionHighlightProfile(SESSION, GONE)).rejects.toBe(
      "Highlight profile not found",
    );
    expect(vi.mocked(invoke)).toHaveBeenCalledTimes(1);
    expect(useSessionStore.getState().sessions[0]?.highlight_profile_id).toBeNull();
  });
});
