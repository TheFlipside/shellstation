import React, { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import {
  DndContext,
  PointerSensor,
  useDraggable,
  useSensor,
  useSensors,
  type DndContextProps,
  type DragStartEvent,
} from "@dnd-kit/core";
import { invoke } from "@tauri-apps/api/core";
import { readText } from "@tauri-apps/plugin-clipboard-manager";
import { useSettingsStore, type CommandButton } from "../stores/settingsStore";
import { useTerminalStore, type SessionType } from "../stores/terminalStore";
import {
  CommandButtonDialog,
  MAX_NAME_LENGTH,
  type CommandButtonFormData,
} from "./CommandButtonDialog";
import { ConfirmDialog } from "./ConfirmDialog";
import { ContextMenu, type ContextMenuItem } from "./ContextMenu";
import { dialogOpen, useOnDialogOpen } from "./DialogKeys";
import { buttonDropAt, buttonPointAt, sameButtonDrop, type ButtonDrop } from "../buttonDrop";
import {
  isSessionClosedError,
  reportSessionClosed,
  scrollTerminalToBottom,
} from "../terminalRegistry";

// eslint-disable-next-line @typescript-eslint/no-empty-function
const noop = (): void => {};

type CommandSegment = { type: "text"; value: string } | { type: "pause" } | { type: "clipboard" };

/**
 * The control character whose three-digit octal code follows the backslash
 * at `backslash`, as SecureCRT's send strings write one (`\003` is Ctrl+C),
 * or null. Only control codes are read: \000 to \037, and \177 (DEL).
 *
 * A printable character written as a code (\040 to \176) is left as written.
 * Read, it would let a command shown as digits type text no one reviewed:
 * `\162\155` would type "rm". A code above \177 is no ASCII character;
 * SecureCRT sends it as a single raw byte, which text cannot carry.
 */
function controlCharacter(raw: string, backslash: number): string | null {
  const digits = raw.slice(backslash + 1, backslash + 4);
  if (!/^[0-7]{3}$/.test(digits)) return null;
  const code = parseInt(digits, 8);
  return code <= 0o37 || code === 0o177 ? String.fromCharCode(code) : null;
}

/**
 * Resolve escape sequences in a command string.
 * Supported: \r (CR), \n (LF), \\ (backslash), \t (tab), \b (backspace),
 * \e (ESC 0x1B), \000 to \037 and \177 (the control character with that
 * octal code, as in SecureCRT), \p (1-second pause marker — handled by
 * caller). \v (clipboard paste) is handled separately at execution time.
 */
function parseEscapes(raw: string): { segments: CommandSegment[] } {
  const segments: CommandSegment[] = [];
  let buf = "";
  let i = 0;

  const flushBuf = (): void => {
    if (buf.length > 0) {
      segments.push({ type: "text", value: buf });
      buf = "";
    }
  };

  while (i < raw.length) {
    if (raw[i] === "\\" && i + 1 < raw.length) {
      const next = raw[i + 1];
      switch (next) {
        case "r":
          buf += "\r";
          i += 2;
          break;
        case "n":
          buf += "\n";
          i += 2;
          break;
        case "t":
          buf += "\t";
          i += 2;
          break;
        case "b":
          buf += "\b";
          i += 2;
          break;
        case "e":
          buf += "\x1b";
          i += 2;
          break;
        case "\\":
          buf += "\\";
          i += 2;
          break;
        case "p":
          flushBuf();
          segments.push({ type: "pause" });
          i += 2;
          break;
        case "v":
          flushBuf();
          segments.push({ type: "clipboard" });
          i += 2;
          break;
        default: {
          // `convert_send_string` in src-tauri/src/import/securecrt.rs scans
          // imported SecureCRT commands the same way; keep the two in step.
          const octal = controlCharacter(raw, i);
          if (octal === null) {
            // Only the backslash is taken: the character after it is read
            // again on its own. That is safe, since only `\\` would start
            // another escape, and its own case above takes it as a pair.
            buf += raw[i];
            i += 1;
          } else {
            buf += octal;
            i += 4;
          }
          break;
        }
      }
    } else {
      buf += raw[i];
      i += 1;
    }
  }
  flushBuf();
  return { segments };
}

function writeCmd(type: SessionType): string {
  if (type === "ssh") return "ssh_write";
  if (type === "telnet") return "telnet_write";
  return "pty_write";
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(resolve, ms);
  });
}

