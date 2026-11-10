/**
 * The page token: one per page load, announced to the backend and passed
 * with every connect, since the backend opens a session only for the page
 * announced last.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import type { ConnectCommand } from "./page";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve()) }));

/** Load the module anew, as a page load does. */
async function loadPage(): Promise<typeof import("./page")> {
  vi.resetModules();
  return import("./page");
}

function calls(): [string, Record<string, unknown> | undefined][] {
  return vi.mocked(invoke).mock.calls as [string, Record<string, unknown> | undefined][];
}

function announcedTokens(): unknown[] {
  return calls()
    .filter(([cmd]) => cmd === "page_announce")
    .map(([, args]) => args?.page);
}

beforeEach(() => {
  vi.mocked(invoke).mockImplementation(() => Promise.resolve(undefined as never));
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("invokeConnect", () => {
  const COMMANDS: ConnectCommand[] = [
    "pty_spawn",
    "ssh_connect",
    "telnet_connect",
    "session_connect",
  ];

  it.each(COMMANDS)("passes the announced token with %s", async (command) => {
    const { invokeConnect } = await loadPage();
    vi.mocked(invoke).mockImplementation((cmd: string) =>
      Promise.resolve((cmd === command ? "conn-1" : undefined) as never),
    );

    await expect(invokeConnect(command, { cols: 80, rows: 24 })).resolves.toBe("conn-1");

    const [token] = announcedTokens();
    expect(calls()).toEqual([
      ["page_announce", { page: token }],
      [command, { cols: 80, rows: 24, page: token }],
    ]);
  });

  it("sends the connect only once the page is announced", async () => {
    const { invokeConnect } = await loadPage();
    let announced: (() => void) | undefined;
    vi.mocked(invoke).mockImplementationOnce(
      () =>
        new Promise<never>((resolve) => {
          announced = () => {
            resolve(undefined as never);
          };
        }),
    );

    const pending = invokeConnect("pty_spawn", {});
    await new Promise<void>((resolve) => {
      setTimeout(resolve, 0);
    });
    expect(calls().map(([cmd]) => cmd)).toEqual(["page_announce"]);

    announced?.();
    await pending;
    expect(calls().map(([cmd]) => cmd)).toEqual(["page_announce", "pty_spawn"]);
  });

  it("does not let the arguments replace the token", async () => {
    const { invokeConnect } = await loadPage();
    await invokeConnect("pty_spawn", { page: "another-page" });

    const [token] = announcedTokens();
    expect(calls()[1]).toEqual(["pty_spawn", { page: token }]);
  });

  it("does not connect when the announcement fails, and announces again next time", async () => {
    const { invokeConnect } = await loadPage();
    vi.mocked(invoke).mockImplementationOnce(() => Promise.reject(new Error("refused")));
    await expect(invokeConnect("pty_spawn", {})).rejects.toThrow("refused");
    expect(calls().map(([cmd]) => cmd)).toEqual(["page_announce"]);

    await invokeConnect("pty_spawn", {});
    expect(calls().map(([cmd]) => cmd)).toEqual(["page_announce", "page_announce", "pty_spawn"]);
  });
});

describe("page token", () => {
  it("is one the backend accepts: letters, digits and hyphens, at most 64", async () => {
    const { announcePage } = await loadPage();
    await announcePage();
    expect(announcedTokens()).toEqual([expect.stringMatching(/^[0-9A-Za-z-]{1,64}$/)]);
  });

  it("stays the same for the page", async () => {
    const { announcePage, invokeConnect } = await loadPage();
    await announcePage();
    await invokeConnect("pty_spawn", {});
    await invokeConnect("ssh_connect", {});

    const [first, ...rest] = announcedTokens();
    expect(rest).toEqual([first, first]);
  });

  it("differs after a page load", async () => {
    await (await loadPage()).announcePage();
    await (await loadPage()).announcePage();

    const [before, after] = announcedTokens();
    expect(after).not.toBe(before);
  });

  it("does not need crypto.randomUUID", async () => {
    // Older WebKit lacks it, and the token is made as the module loads,
    // where a missing function would leave the window blank.
    vi.spyOn(crypto, "randomUUID").mockImplementation(() => {
      throw new Error("not available");
    });
    const { announcePage } = await loadPage();
    await announcePage();
    expect(announcedTokens()).toHaveLength(1);
  });
});

describe("announcePageAtStartup", () => {
  it("announces the page", async () => {
    const { announcePageAtStartup } = await loadPage();
    announcePageAtStartup();
    await vi.waitFor(() => {
      expect(announcedTokens()).toHaveLength(1);
    });
  });

  it("tries once more after a failure, then leaves it to the next connect", async () => {
    vi.useFakeTimers();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const { announcePageAtStartup } = await loadPage();
    vi.mocked(invoke).mockImplementation(() => Promise.reject(new Error("refused")));

    announcePageAtStartup();
    await vi.advanceTimersByTimeAsync(999);
    expect(announcedTokens()).toHaveLength(1);
    expect(warn).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(1);
    expect(announcedTokens()).toHaveLength(2);
    expect(warn).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(10_000);
    expect(announcedTokens()).toHaveLength(2);
  });

  it("does not warn when the retry goes through", async () => {
    vi.useFakeTimers();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const { announcePageAtStartup } = await loadPage();
    vi.mocked(invoke).mockImplementationOnce(() => Promise.reject(new Error("refused")));

    announcePageAtStartup();
    await vi.advanceTimersByTimeAsync(1000);

    expect(announcedTokens()).toHaveLength(2);
    expect(warn).not.toHaveBeenCalled();
  });
});
