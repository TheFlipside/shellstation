/**
 * The notice shown when an older version had the private-address check on
 * and this one could not switch it on in the backend. It must say so in
 * plain words, stay until dismissed, and go once the check is on, when it
 * would no longer be true.
 */
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import i18n from "./i18n";
import { showPrivateIpCheckOffNotice } from "./privateIpCheckNotice";
import { useSettingsStore } from "./stores/settingsStore";
import { useToastStore } from "./stores/toastStore";

// The settings store imports invoke; nothing here may reach a backend.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

beforeAll(async () => {
  await i18n.changeLanguage("en");
});

beforeEach(() => {
  useToastStore.setState(useToastStore.getInitialState(), true);
  useSettingsStore.setState({ restrictPrivateIps: false });
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("showPrivateIpCheckOffNotice", () => {
  it("says the check is off and names the setting, in a toast that stays", () => {
    showPrivateIpCheckOffNotice();
    const toasts = useToastStore.getState().toasts;
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toMatchObject({ level: "error", sticky: true });
    // The real English text, so the placeholders are checked as well.
    expect(toasts[0].message).toContain(
      "Connections to private and loopback addresses are not blocked.",
    );
    expect(toasts[0].message).toContain(
      '"Block connections to private/loopback addresses" under Settings → Security.',
    );
  });

  it("says nothing when the check is on already", () => {
    useSettingsStore.setState({ restrictPrivateIps: true });
    showPrivateIpCheckOffNotice();
    expect(useToastStore.getState().toasts).toEqual([]);
  });

  it("goes once the check is switched on, and not on other changes", () => {
    showPrivateIpCheckOffNotice();
    useSettingsStore.setState({ themeMode: "light" });
    expect(useToastStore.getState().toasts).toHaveLength(1);
    useSettingsStore.setState({ restrictPrivateIps: true });
    expect(useToastStore.getState().toasts).toEqual([]);
  });

  it("can be taken down, leaving other toasts alone", () => {
    const other = useToastStore.getState().addToast("something else", "info");
    const dismiss = showPrivateIpCheckOffNotice();
    dismiss();
    expect(useToastStore.getState().toasts.map((toast) => toast.id)).toEqual([other]);
  });

  it("stops watching once the user has dismissed it", () => {
    const removeToast = vi.spyOn(useToastStore.getState(), "removeToast");
    showPrivateIpCheckOffNotice();
    const [notice] = useToastStore.getState().toasts;
    useToastStore.getState().removeToast(notice.id);
    expect(removeToast).toHaveBeenCalledTimes(1);

    useSettingsStore.setState({ restrictPrivateIps: true });
    expect(removeToast).toHaveBeenCalledTimes(1);
  });
});
