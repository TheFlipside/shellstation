use serde::{Deserialize, Serialize};
use tauri::State;
use uuid::Uuid;

use crate::db::models::Session;
use crate::db::{DbState, PgPoolState};
use crate::login_pins::{
    name_for_message, unconfirmable, JumpHostPin, LoginPinState, SessionPin, UsedAs,
};

use super::MAX_JUMP_HOPS;

// What a user has confirmed of another user's shared session, for the
// dialog in which they assign a credential profile and a login sequence of
// their own to it (**My Login**); see `login_pins.rs` for why. The dialog
// asks what was confirmed before, to show what has changed since, and
// confirms what it shows when the user saves. In both commands the first
// session is the one the dialog is for, and the others are its jump hosts,
// in the order a connection goes on from one to the next.

/// A session as the dialog showed it to the user, and which session it is.
/// The fields are named as the frontend's session has them, which is how the
/// backend sends a session. Of a jump host the dialog shows the host, the
/// port and, as the next entry, the jump host it goes on to; the rest is
/// taken as sent and not looked at.
#[derive(Debug, Deserialize)]
pub struct ShownSession {
    id: String,
    protocol: String,
    hostname: String,
    port: i32,
    username: String,
    legacy_algorithms: bool,
    jump_host_id: Option<String>,
}

/// What was confirmed of a session, for the dialog: of the session it is
/// for, all of it; of a jump host, what a jump host is held to, with the
/// rest left out.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct ConfirmedBefore {
    protocol: Option<String>,
    hostname: String,
    port: i32,
    username: Option<String>,
    legacy_algorithms: Option<bool>,
    jump_host_id: Option<Uuid>,
}

impl From<SessionPin> for ConfirmedBefore {
    fn from(pin: SessionPin) -> Self {
        Self {
            protocol: Some(pin.protocol),
            hostname: pin.hostname,
            port: pin.port,
            username: Some(pin.username),
            legacy_algorithms: Some(pin.legacy_algorithms),
            jump_host_id: pin.jump_host_id,
        }
    }
}

impl From<JumpHostPin> for ConfirmedBefore {
    fn from(pin: JumpHostPin) -> Self {
        Self {
            protocol: None,
            hostname: pin.hostname,
            port: pin.port,
            username: None,
            legacy_algorithms: None,
            jump_host_id: pin.jump_host_id,
        }
    }
}

fn parse_uuid(s: &str) -> Result<Uuid, String> {
    Uuid::parse_str(s).map_err(|e| format!("Invalid UUID '{s}': {e}"))
}

/// As many sessions as one dialog shows: the session and its jump hosts.
const MAX_SHOWN: usize = MAX_JUMP_HOPS + 1;

const CHANGED_SINCE_SHOWN: &str =
    "A session shown here has been changed since this dialog was opened, so \
     nothing was saved. Close the dialog, refresh the session list, and open \
     My Login again to see what the session connects to now.";

/// Check that `now`, a session as it is in the database at this moment,
/// names what the dialog showed of it, and can be confirmed at all.
///
/// The dialog shows the sessions as the window last loaded them, and the
/// user confirms what they see. A session that its owner has changed in the
/// meantime is therefore not confirmed: the pin would be for something the
/// user has not looked at. Nor is one that names what the dialog cannot
/// show for what it is; see [`unconfirmable`].
fn check_as_shown(shown: &ShownSession, now: &Session, used_as: UsedAs) -> Result<(), String> {
    let shown_jump_host = shown.jump_host_id.as_deref().map(parse_uuid).transpose()?;
    let as_jump_host = shown.hostname == now.hostname
        && shown.port == now.port
        && shown_jump_host == now.jump_host_id;
    let as_shown = match used_as {
        UsedAs::JumpHost => as_jump_host,
        UsedAs::Target => {
            as_jump_host
                && shown.protocol == now.protocol
                && shown.username == now.username
                && shown.legacy_algorithms == now.legacy_algorithms
        }
    };
    if !as_shown {
        return Err(CHANGED_SINCE_SHOWN.to_string());
    }
    if let Some(why) = unconfirmable(now, used_as) {
        return Err(format!(
            "\"{}\" cannot be confirmed: {why}. Nothing was saved. Its owner \
             can set it right.",
            name_for_message(&now.name)
        ));
    }
    Ok(())
}