async function executeCommand(
  command: string,
  sessionId: string,
  sessionType: SessionType,
): Promise<void> {
  const { segments } = parseEscapes(command);
  const cmd = writeCmd(sessionType);

  // Sending a command is a user input action, so mirror what xterm.js does
  // for typed input (scrollOnUserInput) and jump the viewport back to the
  // live prompt. Without this, a user who had scrolled up into the
  // scrollback sees nothing happen and has to scroll down manually. Done
  // before the first write so the echoed output lands in view; later output
  // keeps the viewport pinned because it is already at the bottom.
  scrollTerminalToBottom(sessionId);

  try {
    for (const seg of segments) {
      if (seg.type === "text") {
        await invoke(cmd, { id: sessionId, data: seg.value });
      } else if (seg.type === "pause") {
        await sleep(1000);
      } else {
        // seg.type === "clipboard". Only the read is guarded: a failed write
        // must reach the handler below like any other segment's.
        let text = "";
        try {
          text = await readText();
        } catch {
          // Clipboard access denied or unavailable — skip silently.
        }
        if (text) {
          await invoke(cmd, { id: sessionId, data: text });
        }
      }
    }
  } catch (err: unknown) {
    // A tab whose exit event never arrived still looks alive, so a rejected
    // send may be the first sign its session is gone. Hand that to the
    // terminal so it runs the normal exit path.
    if (isSessionClosedError(err)) reportSessionClosed(sessionId);
    throw err;
  }
}

/**
 * Where the dragged button's preview is, from the pointer, in pixels: to the
 * right of it and below, as in the session tree, so that it covers neither
 * the button the pointer is on nor the line that says where a drop goes.
 */
const DRAG_PREVIEW_OFFSET = { x: 12, y: 16 };

/**
 * The library would announce every drag to a screen reader by the buttons'
 * ids, which are UUIDs, and describe a button as one to pick up with the
 * space bar, which presses it. A button is picked up with the pointer only,
 * so it says nothing.
 */
const SILENT_DRAG: DndContextProps["accessibility"] = {
  announcements: {
    onDragStart: () => undefined,
    onDragOver: () => undefined,
    onDragEnd: () => undefined,
    onDragCancel: () => undefined,
  },
  screenReaderInstructions: { draggable: "" },
};

interface CommandBarButtonProps {
  button: CommandButton;
  /** Whether this button is the one being dragged. */
  dragged: boolean;
  /** Whether any button is: the buttons then show no command as a tooltip. */
  dragging: boolean;
  /** Where a drop would go, if beside this button. */
  dropSide: "before" | "after" | null;
  onRun: (button: CommandButton) => void;
  onMenu: (button: CommandButton, x: number, y: number) => void;
}

/**
 * A button of the bar, which is picked up with the pointer to go to another
 * place. The library's attributes for a draggable are left off: they would
 * have a screen reader announce a button to be picked up with the space bar,
 * which presses it. The context menu's Move left and Move right are the way
 * without a pointer.
 */
function CommandBarButton({
  button,
  dragged,
  dragging,
  dropSide,
  onRun,
  onMenu,
}: CommandBarButtonProps): React.JSX.Element {
  const { listeners, setNodeRef } = useDraggable({ id: button.id });
  let className = "command-bar-btn";
  if (dragged) className += " command-bar-btn-dragged";
  if (dropSide) className += ` command-bar-btn-drop-${dropSide}`;
  return (
    <button
      ref={setNodeRef}
      {...listeners}
      type="button"
      className={className}
      data-button-id={button.id}
      style={{ "--btn-color": button.color } as React.CSSProperties}
      title={dragging ? undefined : button.command}
      onClick={() => {
        onRun(button);
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        onMenu(button, e.clientX, e.clientY);
      }}
    >
      {button.name}
    </button>
  );
}

interface CommandBarProps {
  uiScale: number;
}

