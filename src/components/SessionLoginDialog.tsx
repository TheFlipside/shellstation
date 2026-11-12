import React, { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { DialogKeys } from "./DialogKeys";
import { jumpHostChain, type Session } from "../stores/sessionStore";
import { useCredentialProfilesStore } from "../stores/credentialProfilesStore";
import { useLoginSequenceStore } from "../stores/loginSequenceStore";
import { hostAsWritten, shownAsWritten, typedAsWritten } from "../displayText";
import { CustomSelect } from "./CustomSelect";

interface SessionLoginDialogProps {
  /** Another user's shared session. */
  session: Session;
  /** The sessions the user can see, to find the session's jump hosts in. */
  sessions: Session[];
  onClose: () => void;
  onManageCredentials: () => void;
  onManageLoginSequences: () => void;
}

/** What is assigned: a credential profile per session ID, and a login sequence. */
interface Assigned {
  profiles: Record<string, string>;
  loginSequence: string;
}

/**
 * What the user confirmed of a session before: `ConfirmedBefore` in the
 * backend (`commands/session_login.rs`). Of the session the dialog is for it
 * is all of this. Of a jump host it is the host, the port and the jump host
 * it goes on to, and the rest is `null`: a jump host's own protocol, user
 * name and switch are not what it is connected to with, and the dialog does
 * not show them.
 */
interface Before {
  protocol: string | null;
  hostname: string;
  port: number;
  username: string | null;
  legacy_algorithms: boolean | null;
  jump_host_id: string | null;
}

/** What of a session the dialog shows, and the backend holds a session to. */
type Shown = "protocol" | "hostname" | "port" | "username" | "legacy" | "jumpHost";

/**
 * What the dialog shows of a session as it is now, as the backend will hold
 * the session to it once the user has saved.
 */
function beforeOf(s: Session, asJumpHost: boolean): Before {
  return {
    protocol: asJumpHost ? null : s.protocol,
    hostname: s.hostname,
    port: s.port,
    username: asJumpHost ? null : s.username,
    legacy_algorithms: asJumpHost ? null : s.legacy_algorithms,
    jump_host_id: s.jump_host_id,
  };
}

/**
 * What `session` names differently now than the user confirmed; as
 * `SessionPin::changes` and `JumpHostPin::changes` have it in the backend
 * (`login_pins.rs`). A jump host is held to its host, its port and the jump
 * host it goes on to. The switch for legacy algorithms and the jump host
 * count for SSH only: a Telnet session has no use for either.
 */
function changedSince(before: Before, session: Session, asJumpHost: boolean): Set<Shown> {
  const changed = new Set<Shown>();
  const telnet = !asJumpHost && before.protocol === "telnet" && session.protocol === "telnet";
  if (before.hostname !== session.hostname) changed.add("hostname");
  if (before.port !== session.port) changed.add("port");
  if (!telnet && before.jump_host_id !== session.jump_host_id) changed.add("jumpHost");
  if (asJumpHost) return changed;
  if (before.protocol !== session.protocol) changed.add("protocol");
  if (before.username !== session.username) changed.add("username");
  if (!telnet && before.legacy_algorithms !== session.legacy_algorithms) changed.add("legacy");
  return changed;
}

const NOTHING_CHANGED: ReadonlySet<Shown> = new Set<Shown>();

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/** How much of a session's name, and of its owner's role name, is shown. */
const MAX_NAME_CHARS = 64;

// Nothing to do: Escape is taken, and leaves the dialog as it is.
// eslint-disable-next-line @typescript-eslint/no-empty-function
const stay = (): void => {};

/**
 * The user's own login for another user's shared session, in PostgreSQL mode:
 * a credential profile for the session and for each of its jump hosts, and a
 * login sequence.
 *
 * Such a session is its owner's alone to change, and the profile and sequence
 * its row names do not count for anyone else, so the session dialog is not
 * for it. What is picked here goes into the user's own mappings
 * (`session_credentials`, `session_login_sequences`), which the backend
 * connects by. A jump host of the session is its owner's session as well and
 * is not connected to without a profile, so the chain is listed here too.
 *
 * The owner chose the session's name, protocol, host, port and user name,
 * and the hosts and ports of its jump hosts, and the user decides here
 * whether to use a profile of their own for them. They are therefore shown
 * apart from the dialog's own text, each in a field of its own. A host is
 * shown in printable ASCII, with every other character written out
 * (`hostAsWritten`), a user name likewise (`typedAsWritten`), a name with the
 * characters written out that would change how it displays
 * (`shownAsWritten`). A name is shown up to `MAX_NAME_CHARS` characters and a
 * host up to the 255 the app's own commands take, since a row written past
 * them can hold more, and would push the rest of the dialog out of sight.
 * Whatever is not Telnet is shown as the backend connects to it, as SSH.
 *
 * Saving also confirms what the dialog shows. The owner can change what the
 * session names afterwards, so the backend uses what the user assigned only
 * for what the user confirmed (`login_pins.rs`): a save tells it what was
 * shown, before anything is assigned, and it refuses if the session is not
 * that any more. Where the user has assigned something and the session
 * names something else than they confirmed, or was never confirmed, the
 * dialog says so. What has changed is marked, and what was confirmed before
 * is crossed out under what the session names now.
 *
 * The session and its chain are taken as they are when the dialog opens.
 * Nothing can be picked or saved before what is assigned has been read, so
 * that a choice is not replaced by what is read and a save cannot clear a
 * mapping it has not seen. Only what was changed is written. While a save is
 * under way the dialog takes no input and does not close: the writes cannot
 * be taken back, so their outcome is shown.
 */
export function SessionLoginDialog({
  session,
  sessions,
  onClose,
  onManageCredentials,
  onManageLoginSequences,
}: SessionLoginDialogProps): React.JSX.Element {
  const { t } = useTranslation();
  const [target] = useState(session);
  const [chain] = useState(() => jumpHostChain(session, sessions));
  // As the backend has it: whatever is not Telnet is connected to over SSH.
  const isSsh = target.protocol !== "telnet";

  const credentialProfiles = useCredentialProfilesStore((s) => s.profiles);
  const loginSequences = useLoginSequenceStore((s) => s.sequences);

  const [saved, setSaved] = useState<Assigned | null>(null);
  // What the user confirmed of each session shown, by its ID; nothing for
  // one that was never confirmed.
  const [confirmed, setConfirmed] = useState<Record<string, Before | undefined>>({});
  // What the user confirmed, as a jump host, of each session that one of the
  // sessions shown went on to before and does not go on to now.
  const [wentOnTo, setWentOnTo] = useState<Record<string, Before | undefined>>({});
  const [picked, setPicked] = useState<Assigned>({ profiles: {}, loginSequence: "" });
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const locked = saved === null || saving;
  const saveButton = useRef<HTMLButtonElement>(null);

  // Saving disables the button that had the focus. After a save that failed
  // it is given back, next to the message that says so.
  useEffect(() => {
    if (!saving && error && saved) saveButton.current?.focus();
  }, [saving, error, saved]);

  useEffect(() => {
    let cancelled = false;
    type ById = Record<string, Before | undefined>;
    const load = async (): Promise<[Assigned, ById, ById]> => {
      const profiles: Record<string, string> = {};
      if (isSsh) {
        for (const s of [target, ...chain.hops]) {
          const id = await invoke<string | null>("get_session_credential", { sessionId: s.id });
          profiles[s.id] = id ?? "";
        }
      }
      const sequence = await invoke<string | null>("get_session_login_sequence", {
        sessionId: target.id,
      });
      // The session the dialog is for first, then its jump hosts: the
      // backend answers for each as what it is.
      const shown = [target, ...chain.hops];
      const before = await invoke<(Before | null)[]>("session_login_confirmed", {
        sessionIds: shown.map((s) => s.id),
      });
      const confirmedById: ById = {};
      shown.forEach((s, i) => {
        confirmedById[s.id] = before[i] ?? undefined;
      });
      // Where a session went on to another jump host when it was confirmed,
      // the dialog shows what the user confirmed of that one: its host and
      // port then, which its row need not name any more. The backend takes
      // the first ID for the session a dialog is for, so that is sent along
      // and its answer left out.
      const earlier = before
        .map((b) => b?.jump_host_id)
        .filter((id, i): id is string => Boolean(id) && id !== shown[i].jump_host_id)
        .slice(0, 10);
      const wentOnToById: ById = {};
      if (earlier.length > 0) {
        const answers = await invoke<(Before | null)[]>("session_login_confirmed", {
          sessionIds: [target.id, ...earlier],
        });
        earlier.forEach((id, i) => {
          wentOnToById[id] = answers[i + 1] ?? undefined;
        });
      }
      return [{ profiles, loginSequence: sequence ?? "" }, confirmedById, wentOnToById];
    };
    load().then(
      ([assigned, confirmedById, wentOnToById]) => {
        if (cancelled) return;
        setConfirmed(confirmedById);
        setWentOnTo(wentOnToById);
        setSaved(assigned);
        setPicked(assigned);
      },
      (err: unknown) => {
        if (!cancelled) setError(errorText(err));
      },
    );
    return () => {
      cancelled = true;
    };
  }, [target, chain, isSsh]);

  const handleSubmit = (e: React.SyntheticEvent): void => {
    e.preventDefault();
    if (!saved || saving) return;
    setError("");
    setSaving(true);
    // What has been written so far, so that trying again after a failure
    // writes only the rest.
    const done: Assigned = { profiles: { ...saved.profiles }, loginSequence: saved.loginSequence };
    const save = async (): Promise<void> => {
      // First what the user saw, so that nothing is assigned to a session
      // that has been changed since. Nothing is confirmed where nothing is
      // assigned: clearing an assignment needs no look at the session.
      if (Object.values(picked.profiles).some(Boolean) || picked.loginSequence) {
        const shown = [target, ...chain.hops];
        await invoke("session_login_confirm", {
          sessions: shown.map((s) => ({
            id: s.id,
            protocol: s.protocol,
            hostname: s.hostname,
            port: s.port,
            username: s.username,
            legacy_algorithms: s.legacy_algorithms,
            jump_host_id: s.jump_host_id,
          })),
        });
        // It is confirmed from here on, also if assigning then fails.
        const now: Record<string, Before | undefined> = {};
        shown.forEach((s, i) => {
          now[s.id] = beforeOf(s, i > 0);
        });
        setConfirmed(now);
      }
      for (const [sessionId, profileId] of Object.entries(picked.profiles)) {
        if (profileId === done.profiles[sessionId]) continue;
        await invoke("set_session_credential", {
          sessionId,
          credentialProfileId: profileId || null,
        });
        done.profiles[sessionId] = profileId;
      }
      if (picked.loginSequence !== done.loginSequence) {
        await invoke("set_session_login_sequence", {
          sessionId: target.id,
          loginSequenceId: picked.loginSequence || null,
        });
        done.loginSequence = picked.loginSequence;
      }
    };
    save().then(onClose, (err: unknown) => {
      setSaved(done);
      setSaving(false);
      setError(errorText(err));
    });
  };

  const profileOptions = [
    { value: "", label: t("session.credentialProfileNone") },
    ...credentialProfiles.map((p) => ({
      value: p.id,
      label: p.username ? `${p.name} (${p.username})` : p.name,
    })),
  ];
  const pickProfile = (sessionId: string, profileId: string): void => {
    setPicked((p) => ({ ...p, profiles: { ...p.profiles, [sessionId]: profileId } }));
  };

  // The sessions shown, the one the dialog is for first, and for each what
  // it names differently now than the user confirmed of it.
  const shown = [target, ...chain.hops];
  const changes = shown.map((s, i) => {
    const before = confirmed[s.id];
    return before ? changedSince(before, s, i > 0) : NOTHING_CHANGED;
  });
  const before = confirmed[target.id];
  const changed = changes[0] ?? NOTHING_CHANGED;
  // Whether something the user has assigned waits for the user to confirm
  // what its session names: the backend does not use it until then. A
  // profile for a jump host is used through the session the dialog is for,
  // with that session's switch for legacy algorithms, so it waits for that
  // session to be confirmed as well, also where nothing is assigned to it.
  const isAssigned = (s: Session, i: number): boolean =>
    saved !== null && (Boolean(saved.profiles[s.id]) || (i === 0 && Boolean(saved.loginSequence)));
  const toConfirm = shown.some((s, i) => {
    const used = i === 0 ? shown.some(isAssigned) : isAssigned(s, i);
    return used && (!confirmed[s.id] || (changes[i]?.size ?? 0) > 0);
  });

  const given = (isChanged: boolean): string =>
    isChanged ? "dialog-given-value dialog-given-changed" : "dialog-given-value";
  // What was confirmed before, crossed out, under a value that is another now.
  const was = (isChanged: boolean, value: string): React.JSX.Element | null =>
    isChanged ? (
      <del className="dialog-given-was" dir="ltr">
        {value || "—"}
      </del>
    ) : null;
  // The jump host that the `i`th session shown went on to when the user
  // confirmed it, where it goes on to another one now, or to none: crossed
  // out, in the place of the one it goes on to now. It is shown by the host
  // and port the user confirmed of it, not by what its row names now, which
  // its owner can have changed since; where the user confirmed nothing of
  // it, by its ID.
  const wasJumpHost = (i: number): React.JSX.Element | null => {
    const id = i < shown.length ? confirmed[shown[i].id]?.jump_host_id : undefined;
    if (!id || !changes[i]?.has("jumpHost")) return null;
    const old = wentOnTo[id];
    return (
      <del className="dialog-given-was" dir="ltr" id={`sld-was-jump-${String(i)}`}>
        {old ? `${hostAsWritten(old.hostname)}  ${String(old.port)}` : id}
      </del>
    );
  };

  return (
    <div className="dialog-overlay" role="presentation">
      <DialogKeys onEscape={saving ? stay : onClose} />
      <div
        className="dialog dialog-wide"
        role="dialog"
        aria-modal="true"
        aria-labelledby="sld-title"
      >
        <h3 className="dialog-title dialog-quoted-text" id="sld-title">
          {t("sessionLogin.title", { name: shownAsWritten(target.name, MAX_NAME_CHARS) })}
        </h3>
        <p className="dialog-info dialog-quoted-text">
          {t("sessionLogin.info", { owner: shownAsWritten(target.owner, MAX_NAME_CHARS) })}
        </p>
        {toConfirm && (
          <p className="dialog-info dialog-warning" role="status" id="sld-to-confirm">
            {t("sessionLogin.notConfirmed")}
          </p>
        )}
        <form onSubmit={handleSubmit}>
          <div className="dialog-row">
            <div className="dialog-field dialog-field-small">
              <label htmlFor="sld-protocol">{t("session.protocolLabel")}</label>
              <output id="sld-protocol" className={given(changed.has("protocol"))} dir="ltr">
                {hostAsWritten(target.protocol, 16)}
              </output>
              {was(changed.has("protocol"), hostAsWritten(before?.protocol ?? "", 16))}
            </div>
            <div className="dialog-field dialog-field-grow">
              <label htmlFor="sld-host">{t("session.hostnameLabel")}</label>
              <output id="sld-host" className={given(changed.has("hostname"))} dir="ltr">
                {hostAsWritten(target.hostname)}
              </output>
              {was(changed.has("hostname"), hostAsWritten(before?.hostname ?? ""))}
            </div>
            <div className="dialog-field dialog-field-small">
              <label htmlFor="sld-port">{t("session.portLabel")}</label>
              <output id="sld-port" className={given(changed.has("port"))} dir="ltr">
                {String(target.port)}
              </output>
              {was(changed.has("port"), String(before?.port ?? ""))}
            </div>
          </div>
          <div className="dialog-field">
            <label htmlFor="sld-username">{t("session.usernameLabel")}</label>
            <output id="sld-username" className={given(changed.has("username"))} dir="ltr">
              {typedAsWritten(target.username)}
            </output>
            {was(changed.has("username"), typedAsWritten(before?.username ?? ""))}
          </div>
          {isSsh && (
            <div className="dialog-field">
              <label
                htmlFor="sld-legacy"
                className={
                  changed.has("legacy")
                    ? "dialog-checkbox-label dialog-label-changed"
                    : "dialog-checkbox-label"
                }
              >
                <input
                  id="sld-legacy"
                  type="checkbox"
                  checked={target.legacy_algorithms}
                  disabled
                  readOnly
                />
                {t("session.legacyAlgorithmsLabel")}
              </label>
            </div>
          )}
          {isSsh && (
            <div className="dialog-field">
              <label htmlFor="sld-profile">{t("session.credentialProfileLabel")}</label>
              <div className="dialog-row">
                <div className="dialog-field-grow">
                  <CustomSelect
                    id="sld-profile"
                    value={picked.profiles[target.id] ?? ""}
                    onChange={(v) => {
                      pickProfile(target.id, v);
                    }}
                    options={profileOptions}
                    disabled={locked}
                  />
                </div>
                <button type="button" className="dialog-btn" onClick={onManageCredentials}>
                  {t("credentialProfiles.manageLink")}
                </button>
              </div>
            </div>
          )}
          {chain.hops.map((hop, i) => {
            // The session that goes on to this jump host is the one shown
            // before it; what has changed of the jump host itself is next.
            const hopChanged = changes[i + 1] ?? NOTHING_CHANGED;
            const hopBefore = confirmed[hop.id];
            return (
              <div className="dialog-field" key={hop.id}>
                <label
                  htmlFor={`sld-hop-${String(i)}`}
                  className={
                    changes[i]?.has("jumpHost")
                      ? "dialog-quoted-text dialog-label-changed"
                      : "dialog-quoted-text"
                  }
                >
                  {t("sessionLogin.jumpHostProfileLabel", {
                    name: shownAsWritten(hop.name, MAX_NAME_CHARS),
                  })}
                </label>
                {wasJumpHost(i)}
                <div className="dialog-row">
                  <div className="dialog-field-grow">
                    <output
                      id={`sld-hop-host-${String(i)}`}
                      className={given(hopChanged.has("hostname"))}
                      dir="ltr"
                      aria-label={t("session.hostnameLabel")}
                    >
                      {hostAsWritten(hop.hostname)}
                    </output>
                    {was(hopChanged.has("hostname"), hostAsWritten(hopBefore?.hostname ?? ""))}
                  </div>
                  <div className="dialog-field-small">
                    <output
                      id={`sld-hop-port-${String(i)}`}
                      className={given(hopChanged.has("port"))}
                      dir="ltr"
                      aria-label={t("session.portLabel")}
                    >
                      {String(hop.port)}
                    </output>
                    {was(hopChanged.has("port"), String(hopBefore?.port ?? ""))}
                  </div>
                </div>
                <CustomSelect
                  id={`sld-hop-${String(i)}`}
                  value={picked.profiles[hop.id] ?? ""}
                  onChange={(v) => {
                    pickProfile(hop.id, v);
                  }}
                  options={profileOptions}
                  disabled={locked}
                />
                {saved && !picked.profiles[hop.id] && (
                  <p className="dialog-field-note">{t("sessionLogin.jumpHostNeedsProfile")}</p>
                )}
              </div>
            );
          })}
          {/* The jump host the last session shown went on to, where it goes
              on to none now, or to one that is not listed. */}
          {wasJumpHost(chain.hops.length)}
          {!chain.complete && (
            <p className="dialog-info dialog-warning">{t("sessionLogin.jumpHostUnavailable")}</p>
          )}
          <div className="dialog-field">
            <label htmlFor="sld-loginseq">{t("session.loginSequenceLabel")}</label>
            <div className="dialog-row">
              <div className="dialog-field-grow">
                <CustomSelect
                  id="sld-loginseq"
                  value={picked.loginSequence}
                  onChange={(v) => {
                    setPicked((p) => ({ ...p, loginSequence: v }));
                  }}
                  options={[
                    { value: "", label: t("session.loginSequenceNone") },
                    ...loginSequences.map((s) => ({ value: s.id, label: s.name })),
                  ]}
                  disabled={locked}
                />
              </div>
              <button type="button" className="dialog-btn" onClick={onManageLoginSequences}>
                {t("loginSequences.manageLink")}
              </button>
            </div>
          </div>
          {error && (
            <p className="dialog-error" role="alert">
              {error}
            </p>
          )}
          <div className="dialog-actions">
            <button
              type="button"
              className="dialog-btn dialog-btn-cancel"
              onClick={onClose}
              disabled={saving}
            >
              {t("dialog.cancel")}
            </button>
            <button
              type="submit"
              className="dialog-btn dialog-btn-primary"
              disabled={locked}
              ref={saveButton}
            >
              {t("dialog.save")}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