/// Confirm what the dialog showed of a session and its jump hosts, so that
/// what the user assigns to them is used for it.
///
/// Each session is read again here, and all of them are confirmed or none:
/// see [`check_as_shown`]. The sessions have to be the chain the dialog
/// shows: each but the last names the next as its jump host, so that a
/// session is confirmed as a jump host only where it is one.
#[tauri::command]
pub async fn session_login_confirm(
    db: State<'_, DbState>,
    pg: State<'_, PgPoolState>,
    pins: State<'_, LoginPinState>,
    sessions: Vec<ShownSession>,
) -> Result<(), String> {
    if pg.0.is_none() {
        return Err("Sessions are confirmed only in PostgreSQL mode".to_string());
    }
    if sessions.is_empty() || sessions.len() > MAX_SHOWN {
        return Err(format!(
            "Between 1 and {MAX_SHOWN} sessions are confirmed at a time"
        ));
    }
    let mut rows = Vec::with_capacity(sessions.len());
    for (i, shown) in sessions.iter().enumerate() {
        let id = parse_uuid(&shown.id)?;
        // A session that is gone, or no longer shared, reads the same.
        let now = db.0.get_session(id).await?.ok_or_else(|| {
            "A session shown here is no longer there, or no longer shared with \
             you, so nothing was saved. Close the dialog and refresh the \
             session list."
                .to_string()
        })?;
        let used_as = if i == 0 {
            UsedAs::Target
        } else {
            UsedAs::JumpHost
        };
        check_as_shown(shown, &now, used_as)?;
        rows.push(now);
    }
    if !is_chain(&rows) {
        return Err(CHANGED_SINCE_SHOWN.to_string());
    }
    let mut rows = rows.into_iter();
    // Not empty, as checked above.
    let Some(target) = rows.next() else {
        return Err("No session to confirm".to_string());
    };
    pins.confirm(
        (target.id, SessionPin::of(&target)),
        rows.map(|hop| (hop.id, JumpHostPin::of(&hop))).collect(),
    )
}

/// Whether each session but the last names the next as its jump host.
fn is_chain(sessions: &[Session]) -> bool {
    sessions
        .windows(2)
        .all(|pair| pair[0].jump_host_id == Some(pair[1].id))
}

