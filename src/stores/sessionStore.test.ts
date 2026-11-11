/**
 * Which sessions the dialogs offer as a jump host. In PostgreSQL mode a
 * session's jump host has to be a session of the same owner, and the dialogs
 * are for the user's own sessions, so only those are offered.
 */
import { describe, expect, it } from "vitest";
import {
  isJumpHostCandidate,
  jumpHostChoices,
  jumpHostForCopy,
  type Session,
} from "./sessionStore";

function session(overrides: Partial<Session> = {}): Session {
  return {
    id: "00000000-0000-4000-8000-000000000001",
    folder_id: "00000000-0000-4000-8000-000000000002",
    name: "bastion",
    hostname: "bastion.example",
    port: 22,
    protocol: "ssh",
    username: "",
    auth_method: "profile",
    jump_host_id: null,
    tags: '["jumphost"]',
    icon: "desktop",
    sort_order: 0,
    highlight_profile_id: null,
    credential_profile_id: null,
    login_sequence_id: null,
    legacy_algorithms: false,
    owner: "alice",
    visibility: "shared",
    ...overrides,
  };
}

describe("isJumpHostCandidate", () => {
  it("offers a tagged SSH session, whatever its owner, without PostgreSQL", () => {
    expect(isJumpHostCandidate(session({ owner: "local" }), false, null)).toBe(true);
    expect(isJumpHostCandidate(session({ owner: "bob" }), false, null)).toBe(true);
    expect(isJumpHostCandidate(session({ tags: '["JumpHost","eu"]' }), false, null)).toBe(true);
  });

  it("offers no session without the tag, and none that is not SSH", () => {
    expect(isJumpHostCandidate(session({ tags: "[]" }), false, null)).toBe(false);
    expect(isJumpHostCandidate(session({ tags: "not json" }), false, null)).toBe(false);
    expect(isJumpHostCandidate(session({ protocol: "telnet" }), false, null)).toBe(false);
    expect(isJumpHostCandidate(session({ protocol: "telnet" }), true, "alice")).toBe(false);
  });

  it("offers only the user's own sessions in PostgreSQL mode", () => {
    expect(isJumpHostCandidate(session(), true, "alice")).toBe(true);
    expect(isJumpHostCandidate(session({ visibility: "personal" }), true, "alice")).toBe(true);
    // Another user's session, also one that user shares.
    expect(isJumpHostCandidate(session({ owner: "bob" }), true, "alice")).toBe(false);
    // Role names are not folded.
    expect(isJumpHostCandidate(session({ owner: "Alice" }), true, "alice")).toBe(false);
    // The tag still counts for a session of the user's own.
    expect(isJumpHostCandidate(session({ tags: "[]" }), true, "alice")).toBe(false);
  });

  it("offers none while the role is not known", () => {
    expect(isJumpHostCandidate(session(), true, null)).toBe(false);
    expect(isJumpHostCandidate(session({ owner: "" }), true, null)).toBe(false);
  });
});

const EDITED = "00000000-0000-4000-8000-00000000000e";
const OWN = "00000000-0000-4000-8000-00000000000a";
const FOREIGN = "00000000-0000-4000-8000-00000000000b";
const UNTAGGED = "00000000-0000-4000-8000-00000000000c";
const TELNET = "00000000-0000-4000-8000-00000000000d";

const SESSIONS = [
  session({ id: EDITED }),
  session({ id: OWN }),
  session({ id: FOREIGN, owner: "bob" }),
  session({ id: UNTAGGED, tags: "[]" }),
  session({ id: TELNET, protocol: "telnet" }),
];

function ids(sessions: Session[]): string[] {
  return sessions.map((s) => s.id);
}

describe("jumpHostChoices", () => {
  it("lists the candidates without the session being edited", () => {
    expect(ids(jumpHostChoices(SESSIONS, EDITED, "", true, "alice"))).toEqual([OWN]);
    expect(ids(jumpHostChoices(SESSIONS, EDITED, "", false, null))).toEqual([OWN, FOREIGN]);
    // A new session is none of them.
    expect(ids(jumpHostChoices(SESSIONS, undefined, "", true, "alice"))).toEqual([EDITED, OWN]);
  });

  it("keeps the jump host that is set, also one that is no candidate", () => {
    expect(ids(jumpHostChoices(SESSIONS, EDITED, FOREIGN, true, "alice"))).toEqual([OWN, FOREIGN]);
    expect(ids(jumpHostChoices(SESSIONS, EDITED, UNTAGGED, true, "alice"))).toEqual([
      OWN,
      UNTAGGED,
    ]);
    // But not one that is no SSH session, and never the session itself.
    expect(ids(jumpHostChoices(SESSIONS, EDITED, TELNET, true, "alice"))).toEqual([OWN]);
    expect(ids(jumpHostChoices(SESSIONS, EDITED, EDITED, true, "alice"))).toEqual([OWN]);
  });
});

describe("jumpHostForCopy", () => {
  it("keeps the jump host without PostgreSQL, whatever its owner", () => {
    expect(jumpHostForCopy(session({ jump_host_id: FOREIGN }), SESSIONS, false, null)).toBe(
      FOREIGN,
    );
    expect(jumpHostForCopy(session({ jump_host_id: null }), SESSIONS, false, null)).toBeNull();
  });

  it("keeps only a jump host of the user's own in PostgreSQL mode", () => {
    const copyOf = (jumpHostId: string | null, pgUser: string | null): string | null =>
      jumpHostForCopy(session({ jump_host_id: jumpHostId }), SESSIONS, true, pgUser);
    expect(copyOf(OWN, "alice")).toBe(OWN);
    // One that lost its tag is still the user's own.
    expect(copyOf(UNTAGGED, "alice")).toBe(UNTAGGED);
    expect(copyOf(null, "alice")).toBeNull();
    // Another user's, one that is not among the sessions, and any while the
    // role is not known.
    expect(copyOf(FOREIGN, "alice")).toBeNull();
    expect(copyOf("00000000-0000-4000-8000-0000000000ff", "alice")).toBeNull();
    expect(copyOf(OWN, null)).toBeNull();
  });
});
