/**
 * HostVerifyDialog tests.
 *
 * The contract that matters: a first connection's key can be accepted with
 * Enter, but a changed key (or one that could not be checked) only with a
 * click on Accept, so that a keystroke meant for something else cannot trust
 * it. Escape always rejects, and a warning shows the key on record beside
 * the one offered.
 */
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { HostVerifyDialog, type HostVerifyRequest } from "./HostVerifyDialog";

vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
  // The key, then the values it would be given.
  Trans: ({ i18nKey, values }: { i18nKey: string; values: object }) =>
    `${i18nKey} ${JSON.stringify(values)}`,
}));

function request(overrides: Partial<HostVerifyRequest> = {}): HostVerifyRequest {
  return {
    sessionId: "s1",
    host: "example.com",
    port: 22,
    fingerprint: "SHA256:offered",
    keyType: "ssh-ed25519",
    status: "new",
    knownKeys: [],
    ...overrides,
  };
}

const WARNINGS = [
  ["changed", [{ keyType: "ssh-ed25519", fingerprint: "SHA256:recorded" }]],
  ["otherType", [{ keyType: "ecdsa-sha2-nistp256", fingerprint: "SHA256:recorded" }]],
  ["unchecked", []],
] as const;

describe("HostVerifyDialog", () => {
  it("accepts a first connection's key with Enter", () => {
    const onRespond = vi.fn();
    render(<HostVerifyDialog request={request()} onRespond={onRespond} />);
    fireEvent.keyDown(document.body, { key: "Enter" });
    expect(onRespond).toHaveBeenCalledWith("s1", true);
  });

  it.each(WARNINGS)("accepts a %s key only with a click", (status, knownKeys) => {
    const onRespond = vi.fn();
    render(
      <HostVerifyDialog
        request={request({ status, knownKeys: [...knownKeys] })}
        onRespond={onRespond}
      />,
    );
    fireEvent.keyDown(document.body, { key: "Enter" });
    expect(onRespond).not.toHaveBeenCalled();
    fireEvent.click(screen.getByText("hostVerify.acceptAnyway"));
    expect(onRespond).toHaveBeenCalledWith("s1", true);
  });

  it.each([["new", []], ...WARNINGS] as const)("rejects a %s key with Escape", (status, keys) => {
    const onRespond = vi.fn();
    render(
      <HostVerifyDialog
        request={request({ status, knownKeys: [...keys] })}
        onRespond={onRespond}
      />,
    );
    fireEvent.keyDown(document.body, { key: "Escape" });
    expect(onRespond).toHaveBeenCalledWith("s1", false);
  });

  it("gives Enter to a focused button in the dialog", () => {
    const onRespond = vi.fn();
    render(<HostVerifyDialog request={request()} onRespond={onRespond} />);
    const reject = screen.getByText("hostVerify.reject");
    reject.focus();
    fireEvent.keyDown(reject, { key: "Enter" });
    expect(onRespond).toHaveBeenCalledTimes(1);
    expect(onRespond).toHaveBeenCalledWith("s1", false);
  });

  it.each([
    ["changed", false],
    ["new", true],
  ] as const)("does not click a focused button outside a %s prompt", (status, acceptsOnEnter) => {
    const onRespond = vi.fn();
    const outside = vi.fn();
    render(
      <>
        <button type="button" onClick={outside}>
          elsewhere
        </button>
        <HostVerifyDialog request={request({ status })} onRespond={onRespond} />
      </>,
    );
    const button = screen.getByText("elsewhere");
    button.focus();
    fireEvent.keyDown(button, { key: "Enter" });
    expect(outside).not.toHaveBeenCalled();
    expect(onRespond.mock.calls).toEqual(acceptsOnEnter ? [["s1", true]] : []);
  });

  it("does not carry a focused Accept over to the next request", () => {
    const onRespond = vi.fn();
    const { rerender } = render(<HostVerifyDialog request={request()} onRespond={onRespond} />);
    screen.getByText("hostVerify.accept").focus();
    // The next queued request replaces the answered one in the same place.
    rerender(
      <HostVerifyDialog
        request={request({ sessionId: "s2", status: "changed" })}
        onRespond={onRespond}
      />,
    );
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Enter" });
    expect(onRespond).not.toHaveBeenCalled();
  });

  it("is a labelled, described dialog", () => {
    render(<HostVerifyDialog request={request()} onRespond={vi.fn()} />);
    const dialog = screen.getByRole("dialog");
    expect(dialog).toHaveAttribute("aria-modal", "true");
    expect(dialog).toHaveAccessibleName("hostVerify.title");
    expect(dialog.getAttribute("aria-describedby")).toBe("hv-text");
  });

  it("shows a first connection's key without a warning", () => {
    render(<HostVerifyDialog request={request()} onRespond={vi.fn()} />);
    expect(screen.getByText("hostVerify.title")).not.toHaveClass("dialog-warning");
    expect(screen.getByText("hostVerify.accept")).toHaveClass("dialog-btn-primary");
    expect(screen.queryByText("hostVerify.recordedKey")).toBeNull();
    expect(screen.getByText("SHA256:offered")).not.toHaveClass("dialog-fingerprint-suspect");
  });

  it.each(WARNINGS)("shows a %s key as a warning", (status, knownKeys) => {
    render(
      <HostVerifyDialog
        request={request({ status, knownKeys: [...knownKeys] })}
        onRespond={vi.fn()}
      />,
    );
    const titles = {
      changed: "hostVerify.titleChanged",
      otherType: "hostVerify.titleOtherType",
      unchecked: "hostVerify.titleUnchecked",
    };
    expect(screen.getByText(titles[status])).toHaveClass("dialog-warning");
    expect(screen.getByText("hostVerify.acceptAnyway")).toHaveClass("dialog-btn-danger");
    expect(screen.getByText("SHA256:offered")).toHaveClass("dialog-fingerprint-suspect");
    expect(screen.queryAllByText("hostVerify.recordedKey")).toHaveLength(knownKeys.length);
    for (const key of knownKeys) {
      expect(screen.getByText(key.fingerprint)).toHaveClass("dialog-fingerprint-recorded");
    }
  });

  it("names the key types on record in the text", () => {
    render(
      <HostVerifyDialog
        request={request({
          status: "otherType",
          knownKeys: [
            { keyType: "ecdsa-sha2-nistp256", fingerprint: "SHA256:one" },
            { keyType: "ssh-rsa", fingerprint: "SHA256:two" },
            { keyType: "ssh-rsa", fingerprint: "SHA256:three" },
          ],
        })}
        onRespond={vi.fn()}
      />,
    );
    expect(screen.getByRole("dialog")).toHaveTextContent(
      '"knownTypes":"ecdsa-sha2-nistp256, ssh-rsa"',
    );
  });
});