export function CommandBar({ uiScale }: CommandBarProps): React.JSX.Element {
  const { t } = useTranslation();
  const {
    commandButtons,
    addCommandButton,
    updateCommandButton,
    removeCommandButton,
    reorderCommandButtons,
  } = useSettingsStore();
  const [dialogState, setDialogState] = useState<{
    mode: "create" | "edit";
    editId?: string;
    initial?: CommandButtonFormData;
  } | null>(null);
  const [btnCtx, setBtnCtx] = useState<{ x: number; y: number; buttonId: string } | null>(null);
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);
  const [collapsed, setCollapsed] = useState(false);
  const [freeformInput, setFreeformInput] = useState("");

  const focusActiveTerminal = useCallback((): void => {
    const termEl = document.querySelector<HTMLElement>(
      ".terminal-instance:not(.terminal-instance-hidden) .xterm-helper-textarea",
    );
    termEl?.focus();
  }, []);

  const handleButtonClick = useCallback(
    (btn: CommandButton) => {
      const { tabs, activeTabId } = useTerminalStore.getState();
      const activeTab = tabs.find((tb) => tb.id === activeTabId);
      if (!activeTab || activeTab.exited) return;
      executeCommand(btn.command, activeTab.id, activeTab.type).catch(noop);
      focusActiveTerminal();
    },
    [focusActiveTerminal],
  );

  const sendFreeformToActive = useCallback((): void => {
    if (!freeformInput) return;
    const { tabs, activeTabId } = useTerminalStore.getState();
    const activeTab = tabs.find((tb) => tb.id === activeTabId);
    if (!activeTab || activeTab.exited) return;
    executeCommand(freeformInput, activeTab.id, activeTab.type).catch(noop);
    focusActiveTerminal();
  }, [freeformInput, focusActiveTerminal]);

  const sendFreeformToAll = useCallback((): void => {
    if (!freeformInput) return;
    const { tabs } = useTerminalStore.getState();
    const alive = tabs.filter((tb) => !tb.exited);
    if (alive.length === 0) return;
    for (const tab of alive) {
      executeCommand(freeformInput, tab.id, tab.type).catch(noop);
    }
    focusActiveTerminal();
  }, [freeformInput, focusActiveTerminal]);

  // ── Drag and drop ──────────────────────────────────────────────────

  // A button is picked up with the pointer alone, once it has moved 5 px
  // with the primary button down; a press that moves less is a click. The
  // library's keyboard sensor would pick one up on Enter and Space, which
  // press it. After a drag the library stops the click that the release
  // makes, so a dropped button sends nothing.
  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 5 } }));
  const zoom = uiScale / 100;
  const [draggedId, setDraggedId] = useState<string | null>(null);
  const [drop, setDrop] = useState<ButtonDrop | null>(null);
  const dropRef = useRef<ButtonDrop | null>(null);
  // Where the pointer was last, in the window.
  const pointer = useRef<{ x: number; y: number } | null>(null);
  const rowRef = useRef<HTMLDivElement | null>(null);
  const previewRef = useRef<HTMLDivElement | null>(null);

  // The preview is outside the bar, which is scaled with CSS `zoom`, and is
  // placed by the pointer's own coordinates, so no scale comes between the
  // two. It is scaled to the bar's size by a transform of its own.
  const placePreview = useCallback((): void => {
    const preview = previewRef.current;
    const at = pointer.current;
    if (!preview || !at) return;
    const x = String(at.x + DRAG_PREVIEW_OFFSET.x);
    const y = String(at.y + DRAG_PREVIEW_OFFSET.y);
    preview.style.transform = `translate(${x}px, ${y}px) scale(${String(zoom)})`;
  }, [zoom]);
  const setPreview = useCallback(
    (preview: HTMLDivElement | null): void => {
      previewRef.current = preview;
      placePreview();
    },
    [placePreview],
  );

  const endDrag = useCallback((): void => {
    pointer.current = null;
    dropRef.current = null;
    setDrop(null);
    setDraggedId(null);
  }, []);

  // A press on a button gives it the focus, and a dragged button is not
  // clicked, which gives it back to the terminal. Left on the button, the
  // focus would have the next Enter or Space typed "into the terminal"
  // press it and send its command. Not under a dialog, which does not move
  // the focus (§5.6 of the design).
  const endDragAndRefocus = useCallback((): void => {
    endDrag();
    const row = rowRef.current;
    if (row?.contains(document.activeElement) && !dialogOpen()) focusActiveTerminal();
  }, [endDrag, focusActiveTerminal]);

  const handleDragStart = useCallback((event: DragStartEvent): void => {
    // Nothing is picked up under a dialog, as one that opened between the
    // press and the first move.
    if (dialogOpen()) return;
    const from = event.activatorEvent;
    pointer.current = from instanceof MouseEvent ? { x: from.clientX, y: from.clientY } : null;
    setDraggedId(String(event.active.id));
  }, []);

  // What a drop of the dragged button would do where the pointer is now,
  // against the buttons as the store has them now.
  const dropNow = useCallback((): ButtonDrop | null => {
    const at = pointer.current;
    const row = rowRef.current;
    if (draggedId === null || !at || !row || dialogOpen()) return null;
    const point = buttonPointAt(at.x, at.y, row, zoom);
    if (!point) return null;
    const ids = useSettingsStore.getState().commandButtons.map((b) => b.id);
    return buttonDropAt(ids, draggedId, point);
  }, [draggedId, zoom]);

  // A dialog that opens while a button is held ends the drag: the release
  // that follows, under the dialog, drops nothing.
  useOnDialogOpen(endDrag);

  const handleDragEnd = useCallback((): void => {
    // Asked again at the release, and not taken from the last move.
    const dropped = dropNow();
    endDragAndRefocus();
    if (dropped) reorderCommandButtons(dropped.from, dropped.to);
  }, [dropNow, endDragAndRefocus, reorderCommandButtons]);

  // While a button is dragged: where the pointer is, and what a drop there
  // would do.
  useEffect(() => {
    if (draggedId === null) return;
    const judge = (): void => {
      const next = dropNow();
      if (sameButtonDrop(dropRef.current, next)) return;
      dropRef.current = next;
      setDrop(next);
    };
    const handleMove = (e: PointerEvent): void => {
      if (!e.isPrimary) return;
      pointer.current = { x: e.clientX, y: e.clientY };
      placePreview();
      judge();
    };
    document.addEventListener("pointermove", handleMove);
    judge();
    return () => {
      document.removeEventListener("pointermove", handleMove);
    };
  }, [draggedId, dropNow, placePreview]);

  const openButtonMenu = useCallback((btn: CommandButton, x: number, y: number): void => {
    setBtnCtx({ x, y, buttonId: btn.id });
  }, []);

  // The dragged button, for its preview. One that is gone while it is held
  // has none.
  const draggedButton =
    draggedId === null ? undefined : commandButtons.find((b) => b.id === draggedId);

  const handleSave = useCallback(
    (data: CommandButtonFormData) => {
      if (dialogState?.mode === "edit" && dialogState.editId) {
        updateCommandButton(dialogState.editId, data);
      } else {
        addCommandButton({ id: crypto.randomUUID(), ...data });
      }
      setDialogState(null);
    },
    [dialogState, addCommandButton, updateCommandButton],
  );

  const handleSendToAll = useCallback(
    (btn: CommandButton) => {
      const { tabs } = useTerminalStore.getState();
      const alive = tabs.filter((tb) => !tb.exited);
      if (alive.length === 0) return;
      for (const tab of alive) {
        executeCommand(btn.command, tab.id, tab.type).catch(noop);
      }
      focusActiveTerminal();
    },
    [focusActiveTerminal],
  );

  const getContextItems = useCallback(
    (buttonId: string): ContextMenuItem[] => {
      const index = commandButtons.findIndex((b) => b.id === buttonId);
      const { tabs } = useTerminalStore.getState();
      const aliveCount = tabs.filter((tb) => !tb.exited).length;
      return [
        {
          label: t("commandBar.sendToAll"),
          disabled: aliveCount < 2,
          onClick: () => {
            const btn = commandButtons.find((b) => b.id === buttonId);
            if (btn) handleSendToAll(btn);
          },
        },
        {
          label: t("commandBar.edit"),
          onClick: () => {
            const btn = commandButtons.find((b) => b.id === buttonId);
            if (btn) {
              setDialogState({
                mode: "edit",
                editId: btn.id,
                initial: { name: btn.name, command: btn.command, color: btn.color },
              });
            }
          },
        },
        {
          label: t("commandBar.copy"),
          onClick: () => {
            const btn = commandButtons.find((b) => b.id === buttonId);
            if (btn) {
              setDialogState({
                mode: "create",
                initial: {
                  name: (btn.name + " (copy)").slice(0, MAX_NAME_LENGTH),
                  command: btn.command,
                  color: btn.color,
                },
              });
            }
          },
        },
        {
          label: t("commandBar.moveLeft"),
          disabled: index <= 0,
          onClick: () => {
            if (index > 0) reorderCommandButtons(index, index - 1);
          },
        },
        {
          label: t("commandBar.moveRight"),
          disabled: index >= commandButtons.length - 1,
          onClick: () => {
            if (index < commandButtons.length - 1) reorderCommandButtons(index, index + 1);
          },
        },
        {
          label: t("commandBar.delete"),
          danger: true,
          onClick: () => {
            setConfirmDeleteId(buttonId);
          },
        },
      ];
    },
    [commandButtons, t, reorderCommandButtons, handleSendToAll],
  );

  return (
    <>
      <div className="command-bar" style={{ "--ui-scale": uiScale / 100 } as React.CSSProperties}>
        <div
          ref={rowRef}
          className={`command-bar-row${draggedId === null ? "" : " command-bar-row-dragging"}`}
        >
          <button
            type="button"
            className="command-bar-toggle"
            title={t(collapsed ? "commandBar.show" : "commandBar.hide")}
            onClick={() => {
              setCollapsed((prev) => !prev);
            }}
          >
            {collapsed ? "\u25B6" : "\u25BC"}
          </button>
          {!collapsed && (
            <>
              <button
                type="button"
                className="command-bar-add"
                title={t("commandBar.addButton")}
                onClick={() => {
                  setDialogState({ mode: "create" });
                }}
              >
                +
              </button>
              <DndContext
                sensors={sensors}
                autoScroll={false}
                accessibility={SILENT_DRAG}
                onDragStart={handleDragStart}
                onDragEnd={handleDragEnd}
                onDragCancel={endDragAndRefocus}
              >
                {commandButtons.map((btn) => (
                  <CommandBarButton
                    key={btn.id}
                    button={btn}
                    dragged={btn.id === draggedId}
                    dragging={draggedId !== null}
                    dropSide={drop?.mark.id === btn.id ? drop.mark.side : null}
                    onRun={handleButtonClick}
                    onMenu={openButtonMenu}
                  />
                ))}
              </DndContext>
            </>
          )}
        </div>
        {!collapsed && (
          <div className="command-bar-input-row">
            <input
              type="text"
              className="command-bar-input"
              value={freeformInput}
              onChange={(e) => {
                setFreeformInput(e.target.value);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  if (e.shiftKey) {
                    sendFreeformToAll();
                  } else {
                    sendFreeformToActive();
                  }
                }
              }}
              placeholder={t("commandBar.freeformPlaceholder", {
                defaultValue: "Type a command (Enter = active, Shift+Enter = all)",
              })}
            />
            <button
              type="button"
              className="command-bar-send-btn"
              onClick={sendFreeformToActive}
              disabled={!freeformInput}
              title={t("commandBar.sendToActive", { defaultValue: "Send to active session" })}
            >
              {t("commandBar.sendToActiveShort", { defaultValue: "Send" })}
            </button>
            <button
              type="button"
              className="command-bar-send-btn"
              onClick={sendFreeformToAll}
              disabled={!freeformInput}
              title={t("commandBar.sendToAll")}
            >
              {t("commandBar.sendToAllShort", { defaultValue: "Send to all" })}
            </button>
          </div>
        )}
      </div>
      {/* The dragged button's preview, beside the pointer. In the body, as the
          tree's is, and under the dialogs. */}
      {draggedButton &&
        createPortal(
          <div
            ref={setPreview}
            className="command-bar-drag-preview"
            style={{ "--btn-color": draggedButton.color } as React.CSSProperties}
            aria-hidden="true"
          >
            {draggedButton.name}
          </div>,
          document.body,
        )}
      {dialogState && (
        <CommandButtonDialog
          initial={dialogState.initial}
          onSave={handleSave}
          onCancel={() => {
            setDialogState(null);
          }}
        />
      )}
      {btnCtx && (
        <ContextMenu
          x={btnCtx.x}
          y={btnCtx.y}
          items={getContextItems(btnCtx.buttonId)}
          onClose={() => {
            setBtnCtx(null);
          }}
        />
      )}
      {confirmDeleteId && (
        <ConfirmDialog
          message={t("commandBar.deleteConfirm")}
          confirmLabel={t("commandBar.delete")}
          onConfirm={() => {
            removeCommandButton(confirmDeleteId);
            setConfirmDeleteId(null);
          }}
          onCancel={() => {
            setConfirmDeleteId(null);
          }}
        />
      )}
    </>
  );
}
