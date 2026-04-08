import React, { useCallback, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { useSettingsStore, type CommandButton } from "../stores/settingsStore";
import { useTerminalStore, type SessionType } from "../stores/terminalStore";
import {
  CommandButtonDialog,
  MAX_NAME_LENGTH,
  type CommandButtonFormData,
} from "./CommandButtonDialog";
import { ConfirmDialog } from "./ConfirmDialog";
import { ContextMenu, type ContextMenuItem } from "./ContextMenu";

// eslint-disable-next-line @typescript-eslint/no-empty-function
const noop = (): void => {};

type CommandSegment = { type: "text"; value: string } | { type: "pause" } | { type: "clipboard" };

/**
 * Resolve escape sequences in a command string.
 * Supported: \r (CR), \n (LF), \\ (backslash), \t (tab), \b (backspace),
 * \e (ESC 0x1B), \p (1-second pause marker — handled by caller).
 * \v (clipboard paste) is handled separately at execution time.
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
        default:
          buf += raw[i];
          i += 1;
          break;
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

  for (const seg of segments) {
    if (seg.type === "text") {
      await invoke(cmd, { id: sessionId, data: seg.value });
    } else if (seg.type === "pause") {
      await sleep(1000);
    } else {
      // seg.type === "clipboard"
      try {
        const text = await navigator.clipboard.readText();
        if (text) {
          await invoke(cmd, { id: sessionId, data: text });
        }
      } catch {
        // Clipboard access denied or unavailable — skip silently.
      }
    }
  }
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

  const handleButtonClick = useCallback((btn: CommandButton) => {
    const { tabs, activeTabId } = useTerminalStore.getState();
    const activeTab = tabs.find((tb) => tb.id === activeTabId);
    if (!activeTab || activeTab.exited) return;
    executeCommand(btn.command, activeTab.id, activeTab.type).catch(noop);
    // Return focus to the active terminal so the user can interact immediately.
    const termEl = document.querySelector<HTMLElement>(
      ".terminal-instance:not(.terminal-instance-hidden) .xterm-helper-textarea",
    );
    termEl?.focus();
  }, []);

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

  const getContextItems = useCallback(
    (buttonId: string): ContextMenuItem[] => {
      const index = commandButtons.findIndex((b) => b.id === buttonId);
      return [
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
    [commandButtons, t, reorderCommandButtons],
  );

  return (
    <>
      <div className="command-bar" style={{ "--ui-scale": uiScale / 100 } as React.CSSProperties}>
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
            {commandButtons.map((btn) => (
              <button
                key={btn.id}
                type="button"
                className="command-bar-btn"
                style={{ "--btn-color": btn.color } as React.CSSProperties}
                title={btn.command}
                onClick={() => {
                  handleButtonClick(btn);
                }}
                onContextMenu={(e) => {
                  e.preventDefault();
                  setBtnCtx({ x: e.clientX, y: e.clientY, buttonId: btn.id });
                }}
              >
                {btn.name}
              </button>
            ))}
          </>
        )}
      </div>
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
