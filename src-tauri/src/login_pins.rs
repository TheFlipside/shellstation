//! What another user's session named when this user confirmed it.
//!
//! In `PostgreSQL` mode a user can assign a credential profile and a login
//! sequence of their own to another user's shared session (the per-user
//! mappings, `commands/session_credentials.rs`). A mapping is for a session,
//! and the session's owner can change what the session names afterwards:
//! its host and port, its protocol, its user name, its jump host, its switch
//! for legacy algorithms. Without more, the user's next connection would use
//! the profile, and run the sequence, for whatever the row says by then: a
//! password would go to a host of the owner's choosing, and a login sequence
//! in clear text to whatever answers on Telnet.
//!
//! So the app remembers, for each such session, what it named when the user
//! looked at it in **My Login** and saved. A connect uses what the user
//! assigned to a session that is not theirs only while the session still
//! names that; see [`refusal`]. Anything else is refused before a connection
//! is opened, and the user confirms the session again in the same dialog,
//! which shows what it names now.
//!
//! A session is confirmed for the way the dialog showed it: as the session
//! connected to ([`SessionPin`]), with everything a connection to it goes
//! by, or as a jump host on the way to another ([`JumpHostPin`]), with its
//! host, its port and the jump host it goes on to. The two are kept apart.
//! The dialog shows less of a jump host, since its own protocol, user name
//! and switch are not what it is connected to with, and what the user has
//! not been shown must not count as confirmed when the same session is
//! later connected to by itself.
//!
//! What is confirmed is kept in a file of the user's own, in the app's
//! config directory, and not in the shared database: what one user has
//! confirmed is nothing another role could write there, whatever its
//! privileges, and the shared schema stays as it is. It is kept by session
//! alone, not by database or role: an ID is random, and what counts is that
//! the session still names what was confirmed.
//!
//! This rests on a row's `owner`, as the rest of the per-user rules do: it
//! is for sessions that are not the user's own, and a user who can write
//! another owner into a row of their own, as under Option B
//! (`docs/DESIGN.md` §4.5.2), gets past it as past those.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::models::Session;

/// What a session named when the user confirmed it as the session connected
/// to: everything of its row that a connection to it goes by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPin {
    pub protocol: String,
    pub hostname: String,
    pub port: i32,
    /// The row's own user name: it is what `\s` in a login sequence stands
    /// for where the user has assigned no profile, and on Telnet always.
    pub username: String,
    pub legacy_algorithms: bool,
    pub jump_host_id: Option<Uuid>,
}

/// What a session named when the user confirmed it as a jump host: its
/// host, its port and the jump host it goes on to. It is connected to over
/// SSH, with the user's own profile and the switch for legacy algorithms of
/// the session that goes through it, so no more of its row is used. That
/// switch is part of what is confirmed of the session that goes through it,
/// which is why a connect that uses a profile for a jump host holds that
/// session to what was confirmed of it as well; see `resolve_jump_chain`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JumpHostPin {
    pub hostname: String,
    pub port: i32,
    pub jump_host_id: Option<Uuid>,
}

impl SessionPin {
    /// What `session` names now.
    #[must_use]
    pub fn of(session: &Session) -> Self {
        Self {
            protocol: session.protocol.clone(),
            hostname: session.hostname.clone(),
            port: session.port,
            username: session.username.clone(),
            legacy_algorithms: session.legacy_algorithms,
            jump_host_id: session.jump_host_id,
        }
    }

    /// The part of it that counts where the session is used as a jump host.
    /// Whoever confirmed a session as the one connected to has seen all of
    /// this.
    #[must_use]
    pub fn as_jump_host(&self) -> JumpHostPin {
        JumpHostPin {
            hostname: self.hostname.clone(),
            port: self.port,
            jump_host_id: self.jump_host_id,
        }
    }

    /// What `session` names differently now, in words for a message.
    fn changes(&self, session: &Session) -> Vec<String> {
        let mut changes = Vec::new();
        if self.protocol != session.protocol {
            changes.push(format!(
                "protocol {} → {}",
                ascii_for_message(&self.protocol),
                ascii_for_message(&session.protocol)
            ));
        }
        // Telnet has no algorithms to negotiate and goes through no jump
        // host, so there neither the switch nor the jump host is anything a
        // connection goes by.
        let telnet = self.protocol == "telnet" && session.protocol == "telnet";
        let mut named = self.as_jump_host().changes(session);
        if telnet {
            named.retain(|change| change != JUMP_HOST_CHANGED);
        }
        changes.extend(named);
        if self.username != session.username {
            changes.push(format!(
                "user name \"{}\" → \"{}\"",
                ascii_for_message(&self.username),
                ascii_for_message(&session.username)
            ));
        }
        if !telnet && self.legacy_algorithms != session.legacy_algorithms {
            changes.push(format!(
                "legacy algorithms {} → {}",
                on_off(self.legacy_algorithms),
                on_off(session.legacy_algorithms)
            ));
        }
        changes
    }
}

impl JumpHostPin {
    /// What `session` names now, as a jump host.
    #[must_use]
    pub fn of(session: &Session) -> Self {
        SessionPin::of(session).as_jump_host()
    }

    fn changes(&self, session: &Session) -> Vec<String> {
        let mut changes = Vec::new();
        if self.hostname != session.hostname {
            changes.push(format!(
                "host {} → {}",
                ascii_for_message(&self.hostname),
                ascii_for_message(&session.hostname)
            ));
        }
        if self.port != session.port {
            changes.push(format!("port {} → {}", self.port, session.port));
        }
        if self.jump_host_id != session.jump_host_id {
            changes.push(JUMP_HOST_CHANGED.to_string());
        }
        changes
    }
}

