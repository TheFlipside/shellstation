import React, { useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import { ContextMenu, type ContextMenuItem } from "./ContextMenu";
import { dialogOpen } from "./DialogKeys";

// A clipboard that cannot be read or written leaves the field as it is and
// says nothing, as in the terminal: there is nothing the user could do about
// it here, and the keyboard still pastes.
// eslint-disable-next-line @typescript-eslint/no-empty-function
const noop = (): void => {};

/**
 * The longest query the field takes, in UTF-16 units. A clipboard can hold
 * megabytes, and the backend searches by the first 200 bytes of a query
 * (`MAX_SEARCH_QUERY_LEN` in `commands/session.rs`). For a query in ASCII the
 * two limits are the same; one with other characters in it is still cut
 * short by the backend, as it was before the field had a limit.
 */
export const MAX_QUERY_LENGTH = 200;

/**
 * Whether the UTF-16 unit is a line break or another control character (C0,
 * DEL, C1), or half of a character without its other half: nothing that a
 * field of one line holds, or that a session's name, host or user name has.
 * A half by itself is no text at all, and the backend refuses a command
 * whose parameter has one.
 */
function isControl(text: string, index: number): boolean {
  const code = text.charCodeAt(index);
  if (code < 0x20 || (code >= 0x7f && code <= 0x9f)) return true;
  if (code >= 0xd800 && code <= 0xdbff) {
    // A first half, and no second one after it. Past the end there is NaN.
    const next = text.charCodeAt(index + 1);
    return !(next >= 0xdc00 && next <= 0xdfff);
  }
  if (code >= 0xdc00 && code <= 0xdfff) {
    // A second half, and no first one before it.
    const before = index > 0 ? text.charCodeAt(index - 1) : Number.NaN;
    return !(before >= 0xd800 && before <= 0xdbff);
  }
  return false;
}

/**
 * Clipboard text as a field of one line takes it, and at most `room` UTF-16
 * units of it: without the line breaks around it, which a copied line brings
 * along, and with one space for each run of them inside. A tab, the other
 * control characters and half a character by itself count as line breaks.
 *
 * One pass over the text, which ends once the room is filled: what is on the
 * clipboard can be megabytes of anything. Not a pattern, since one anchored
 * at the end (`/[\r\n]+$/`) starts over at every line break of a run that is
 * not at the end, and takes time by the square of that run's length.
 */
export function singleLine(text: string, room: number = Number.POSITIVE_INFINITY): string {
  let line = "";
  // Whether the line ends in the space of a run, with nothing after it yet.
  let joined = false;
  let at = 0;
  while (at < text.length && isControl(text, at)) at += 1;
  while (at < text.length && line.length < room) {
    if (isControl(text, at)) {
      while (at < text.length && isControl(text, at)) at += 1;
      // No space for a run at the end, which is dropped like the one at the
      // start, nor for one that nothing would follow for lack of room.
      if (at === text.length || line.length + 1 >= room) break;
      line += " ";
      joined = true;
      continue;
    }
    const limit = Math.min(text.length, at + (room - line.length));
    let end = at;
    while (end < limit && !isControl(text, end)) end += 1;
    // Cut short by the room: not between the two units of one character.
    if (end === limit && end < text.length) {
      const last = text.charCodeAt(end - 1);
      if (last >= 0xd800 && last <= 0xdbff) end -= 1;
    }
    // The room that is left is half of the next character's.
    if (end === at) break;
    line += text.slice(at, end);
    joined = false;
    at = end;
  }
  return joined ? line.slice(0, -1) : line;
}

/** The field's menu: where it is, and whether the field had a selection when it opened. */
interface MenuState {
  x: number;
  y: number;
  selection: boolean;
}

interface SearchFieldProps {
  value: string;
  placeholder: string;
  /** Called with the field's new text, as typed or pasted. */
  onChange: (value: string) => void;
  /** Called on Escape, unless that closed the field's menu. */
  onEscape: () => void;
}

/**
 * The sidebar's search field, with a context menu of its own for Copy and
 * Paste.
 *
 * The webview's context menu is suppressed for the whole window (`main.tsx`):
 * it is a web page's menu, with a refresh and, in a development build, the
 * inspector. A text field's usual menu went with it, so a right-click here
 * pasted nothing. This menu is the app's, like the one of a session or a tab,
 * and holds only the two entries. It reads and writes the clipboard through
 * the clipboard plugin, as the terminal does, and reads it only on a click on
 * Paste.
 */
export function SearchField({
  value,
  placeholder,
  onChange,
  onEscape,
}: SearchFieldProps): React.JSX.Element {
  const { t } = useTranslation();
  const inputRef = useRef<HTMLInputElement>(null);
  const [menu, setMenu] = useState<MenuState | null>(null);

  // Where the caret goes after a paste. A field whose text is set from
  // outside has its caret at the end; it belongs behind what was pasted. An
  // object, so that two pastes that end at the same place are two changes.
  const [caret, setCaret] = useState<{ at: number } | null>(null);
  useLayoutEffect(() => {
    if (caret === null) return;
    inputRef.current?.setSelectionRange(caret.at, caret.at);
  }, [caret]);

  const copy = (): void => {
    const input = inputRef.current;
    if (!input) return;
    // A field keeps its selection while the menu's button has the focus.
    const text = input.value.slice(input.selectionStart ?? 0, input.selectionEnd ?? 0);
    if (text) writeText(text).catch(noop);
    input.focus();
  };

  const paste = (): void => {
    readText()
      .then((clipboard) => {
        const input = inputRef.current;
        if (!input?.isConnected) return;
        // The clipboard answers later. A dialog that has opened by then lies
        // over the field, and nothing is typed into what is behind a dialog.
        if (dialogOpen()) return;
        // The field as it is now, not as it was at the click.
        const current = input.value;
        const start = input.selectionStart ?? current.length;
        const end = input.selectionEnd ?? start;
        // As much as a paste from the keyboard would put in: what the field
        // has room for, with the selection gone.
        const room = MAX_QUERY_LENGTH - (current.length - (end - start));
        if (room <= 0) return;
        const text = singleLine(clipboard, room);
        if (!text) return;
        // The parent has the new text in place when this returns, as the
        // sidebar's store has: the caret is set against the text of the
        // render that follows.
        onChange(current.slice(0, start) + text + current.slice(end));
        setCaret({ at: start + text.length });
        input.focus();
      })
      .catch(noop);
  };

  const items: ContextMenuItem[] = [
    { label: t("contextMenu.copy"), onClick: copy, disabled: !menu?.selection },
    { label: t("contextMenu.paste"), onClick: paste },
  ];

  return (
    <>
      <input
        ref={inputRef}
        type="text"
        placeholder={placeholder}
        value={value}
        maxLength={MAX_QUERY_LENGTH}
        onChange={(e) => {
          setMenu(null);
          onChange(e.target.value);
        }}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            e.stopPropagation();
            // With the menu open, Escape is for the menu.
            if (menu !== null) {
              setMenu(null);
            } else {
              onEscape();
            }
            return;
          }
          // A key can change the selection, which Copy was offered by.
          setMenu(null);
        }}
        onContextMenu={(e) => {
          e.preventDefault();
          const input = e.currentTarget;
          setMenu({
            x: e.clientX,
            y: e.clientY,
            selection: input.selectionStart !== input.selectionEnd,
          });
        }}
      />
      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          items={items}
          onClose={() => {
            setMenu(null);
          }}
        />
      )}
    </>
  );
}
