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
 * and control characters. Keep the two in step; the test of the session log
 * notice lists them as well. Directional marks and joiners stay, as they do
 * there, since right-to-left names and emoji sequences need them.
 */
export const DISPLAY_CONTROL_RE =
  /[\p{Cc}\u202a-\u202e\u2066-\u2069\u2028\u2029\u200b\ufeff\u00ad\u2060-\u2064\ufff9-\ufffb\u3164\uffa0]/gu;

/** Everything but the printable characters of ASCII, space to tilde. */
const NOT_PLAIN_ASCII_RE = /[^ -~]/gu;

/** A character as its code point, such as `\u{202E}`. */
function codePointWritten(ch: string): string {
  const code = ch.codePointAt(0) ?? 0;
  return `\\u{${code.toString(16).toUpperCase().padStart(4, "0")}}`;
}

/**
 * The first `maxChars` characters of `text`, and whether that left any out.
 * By code point, so that a cut never splits a surrogate pair.
 */
function cut(text: string, maxChars: number): [string, boolean] {
  const chars = Array.from(text);
  if (chars.length <= maxChars) return [text, false];
  return [chars.slice(0, maxChars).join(""), true];
}

/**
 * A name or role as another user wrote it, for showing where the user
 * decides something by it: each of the characters above is written out as
 * its code point, such as `\u{202E}`, in place of taking effect, and the
 * text is cut after `maxChars` characters, with an ellipsis.
 *
 * Dropping the characters, as a notice that only quotes a name does, would
 * show a name that is not the one meant. Written out, the text can neither
 * reorder what stands around it nor look blank. The cut keeps a name of any
 * length, which a row written past the app's own checks can have, from
 * pushing what the user needs to read out of sight.
 *
 * A name can still be in any script, and so look like another name. For a
 * host there is [`hostAsWritten`].
 */
export function shownAsWritten(text: string, maxChars: number): string {
  const [shown, more] = cut(text, maxChars);
  return shown.replace(DISPLAY_CONTROL_RE, codePointWritten) + (more ? "…" : "");
}

/** The longest host the app's own commands take, in characters at the most. */
const MAX_HOST_CHARS = 255;

/**
 * A host as another user wrote it, for showing where the user decides to
 * send something there: in printable ASCII, with every other character
 * written out as its code point, and cut after `maxChars` characters, with
 * an ellipsis.
 *
 * This is stricter than [`shownAsWritten`], since a host is where a
 * credential goes. A host name in another script can look like one the user
 * trusts, letter for letter (a Cyrillic `а` for a Latin `a`), and marks,
 * selectors and right-to-left letters, which a name needs, can make two
 * hosts look alike or change the order a host reads in. Written out, such a
 * host reads as what it is. A host that is in another script in earnest is
 * shown the same way: the price of telling the two apart nowhere else.
 */
export function hostAsWritten(text: string, maxChars: number = MAX_HOST_CHARS): string {
  const [shown, more] = cut(text, maxChars);
  return shown.replace(NOT_PLAIN_ASCII_RE, codePointWritten) + (more ? "…" : "");
}

/** A backslash, or a character outside printable ASCII. */
const BACKSLASH_OR_NOT_PLAIN_ASCII_RE = /[^ -[\]-~]/gu;

/**
 * A user name as another user wrote it, which a connection types at a host:
 * shown as [`hostAsWritten`] shows a host, and with a backslash doubled.
 *
 * A user name can hold a backslash in earnest, as one of a Windows domain
 * does (`CORP\admin`), where a host cannot. Doubled, a backslash that was
 * written cannot be taken for the start of a character written out: the
 * text `\u{0430}` as someone typed it shows as `\\u{0430}`.
 */
export function typedAsWritten(text: string, maxChars: number = MAX_HOST_CHARS): string {
  const [shown, more] = cut(text, maxChars);
  return (
    shown.replace(BACKSLASH_OR_NOT_PLAIN_ASCII_RE, (ch) =>
      ch === "\\" ? "\\\\" : codePointWritten(ch),
    ) + (more ? "…" : "")
  );
}