/// How a message names a change of the jump host a session goes on to.
const JUMP_HOST_CHANGED: &str = "jump host";

fn on_off(on: bool) -> &'static str {
    if on {
        "on"
    } else {
        "off"
    }
}

/// How a connect uses a session: as the one connected to, or as a jump host
/// on the way there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsedAs {
    Target,
    JumpHost,
}

/// The longest host or user name that can be confirmed, in characters. The
/// app's own commands let a host be as many bytes, so no more characters.
const MAX_FIELD_CHARS: usize = 255;

/// Why what `session` names cannot be confirmed at all, or `None`.
///
/// The dialog shows the user what they confirm, and has to show it so that
/// two values never look alike and none is shown in part: a host in
/// printable ASCII with every other character written out, cut where it is
/// longer than a host may be. That does not hold for everything a row can
/// name. A statement of its owner's own can write a protocol that is neither
/// of the two, which a connect takes for SSH, a port outside 1 to 65535,
/// which a connect would cut down to another port than is shown, and a host
/// longer than is shown; the app's own commands refuse all three. They take
/// the rest, though no session that works has any of it: no host at all, or
/// one with a backslash in it, which reads like a character written out, or
/// with a space or a control character in it, of which a space at its end
/// does not show, or with any other character that no host name has and an
/// SSH connect refuses, an invisible one among them
/// (`validate_host_name` in `ssh/mod.rs`; a Telnet connect does not ask); or
/// one that takes more room written out than a host may take, as one of many
/// letters of another script does, each of them eight characters, which
/// would put its end far from its start; and a user name longer than is
/// shown, with a control character in it, which a login sequence would type
/// at the host as a key, or with a space at its start or end. Such a session
/// is not confirmed and nothing the user assigned is used for it.
#[must_use]
pub fn unconfirmable(session: &Session, used_as: UsedAs) -> Option<&'static str> {
    let host = &session.hostname;
    if host.is_empty() {
        return Some("it names no host");
    }
    if host.chars().count() > MAX_FIELD_CHARS {
        return Some("its host is longer than a host can be");
    }
    if host.contains('\\') {
        return Some("its host has a backslash in it");
    }
    if host.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Some("its host has a space or a control character in it");
    }
    if crate::ssh::validate_host_name(host).is_err() {
        return Some("its host has a character in it that no host name has");
    }
    if written_out_len(host) > MAX_FIELD_CHARS {
        return Some("its host takes more room written out than a host can");
    }
    if !(1..=65535).contains(&session.port) {
        return Some("its port is none");
    }
    if used_as == UsedAs::Target {
        if session.protocol != "ssh" && session.protocol != "telnet" {
            return Some("its protocol is neither SSH nor Telnet");
        }
        let user = &session.username;
        if user.chars().count() > MAX_FIELD_CHARS {
            return Some("its user name is longer than one can be");
        }
        if user.chars().any(char::is_control) {
            return Some("its user name has a control character in it");
        }
        if user.starts_with(char::is_whitespace) || user.ends_with(char::is_whitespace) {
            return Some("its user name starts or ends with a space");
        }
    }
    None
}

/// Whether a connect needs the user to have confirmed what a session names.
///
/// It does where the session is not the user's own (`row_defaults` is what
/// `row_defaults_apply` in `commands/session.rs` says of it) and something
/// the user assigned to it, a credential profile or a login sequence, is
/// about to be used for it. A session of the user's own names what the user
/// had it name. And where nothing of the user's is used, nothing of the
/// user's goes anywhere: the session is then connected to with the user name
/// its row has and what the user types.
#[must_use]
pub fn needs_confirmation(pg_mode: bool, row_defaults: bool, uses_own_mapping: bool) -> bool {
    pg_mode && !row_defaults && uses_own_mapping
}

/// What the user has confirmed of one session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Confirmed {
    /// As the session connected to.
    pub target: Option<SessionPin>,
    /// As a jump host.
    pub jump_host: Option<JumpHostPin>,
}

impl Confirmed {
    /// What the user has confirmed of the session as a jump host, to show
    /// it: what was confirmed of it as one, or else as the session
    /// connected to, which holds as much.
    #[must_use]
    pub fn as_jump_host(&self) -> Option<JumpHostPin> {
        self.jump_host
            .clone()
            .or_else(|| self.target.as_ref().map(SessionPin::as_jump_host))
    }
}

