/**
 * The pace of the session search.
 *
 * A search reads every folder and every session the user can see, in
 * PostgreSQL mode from the shared database. The contract pinned here: the
 * field's text is searched for once it has stood for `SEARCH_PAUSE_MS`, and
 * not at each change; the window sends one search at a time, and the next
 * one with the text the field has by then; an answer is shown only for the
 * text the field still has; and the search after a refresh takes its turn
 * among the others, without the refresh waiting for it.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Folder, SearchResults } from "./sessionStore";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

type StoreModule = typeof import("./sessionStore");

/** A search the backend was asked for and has not answered. */
interface Asked {
  query: string;
  answer: (results: SearchResults) => void;
  fail: (reason: string) => void;
}

/** Every search the backend was asked for, in the order they were sent. */
let asked: Asked[] = [];
let store: StoreModule["useSessionStore"];
let pause: number;

function found(name: string): SearchResults {
  const folder: Folder = {
    id: `f-${name}`,
    name,
    parent_id: null,
    sort_order: 0,
    owner: "local",
    visibility: "personal",
  };
  return { folders: [folder], sessions: [] };
}

/** The queries of the searches that were sent. */
function queries(): string[] {
  return asked.map((search) => search.query);
}

/** Lets what waits on an answer go on, without any time passing. */
async function settle(): Promise<void> {
  await vi.advanceTimersByTimeAsync(0);
}

function type(query: string): void {
  store.getState().searchSessions(query);
}

/** Types `query` and waits until the pause is over. */
async function typeAndPause(query: string): Promise<void> {
  type(query);
  await vi.advanceTimersByTimeAsync(pause);
}

/** A refresh, which is done once the folders and sessions are read. */
async function refresh(): Promise<void> {
  await store.getState().loadAll();
}

beforeEach(async () => {
  vi.useFakeTimers();
  // The pace is kept in the module, so each test has a module of its own.
  vi.resetModules();
  asked = [];
  const { invoke } = await import("@tauri-apps/api/core");
  vi.mocked(invoke).mockImplementation((command: string, args?: unknown) => {
    if (command !== "session_search") return Promise.resolve([]);
    const { query } = args as { query: string };
    return new Promise<SearchResults>((resolve, reject) => {
      asked.push({
        query,
        answer: resolve,
        fail: (reason) => {
          reject(new Error(reason));
        },
      });
    });
  });
  const module = await import("./sessionStore");
  store = module.useSessionStore;
  pause = module.SEARCH_PAUSE_MS;
});

afterEach(() => {
  vi.clearAllTimers();
  vi.useRealTimers();
});

