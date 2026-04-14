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
  // Typeahead buffer: accumulates printable keys pressed while the dropdown
  // is open, so the user can jump to an option by typing its name. Resets
  // after TYPEAHEAD_TIMEOUT_MS of inactivity.
  const typeaheadRef = useRef<{ buffer: string; timer: number | null }>({
    buffer: "",
    timer: null,
  });

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

  const TYPEAHEAD_TIMEOUT_MS = 600;

  // Find the index of the first option whose label matches the typeahead
  // buffer. Prefers a case-insensitive prefix match; falls back to any
  // substring match so users can jump to an entry by any part of its name.
  const findTypeaheadIndex = useCallback(
    (buffer: string, from: number): number => {
      if (!buffer) return -1;
      const needle = buffer.toLowerCase();
      const n = options.length;
      for (let step = 0; step < n; step += 1) {
        const i = (from + step) % n;
        if (options[i].label.toLowerCase().startsWith(needle)) return i;
      }
      for (let step = 0; step < n; step += 1) {
        const i = (from + step) % n;
        if (options[i].label.toLowerCase().includes(needle)) return i;
      }
      return -1;
    },
    [options],
  );

  const scheduleTypeaheadReset = useCallback(() => {
    const state = typeaheadRef.current;
    if (state.timer !== null) {
      window.clearTimeout(state.timer);
    }
    state.timer = window.setTimeout(() => {
      typeaheadRef.current.buffer = "";
      typeaheadRef.current.timer = null;
    }, TYPEAHEAD_TIMEOUT_MS);
  }, []);

  const handleTypeahead = useCallback(
    (char: string): boolean => {
      const state = typeaheadRef.current;
      // When the same single character is tapped repeatedly, cycle through
      // matching options instead of appending to the buffer — mirrors the
      // native <select> behavior users already expect.
      const repeatSameChar = state.buffer.length === 1 && state.buffer === char;
      state.buffer = repeatSameChar ? char : state.buffer + char;
      const startFrom = repeatSameChar ? (focusIndex + 1) % Math.max(options.length, 1) : 0;
      const idx = findTypeaheadIndex(state.buffer, startFrom);
      if (idx >= 0) {
        setFocusIndex(idx);
      }
      scheduleTypeaheadReset();
      return idx >= 0;
    },
    [findTypeaheadIndex, focusIndex, options.length, scheduleTypeaheadReset],
  );

  const handleTriggerKeyDown = (e: React.KeyboardEvent): void => {
    // Keyboard handling lives on the trigger (focus stays there even while
    // the dropdown is open) so both closed-state and open-state navigation
    // flow through a single handler.
    if (!open) {
      if (e.key === "Enter" || e.key === " " || e.key === "ArrowDown") {
        e.preventDefault();
        openDropdown();
      }
      return;
    }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setFocusIndex((i) => Math.min(i + 1, options.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setFocusIndex((i) => Math.max(i - 1, 0));
    } else if (e.key === "Home") {
      e.preventDefault();
      setFocusIndex(0);
    } else if (e.key === "End") {
      e.preventDefault();
      setFocusIndex(options.length - 1);
    } else if (e.key === "Enter" && focusIndex >= 0) {
      e.preventDefault();
      onChange(options[focusIndex].value);
      close();
      triggerRef.current?.focus();
    } else if (e.key === "Tab") {
      close();
    } else if (e.key === "Backspace") {
      e.preventDefault();
      typeaheadRef.current.buffer = typeaheadRef.current.buffer.slice(0, -1);
      if (typeaheadRef.current.buffer) {
        const idx = findTypeaheadIndex(typeaheadRef.current.buffer, 0);
        if (idx >= 0) setFocusIndex(idx);
        scheduleTypeaheadReset();
      }
    } else if (e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) {
      // Printable character — feed it into the typeahead buffer.
      e.preventDefault();
      handleTypeahead(e.key);
    }
  };

  // Clear any pending typeahead timer on unmount.
  useEffect(() => {
    return () => {
      const state = typeaheadRef.current;
      if (state.timer !== null) {
        window.clearTimeout(state.timer);
      }
    };
  }, []);

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
