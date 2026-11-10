import i18n from "./i18n";
import { useTerminalStore } from "./stores/terminalStore";
import { useToastStore } from "./stores/toastStore";

/**
 * The event the backend sends to a session's tab when session logging is on
 * but this session has no log: its file or its directory could not be
 * created, too many logs are still being written, or the log was given up
 * later, because its file stopped taking writes. The backend sends it once
 * and without a payload: with the session's first output for a log that
 * could not be created, and later for one that was given up. The reason is
 * in the application log.
 */
export function sessionNotLoggedEvent(sessionId: string): string {
  return `terminal-log-failed-${sessionId}`;
}

/**
 * Characters that change how a name displays rather than what it says:
 * control characters, the bidi embedding, override and isolate controls,
 * which can make the text around a name read as something else, the line
 * and paragraph separators, and the invisible characters that make a name
 * look blank or like another one (zero-width space, BOM, soft hyphen, word
 * joiner and invisible operators, interlinear annotation marks, the
 * standalone Hangul fillers).
 *
 * It is the set the import drops from a name for this reason, code point for
 * code point: `is_display_spoofing_char` in `src-tauri/src/import/mod.rs`,
 * and control characters. Keep the two in step; the test lists them as well.
 * Directional marks and joiners stay, as they do there, since right-to-left
 * names and emoji sequences need them.
 */
const DISPLAY_CONTROL_RE =
  /[\p{Cc}\u202a-\u202e\u2066-\u2069\u2028\u2029\u200b\ufeff\u00ad\u2060-\u2064\ufff9-\ufffb\u3164\uffa0]/gu;

/** How much of a session name the notice quotes, in characters. */
const MAX_NAME_CHARS = 64;

/**
 * A tab title as the notice quotes it. A title is a session name, which can
 * come from a shared database or be typed with anything in it, and can be
 * 255 characters long: the notice quotes it without the characters above,
 * and cut short, so that a name can neither reorder the warning nor bury
 * it. A name can still hold a quotation mark and words of its own, as it
 * can in its tab; the warning itself always follows it.
 */
export function nameForNotice(title: string): string {
  // By code point, so that a cut never splits a surrogate pair.
  const chars = Array.from(title.replace(DISPLAY_CONTROL_RE, ""));
  if (chars.length <= MAX_NAME_CHARS) return chars.join("");
  return `${chars.slice(0, MAX_NAME_CHARS).join("")}…`;
}

/**
 * Say that a session goes on without a log, naming its tab. The session
 * works as usual, so nothing else would tell: the user would only find out
 * when looking for the log. The notice stays until it is dismissed, so that
 * it is not missed while the user is typing, or with toasts set to close
 * after a second. A tab that is gone by now gets none.
 *
 * The toast store shows the same sticky toast once, so while a notice is up,
 * another session with the same name gets none of its own: a reconnect, but
 * also a second tab of the same session.
 */
export function showSessionNotLoggedNotice(sessionId: string): void {
  const tab = useTerminalStore.getState().tabs.find((t) => t.id === sessionId);
  if (!tab) return;
  useToastStore
    .getState()
    .addToast(i18n.t("terminal.notLogged", { name: nameForNotice(tab.title) }), "warning", {
      sticky: true,
    });
}
