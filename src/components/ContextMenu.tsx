import React, { useEffect, useRef } from "react";
import ReactDOM from "react-dom";
import { useSettingsStore } from "../stores/settingsStore";

export interface ContextMenuItem {
  label: string;
  onClick: () => void;
  danger?: boolean;
  disabled?: boolean;
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
  const zoom = uiScale / 100;

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

  // After mount, flip the menu upward/leftward if it overflows the viewport.
  // getBoundingClientRect() returns the visually-rendered rect (post-scale).
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    if (rect.bottom > window.innerHeight) {
      // Position so the bottom of the menu aligns with the click point.
      const newTop = y - rect.height;
      el.style.top = `${String(Math.max(0, newTop))}px`;
    }
    if (rect.right > window.innerWidth) {
      const newLeft = x - rect.width;
      el.style.left = `${String(Math.max(0, newLeft))}px`;
    }
  }, [x, y]);

  // Render via portal at document.body so the menu escapes any zoomed /
  // overflow-hidden ancestors. Use transform:scale instead of the CSS zoom
  // property — zoom creates a new containing block that interacts badly
  // with overflow:hidden on ancestor elements.
  const style: React.CSSProperties = {
    left: x,
    top: y,
    transform: `scale(${String(zoom)})`,
    transformOrigin: "top left",
  };

  return ReactDOM.createPortal(
    <div ref={ref} className="context-menu" style={style}>
      {items.map((item) => (
        <button
          key={item.label}
          type="button"
          className={`context-menu-item${item.danger ? " context-menu-item-danger" : ""}${item.disabled ? " context-menu-item-disabled" : ""}`}
          disabled={item.disabled}
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
