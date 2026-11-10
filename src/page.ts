import { invoke } from "@tauri-apps/api/core";

/**
 * Identifies this page to the backend, which keeps the sessions.
 *
 * A reload starts the frontend over without its tabs, so a session of the
 * page before could never be shown or closed again. The backend therefore
 * closes every session when a new page announces itself, and opens one only
 * for the page announced last. Made up anew on each load, the token travels
 * with every connect, so a connect this page sent in the instant of a reload
 * is refused whenever it arrives.
 *
 * It is not a secret, only a name no other page load has. Built from
 * `getRandomValues` rather than `randomUUID`, which older WebKit lacks: this
 * runs as the module loads, where a missing function would leave the window
 * blank.
 */
const token = Array.from(crypto.getRandomValues(new Uint8Array(16)), (byte) =>
  byte.toString(16).padStart(2, "0"),
).join("");

/** The commands that open a session, each of which takes the page's token. */
export type ConnectCommand = "pty_spawn" | "ssh_connect" | "telnet_connect" | "session_connect";

/**
 * Tell the backend that this page is the one showing now. It closes whatever
 * an earlier page left behind; announcing again changes nothing.
 */
export function announcePage(): Promise<void> {
  return invoke("page_announce", { page: token });
}

/**
 * Open a session: the only way to call a connect command, so that none goes
 * out without the page's token. Resolves to the new session's id.
 *
 * Announces the page first each time: a connect is refused unless its page
 * is the announced one, and the announcement at startup may have failed or
 * been overtaken by the backend's own cleanup after the page load.
 */
export async function invokeConnect(
  command: ConnectCommand,
  args: Record<string, unknown>,
): Promise<string> {
  await announcePage();
  return invoke<string>(command, { ...args, page: token });
}

/** How long after a failed announcement at startup the one retry follows. */
const STARTUP_RETRY_MS = 1000;

/**
 * Announce the page as it starts, so that sessions an earlier page left
 * behind are closed now rather than at this page's first connect, which may
 * never come. Tried once more if it fails.
 */
export function announcePageAtStartup(): void {
  announcePage()
    .catch(async () => {
      await new Promise<void>((resolve) => {
        setTimeout(resolve, STARTUP_RETRY_MS);
      });
      await announcePage();
    })
    .catch((e: unknown) => {
      // eslint-disable-next-line no-console -- diagnostic detail goes to devtools, not the UI
      console.warn("page_announce failed:", e);
    });
}