/// Why a connect does not use what the user assigned to `session`, or `None`
/// where the session names what the user confirmed.
///
/// `confirmed` is what the user confirmed of the session. `role_known` is
/// whether the app knows which database role it connected as: without that
/// it takes no session for the user's own, and the dialog that confirms one
/// is not offered, so the message says what helps then.
///
/// A session used as a jump host passes if it names what was confirmed of
/// it as one, or what was confirmed of it as the session connected to. One
/// connected to passes only by the latter.
#[must_use]
pub fn refusal(
    confirmed: &Confirmed,
    session: &Session,
    used_as: UsedAs,
    role_known: bool,
) -> Option<String> {
    let what = match used_as {
        UsedAs::Target => "Session",
        UsedAs::JumpHost => "Jump host",
    };
    let name = name_for_message(&session.name);
    if !role_known {
        return Some(format!(
            "{what} \"{name}\": the app could not ask the database which role \
             it is connected as, so it cannot tell your own sessions from \
             other users', and uses no credential profile or login sequence \
             of yours for one. Restart ShellStation."
        ));
    }
    if let Some(why) = unconfirmable(session, used_as) {
        return Some(format!(
            "{what} \"{name}\" is not your own, and {why}. Such a session \
             cannot be confirmed, so your credential profile and login \
             sequence are not used for it. Its owner can set it right."
        ));
    }
    let changes = match used_as {
        UsedAs::Target => confirmed.target.as_ref().map(|pin| pin.changes(session)),
        UsedAs::JumpHost => {
            let as_one = confirmed.jump_host.as_ref().map(|pin| pin.changes(session));
            let as_target = confirmed
                .target
                .as_ref()
                .map(|pin| pin.as_jump_host().changes(session));
            match (as_one, as_target) {
                (Some(a), Some(b)) => Some(if b.is_empty() { b } else { a }),
                (a, b) => a.or(b),
            }
        }
    };
    let Some(changes) = changes else {
        return Some(format!(
            "{what} \"{name}\" is not your own, and you have not confirmed \
             what it connects to. Your credential profile and login sequence \
             are not used for it until you have: open My Login from the \
             context menu of the session you are connecting to, check what \
             it shows, and save."
        ));
    };
    if changes.is_empty() {
        return None;
    }
    Some(format!(
        "{what} \"{name}\" is not your own, and has been changed since you \
         confirmed what it connects to ({}). Your credential profile and \
         login sequence are not used for it until you have confirmed it \
         again: open My Login from the context menu of the session you are \
         connecting to, check what it shows, and save.",
        changes.join(", ")
    ))
}

/// How many characters a host takes as the dialog and a message show it:
/// one for each of printable ASCII, and for any other the length of its code
/// point written out, which is eight for most.
fn written_out_len(host: &str) -> usize {
    host.chars()
        .map(|c| {
            if c == ' ' || c.is_ascii_graphic() {
                1
            } else {
                let mut written = String::new();
                code_point_written(c, &mut written);
                written.len()
            }
        })
        .sum()
}

/// How much of a session's name a message quotes, in characters.
const MAX_NAME_CHARS: usize = 64;

fn code_point_written(c: char, out: &mut String) {
    use std::fmt::Write;
    let _ = write!(out, "\\u{{{:04X}}}", u32::from(c));
}

/// A session's name as another user wrote it, for a message that quotes it:
/// cut, and with the characters written out that would change how the
/// message displays or end the quotation. The dialog shows a name the same
/// way (`shownAsWritten` in `src/displayText.ts`).
pub(crate) fn name_for_message(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().take(MAX_NAME_CHARS) {
        if c == '"' || c.is_control() || crate::import::is_display_spoofing_char(c) {
            code_point_written(c, &mut out);
        } else {
            out.push(c);
        }
    }
    if name.chars().count() > MAX_NAME_CHARS {
        out.push('…');
    }
    out
}

/// A host, protocol or user name as another user wrote it, for a message:
/// cut, and in printable ASCII, with every other character written out and
/// a backslash of its own doubled, so that one that was written is told
/// from one that was not. A host in another script can look like one the
/// user trusts; the dialog shows these the same way (`hostAsWritten` and
/// `typedAsWritten` in `src/displayText.ts`).
fn ascii_for_message(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars().take(MAX_FIELD_CHARS) {
        if c == '\\' {
            out.push_str("\\\\");
        } else if (c == ' ' || c.is_ascii_graphic()) && c != '"' {
            out.push(c);
        } else {
            code_point_written(c, &mut out);
        }
    }
    if text.chars().count() > MAX_FIELD_CHARS {
        out.push('…');
    }
    out
}

/// The file the pins are kept in, as it is written.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PinFile {
    /// What was confirmed of a session as the one connected to.
    #[serde(default)]
    sessions: HashMap<Uuid, SessionPin>,
    /// What was confirmed of a session as a jump host.
    #[serde(default)]
    jump_hosts: HashMap<Uuid, JumpHostPin>,
}

impl PinFile {
    fn confirmed(&self, id: Uuid) -> Confirmed {
        Confirmed {
            target: self.sessions.get(&id).cloned(),
            jump_host: self.jump_hosts.get(&id).cloned(),
        }
    }
}

/// The largest pin file that is read. A pin is some 200 bytes, so this is
/// room for tens of thousands; a file beyond it is not one the app wrote.
const MAX_PIN_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// What reading the file came to.
enum Read {
    /// What it holds; nothing where there is no file, or where it is not a
    /// file the app wrote.
    Pins(PinFile),
    /// It is there and could not be read this time, as while another
    /// program holds it.
    Failed(String),
}

/// The pins, and the file they are kept in.
///
/// The file is read the first time a pin is asked for. One that is missing
/// holds no pins, and neither does one that is not what the app writes:
/// every session then has to be confirmed again, which asks more of the user
/// and gives nothing away. One that is there but cannot be read this time
/// holds none for now either, and is read again the next time; until it can
/// be read, nothing is confirmed, since writing the file then would drop
/// what it holds. It is written whole, to a new file that replaces the old
/// one, and for its owner alone.
pub struct LoginPinState {
    path: PathBuf,
    pins: Mutex<Option<PinFile>>,
}