/// What the user confirmed before of the session the dialog is for, the
/// first, and of its jump hosts, the others; `null` for one that was never
/// confirmed. Of a jump host it is what was confirmed of it as one, or else
/// as a session connected to, as far as a jump host is held to that.
#[tauri::command]
pub async fn session_login_confirmed(
    pins: State<'_, LoginPinState>,
    session_ids: Vec<String>,
) -> Result<Vec<Option<ConfirmedBefore>>, String> {
    if session_ids.len() > MAX_SHOWN {
        return Err(format!(
            "At most {MAX_SHOWN} sessions are asked for at a time"
        ));
    }
    let mut before = Vec::with_capacity(session_ids.len());
    for (i, id) in session_ids.iter().enumerate() {
        let confirmed = pins.confirmed(parse_uuid(id)?)?;
        before.push(if i == 0 {
            confirmed.target.map(ConfirmedBefore::from)
        } else {
            confirmed.as_jump_host().map(ConfirmedBefore::from)
        });
    }
    Ok(before)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::login_pins::tests::session as a_session;

    fn session() -> Session {
        let mut session = a_session("core");
        session.jump_host_id = Some(Uuid::new_v4());
        session
    }

    fn shown(session: &Session) -> ShownSession {
        ShownSession {
            id: session.id.to_string(),
            protocol: session.protocol.clone(),
            hostname: session.hostname.clone(),
            port: session.port,
            username: session.username.clone(),
            legacy_algorithms: session.legacy_algorithms,
            jump_host_id: session.jump_host_id.map(|id| id.to_string()),
        }
    }

    #[test]
    fn a_session_is_confirmed_as_the_dialog_showed_it() {
        let now = session();
        assert_eq!(check_as_shown(&shown(&now), &now, UsedAs::Target), Ok(()));
        assert_eq!(check_as_shown(&shown(&now), &now, UsedAs::JumpHost), Ok(()));
        // The jump host's ID is read in any form `Uuid` reads.
        let mut upper = shown(&now);
        upper.jump_host_id = upper.jump_host_id.map(|id| id.to_uppercase());
        assert_eq!(check_as_shown(&upper, &now, UsedAs::Target), Ok(()));
    }

    #[test]
    fn a_session_changed_since_it_was_shown_is_not_confirmed() {
        let now = session();
        let stale = |used_as, change: fn(&mut ShownSession)| {
            let mut shown = shown(&now);
            change(&mut shown);
            check_as_shown(&shown, &now, used_as)
        };
        for used_as in [UsedAs::Target, UsedAs::JumpHost] {
            for result in [
                stale(used_as, |s| s.hostname = "before.example".to_string()),
                stale(used_as, |s| s.port = 2222),
                stale(used_as, |s| s.jump_host_id = None),
                stale(used_as, |s| {
                    s.jump_host_id = Some(Uuid::new_v4().to_string())
                }),
            ] {
                let error = result.unwrap_err();
                assert!(
                    error.contains("changed since this dialog was opened"),
                    "{error}"
                );
                assert!(error.contains("nothing was saved"), "{error}");
            }
            assert!(
                stale(used_as, |s| s.jump_host_id = Some("no-uuid".to_string()))
                    .unwrap_err()
                    .starts_with("Invalid UUID")
            );
        }
        // What the dialog shows of the session it is for, and not of a jump
        // host.
        for change in [
            (|s| s.protocol = "telnet".to_string()) as fn(&mut ShownSession),
            |s| s.username = "root".to_string(),
            |s| s.legacy_algorithms = true,
        ] {
            assert!(stale(UsedAs::Target, change).is_err());
            assert_eq!(stale(UsedAs::JumpHost, change), Ok(()));
        }
    }

    #[test]
    fn what_the_dialog_cannot_show_for_what_it_is_is_not_confirmed() {
        let mut now = session();
        now.port = 65536 + 22;
        let error = check_as_shown(&shown(&now), &now, UsedAs::Target).unwrap_err();
        // It says which of the sessions shown it is.
        assert!(
            error.starts_with("\"core\" cannot be confirmed: its port is none"),
            "{error}"
        );
        assert!(error.contains("Nothing was saved"), "{error}");
        // A protocol that is neither counts for the session, not for a jump
        // host, whose own protocol is not used.
        let mut now = session();
        now.protocol = "ssh2".to_string();
        assert!(check_as_shown(&shown(&now), &now, UsedAs::Target).is_err());
        assert_eq!(check_as_shown(&shown(&now), &now, UsedAs::JumpHost), Ok(()));
    }

    #[test]
    fn the_sessions_confirmed_together_are_a_chain() {
        let far = a_session("far");
        let mut near = a_session("near");
        near.jump_host_id = Some(far.id);
        let mut target = a_session("target");
        target.jump_host_id = Some(near.id);

        assert!(is_chain(&[target.clone()]));
        assert!(is_chain(&[target.clone(), near.clone(), far.clone()]));
        // The last may go on to one the dialog could not show.
        assert!(is_chain(&[target.clone(), near.clone()]));
        // But none is confirmed as a jump host that is not one of these.
        assert!(!is_chain(&[target.clone(), far.clone()]));
        assert!(!is_chain(&[near.clone(), target.clone()]));
        assert!(!is_chain(&[target.clone(), target]));
        assert!(!is_chain(&[far, near]));
    }

    #[test]
    fn a_shown_session_is_read_as_the_dialog_sends_one() {
        // The dialog passes on these fields of a session as it got them from
        // the backend.
        let now = session();
        let sent = serde_json::json!({
            "id": now.id,
            "protocol": now.protocol,
            "hostname": now.hostname,
            "port": now.port,
            "username": now.username,
            "legacy_algorithms": now.legacy_algorithms,
            "jump_host_id": now.jump_host_id,
        });
        let shown: ShownSession = serde_json::from_value(sent).unwrap();
        assert_eq!(check_as_shown(&shown, &now, UsedAs::Target), Ok(()));
        // And a whole session, as the backend sends one, has all of them.
        let whole: ShownSession =
            serde_json::from_value(serde_json::to_value(&now).unwrap()).unwrap();
        assert_eq!(check_as_shown(&whole, &now, UsedAs::Target), Ok(()));
    }

    #[test]
    fn what_was_confirmed_is_sent_with_what_a_jump_host_is_not_held_to_left_out() {
        let now = session();
        let of_session = serde_json::to_value(ConfirmedBefore::from(SessionPin::of(&now))).unwrap();
        assert_eq!(
            of_session,
            serde_json::json!({
                "protocol": "ssh",
                "hostname": "switch.example",
                "port": 22,
                "username": "admin",
                "legacy_algorithms": false,
                "jump_host_id": now.jump_host_id,
            })
        );
        let of_jump_host =
            serde_json::to_value(ConfirmedBefore::from(JumpHostPin::of(&now))).unwrap();
        assert_eq!(
            of_jump_host,
            serde_json::json!({
                "protocol": null,
                "hostname": "switch.example",
                "port": 22,
                "username": null,
                "legacy_algorithms": null,
                "jump_host_id": now.jump_host_id,
            })
        );
    }
}
