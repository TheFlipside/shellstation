/**
 * UpdateDialog: installing an update ends the app, on Windows from inside
 * the updater plugin, so the dialog has the backend close every session
 * first. It does so only once the update is downloaded, so that a download
 * that fails costs no session, and no failure of the closing may keep the
 * update from being installed or leave an app that cannot connect again.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { UpdateDialog } from "./UpdateDialog";
import { useSettingsStore } from "../stores/settingsStore";

vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));
vi.mock("../i18n", () => ({
  default: { t: (key: string) => key, changeLanguage: vi.fn() },
}));
/**
 * Handlers registered with `listen`, with their events. The dialog registers
 * its handler anew when it renders, and the old one is removed a moment
 * after, so they are kept one by one.
 */
const listeners = new Set<{ event: string; handler: () => void }>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: (event: string, handler: () => void) => {
    const listener = { event, handler };
    listeners.add(listener);
    return Promise.resolve(() => {
      listeners.delete(listener);
    });
  },
}));

/** What the dialog did, in order. */
let steps: string[] = [];

const closeSessionsForUpdate = vi.fn<() => Promise<void>>();
const updateInstallFailed = vi.fn<() => Promise<void>>();
vi.mock("../page", () => ({
  closeSessionsForUpdate: () => closeSessionsForUpdate(),
  updateInstallFailed: () => updateInstallFailed(),
}));

const relaunch = vi.fn<() => Promise<void>>();
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: () => relaunch() }));

const update = {
  currentVersion: "1.0.0",
  version: "1.1.0",
  body: "",
  download: vi.fn<() => Promise<void>>(),
  install: vi.fn<() => Promise<void>>(),
  downloadAndInstall: vi.fn<() => Promise<void>>(),
  close: vi.fn<() => Promise<void>>(),
};
vi.mock("@tauri-apps/plugin-updater", () => ({ check: () => Promise.resolve(update) }));

/** A step that records itself and then resolves, or fails with `error`. */
function step(name: string, error?: string): () => Promise<void> {
  return () => {
    steps.push(name);
    return error === undefined ? Promise.resolve() : Promise.reject(new Error(error));
  };
}

