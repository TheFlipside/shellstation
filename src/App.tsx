import React, { useCallback, useRef, useState } from "react";
import "./App.css";
import { SessionSidebar } from "./components/SessionSidebar";
import { TerminalTabs } from "./components/TerminalTabs";

function App(): React.JSX.Element {
  const [sidebarWidth, setSidebarWidth] = useState(260);
  const dragging = useRef(false);

  const handleMouseDown = useCallback(() => {
    dragging.current = true;
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";

    const handleMouseMove = (e: MouseEvent): void => {
      if (!dragging.current) return;
      const newWidth = Math.max(160, Math.min(600, e.clientX));
      setSidebarWidth(newWidth);
    };

    const handleMouseUp = (): void => {
      dragging.current = false;
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
      document.removeEventListener("mousemove", handleMouseMove);
      document.removeEventListener("mouseup", handleMouseUp);
    };

    document.addEventListener("mousemove", handleMouseMove);
    document.addEventListener("mouseup", handleMouseUp);
  }, []);

  return (
    <div className="app">
      <div className="app-layout">
        <div className="app-sidebar" style={{ width: `${String(sidebarWidth)}px` }}>
          <SessionSidebar />
        </div>
        <div
          className="app-divider"
          onMouseDown={handleMouseDown}
          role="separator"
          aria-orientation="vertical"
          tabIndex={0}
        />
        <div className="app-main">
          <TerminalTabs />
        </div>
      </div>
    </div>
  );
}

export default App;
