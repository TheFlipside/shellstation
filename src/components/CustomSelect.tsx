import React, { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

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
      const target = e.target as Node;
      const insideTrigger = wrapperRef.current?.contains(target) ?? false;
      // The dropdown is portaled to document.body and is not a DOM descendant
      // of the wrapper, so check it separately.
      const insideDropdown = listRef.current?.contains(target) ?? false;
      if (!insideTrigger && !insideDropdown) {
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

  // Compute fixed-positioned coordinates for the portal-mounted dropdown.
  // Portal into the nearest .dialog-overlay (which owns the CSS zoom) so
  // that position:fixed shares the same coordinate space as the trigger.
  //
  // Cross-engine zoom compensation: CSS zoom is non-standard and engines
  // disagree on whether getBoundingClientRect returns pre- or post-zoom
  // coords. We detect the effective zoom empirically by comparing
  // offsetWidth (always layout pixels) with rect.width (visual on
  // Chromium, layout on WebKitGTK). Dividing rect values by this ratio
  // normalises both engines into the layout coordinate space that
  // position:fixed uses inside the zoomed container.
  const [portalTarget, setPortalTarget] = useState<HTMLElement | null>(null);
  const [dropdownPos, setDropdownPos] = useState<{
    top: number;
    left: number;
    width: number;
    maxHeight: number;
    viewportH: number;
    flipUp: boolean;
  } | null>(null);

  const updatePosition = useCallback(() => {
    const trigger = triggerRef.current;
    if (!trigger) return;
    const overlay = trigger.closest<HTMLElement>(".dialog-overlay");
    setPortalTarget(overlay ?? document.body);
    const rect = trigger.getBoundingClientRect();
    const layoutWidth = trigger.offsetWidth;
    const effectiveZoom = layoutWidth > 0 ? rect.width / layoutWidth : 1;
    const top = rect.top / effectiveZoom;
    const bottom = rect.bottom / effectiveZoom;
    const left = rect.left / effectiveZoom;
    const width = layoutWidth;
    const viewportH = window.innerHeight / effectiveZoom;
    const margin = 4;
    const desiredMax = 240;
    const spaceBelow = viewportH - bottom - margin;
    const spaceAbove = top - margin;
    const flipUp = spaceBelow < Math.min(desiredMax, 160) && spaceAbove > spaceBelow;
    const maxHeight = Math.max(80, Math.min(desiredMax, flipUp ? spaceAbove : spaceBelow));
    setDropdownPos({
      top: flipUp ? top - margin : bottom + margin,
      left,
      width,
      maxHeight,
      viewportH,
      flipUp,
    });
  }, []);

  useLayoutEffect(() => {
    if (!open) {
      setDropdownPos(null);
      return;
    }
    updatePosition();
    const handleReposition = (): void => {
      updatePosition();
    };
    window.addEventListener("resize", handleReposition);
    window.addEventListener("scroll", handleReposition, true);
    return () => {
      window.removeEventListener("resize", handleReposition);
      window.removeEventListener("scroll", handleReposition, true);
    };
  }, [open, updatePosition]);

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
      {open &&
        dropdownPos &&
        portalTarget &&
        createPortal(
          <div
            ref={listRef}
            className={`custom-select-dropdown${dropdownPos.flipUp ? " custom-select-dropdown-flip" : ""}`}
            role="listbox"
            tabIndex={-1}
            style={{
              position: "fixed",
              top: dropdownPos.flipUp ? "auto" : dropdownPos.top,
              bottom: dropdownPos.flipUp ? dropdownPos.viewportH - dropdownPos.top : "auto",
              left: dropdownPos.left,
              width: dropdownPos.width,
              maxHeight: dropdownPos.maxHeight,
            }}
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
          </div>,
          portalTarget,
        )}
    </div>
  );
}
