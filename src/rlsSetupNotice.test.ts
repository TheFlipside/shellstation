/**
 * The notice shown in PostgreSQL mode when the row-level security setup is
 * not in the database as this version applies it, or could not be checked.
 * It must say so in the window, name what is affected, and stay until
 * dismissed.
 */
import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import i18n from "./i18n";
import { showRlsSetupNotice } from "./rlsSetupNotice";
import { useToastStore } from "./stores/toastStore";

// Nothing here may reach a backend.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

beforeAll(async () => {
  await i18n.changeLanguage("en");
});

beforeEach(() => {
  useToastStore.setState(useToastStore.getInitialState(), true);
});

describe("showRlsSetupNotice", () => {
  it("says nothing when the setup is in place, or there is none to check", () => {
    showRlsSetupNotice({ state: "inPlace" });
    showRlsSetupNotice(null);
    showRlsSetupNotice(undefined);
    expect(useToastStore.getState().toasts).toEqual([]);
  });

  it("names the tables and the functions, in a toast that stays", () => {
    showRlsSetupNotice({ state: "incomplete", tables: ["folders", "sessions"], functions: true });
    const toasts = useToastStore.getState().toasts;
    expect(toasts).toHaveLength(1);
    // Closed alone, so that it does not go with a pile of other notices.
    expect(toasts[0]).toMatchObject({ level: "error", sticky: true, closeAlone: true });
    // The real English text, so the placeholder is checked as well.
    expect(toasts[0].message).toContain(
      "Row-level security on this database is not set up the way this version of ShellStation sets it up.",
    );
    expect(toasts[0].message).toContain("Affected: folders, sessions, trigger functions.");
    expect(toasts[0].message).toContain("tell your database administrator");
  });

  it("names only what is affected", () => {
    showRlsSetupNotice({ state: "incomplete", tables: ["session_credentials"], functions: false });
    expect(useToastStore.getState().toasts[0].message).toContain("Affected: session_credentials.");

    useToastStore.setState(useToastStore.getInitialState(), true);
    showRlsSetupNotice({ state: "incomplete", tables: [], functions: true });
    expect(useToastStore.getState().toasts[0].message).toContain("Affected: trigger functions.");
  });

  it("says when the setup could not be checked, as a warning that stays", () => {
    showRlsSetupNotice({ state: "unchecked" });
    const toasts = useToastStore.getState().toasts;
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toMatchObject({ level: "warning", sticky: true, closeAlone: true });
    expect(toasts[0].message).toContain(
      "ShellStation could not check whether row-level security is set up on this database.",
    );
  });

  it("shows the same notice once", () => {
    const status = { state: "incomplete", tables: ["folders"], functions: false } as const;
    showRlsSetupNotice({ ...status, tables: [...status.tables] });
    showRlsSetupNotice({ ...status, tables: [...status.tables] });
    expect(useToastStore.getState().toasts).toHaveLength(1);
  });

  it("has the notice in every language, with its placeholder", () => {
    const languages = Object.keys(i18n.options.resources ?? {});
    expect(languages.length).toBeGreaterThan(1);
    // The language's own text, which `t` would replace with the English one
    // where it is missing.
    const own = (lng: string, key: string): unknown => i18n.getResource(lng, "translation", key);
    for (const lng of languages) {
      expect(own(lng, "settings.rlsSetupIncomplete"), lng).toEqual(
        expect.stringContaining("{{parts}}"),
      );
      for (const key of ["settings.rlsSetupUnchecked", "settings.rlsSetupFunctions"]) {
        expect(own(lng, key), `${lng} ${key}`).toEqual(expect.stringMatching(/\S/));
      }
      const shown = i18n.t("settings.rlsSetupIncomplete", { lng, parts: "folders" });
      expect(shown, lng).toContain("folders");
      expect(shown, lng).not.toContain("{{");
    }
  });
});
