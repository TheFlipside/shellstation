/**
 * HostVerifyDialog with real translations.
 *
 * The host name in the prompt's text comes from a session's settings, which
 * an import file or another user of a shared session can set, and the app
 * turns i18next's escaping off. Trans puts the values into the translation
 * before it reads the markup, so this checks that a host name renders as
 * text and cannot add or close elements in a security prompt.
 */
import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";
import i18next from "i18next";
import { I18nextProvider, initReactI18next } from "react-i18next";
import en from "../locales/en/translation.json";
import { HostVerifyDialog, type HostVerifyRequest } from "./HostVerifyDialog";

async function englishAsInTheApp(): Promise<typeof i18next> {
  const i18n = i18next.createInstance();
  await i18n.use(initReactI18next).init({
    lng: "en",
    resources: { en: { translation: en } },
    interpolation: { escapeValue: false },
  });
  return i18n;
}

const HOST = "x</1><br><strong>VERIFIED</strong><i>&amp;</i><1>y";

describe("HostVerifyDialog translations", () => {
  it.each(["new", "changed", "otherType", "unchecked"] as const)(
    "shows a %s prompt's host name as text",
    async (status) => {
      const i18n = await englishAsInTheApp();
      const request: HostVerifyRequest = {
        sessionId: "s1",
        host: HOST,
        port: 22,
        fingerprint: "SHA256:offered",
        keyType: "ssh-ed25519",
        status,
        knownKeys: [],
      };
      const { container } = render(
        <I18nextProvider i18n={i18n}>
          <HostVerifyDialog request={request} onRespond={() => undefined} />
        </I18nextProvider>,
      );
      const text = container.querySelector("#hv-text");
      expect(text?.textContent).toContain(`${HOST}:22`);
      // Only the translation's own <1>, around host and port.
      expect(text?.querySelectorAll("*")).toHaveLength(1);
      expect(text?.querySelector("strong")?.textContent).toBe(`${HOST}:22`);
    },
  );

  it("shows key types in the explanation as text", async () => {
    const i18n = await englishAsInTheApp();
    const request: HostVerifyRequest = {
      sessionId: "s1",
      host: "example.com",
      port: 22,
      fingerprint: "SHA256:offered",
      keyType: "<br>offered",
      status: "otherType",
      knownKeys: [{ keyType: "</1><i>recorded</i>", fingerprint: "SHA256:recorded" }],
    };
    const { container } = render(
      <I18nextProvider i18n={i18n}>
        <HostVerifyDialog request={request} onRespond={() => undefined} />
      </I18nextProvider>,
    );
    const text = container.querySelector("#hv-text");
    expect(text?.textContent).toContain("a </1><i>recorded</i> key");
    expect(text?.textContent).toContain("a <br>offered key");
    expect(text?.querySelectorAll("*")).toHaveLength(1);
  });

  it("shows ordinary values as they are", async () => {
    const i18n = await englishAsInTheApp();
    const request: HostVerifyRequest = {
      sessionId: "s1",
      host: "[fe80::1%eth0]",
      port: 2222,
      fingerprint: "SHA256:offered",
      keyType: "sk-ssh-ed25519@openssh.com",
      status: "otherType",
      knownKeys: [
        { keyType: "ecdsa-sha2-nistp256", fingerprint: "SHA256:one" },
        { keyType: "ssh-rsa", fingerprint: "SHA256:two" },
      ],
    };
    const { container } = render(
      <I18nextProvider i18n={i18n}>
        <HostVerifyDialog request={request} onRespond={() => undefined} />
      </I18nextProvider>,
    );
    expect(container.querySelector("#hv-text")?.textContent).toBe(
      "[fe80::1%eth0]:2222 is on record with a ecdsa-sha2-nistp256, ssh-rsa key, but now " +
        "offers a sk-ssh-ed25519@openssh.com key. Someone could be intercepting the " +
        "connection (a man-in-the-middle attack), or the host was given a new kind of key.",
    );
  });
});
