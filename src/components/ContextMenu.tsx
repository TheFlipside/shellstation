import React, { useEffect, useRef } from "react";
import ReactDOM from "react-dom";
import { useSettingsStore } from "../stores/settingsStore";

export interface ContextMenuItem {
  label: string;
  onClick: () => void;
  danger?: boolean;
}

interface ContextMenuProps {
  x: number;
  y: number;
  items: ContextMenuItem[];
  onClose: () => void;
}

export function ContextMenu({ x, y, items, onClose }: ContextMenuProps): React.JSX.Element {
  const ref = useRef<HTMLDivElement>(null);
  const uiScale = useSettingsStore((s) => s.uiScale);

  useEffect(() => {
    const handleClick = (e: MouseEvent): void => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        onClose();
      }
    };
    const handleKey = (e: KeyboardEvent): void => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("mousedown", handleClick);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("mousedown", handleClick);
      document.removeEventListener("keydown", handleKey);
    };
  }, [onClose]);

  const zoom = uiScale / 100;

  // Render via portal at document.body so the menu is outside any zoomed
  // ancestor containers.  position:fixed then works relative to the true
  // viewport and coordinates only need compensating for the menu's own zoom.
  return ReactDOM.createPortal(
    <div ref={ref} className="context-menu" style={{ left: x / zoom, top: y / zoom, zoom }}>
      {items.map((item) => (
        <button
          key={item.label}
          type="button"
          className={`context-menu-item ${item.danger ? "context-menu-item-danger" : ""}`}
          onClick={() => {
            item.onClick();
            onClose();
          }}
        >
          {item.label}
        </button>
      ))}
    </div>,
    document.body,
  );
}