describe("the pace of the session search", () => {
  it("pauses for a quarter of a second", () => {
    // The other tests go by the constant, whatever it is.
    expect(pause).toBe(250);
  });

  it("searches once the text has stood for the pause, for the text it has then", async () => {
    type("r");
    await vi.advanceTimersByTimeAsync(pause - 1);
    type("ro");
    await vi.advanceTimersByTimeAsync(pause - 1);
    type("rou");
    await vi.advanceTimersByTimeAsync(pause - 1);
    // Three changes, each before the pause of the one before was over.
    expect(queries()).toEqual([]);
    expect(store.getState().searchQuery).toBe("rou");
    expect(store.getState().searchResults).toBeNull();

    await vi.advanceTimersByTimeAsync(1);
    expect(queries()).toEqual(["rou"]);

    asked[0].answer(found("router"));
    await settle();
    expect(store.getState().searchResults).toEqual(found("router"));
    // And nothing more is sent for it.
    await vi.advanceTimersByTimeAsync(10 * pause);
    expect(queries()).toEqual(["rou"]);
  });

  it("sends one search at a time, and the next one for the text the field has", async () => {
    await typeAndPause("ab");
    expect(queries()).toEqual(["ab"]);

    // The backend has not answered. Two more texts, each with its pause.
    await typeAndPause("abc");
    await typeAndPause("abcd");
    expect(queries()).toEqual(["ab"]);

    // The answer comes for a text the field no longer has: it is not shown,
    // and one search is sent for the two texts, with the later one.
    asked[0].answer(found("ab"));
    await settle();
    expect(store.getState().searchResults).toBeNull();
    expect(queries()).toEqual(["ab", "abcd"]);

    asked[1].answer(found("abcd"));
    await settle();
    expect(store.getState().searchResults).toEqual(found("abcd"));
    await vi.advanceTimersByTimeAsync(10 * pause);
    expect(queries()).toEqual(["ab", "abcd"]);
  });

  it("sends the text as it is when the turn comes, and not once more after its pause", async () => {
    await typeAndPause("ab");
    await typeAndPause("abc");
    // Typed on, and the answer comes before that pause is over.
    type("abcd");
    asked[0].answer(found("ab"));
    await settle();
    expect(queries()).toEqual(["ab", "abcd"]);

    asked[1].answer(found("abcd"));
    await vi.advanceTimersByTimeAsync(10 * pause);
    expect(queries()).toEqual(["ab", "abcd"]);
    expect(store.getState().searchResults).toEqual(found("abcd"));
  });

  it("leaves a text its pause where no search was asked for while one was under way", async () => {
    await typeAndPause("ab");
    // Typed while the search is under way, and the answer comes before the
    // pause is over: nothing has asked for a search yet.
    type("abc");
    asked[0].answer(found("ab"));
    await settle();
    expect(queries()).toEqual(["ab"]);
    await vi.advanceTimersByTimeAsync(pause);
    expect(queries()).toEqual(["ab", "abc"]);
  });

  it("shows the answer for a text that the field has again", async () => {
    await typeAndPause("ab");
    type("abc");
    type("ab");
    asked[0].answer(found("ab"));
    await settle();
    expect(store.getState().searchResults).toEqual(found("ab"));
    // The field's last change has a pause of its own, which brings one more
    // search for the same text. One could do without it; it finds the same.
    await vi.advanceTimersByTimeAsync(10 * pause);
    expect(queries()).toEqual(["ab", "ab"]);
  });

  it("takes a text with a space more for another text, and sends it as it is", async () => {
    await typeAndPause("ab");
    await typeAndPause(" ab ");
    asked[0].answer(found("ab"));
    await settle();
    // Not the answer for what the field has now.
    expect(store.getState().searchResults).toBeNull();
    expect(queries()).toEqual(["ab", " ab "]);
  });

  it("does not search for a text that is cleared within the pause", async () => {
    type("ab");
    await vi.advanceTimersByTimeAsync(pause - 1);
    store.getState().clearSearch();
    await vi.advanceTimersByTimeAsync(10 * pause);
    expect(queries()).toEqual([]);
  });

  it("does not search for a text that an empty one follows within the pause", async () => {
    type("ab");
    type("");
    await vi.advanceTimersByTimeAsync(10 * pause);
    expect(queries()).toEqual([]);
  });

  it("does not search for spaces alone, and takes the results away at once", async () => {
    store.setState({ searchResults: found("before") });
    type("   ");
    expect(store.getState().searchResults).toBeNull();
    await vi.advanceTimersByTimeAsync(10 * pause);
    expect(queries()).toEqual([]);
  });

  it("gives a text typed right after a clear a pause of its own", async () => {
    type("ab");
    await vi.advanceTimersByTimeAsync(pause - 1);
    store.getState().clearSearch();
    type("cd");
    await vi.advanceTimersByTimeAsync(pause - 1);
    expect(queries()).toEqual([]);
    await vi.advanceTimersByTimeAsync(1);
    expect(queries()).toEqual(["cd"]);
  });

  it("drops the answer of a search whose field was cleared meanwhile", async () => {
    await typeAndPause("ab");
    store.getState().clearSearch();
    asked[0].answer(found("ab"));
    await settle();
    expect(store.getState().searchQuery).toBe("");
    expect(store.getState().searchResults).toBeNull();
    // And the next text is searched for as any other.
    await typeAndPause("cd");
    expect(queries()).toEqual(["ab", "cd"]);
  });

  it("keeps the results at a search that fails, and goes on searching", async () => {
    await typeAndPause("ab");
    asked[0].answer(found("ab"));
    await settle();

    await typeAndPause("abc");
    asked[1].fail("Failed to search sessions");
    await settle();
    expect(store.getState().searchResults).toEqual(found("ab"));

    await typeAndPause("abcd");
    expect(queries()).toEqual(["ab", "abc", "abcd"]);
    asked[2].answer(found("abcd"));
    await settle();
    expect(store.getState().searchResults).toEqual(found("abcd"));
  });
});

