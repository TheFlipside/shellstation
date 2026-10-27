/**
 * Settings store: the data export / import helpers.
 *
 * An imported file is untrusted input that lands in persisted state, so
 * these tests pin that every value goes through the same validation as a
 * change made in the Settings dialog, and that a command button import adds
 * buttons without ever changing or duplicating the ones already there.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  getExportableCommandButtons,
  getExportableSettings,
  importCommandButtons,
  importSettings,
  useSettingsStore,
  type CommandButton,
} from "./settingsStore";

// Held separately so assertions never reference it as an unbound method.
const { changeLanguage } = vi.hoisted(() => ({
  changeLanguage: vi.fn(() => Promise.resolve()),
}));
vi.mock("../i18n", () => ({
  default: { t: (key: string) => key, changeLanguage },
}));

const ID_A = "9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d";
const ID_B = "1b9d6bcd-bbfd-4b2d-9b5d-ab8dfbbd4bed";
const ID_C = "6ec0bd7f-11c0-43da-975e-2a8ad9ebae0b";

function button(id: string, name: string, command = "uptime\\r"): CommandButton {
  return { id, name, command, color: "#89b4fa" };
}

beforeEach(() => {
  useSettingsStore.setState(useSettingsStore.getInitialState(), true);
});

describe("getExportableSettings", () => {
  it("carries every persisted setting except the command buttons", () => {
    useSettingsStore.setState({ themeMode: "light", commandButtons: [button(ID_A, "a")] });
    const exported = getExportableSettings();
    expect(exported).not.toHaveProperty("commandButtons");
    expect(exported.themeMode).toBe("light");
    // No actions leak into the file.
    for (const value of Object.values(exported)) {
      expect(typeof value).not.toBe("function");
    }
    const state = useSettingsStore.getState() as unknown as Record<string, unknown>;
    const dataKeys = Object.keys(state).filter(
      (key) => typeof state[key] !== "function" && key !== "commandButtons",
    );
    expect(Object.keys(exported).sort()).toEqual(dataKeys.sort());
  });
});

describe("importSettings", () => {
  it("round-trips an export", () => {
    useSettingsStore.setState({
      themeMode: "light",
      uiScale: 110,
      terminalFontFamily: "Fira Code",
      terminalBackgroundColor: "#101010",
      copyOnSelect: true,
      keepaliveInterval: 30,
    });
    const exported = getExportableSettings();
    useSettingsStore.setState(useSettingsStore.getInitialState(), true);

    importSettings(exported);

    expect(getExportableSettings()).toEqual(exported);
  });

  it("ignores values of the wrong type and keeps the current one", () => {
    importSettings({
      themeMode: "neon",
      copyOnSelect: "yes",
      uiScale: "110",
      terminalFontFamily: "Comic Sans MS",
      terminalBackgroundColor: "red",
      sidebarWidth: 5000,
    });
    const initial = useSettingsStore.getInitialState();
    const state = useSettingsStore.getState();
    expect(state.themeMode).toBe(initial.themeMode);
    expect(state.copyOnSelect).toBe(initial.copyOnSelect);
    expect(state.uiScale).toBe(initial.uiScale);
    expect(state.terminalFontFamily).toBe(initial.terminalFontFamily);
    expect(state.terminalBackgroundColor).toBe(initial.terminalBackgroundColor);
    expect(state.sidebarWidth).toBe(initial.sidebarWidth);
  });

  it("only accepts whole numbers in range for values the backend reads as integers", () => {
    importSettings({ connectTimeout: 1.5, keepaliveInterval: 1e12, keepaliveMax: 101 });
    const initial = useSettingsStore.getInitialState();
    expect(useSettingsStore.getState().connectTimeout).toBe(initial.connectTimeout);
    expect(useSettingsStore.getState().keepaliveInterval).toBe(initial.keepaliveInterval);
    expect(useSettingsStore.getState().keepaliveMax).toBe(initial.keepaliveMax);

    importSettings({ connectTimeout: 60, keepaliveInterval: 0, keepaliveMax: 10 });
    expect(useSettingsStore.getState().connectTimeout).toBe(60);
    expect(useSettingsStore.getState().keepaliveInterval).toBe(0);
    expect(useSettingsStore.getState().keepaliveMax).toBe(10);
  });

  it.each([
    ["connectTimeout", 3600, [0, 3601]],
    ["keepaliveInterval", 3600, [-1, 3601]],
    ["keepaliveMax", 100, [0, 101]],
  ] as const)("takes %s up to %d and nothing outside its range", (key, max, rejected) => {
    importSettings({ [key]: max });
    expect(useSettingsStore.getState()[key]).toBe(max);
    for (const value of rejected) {
      importSettings({ [key]: value });
      expect(useSettingsStore.getState()[key]).toBe(max);
    }
  });

  it("clamps the UI scale the way the Settings dialog does", () => {
    importSettings({ uiScale: 1000 });
    expect(useSettingsStore.getState().uiScale).toBe(125);
  });

  it.each(["restrictPrivateIps", "confirmMultilinePaste", "autoCheckForUpdates"] as const)(
    "can switch the safety setting %s on, but never off",
    (key) => {
      useSettingsStore.setState({ [key]: false });
      importSettings({ [key]: true });
      expect(useSettingsStore.getState()[key]).toBe(true);
      importSettings({ [key]: false });
      expect(useSettingsStore.getState()[key]).toBe(true);
    },
  );

  it("switches the UI language, but keeps it when the file follows the system", () => {
    importSettings({ language: "de" });
    expect(useSettingsStore.getState().language).toBe("de");
    expect(changeLanguage).toHaveBeenCalledWith("de");

    importSettings({ language: "" });
    expect(useSettingsStore.getState().language).toBe("de");

    importSettings({ language: "xx" });
    expect(useSettingsStore.getState().language).toBe("de");
  });

  it("ignores keys it does not know, including the command buttons", () => {
    importSettings({ commandButtons: [button(ID_A, "a")], somethingNew: 1 });
    expect(useSettingsStore.getState().commandButtons).toEqual([]);
    expect(useSettingsStore.getState()).not.toHaveProperty("somethingNew");
  });
});

describe("importCommandButtons", () => {
  it("adds buttons after the existing ones", () => {
    useSettingsStore.setState({ commandButtons: [button(ID_A, "a")] });
    const result = importCommandButtons([button(ID_B, "b"), button(ID_C, "c")]);
    expect(result).toEqual({ added: 2, skipped: 0 });
    expect(useSettingsStore.getState().commandButtons.map((b) => b.name)).toEqual(["a", "b", "c"]);
  });

  it("never changes an existing button with the same id", () => {
    useSettingsStore.setState({ commandButtons: [button(ID_A, "local", "ls\\r")] });
    const result = importCommandButtons([button(ID_A.toUpperCase(), "remote", "rm\\r")]);
    expect(result).toEqual({ added: 0, skipped: 1 });
    expect(useSettingsStore.getState().commandButtons).toEqual([button(ID_A, "local", "ls\\r")]);
  });

  it("matches an existing id written in uppercase too", () => {
    const upper = ID_A.toUpperCase();
    useSettingsStore.setState({ commandButtons: [button(upper, "local", "ls\\r")] });
    const result = importCommandButtons([button(ID_A, "remote", "rm\\r")]);
    expect(result).toEqual({ added: 0, skipped: 1 });
    expect(useSettingsStore.getState().commandButtons).toEqual([button(upper, "local", "ls\\r")]);
  });

  it("skips a button with the same name and command under another id", () => {
    useSettingsStore.setState({ commandButtons: [button(ID_A, "a", "uptime\\r")] });
    const result = importCommandButtons([
      button(ID_B, "a", "uptime\\r"),
      button(ID_C, "a", "uptime -p\\r"),
    ]);
    expect(result).toEqual({ added: 1, skipped: 1 });
    expect(useSettingsStore.getState().commandButtons.map((b) => b.id)).toEqual([ID_A, ID_C]);
  });

  it("skips duplicates within the file itself", () => {
    const result = importCommandButtons([button(ID_A, "a"), button(ID_A, "a")]);
    expect(result).toEqual({ added: 1, skipped: 1 });
  });

  it("skips malformed entries and stores only the known fields", () => {
    const result = importCommandButtons([
      null,
      "uptime",
      { ...button("not-a-uuid", "a") },
      { ...button(ID_B, "b"), color: "red" },
      { ...button(ID_C, "x".repeat(33)) },
      { ...button(ID_A, "a"), extra: "<script>" },
    ]);
    expect(result).toEqual({ added: 1, skipped: 5 });
    expect(useSettingsStore.getState().commandButtons).toEqual([button(ID_A, "a")]);
  });

  it.each([
    ["a control character", "uptime\r"],
    ["a zero-width space", "upti\u200bme"],
    ["a text-direction override", "\u202eemitpu"],
    ["a line separator", "up\u2028time"],
    ["a tag character", "uptime\u{E0041}"],
    ["a no-break space", "uptime\u00a0-p"],
    ["a blank braille pattern", "uptime\u2800"],
    ["a lone surrogate", "uptime\ud800"],
  ])("skips a button whose name or command holds %s", (_label, text) => {
    const result = importCommandButtons([button(ID_A, text), button(ID_B, "b", text)]);
    expect(result).toEqual({ added: 0, skipped: 2 });
  });

  it("keeps emoji sequences in a name, but not the joiner in a command", () => {
    const family = "\u{1F468}\u200d\u{1F469}";
    const result = importCommandButtons([button(ID_A, family), button(ID_B, "b", family)]);
    expect(result).toEqual({ added: 1, skipped: 1 });
    expect(useSettingsStore.getState().commandButtons[0]?.name).toBe(family);
  });

  it("keeps icon-font glyphs in a name, but not in a command", () => {
    const result = importCommandButtons([
      button(ID_A, "\ue0b0 deploy"),
      button(ID_B, "b", "uptime\ue0b0"),
      button(ID_C, "c", "uptime\ufe0f"),
    ]);
    expect(result).toEqual({ added: 1, skipped: 2 });
  });

  it("stops at the button limit", () => {
    const full = Array.from({ length: 1000 }, (_, i) =>
      button(`00000000-0000-4000-8000-${String(i).padStart(12, "0")}`, `b${String(i)}`),
    );
    useSettingsStore.setState({ commandButtons: full });
    const result = importCommandButtons([button(ID_A, "a")]);
    expect(result).toEqual({ added: 0, skipped: 1 });
    expect(useSettingsStore.getState().commandButtons).toHaveLength(1000);
  });

  it("fills a nearly full bar up to the limit and skips the rest", () => {
    const almostFull = Array.from({ length: 999 }, (_, i) =>
      button(`00000000-0000-4000-8000-${String(i).padStart(12, "0")}`, `b${String(i)}`),
    );
    useSettingsStore.setState({ commandButtons: almostFull });
    const result = importCommandButtons([button(ID_A, "a"), button(ID_B, "b"), button(ID_C, "c")]);
    expect(result).toEqual({ added: 1, skipped: 2 });
    const buttons = useSettingsStore.getState().commandButtons;
    expect(buttons).toHaveLength(1000);
    expect(buttons[999]?.id).toBe(ID_A);
  });

  it("round-trips an export into an empty bar", () => {
    useSettingsStore.setState({ commandButtons: [button(ID_A, "a"), button(ID_B, "b")] });
    const exported = getExportableCommandButtons();
    useSettingsStore.setState({ commandButtons: [] });
    importCommandButtons(exported);
    expect(getExportableCommandButtons()).toEqual(exported);
  });
});