impl LoginPinState {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            pins: Mutex::new(None),
        }
    }

    fn read(&self) -> Read {
        use std::io::Read as _;
        let file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Read::Pins(PinFile::default())
            }
            Err(e) => return Read::Failed(e.to_string()),
        };
        let mut contents = Vec::new();
        // One byte more than is taken, to tell a file of exactly that size
        // from a longer one.
        if let Err(e) = file.take(MAX_PIN_FILE_BYTES + 1).read_to_end(&mut contents) {
            return Read::Failed(e.to_string());
        }
        let parsed = if contents.len() as u64 > MAX_PIN_FILE_BYTES {
            Err("it is too large".to_string())
        } else {
            serde_json::from_slice::<PinFile>(&contents).map_err(|e| e.to_string())
        };
        Read::Pins(parsed.unwrap_or_else(|e| {
            tracing::warn!(
                "The file of confirmed sessions is not one the app wrote; sessions of other users have to be confirmed again: {e}"
            );
            PinFile::default()
        }))
    }

    /// Run `f` on the pins. Where the file could not be read this time, `f`
    /// gets none and `unread` says why; the file is read again next time.
    fn with_pins<T>(&self, f: impl FnOnce(&mut PinFile, Option<&str>) -> T) -> Result<T, String> {
        let mut pins = self
            .pins
            .lock()
            .map_err(|e| format!("Confirmed sessions lock poisoned: {e}"))?;
        if let Some(pins) = pins.as_mut() {
            return Ok(f(pins, None));
        }
        match self.read() {
            Read::Pins(read) => Ok(f(pins.insert(read), None)),
            Read::Failed(why) => {
                tracing::warn!(
                    "Could not read the file of confirmed sessions; nothing counts as confirmed until it can be read: {why}"
                );
                Ok(f(&mut PinFile::default(), Some(&why)))
            }
        }
    }

    /// What the user confirmed of a session.
    pub fn confirmed(&self, id: Uuid) -> Result<Confirmed, String> {
        self.with_pins(|pins, _| pins.confirmed(id))
    }

    /// Why a connect does not use what the user assigned to `session`; see
    /// [`refusal`].
    pub fn check(
        &self,
        session: &Session,
        used_as: UsedAs,
        role_known: bool,
    ) -> Result<(), String> {
        let (confirmed, unread) = self
            .with_pins(|pins, unread| (pins.confirmed(session.id), unread.map(str::to_string)))?;
        if let Some(why) = unread {
            // Nothing counts as confirmed, and the message says why, where
            // "you have not confirmed" would be wrong.
            return Err(format!(
                "What you have confirmed of other users' sessions could not \
                 be read just now ({why}), so your credential profile and \
                 login sequence are not used for one. Try again."
            ));
        }
        match refusal(&confirmed, session, used_as, role_known) {
            Some(why) => Err(why),
            None => Ok(()),
        }
    }

    /// Remember what the user confirmed, of one session as the one connected
    /// to and of its jump hosts as that, and write it down. Where it cannot
    /// be written, nothing is remembered either, so that what the app goes
    /// by is what the next start finds.
    pub fn confirm(
        &self,
        target: (Uuid, SessionPin),
        jump_hosts: Vec<(Uuid, JumpHostPin)>,
    ) -> Result<(), String> {
        self.with_pins(|pins, unread| {
            if let Some(why) = unread {
                return Err(format!(
                    "Nothing was saved: what you confirmed before could not be read ({why}). Try again."
                ));
            }
            let mut next = pins.clone();
            next.sessions.insert(target.0, target.1);
            next.jump_hosts.extend(jump_hosts);
            let contents = serde_json::to_vec_pretty(&next)
                .map_err(|e| format!("Failed to write down what was confirmed: {e}"))?;
            self.path
                .parent()
                .map_or(Ok(()), crate::private_fs::create_private_dir)
                .and_then(|()| crate::private_fs::write_private_file(&self.path, &contents))
                .map_err(|e| format!("Failed to write down what was confirmed: {e}"))?;
            *pins = next;
            Ok(())
        })?
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn session(name: &str) -> Session {
        Session {
            id: Uuid::new_v4(),
            folder_id: Uuid::new_v4(),
            name: name.to_string(),
            hostname: "switch.example".to_string(),
            port: 22,
            protocol: "ssh".to_string(),
            username: "admin".to_string(),
            auth_method: "password".to_string(),
            jump_host_id: None,
            tags: "[]".to_string(),
            icon: "desktop".to_string(),
            sort_order: 0,
            highlight_profile_id: None,
            credential_profile_id: None,
            legacy_algorithms: false,
            login_sequence_id: None,
            owner: "bob".to_string(),
            visibility: "shared".to_string(),
        }
    }

    /// A directory of a test's own, removed when the test is over.
    pub(crate) struct TestDir(pub(crate) PathBuf);

    impl TestDir {
        pub(crate) fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("ss-logins-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        /// The file of the pins, in a directory that is not there yet.
        pub(crate) fn file(&self) -> PathBuf {
            self.0.join("sub").join("session-logins.json")
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn store() -> (TestDir, LoginPinState) {
        let dir = TestDir::new();
        let pins = LoginPinState::new(dir.file());
        (dir, pins)
    }

    fn as_target(session: &Session) -> Confirmed {
        Confirmed {
            target: Some(SessionPin::of(session)),
            jump_host: None,
        }
    }

    fn as_jump_host(session: &Session) -> Confirmed {
        Confirmed {
            target: None,
            jump_host: Some(JumpHostPin::of(session)),
        }
    }

    #[test]
    fn only_another_users_session_with_something_of_the_users_needs_confirming() {
        // `SQLite`: every session is the user's own.
        assert!(!needs_confirmation(false, true, true));
        assert!(!needs_confirmation(false, false, true));
        // `PostgreSQL`: the user's own session names what the user had it name.
        assert!(!needs_confirmation(true, true, true));
        // Another user's, with nothing of the user's assigned: nothing of the
        // user's goes anywhere.
        assert!(!needs_confirmation(true, false, false));
        assert!(needs_confirmation(true, false, true));
    }

    #[test]
    fn a_session_that_names_what_was_confirmed_is_not_refused() {
        let s = session("core");
        assert_eq!(refusal(&as_target(&s), &s, UsedAs::Target, true), None);
        assert_eq!(refusal(&as_jump_host(&s), &s, UsedAs::JumpHost, true), None);
        // What a pin does not hold does not count: the name, the tags.
        let mut renamed = s.clone();
        renamed.name = "edge".to_string();
        renamed.tags = "[\"jumphost\"]".to_string();
        renamed.icon = "server".to_string();
        assert_eq!(
            refusal(&as_target(&s), &renamed, UsedAs::Target, true),
            None
        );
    }

    #[test]
    fn a_session_that_was_never_confirmed_is_refused() {
        let s = session("core");
        let why = refusal(&Confirmed::default(), &s, UsedAs::Target, true).unwrap();
        assert!(why.starts_with("Session \"core\" is not your own"), "{why}");
        assert!(why.contains("have not confirmed"), "{why}");
        assert!(why.contains("My Login"), "{why}");
        let why = refusal(&Confirmed::default(), &s, UsedAs::JumpHost, true).unwrap();
        assert!(
            why.starts_with("Jump host \"core\" is not your own"),
            "{why}"
        );
    }

    #[test]
    fn each_change_of_what_a_session_names_is_refused_and_named() {
        let s = session("core");
        let confirmed = as_target(&s);
        let changed = |change: fn(&mut Session)| {
            let mut s = s.clone();
            change(&mut s);
            refusal(&confirmed, &s, UsedAs::Target, true)
        };
        let why = changed(|s| s.hostname = "elsewhere.example".to_string()).unwrap();
        assert!(
            why.contains("has been changed since you confirmed"),
            "{why}"
        );
        assert!(
            why.contains("(host switch.example → elsewhere.example)"),
            "{why}"
        );
        let why = changed(|s| s.port = 2222).unwrap();
        assert!(why.contains("(port 22 → 2222)"), "{why}");
        let why = changed(|s| s.protocol = "telnet".to_string()).unwrap();
        assert!(why.contains("(protocol ssh → telnet)"), "{why}");
        let why = changed(|s| s.username = "root".to_string()).unwrap();
        assert!(why.contains("(user name \"admin\" → \"root\")"), "{why}");
        let why = changed(|s| s.legacy_algorithms = true).unwrap();
        assert!(why.contains("(legacy algorithms off → on)"), "{why}");
        let why = changed(|s| s.jump_host_id = Some(Uuid::new_v4())).unwrap();
        assert!(why.contains("(jump host)"), "{why}");
        // Several at once are all named.
        let why = changed(|s| {
            s.protocol = "telnet".to_string();
            s.hostname = "elsewhere.example".to_string();
            s.port = 23;
        })
        .unwrap();
        assert!(
            why.contains(
                "(protocol ssh → telnet, host switch.example → elsewhere.example, port 22 → 23)"
            ),
            "{why}"
        );
        // Taking a jump host away is a change as well: the host is then
        // looked up and connected to from the user's own machine.
        let mut via = s.clone();
        via.jump_host_id = Some(Uuid::new_v4());
        assert!(refusal(&as_target(&via), &s, UsedAs::Target, true).is_some());
    }

    #[test]
    fn the_switch_for_legacy_algorithms_counts_for_ssh_only() {
        let mut telnet = session("console");
        telnet.protocol = "telnet".to_string();
        let confirmed = as_target(&telnet);
        let mut switched = telnet.clone();
        switched.legacy_algorithms = true;
        assert_eq!(refusal(&confirmed, &switched, UsedAs::Target, true), None);
        // But not where the protocol changes with it.
        switched.protocol = "ssh".to_string();
        let why = refusal(&confirmed, &switched, UsedAs::Target, true).unwrap();
        assert!(why.contains("protocol telnet → ssh"), "{why}");
        assert!(why.contains("legacy algorithms off → on"), "{why}");
    }

    #[test]
    fn a_telnet_session_is_not_held_to_a_jump_host_it_does_not_go_through() {
        let mut telnet = session("console");
        telnet.protocol = "telnet".to_string();
        let confirmed = as_target(&telnet);
        let mut via = telnet.clone();
        via.jump_host_id = Some(Uuid::new_v4());
        assert_eq!(refusal(&confirmed, &via, UsedAs::Target, true), None);
        // An SSH session is, and so is one that becomes one.
        via.protocol = "ssh".to_string();
        let why = refusal(&confirmed, &via, UsedAs::Target, true).unwrap();
        assert!(why.contains("protocol telnet → ssh, jump host"), "{why}");
    }

    #[test]
    fn a_jump_host_is_held_to_its_host_its_port_and_where_it_goes_on() {
        let s = session("bastion");
        let confirmed = as_jump_host(&s);
        let changed = |change: fn(&mut Session)| {
            let mut s = s.clone();
            change(&mut s);
            refusal(&confirmed, &s, UsedAs::JumpHost, true)
        };
        assert!(changed(|s| s.hostname = "elsewhere.example".to_string()).is_some());
        assert!(changed(|s| s.port = 2222).is_some());
        assert!(changed(|s| s.jump_host_id = Some(Uuid::new_v4())).is_some());
        // Its own protocol, user name and switch are not what it is
        // connected to with.
        assert_eq!(changed(|s| s.protocol = "telnet".to_string()), None);
        assert_eq!(changed(|s| s.username = "root".to_string()), None);
        assert_eq!(changed(|s| s.legacy_algorithms = true), None);
    }

    #[test]
    fn what_was_confirmed_of_a_jump_host_does_not_confirm_it_as_a_session() {
        // The dialog shows less of a jump host than of the session connected
        // to. Its owner changes what it does not show, and the user saves the
        // dialog of a session that goes through it.
        let mut bastion = session("bastion");
        let was = as_target(&bastion);
        bastion.protocol = "telnet".to_string();
        bastion.legacy_algorithms = true;
        let both = Confirmed {
            jump_host: Some(JumpHostPin::of(&bastion)),
            ..was
        };
        assert_eq!(refusal(&both, &bastion, UsedAs::JumpHost, true), None);
        // Connected to by itself, it is still held to what the user saw of it.
        let why = refusal(&both, &bastion, UsedAs::Target, true).unwrap();
        assert!(why.contains("protocol ssh → telnet"), "{why}");
        // And one confirmed as a jump host only was never confirmed as more.
        let why = refusal(&as_jump_host(&bastion), &bastion, UsedAs::Target, true).unwrap();
        assert!(why.contains("have not confirmed"), "{why}");
    }

    #[test]
    fn a_session_confirmed_as_one_passes_as_a_jump_host() {
        // Whoever confirmed it as the session connected to has seen its
        // host, its port and where it goes on.
        let s = session("bastion");
        assert_eq!(refusal(&as_target(&s), &s, UsedAs::JumpHost, true), None);
        let mut moved = s.clone();
        moved.hostname = "elsewhere.example".to_string();
        let why = refusal(&as_target(&s), &moved, UsedAs::JumpHost, true).unwrap();
        assert!(
            why.contains("host switch.example → elsewhere.example"),
            "{why}"
        );
        // With both, either will do, and the message is of the jump host's.
        let both = Confirmed {
            target: Some(SessionPin::of(&s)),
            jump_host: Some(JumpHostPin::of(&moved)),
        };
        assert_eq!(refusal(&both, &s, UsedAs::JumpHost, true), None);
        assert_eq!(refusal(&both, &moved, UsedAs::JumpHost, true), None);
        let mut third = s.clone();
        third.hostname = "third.example".to_string();
        let why = refusal(&both, &third, UsedAs::JumpHost, true).unwrap();
        assert!(
            why.contains("host elsewhere.example → third.example"),
            "{why}"
        );
    }

    #[test]
    fn what_cannot_be_shown_for_what_it_is_cannot_be_confirmed() {
        let unconfirmable_as = |used_as, change: fn(&mut Session)| {
            let mut s = session("core");
            change(&mut s);
            (unconfirmable(&s, used_as), s)
        };
        // A protocol that is neither: a connect takes it for SSH.
        for protocol in ["SSH", "ssh2", "", "Telnet"] {
            let mut s = session("core");
            s.protocol = protocol.to_string();
            assert!(unconfirmable(&s, UsedAs::Target).is_some(), "{protocol}");
            // A jump host's own protocol is not used.
            assert_eq!(unconfirmable(&s, UsedAs::JumpHost), None, "{protocol}");
        }
        // A port a connect would cut down to another one.
        for used_as in [UsedAs::Target, UsedAs::JumpHost] {
            assert!(unconfirmable_as(used_as, |s| s.port = 65536 + 22)
                .0
                .is_some());
            assert!(unconfirmable_as(used_as, |s| s.port = 0).0.is_some());
            assert!(unconfirmable_as(used_as, |s| s.port = -1).0.is_some());
            assert_eq!(unconfirmable_as(used_as, |s| s.port = 65535).0, None);
            assert_eq!(unconfirmable_as(used_as, |s| s.port = 1).0, None);
            // A host that is shown cut, or that reads like a character
            // written out.
            assert!(unconfirmable_as(used_as, |s| s.hostname = "h".repeat(256))
                .0
                .is_some());
            assert_eq!(
                unconfirmable_as(used_as, |s| s.hostname = "h".repeat(255)).0,
                None
            );
            assert!(
                unconfirmable_as(used_as, |s| s.hostname = "a\\u{0430}".to_string())
                    .0
                    .is_some()
            );
            assert!(unconfirmable_as(used_as, |s| s.hostname = String::new())
                .0
                .is_some());
            // One in another script is shown written out, and can be confirmed.
            assert_eq!(
                unconfirmable_as(used_as, |s| s.hostname =
                    "b\u{430}stion.example".to_string())
                .0,
                None
            );
        }
        assert!(
            unconfirmable_as(UsedAs::Target, |s| s.username = "u".repeat(256))
                .0
                .is_some()
        );
        assert_eq!(
            unconfirmable_as(UsedAs::JumpHost, |s| s.username = "u".repeat(256)).0,
            None
        );
        // A host with a space in it, of which one at the end would not show,
        // or with a control character.
        for used_as in [UsedAs::Target, UsedAs::JumpHost] {
            for host in [
                "switch.example ",
                " switch.example",
                "a b",
                "a\tb",
                "a\nb",
                "a\u{a0}b",
            ] {
                let mut s = session("core");
                s.hostname = host.to_string();
                assert!(unconfirmable(&s, used_as).is_some(), "{host:?}");
            }
        }
        for used_as in [UsedAs::Target, UsedAs::JumpHost] {
            // A character that no host name has, an invisible one among
            // them: an SSH connect refuses these, a Telnet connect does not
            // ask.
            for host in [
                "a\u{200b}b.example",
                "a\u{ad}b.example",
                "a\u{fe0f}b",
                "x,example.com",
                "user@host",
                "host:22]",
                "a|b",
            ] {
                let mut s = session("core");
                s.hostname = host.to_string();
                assert_eq!(
                    unconfirmable(&s, used_as),
                    Some("its host has a character in it that no host name has"),
                    "{host:?}"
                );
            }
            // An address in brackets is a host.
            for host in ["[::1]", "[fe80::1%eth0]", "10.0.0.2", "db-1.corp.example."] {
                let mut s = session("core");
                s.hostname = host.to_string();
                assert_eq!(unconfirmable(&s, used_as), None, "{host:?}");
            }
            // One of many letters of another script takes eight characters
            // for each, and its end would be far from its start.
            let mut s = session("core");
            s.hostname = format!("{}.example", "\u{430}".repeat(30));
            assert_eq!(unconfirmable(&s, used_as), None);
            s.hostname = format!("{}.example", "\u{430}".repeat(31));
            assert_eq!(
                unconfirmable(&s, used_as),
                Some("its host takes more room written out than a host can")
            );
        }
        assert_eq!(written_out_len("a b~"), 4);
        assert_eq!(written_out_len("\u{430}"), 8);
        assert_eq!(written_out_len("\u{1f600}"), 9);
        // A user name that a login sequence would type keys with, or that
        // would not show whole.
        for user in ["ro\rot", "root\n", "\u{1b}[A", " root", "root "] {
            let mut s = session("core");
            s.username = user.to_string();
            assert!(unconfirmable(&s, UsedAs::Target).is_some(), "{user:?}");
            assert_eq!(unconfirmable(&s, UsedAs::JumpHost), None, "{user:?}");
        }
        for user in ["", "first last", "first.last@example.com"] {
            let mut s = session("core");
            s.username = user.to_string();
            assert_eq!(unconfirmable(&s, UsedAs::Target), None, "{user:?}");
        }
        // A user name may have a backslash, as a domain's has.
        assert_eq!(
            unconfirmable_as(UsedAs::Target, |s| s.username = "CORP\\admin".to_string()).0,
            None
        );

        // A connect refuses such a session whatever was confirmed of it.
        let (_, odd) = unconfirmable_as(UsedAs::Target, |s| s.port = 65536 + 22);
        let why = refusal(&as_target(&odd), &odd, UsedAs::Target, true).unwrap();
        assert!(why.contains("its port is none"), "{why}");
        assert!(why.contains("cannot be confirmed"), "{why}");
        assert!(why.contains("not used for it"), "{why}");
        assert!(!why.contains("My Login"), "{why}");
    }

    #[test]
    fn without_a_known_role_the_message_says_to_restart() {
        let s = session("core");
        for confirmed in [Confirmed::default(), as_target(&s)] {
            let why = refusal(&confirmed, &s, UsedAs::Target, false).unwrap();
            assert!(why.contains("Restart ShellStation"), "{why}");
            assert!(!why.contains("My Login"), "{why}");
        }
    }

    #[test]
    fn a_message_shows_what_another_user_wrote_as_written() {
        // A right-to-left override, a line break and a quotation mark in the
        // name, a Cyrillic letter and a zero-width space in the hosts.
        let mut s = session("co\u{202e}re\n\" is yours");
        let confirmed = as_target(&s);
        s.hostname = "b\u{430}stion.example".to_string();
        s.username = "ro\"ot".to_string();
        let why = refusal(&confirmed, &s, UsedAs::Target, true).unwrap();
        assert!(
            why.starts_with("Session \"co\\u{202E}re\\u{000A}\\u{0022} is yours\""),
            "{why}"
        );
        assert!(
            why.contains("host switch.example → b\\u{0430}stion.example"),
            "{why}"
        );
        assert!(
            why.contains("user name \"admin\" → \"ro\\u{0022}ot\""),
            "{why}"
        );
        assert!(!why.contains('\u{202e}') && !why.contains('\n'), "{why}");
        // And no more of either than a message has room for.
        assert_eq!(
            name_for_message(&"n".repeat(65)),
            format!("{}…", "n".repeat(64))
        );
        assert_eq!(name_for_message(&"n".repeat(64)), "n".repeat(64));
        assert_eq!(
            ascii_for_message(&"h".repeat(256)),
            format!("{}…", "h".repeat(255))
        );
        // A name can be in any script.
        assert_eq!(name_for_message("Büro サーバー"), "Büro サーバー");
        assert_eq!(ascii_for_message("a b~"), "a b~");
        assert_eq!(ascii_for_message("a\u{200b}b"), "a\\u{200B}b");
        // A backslash that was written is told from one that was not.
        assert_eq!(ascii_for_message("CORP\\admin"), "CORP\\\\admin");
        assert_eq!(ascii_for_message("\\u{0430}"), "\\\\u{0430}");
        assert_eq!(ascii_for_message("\u{430}"), "\\u{0430}");
        assert_eq!(ascii_for_message("ü\t"), "\\u{00FC}\\u{0009}");
    }

    #[test]
    fn what_was_confirmed_is_remembered_across_a_restart() {
        let (dir, pins) = store();
        let core = session("core");
        let mut bastion = session("bastion");
        bastion.hostname = "bastion.example".to_string();
        let other = session("other");

        // Nothing is confirmed where there is no file.
        assert_eq!(pins.confirmed(core.id).unwrap(), Confirmed::default());
        assert!(pins.check(&core, UsedAs::Target, true).is_err());

        pins.confirm(
            (core.id, SessionPin::of(&core)),
            vec![(bastion.id, JumpHostPin::of(&bastion))],
        )
        .unwrap();
        assert!(pins.check(&core, UsedAs::Target, true).is_ok());
        assert!(pins.check(&bastion, UsedAs::JumpHost, true).is_ok());
        // Each for what it was confirmed as.
        assert!(pins.check(&bastion, UsedAs::Target, true).is_err());
        assert!(pins.check(&other, UsedAs::Target, true).is_err());

        // Another instance finds the same in the file.
        let again = LoginPinState::new(dir.file());
        assert_eq!(again.confirmed(core.id).unwrap(), as_target(&core));
        assert_eq!(again.confirmed(bastion.id).unwrap(), as_jump_host(&bastion));
        assert_eq!(again.confirmed(other.id).unwrap(), Confirmed::default());
        // Confirming again replaces what was confirmed, and keeps the rest.
        let mut moved = core.clone();
        moved.hostname = "elsewhere.example".to_string();
        assert!(again.check(&moved, UsedAs::Target, true).is_err());
        again
            .confirm((moved.id, SessionPin::of(&moved)), Vec::new())
            .unwrap();
        assert!(again.check(&moved, UsedAs::Target, true).is_ok());
        assert!(again.check(&core, UsedAs::Target, true).is_err());
        assert!(again.check(&bastion, UsedAs::JumpHost, true).is_ok());
    }

    #[test]
    fn a_file_the_app_did_not_write_holds_nothing_confirmed() {
        let (dir, pins) = store();
        let core = session("core");
        pins.confirm((core.id, SessionPin::of(&core)), Vec::new())
            .unwrap();
        let path = dir.file();

        for contents in ["not json", "[]", "{\"sessions\": {\"no-uuid\": {}}}", ""] {
            std::fs::write(&path, contents).unwrap();
            let pins = LoginPinState::new(path.clone());
            assert_eq!(
                pins.confirmed(core.id).unwrap(),
                Confirmed::default(),
                "{contents}"
            );
            assert!(pins.check(&core, UsedAs::Target, true).is_err());
            // It is written anew by the next confirmation.
            pins.confirm((core.id, SessionPin::of(&core)), Vec::new())
                .unwrap();
            let pins = LoginPinState::new(path.clone());
            assert!(pins.check(&core, UsedAs::Target, true).is_ok());
        }
        // A file without the lists holds none, and is no error; nor is one
        // from before jump hosts were kept apart.
        std::fs::write(&path, "{}").unwrap();
        let pins = LoginPinState::new(path.clone());
        assert_eq!(pins.confirmed(core.id).unwrap(), Confirmed::default());
        // One that is larger than the app would write.
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_PIN_FILE_BYTES + 1).unwrap();
        drop(file);
        let pins = LoginPinState::new(path);
        assert_eq!(pins.confirmed(core.id).unwrap(), Confirmed::default());
    }

    #[test]
    fn a_file_that_cannot_be_read_for_now_is_not_written_over() {
        let dir = TestDir::new();
        // A directory where the file should be: it is there, and reading it
        // fails, as reading a file does that another program holds.
        let path = dir.0.join("session-logins.json");
        std::fs::create_dir(&path).unwrap();
        let pins = LoginPinState::new(path.clone());
        let core = session("core");
        assert_eq!(pins.confirmed(core.id).unwrap(), Confirmed::default());
        // A connect is refused, and told that it is for now.
        let error = pins.check(&core, UsedAs::Target, true).unwrap_err();
        assert!(error.contains("could not be read just now"), "{error}");
        assert!(!error.contains("have not confirmed"), "{error}");
        let error = pins
            .confirm((core.id, SessionPin::of(&core)), Vec::new())
            .unwrap_err();
        assert!(error.starts_with("Nothing was saved"), "{error}");
        assert!(error.contains("could not be read"), "{error}");
        assert!(path.is_dir());

        // Once it can be read again, the same instance reads it: what it
        // holds counts, and is kept when more is confirmed.
        std::fs::remove_dir(&path).unwrap();
        let other = session("other");
        LoginPinState::new(path.clone())
            .confirm((other.id, SessionPin::of(&other)), Vec::new())
            .unwrap();
        assert!(pins.check(&other, UsedAs::Target, true).is_ok());
        pins.confirm((core.id, SessionPin::of(&core)), Vec::new())
            .unwrap();
        let again = LoginPinState::new(path);
        assert!(again.check(&other, UsedAs::Target, true).is_ok());
        assert!(again.check(&core, UsedAs::Target, true).is_ok());
    }

    #[test]
    fn what_cannot_be_written_down_is_not_remembered() {
        let (dir, pins) = store();
        let core = session("core");
        let other = session("other");
        pins.confirm((core.id, SessionPin::of(&core)), Vec::new())
            .unwrap();
        // The file's directory is replaced by a file, so nothing can be
        // written into it.
        std::fs::remove_dir_all(dir.0.join("sub")).unwrap();
        std::fs::write(dir.0.join("sub"), "in the way").unwrap();
        let error = pins
            .confirm((other.id, SessionPin::of(&other)), Vec::new())
            .unwrap_err();
        assert!(
            error.starts_with("Failed to write down what was confirmed"),
            "{error}"
        );
        assert!(pins.check(&other, UsedAs::Target, true).is_err());
        // What was confirmed before is still remembered.
        assert!(pins.check(&core, UsedAs::Target, true).is_ok());
    }
}
