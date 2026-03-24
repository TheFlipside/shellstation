import React from "react";
import "./App.css";
import { TerminalTabs } from "./components/TerminalTabs";

function App(): React.JSX.Element {
  return (
    <div className="app">
      <TerminalTabs />
    </div>
  );
}

export default App;