/** A promise settled from outside, for a step that is still under way. */
function deferred(): { promise: Promise<void>; resolve: () => void } {
  let resolve: () => void = () => undefined;
  const promise = new Promise<void>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

/** Let pending promise callbacks run. */
async function settle(): Promise<void> {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

/** Render the dialog, let its check find the update, and click install. */
async function startInstall(): Promise<void> {
  render(<UpdateDialog />);
  fireEvent.click(await screen.findByRole("button", { name: "updates.installAndRestart" }));
  await settle();
}

function pressEscape(): void {
  fireEvent.keyDown(document.body, { key: "Escape" });
}

/** Pick "Check for Updates…" in the Help menu. */
async function checkFromMenu(): Promise<void> {
  await settle();
  const handlers = [...listeners].filter(({ event }) => event === "updater:check-requested");
  expect(handlers).toHaveLength(1);
  act(() => {
    handlers[0]?.handler();
  });
  await settle();
}

/** The errors the mocks fail with, none of which the dialog may show. */
const RAW_ERRORS = /offline|access denied|not allowed|refused|gone/;

function expectGenericError(): void {
  expect(screen.getByText("updates.errorTitle")).toBeInTheDocument();
  expect(screen.getByText("updates.errorUnknown")).toBeInTheDocument();
  expect(screen.queryByText(RAW_ERRORS)).not.toBeInTheDocument();
}

beforeEach(() => {
  steps = [];
  listeners.clear();
  useSettingsStore.setState({ autoCheckForUpdates: true });
  update.download.mockReset().mockImplementation(step("download"));
  update.install.mockReset().mockImplementation(step("install"));
  update.downloadAndInstall.mockReset().mockImplementation(step("downloadAndInstall"));
  update.close.mockReset().mockImplementation(step("close"));
  closeSessionsForUpdate.mockReset().mockImplementation(step("closeSessions"));
  updateInstallFailed.mockReset().mockImplementation(step("installFailed"));
  relaunch.mockReset().mockImplementation(step("relaunch"));
  vi.spyOn(console, "warn").mockImplementation(() => undefined);
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("UpdateDialog install", () => {
  it("closes the sessions after the download and before the install", async () => {
    await startInstall();

    // Never the plugin's one call for both, which leaves no moment between.
    expect(steps).toEqual(["download", "closeSessions", "install", "relaunch"]);
    expect(screen.getByText("updates.readyTitle")).toBeInTheDocument();
  });

  it("does not install before the sessions are closed", async () => {
    const closing = deferred();
    closeSessionsForUpdate.mockImplementation(() => {
      steps.push("closeSessions");
      return closing.promise;
    });
    await startInstall();
    expect(steps).toEqual(["download", "closeSessions"]);

    closing.resolve();
    await settle();
    expect(steps).toEqual(["download", "closeSessions", "install", "relaunch"]);
  });

  it("closes no session for a download that fails", async () => {
    update.download.mockImplementation(step("download", "offline"));
    await startInstall();

    expect(steps).toEqual(["download", "close"]);
    expectGenericError();
  });

  it("installs all the same when the sessions cannot be closed", async () => {
    closeSessionsForUpdate.mockImplementation(step("closeSessions", "not allowed"));
    await startInstall();

    expect(steps).toEqual(["download", "closeSessions", "install", "relaunch"]);
    expect(screen.queryByText("updates.errorTitle")).not.toBeInTheDocument();
  });

  it("lets the app connect again when the install fails", async () => {
    update.install.mockImplementation(step("install", "access denied"));
    await startInstall();

    expect(steps).toEqual(["download", "closeSessions", "install", "installFailed", "close"]);
    expectGenericError();
  });

  it("lets the app connect again when the closing failed and then the install", async () => {
    // The backend may have closed the sessions, and kept the page from
    // opening new ones, before the call failed.
    closeSessionsForUpdate.mockImplementation(step("closeSessions", "not allowed"));
    update.install.mockImplementation(step("install", "access denied"));
    await startInstall();

    expect(steps).toEqual(["download", "closeSessions", "install", "installFailed", "close"]);
    expectGenericError();
  });

  it("says that the update failed only once the app can connect again", async () => {
    update.install.mockImplementation(step("install", "access denied"));
    const told = deferred();
    updateInstallFailed.mockImplementation(() => told.promise);
    await startInstall();
    expect(screen.queryByText("updates.errorTitle")).not.toBeInTheDocument();

    told.resolve();
    await settle();
    expectGenericError();
  });

  it("lets the app connect again when the restart fails", async () => {
    relaunch.mockImplementation(step("relaunch", "not allowed"));
    await startInstall();

    expect(steps).toEqual([
      "download",
      "closeSessions",
      "install",
      "relaunch",
      "installFailed",
      "close",
    ]);
    expectGenericError();
  });

  it("shows the failure even when the backend cannot be told of it", async () => {
    update.install.mockImplementation(step("install", "access denied"));
    updateInstallFailed.mockImplementation(step("installFailed", "refused"));
    update.close.mockImplementation(step("close", "gone"));
    await startInstall();

    expect(steps).toEqual(["download", "closeSessions", "install", "installFailed", "close"]);
    expectGenericError();
  });

  it("starts one install for two clicks", async () => {
    render(<UpdateDialog />);
    const button = await screen.findByRole("button", { name: "updates.installAndRestart" });
    fireEvent.click(button);
    fireEvent.click(button);
    await settle();

    expect(steps).toEqual(["download", "closeSessions", "install", "relaunch"]);
  });
});

describe("UpdateDialog check from the menu", () => {
  it("is ignored while an update installs", async () => {
    // It would take the dialog away, with the sessions closing behind it,
    // and then offer the update for a second install beside the first.
    const closing = deferred();
    closeSessionsForUpdate.mockImplementation(() => {
      steps.push("closeSessions");
      return closing.promise;
    });
    await startInstall();

    await checkFromMenu();
    expect(screen.getByText("updates.readyTitle")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "updates.installAndRestart" }),
    ).not.toBeInTheDocument();

    closing.resolve();
    await settle();
    expect(steps).toEqual(["download", "closeSessions", "install", "relaunch"]);
  });

  it("offers the update again after an install that failed", async () => {
    update.install.mockImplementation(step("install", "access denied"));
    await startInstall();
    expectGenericError();

    await checkFromMenu();
    expect(screen.getByRole("button", { name: "updates.installAndRestart" })).toBeInTheDocument();
  });
});

describe("UpdateDialog Escape", () => {
  it("dismisses an update that is offered", async () => {
    render(<UpdateDialog />);
    await screen.findByRole("button", { name: "updates.installAndRestart" });

    pressEscape();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("does not dismiss the dialog while the update downloads", async () => {
    const downloading = deferred();
    update.download.mockImplementation(() => downloading.promise);
    await startInstall();

    pressEscape();
    expect(screen.getByText("updates.downloadingTitle")).toBeInTheDocument();
    downloading.resolve();
    await settle();
  });

  it("does not dismiss the dialog while the sessions close and the update installs", async () => {
    // Without the dialog the user would be left with an app that refuses
    // every connect and restarts a moment later.
    const closing = deferred();
    closeSessionsForUpdate.mockImplementation(() => closing.promise);
    await startInstall();
    expect(screen.getByText("updates.readyTitle")).toBeInTheDocument();

    pressEscape();
    expect(screen.getByText("updates.readyTitle")).toBeInTheDocument();
    closing.resolve();
    await settle();
    expect(steps).toEqual(["download", "install", "relaunch"]);
    expect(screen.getByText("updates.readyTitle")).toBeInTheDocument();
  });

  it("dismisses the dialog after a failure", async () => {
    update.download.mockImplementation(step("download", "offline"));
    await startInstall();
    expect(screen.getByText("updates.errorTitle")).toBeInTheDocument();

    pressEscape();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });
});
