/**
 * Which sessions the dialogs offer as a jump host. In PostgreSQL mode a
 * session's jump host has to be a session of the same owner, and the dialogs
 * are for the user's own sessions, so only those are offered. And which jump
 * hosts another user's shared session goes through, for the dialog in which
 * the user assigns their own credential profiles to them.
 */
import { describe, expect, it } from "vitest";
import {
  isJumpHostCandidate,
  jumpHostChain,
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

describe("jumpHostChain", () => {
  const TARGET = "00000000-0000-4000-8000-0000000000a0";
  const HOP1 = "00000000-0000-4000-8000-0000000000a1";
  const HOP2 = "00000000-0000-4000-8000-0000000000a2";
  const bob = (id: string, jumpHostId: string | null): Session =>
    session({ id, owner: "bob", jump_host_id: jumpHostId });

  it("is empty and complete for a session without a jump host", () => {
    const target = bob(TARGET, null);
    expect(jumpHostChain(target, [target])).toEqual({ hops: [], complete: true });
  });

  it("lists the jump hosts, the one the session names first", () => {
    const all = [bob(TARGET, HOP1), bob(HOP1, HOP2), bob(HOP2, null)];
    const chain = jumpHostChain(all[0], all);
    expect(ids(chain.hops)).toEqual([HOP1, HOP2]);
    expect(chain.complete).toBe(true);
  });

  it("stops at a jump host the user cannot see", () => {
    // Its owner has not shared it, so it is not among the sessions.
    const all = [bob(TARGET, HOP1), bob(HOP1, HOP2)];
    const chain = jumpHostChain(all[0], all);
    expect(ids(chain.hops)).toEqual([HOP1]);
    expect(chain.complete).toBe(false);
  });

  it("stops at a jump host of another owner than the session naming it", () => {
    // The backend refuses such a link, also where the jump host is the
    // connecting user's own session.
    const all = [
      bob(TARGET, HOP1),
      bob(HOP1, HOP2),
      session({ id: HOP2, owner: "alice", jump_host_id: null }),
    ];
    const chain = jumpHostChain(all[0], all);
    expect(ids(chain.hops)).toEqual([HOP1]);
    expect(chain.complete).toBe(false);
    // Also the first one: a row that still has the placeholder owner.
    const first = [bob(TARGET, HOP1), session({ id: HOP1, owner: "local" })];
    expect(jumpHostChain(first[0], first)).toEqual({ hops: [], complete: false });
  });

  it("stops where the chain comes back to a session it has been through", () => {
    const loop = [bob(TARGET, HOP1), bob(HOP1, HOP2), bob(HOP2, HOP1)];
    expect(jumpHostChain(loop[0], loop)).toMatchObject({ complete: false });
    expect(ids(jumpHostChain(loop[0], loop).hops)).toEqual([HOP1, HOP2]);
    const toTarget = [bob(TARGET, HOP1), bob(HOP1, TARGET)];
    expect(ids(jumpHostChain(toTarget[0], toTarget).hops)).toEqual([HOP1]);
    expect(jumpHostChain(toTarget[0], toTarget).complete).toBe(false);
    const toItself = [bob(TARGET, TARGET)];
    expect(jumpHostChain(toItself[0], toItself)).toEqual({ hops: [], complete: false });
  });

  it("goes no further than the backend connects through", () => {
    const id = (n: number): string => `00000000-0000-4000-8000-0000000001${String(n + 10)}`;
    // A target and 11 jump hosts, each naming the next.
    const all = Array.from({ length: 12 }, (_, n) => bob(id(n), n < 11 ? id(n + 1) : null));
    const chain = jumpHostChain(all[0], all);
    expect(chain.hops).toHaveLength(10);
    expect(chain.complete).toBe(false);
    // Ten are connected through.
    const ten = all.slice(0, 11).map((s, n) => (n === 10 ? { ...s, jump_host_id: null } : s));
    expect(jumpHostChain(ten[0], ten)).toMatchObject({ complete: true });
    expect(jumpHostChain(ten[0], ten).hops).toHaveLength(10);
  });

  it("has no jump hosts for a Telnet session", () => {
    // Of the same owner, so that only the protocol keeps the jump host out.
    const all = [{ ...bob(TARGET, HOP1), protocol: "telnet" }, bob(HOP1, null)];
    expect(jumpHostChain(all[0], all)).toEqual({ hops: [], complete: true });
  });
});