describe("the search after a refresh", () => {
  it("is sent with the refresh, which does not wait for its answer", async () => {
    store.setState({ searchQuery: "lab", searchResults: found("before") });
    // Done, with the search sent and not answered.
    await refresh();
    expect(queries()).toEqual(["lab"]);
    expect(store.getState().searchResults).toEqual(found("before"));

    asked[0].answer(found("lab"));
    await settle();
    expect(store.getState().searchResults).toEqual(found("lab"));
  });

  it("is not sent without a text in the field", async () => {
    await refresh();
    store.setState({ searchQuery: "  " });
    await refresh();
    expect(queries()).toEqual([]);
  });

  it("waits for a search that is under way, and is one search for two refreshes", async () => {
    await typeAndPause("lab");
    expect(queries()).toEqual(["lab"]);

    // Neither refresh waits, and nothing more is sent while the first
    // search has no answer.
    await refresh();
    await refresh();
    expect(queries()).toEqual(["lab"]);

    // Its answer is from before the refresh. It is for the field's text and
    // is shown, and the search after the refresh follows it.
    asked[0].answer(found("stale"));
    await settle();
    expect(store.getState().searchResults).toEqual(found("stale"));
    expect(queries()).toEqual(["lab", "lab"]);

    asked[1].answer(found("lab"));
    await settle();
    expect(store.getState().searchResults).toEqual(found("lab"));
    await vi.advanceTimersByTimeAsync(10 * pause);
    expect(queries()).toEqual(["lab", "lab"]);
  });

  it("is one search with a text that was typed meanwhile, for that text", async () => {
    await typeAndPause("lab");
    // While the search is under way: a refresh, and the user types on.
    await refresh();
    await typeAndPause("labs");
    expect(queries()).toEqual(["lab"]);

    asked[0].answer(found("lab"));
    await settle();
    // One search for the two, with the text the field has now.
    expect(queries()).toEqual(["lab", "labs"]);
    asked[1].answer(found("labs"));
    await settle();
    expect(store.getState().searchResults).toEqual(found("labs"));
    await vi.advanceTimersByTimeAsync(10 * pause);
    expect(queries()).toEqual(["lab", "labs"]);
  });

  it("follows the one under way though a refresh asks again while it is sent", async () => {
    await typeAndPause("lab");
    await refresh();
    asked[0].answer(found("stale"));
    await settle();
    expect(queries()).toEqual(["lab", "lab"]);

    // A refresh while the second search is under way gets one of its own
    // after it: the second was sent before this refresh read anything.
    await refresh();
    expect(queries()).toEqual(["lab", "lab"]);
    asked[1].answer(found("lab"));
    await settle();
    expect(queries()).toEqual(["lab", "lab", "lab"]);
    asked[2].answer(found("lab again"));
    await settle();
    expect(store.getState().searchResults).toEqual(found("lab again"));
    await vi.advanceTimersByTimeAsync(10 * pause);
    expect(queries()).toEqual(["lab", "lab", "lab"]);
  });

  it("does not show its answer for a text the field no longer has", async () => {
    store.setState({ searchQuery: "lab", searchResults: found("before") });
    await refresh();
    type("labs");
    asked[0].answer(found("lab"));
    await settle();
    expect(store.getState().searchResults).toEqual(found("before"));
    // The text's own search follows after its pause.
    await vi.advanceTimersByTimeAsync(pause);
    expect(queries()).toEqual(["lab", "labs"]);
  });

  it("takes the place of the search a pause was running for", async () => {
    type("lab");
    await vi.advanceTimersByTimeAsync(pause - 1);
    await refresh();
    expect(queries()).toEqual(["lab"]);

    asked[0].answer(found("lab"));
    await vi.advanceTimersByTimeAsync(10 * pause);
    expect(queries()).toEqual(["lab"]);
  });

  it("takes the results away when it fails, where a search of the field keeps them", async () => {
    await typeAndPause("lab");
    asked[0].answer(found("lab"));
    await settle();

    await refresh();
    asked[1].fail("Failed to search folders");
    await settle();
    // The tree is shown, with the text still in the field.
    expect(store.getState().searchResults).toBeNull();
    expect(store.getState().searchQuery).toBe("lab");

    // The failure was the refresh's own: the next search of the field that
    // fails leaves what is shown.
    await typeAndPause("labs");
    asked[2].answer(found("labs"));
    await settle();
    await typeAndPause("labs2");
    asked[3].fail("Failed to search sessions");
    await settle();
    expect(store.getState().searchResults).toEqual(found("labs"));
  });

  it("keeps the results when the search under way fails, and not when its own does", async () => {
    await typeAndPause("lab");
    asked[0].answer(found("lab"));
    await settle();

    // A search of the field is under way when the refresh asks for its own.
    await typeAndPause("labs");
    await refresh();
    expect(queries()).toEqual(["lab", "labs"]);

    // That one fails: it was the field's, and what is shown stays.
    asked[1].fail("Failed to search sessions");
    await settle();
    expect(store.getState().searchResults).toEqual(found("lab"));
    expect(queries()).toEqual(["lab", "labs", "labs"]);

    // The refresh's own fails: what is shown may no longer be there.
    asked[2].fail("Failed to search sessions");
    await settle();
    expect(store.getState().searchResults).toBeNull();
  });

  it("takes the results away when the search it shares with the field's text fails", async () => {
    await typeAndPause("lab");
    asked[0].answer(found("lab"));
    await settle();

    // A search under way, then a refresh and a text typed: one search
    // follows for both, and it is the one after the refresh as well.
    await typeAndPause("labs");
    await refresh();
    await typeAndPause("labs2");
    asked[1].answer(found("labs"));
    await settle();
    expect(queries()).toEqual(["lab", "labs", "labs2"]);
    asked[2].fail("Failed to search sessions");
    await settle();
    expect(store.getState().searchResults).toBeNull();
  });

  it("leaves a later failure alone when the field was cleared before its turn came", async () => {
    await typeAndPause("lab");
    // A refresh asks for its search, and the field is cleared before the
    // search under way has its answer: there is nothing to search for.
    await refresh();
    store.getState().clearSearch();
    asked[0].answer(found("lab"));
    await settle();
    expect(queries()).toEqual(["lab"]);

    // The next search of the field is the field's own: where it fails, what
    // is shown stays.
    store.setState({ searchResults: found("shown") });
    await typeAndPause("ab");
    asked[1].fail("Failed to search sessions");
    await settle();
    expect(store.getState().searchResults).toEqual(found("shown"));
  });

  it("leaves a later failure alone when it had no text to search for", async () => {
    // A refresh with the field empty, and one with spaces alone in it.
    await refresh();
    store.setState({ searchQuery: "  " });
    await refresh();

    store.setState({ searchQuery: "lab", searchResults: found("lab") });
    await typeAndPause("labs");
    asked[0].fail("Failed to search sessions");
    await settle();
    expect(store.getState().searchResults).toEqual(found("lab"));
  });
});
