/**
 * The notice shown when a session goes on without a log although session
 * logging is on. It must name the session, stay until dismissed, and be
 * shown once per session name however often it is asked for.
 */
import { beforeAll, beforeEach, describe, expect, it } from "vitest";
import i18n from "./i18n";
import {
  nameForNotice,
  sessionNotLoggedEvent,
  showSessionNotLoggedNotice,
} from "./sessionLogNotice";
import { useTerminalStore } from "./stores/terminalStore";
import { useToastStore } from "./stores/toastStore";

beforeAll(async () => {
  await i18n.changeLanguage("en");
});

beforeEach(() => {
  useToastStore.setState(useToastStore.getInitialState(), true);
  useTerminalStore.setState(useTerminalStore.getInitialState(), true);
});

describe("sessionNotLoggedEvent", () => {
  it("is the name the backend sends the event under", () => {
    // Kept in step with `not_logged_event` in session_logger.rs, which has
    // the same check.
    expect(sessionNotLoggedEvent("abc-1")).toBe("terminal-log-failed-abc-1");
  });
});

describe("showSessionNotLoggedNotice", () => {
  it("names the session, in a warning that stays", () => {
    useTerminalStore.getState().addTab("s1", "core-router", "ssh");
    showSessionNotLoggedNotice("s1");
    const toasts = useToastStore.getState().toasts;
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toMatchObject({ level: "warning", sticky: true });
    // The real English text, so the placeholder is checked as well.
    expect(toasts[0].message).toBe(
      'Session "core-router" is not being logged: its log file could not be created or written to. The application log has the details.',
    );
  });

  it("shows a name as it is, without HTML escaping", () => {
    useTerminalStore.getState().addTab("s1", "admin@<core> & edge", "ssh");
    showSessionNotLoggedNotice("s1");
    expect(useToastStore.getState().toasts[0].message).toContain('"admin@<core> & edge"');
  });

  it("quotes a name without the characters that change how it displays", () => {
    // A right-to-left override, an isolate with its end, a line separator
    // and a control character; the joiner in the emoji and the directional
    // mark stay.
    const title = "edge\u202e-\u2066sw\u2069\u2028\u0007 👩‍💻\u200e";
    useTerminalStore.getState().addTab("s1", title, "ssh");
    showSessionNotLoggedNotice("s1");
    expect(useToastStore.getState().toasts[0].message).toBe(
      'Session "edge-sw 👩‍💻\u200e" is not being logged: its log file could not be created or written to. The application log has the details.',
    );
  });

  it("drops the characters the import drops from a name, and keeps the ones it keeps", () => {
    // `is_display_spoofing_char` in src-tauri/src/import/mod.rs, range by
    // range, and control characters (C0, DEL and C1).
    const dropped: [number, number][] = [
      [0x202a, 0x202e],
      [0x2066, 0x2069],
      [0x2028, 0x2029],
      [0x200b, 0x200b],
      [0xfeff, 0xfeff],
      [0x00ad, 0x00ad],
      [0x2060, 0x2064],
      [0xfff9, 0xfffb],
      [0x3164, 0x3164],
      [0xffa0, 0xffa0],
      [0x0000, 0x001f],
      [0x007f, 0x009f],
    ];
    for (const [first, last] of dropped) {
      for (let code = first; code <= last; code++) {
        const name = `a${String.fromCodePoint(code)}b`;
        expect(nameForNotice(name), `U+${code.toString(16)}`).toBe("ab");
      }
    }
    // Directional marks and joiners, and the characters on either side of
    // each range above.
    const kept = [
      0x200e, 0x200f, 0x061c, 0x200c, 0x200d, 0x0020, 0x00a0, 0x00ac, 0x00ae, 0x200a, 0x2027,
      0x202f, 0x205f, 0x2065, 0x206a, 0xfff8, 0xfffc, 0x3163, 0x3165, 0xff9f, 0xffa1, 0xfefe,
    ];
    for (const code of kept) {
      const name = `a${String.fromCodePoint(code)}b`;
      expect(nameForNotice(name), `U+${code.toString(16)}`).toBe(name);
    }
    // A name of nothing but such characters is quoted as empty.
    expect(nameForNotice(String.fromCodePoint(0x3164, 0x200b, 0xffa0))).toBe("");
  });

  it("cuts a long name short, by character", () => {
    expect(nameForNotice("a".repeat(64))).toBe("a".repeat(64));
    expect(nameForNotice("a".repeat(65))).toBe(`${"a".repeat(64)}…`);
    // Characters outside the BMP count as one each and are not split.
    expect(nameForNotice("😀".repeat(255))).toBe(`${"😀".repeat(64)}…`);
    // Dropped characters do not count towards the length.
    expect(nameForNotice(`${"\u202e".repeat(100)}router`)).toBe("router");

    useTerminalStore.getState().addTab("s1", "x".repeat(255), "ssh");
    showSessionNotLoggedNotice("s1");
    expect(useToastStore.getState().toasts[0].message).toContain(`"${"x".repeat(64)}…" is not`);
  });

  it("is shown once for a session reconnected under the same name", () => {
    useTerminalStore.getState().addTab("s1", "core-router", "ssh");
    showSessionNotLoggedNotice("s1");
    useTerminalStore.getState().replaceTab("s1", "s2", "ssh");
    showSessionNotLoggedNotice("s2");
    expect(useToastStore.getState().toasts).toHaveLength(1);
  });

  it("is shown for each session by name", () => {
    useTerminalStore.getState().addTab("s1", "core-router", "ssh");
    useTerminalStore.getState().addTab("s2", "edge-switch", "telnet");
    showSessionNotLoggedNotice("s1");
    showSessionNotLoggedNotice("s2");
    const messages = useToastStore.getState().toasts.map((t) => t.message);
    expect(messages).toHaveLength(2);
    expect(messages[0]).toContain('"core-router"');
    expect(messages[1]).toContain('"edge-switch"');
  });

  it("says nothing for a tab that is gone", () => {
    showSessionNotLoggedNotice("closed");
    expect(useToastStore.getState().toasts).toEqual([]);
  });
});
