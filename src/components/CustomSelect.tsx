import React, { useCallback, useEffect, useRef, useState } from "react";

export interface SelectOption {
  value: string;
  label: string;
}

interface CustomSelectProps {
  id?: string;
  value: string;
  options: SelectOption[];
  onChange: (value: string) => void;
  placeholder?: string;
}

export function CustomSelect({
  id,
  value,
  options,
  onChange,
  placeholder,
}: CustomSelectProps): React.JSX.Element {
  const [open, setOpen] = useState(false);
  const [focusIndex, setFocusIndex] = useState(-1);
  const wrapperRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const selected = options.find((o) => o.value === value);
  const label = selected ? selected.label : (placeholder ?? "");

  const close = useCallback(() => {
    setOpen(false);
    setFocusIndex(-1);
  }, []);

  // Close on outside click or scroll
  useEffect(() => {
    if (!open) return;
    const handleClick = (e: MouseEvent): void => {
      if (wrapperRef.current && !wrapperRef.current.contains(e.target as Node)) {
        close();
      }
    };
    const handleKey = (e: KeyboardEvent): void => {
      if (e.key === "Escape") {
        close();
        triggerRef.current?.focus();
      }
    };
    document.addEventListener("mousedown", handleClick);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("mousedown", handleClick);
      document.removeEventListener("keydown", handleKey);
    };
  }, [open, close]);

  const openDropdown = useCallback(() => {
    const idx = options.findIndex((o) => o.value === value);
    setFocusIndex(idx >= 0 ? idx : 0);
    setOpen(true);
  }, [options, value]);

  const handleTriggerKeyDown = (e: React.KeyboardEvent): void => {
    if (e.key === "Enter" || e.key === " " || e.key === "ArrowDown") {
      e.preventDefault();
      openDropdown();
    }
  };

  const handleListKeyDown = (e: React.KeyboardEvent): void => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setFocusIndex((i) => Math.min(i + 1, options.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setFocusIndex((i) => Math.max(i - 1, 0));
    } else if (e.key === "Enter" && focusIndex >= 0) {
      e.preventDefault();
      onChange(options[focusIndex].value);
      close();
      triggerRef.current?.focus();
    } else if (e.key === "Tab") {
      close();
    }
  };

  // Scroll focused item into view
  useEffect(() => {
    if (!open || focusIndex < 0) return;
    const list = listRef.current;
    if (!list) return;
    const item = list.children[focusIndex] as HTMLElement | undefined;
    item?.scrollIntoView({ block: "nearest" });
  }, [open, focusIndex]);

  // Flip upward if dropdown overflows viewport
  const [flipUp, setFlipUp] = useState(false);
  useEffect(() => {
    if (!open) {
      setFlipUp(false);
      return;
    }
    const el = listRef.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    if (rect.bottom > window.innerHeight) {
      setFlipUp(true);
    }
  }, [open]);

  return (
    <div ref={wrapperRef} className="custom-select-wrapper">
      <button
        ref={triggerRef}
        id={id}
        type="button"
        className={`custom-select-trigger${open ? " custom-select-trigger-open" : ""}`}
        onClick={() => {
          if (open) {
            close();
          } else {
            openDropdown();
          }
        }}
        onKeyDown={handleTriggerKeyDown}
        aria-haspopup="listbox"
        aria-expanded={open}
      >
        <span className="custom-select-label">{label}</span>
        <span className="custom-select-arrow" aria-hidden="true">
          &#x25BE;
        </span>
      </button>
      {open && (
        <div
          ref={listRef}
          className={`custom-select-dropdown${flipUp ? " custom-select-dropdown-flip" : ""}`}
          role="listbox"
          onKeyDown={handleListKeyDown}
          tabIndex={-1}
        >
          {options.map((opt, i) => (
            <div
              key={opt.value}
              className={`custom-select-option${opt.value === value ? " custom-select-option-selected" : ""}${i === focusIndex ? " custom-select-option-focused" : ""}`}
              role="option"
              aria-selected={opt.value === value}
              onMouseEnter={() => {
                setFocusIndex(i);
              }}
              onClick={() => {
                onChange(opt.value);
                close();
                triggerRef.current?.focus();
              }}
            >
              {opt.label}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
