/**
 * pickFile tests.
 *
 * Every file import in Settings (SecureCRT, mRemoteNG, database JSON,
 * highlight profiles) goes through this helper, so a pick it drops is an
 * import that does nothing at all — no IPC call, no toast, no log line. The
 * contract pinned here: a pick settles only on the input's own `change` or
 * `cancel`, never on window focus, and no hidden input outlives its pick by
 * more than the next one.
 */
import { afterEach, describe, expect, it } from "vitest";
import { pickFile } from "./pickFile";

// A test that fails mid-pick would otherwise leave its hidden input behind for
// the next test's `pickerInputs()` to count.
afterEach(() => {
  for (const input of pickerInputs()) input.remove();
});

function pickerInputs(): NodeListOf<HTMLInputElement> {
  return document.querySelectorAll<HTMLInputElement>('input[type="file"]');
}

/** What the native picker does on confirm: populate `files`, then fire `change`. */
function choose(input: HTMLInputElement, file: File): void {
  // jsdom's `files` getter cannot be populated from script.
  Object.defineProperty(input, "files", { value: [file] });
  input.dispatchEvent(new Event("change"));
}

describe("pickFile", () => {
  it("keeps the chosen file when window focus returns before change", async () => {
    const pick = pickFile(".xml");
    const [input] = pickerInputs();
    const file = new File(["<VanDyke/>"], "export.xml");

    // WebView2's order: the window regains focus as the native dialog closes,
    // and the file reaches the page several frames later.
    window.dispatchEvent(new Event("focus"));
    await new Promise((resolve) => setTimeout(resolve, 100));
    choose(input, file);

    await expect(pick).resolves.toBe(file);
    expect(pickerInputs()).toHaveLength(0);
  });

  it("resolves null and removes the input when the picker is cancelled", async () => {
    const pick = pickFile(".json");

    pickerInputs()[0].dispatchEvent(new Event("cancel"));

    await expect(pick).resolves.toBeNull();
    expect(pickerInputs()).toHaveLength(0);
  });

  it("settles a pick whose cancel never arrived when the next pick starts", async () => {
    // A webview without the `cancel` event gives no signal at all on cancel.
    const abandoned = pickFile(".ini");
    const [abandonedInput] = pickerInputs();
    const next = pickFile(".xml");

    await expect(abandoned).resolves.toBeNull();
    const inputs = pickerInputs();
    expect(inputs).toHaveLength(1);
    expect(inputs[0].accept).toBe(".xml");

    // A straggling event from the superseded picker must not settle the new one.
    abandonedInput.dispatchEvent(new Event("cancel"));

    const file = new File(["<VanDyke/>"], "export.xml");
    choose(inputs[0], file);
    await expect(next).resolves.toBe(file);
  });
});
