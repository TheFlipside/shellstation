//! `SecureCRT` `XML` session-store parser.
//!
//! `SecureCRT` stores sessions as a recursive `<key>` tree where a key is
//! a folder iff it has child `<key>` elements, and a session iff it has
//! a `<dword name="Is Session">1</dword>` plus a hostname. The same
//! `<key>` opening tag can be either kind, and the only way to know is
//! to see what comes next — so the parser commits to a folder
//! interpretation on the *first* child key it encounters, otherwise
//! collects properties until the closing tag and treats the key as a
//! session if `Is Session` was set.
//!
//! That look-ahead pattern is why `parse_key_block` is large: the
//! branchiness reflects a genuinely ambiguous grammar, not avoidable
//! accidental complexity. Each branch is exercised by a fixture in the
//! test module at the bottom of this file.

use quick_xml::events::Event;
use quick_xml::Reader;

use super::{
    check_parsed_counts, dropped_sessions_note, ImportedFolder, ImportedSession, ParseResult,
    SkippedSession,
};

/// Maximum allowed folder nesting depth to prevent stack exhaustion from
/// pathologically deep `XML` structures.
const MAX_NESTING_DEPTH: usize = 100;

/// Names of `SecureCRT`'s built-in session templates. They sit directly under
/// `Sessions`, and are skipped there only: further down, the same name is a
/// user's own folder or session.
const SKIP_SESSIONS: &[&str] = &[
    "Default",
    "Default_LocalShell",
    "Default_RDP",
    "Default_Serial",
];

/// Parse a `SecureCRT` `XML` export into folders and sessions.
///
/// `SecureCRT` uses a nested `<key>` structure under `<key name="Sessions">`.
/// Each key is either a folder (contains child keys) or a session
/// (has `<dword name="Is Session">1</dword>`).
pub fn parse(xml: &str) -> Result<ParseResult, String> {
    let mut reader = Reader::from_str(xml);
    // Already the default; the parse relies on every end tag matching its
    // start tag to tell a whole file from a truncated one.
    reader.config_mut().check_end_names = true;

    // Navigate to the <key name="Sessions"> block.
    if !find_sessions_key(&mut reader)? {
        return Err("No <key name=\"Sessions\"> block found in SecureCRT XML".to_string());
    }

    let mut folders = Vec::new();
    let mut sessions = Vec::new();
    let mut skipped = Vec::new();
    let mut warnings = Vec::new();

    let root_temp_id: usize = 0;
    let mut next_temp_id: usize = 1;

    // Parse the children of the Sessions key.
    parse_key_children(
        &mut reader,
        root_temp_id,
        &mut next_temp_id,
        &mut folders,
        &mut sessions,
        &mut skipped,
        &mut warnings,
        0,
    )?;

    Ok((folders, sessions, skipped, warnings))
}

/// Advance the reader past `<key name="Sessions">`, leaving it positioned
/// at the first child element. Returns `false` if the block is not found.
///
/// If the `XML` contains multiple `<key name="Sessions">` blocks only the
/// first one is processed; `SecureCRT` exports are expected to have exactly one.
fn find_sessions_key(reader: &mut Reader<&[u8]>) -> Result<bool, String> {
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) if e.local_name().as_ref() == b"key" => {
                if get_attr(e, "name").as_deref() == Some("Sessions") {
                    return Ok(true);
                }
                // Not the Sessions key — skip its entire subtree.
                skip_through_end(reader, b"key")?;
            }
            Ok(Event::Eof) => return Ok(false),
            Err(e) => {
                return Err(format!(
                    "XML parse error at position {}: {e}",
                    reader.error_position()
                ));
            }
            _ => {}
        }
    }
}

/// Parse immediate `<key>` children of the current block until the
/// matching `</key>` end tag is reached.
// Mutable output vectors must be threaded through the recursive descent.
#[allow(clippy::too_many_arguments)]
fn parse_key_children(
    reader: &mut Reader<&[u8]>,
    parent_temp_id: usize,
    next_temp_id: &mut usize,
    folders: &mut Vec<ImportedFolder>,
    sessions: &mut Vec<ImportedSession>,
    skipped: &mut Vec<SkippedSession>,
    warnings: &mut Vec<String>,
    depth: usize,
) -> Result<(), String> {
    loop {
        check_parsed_counts(folders.len(), sessions.len(), skipped.len(), warnings.len())?;
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                if e.local_name().as_ref() == b"key" {
                    if let Some(key_name) = get_attr(e, "name") {
                        parse_key_block(
                            reader,
                            &key_name,
                            parent_temp_id,
                            next_temp_id,
                            folders,
                            sessions,
                            skipped,
                            warnings,
                            depth,
                        )?;
                    } else {
                        let dropped = skip_dropped_key(reader, skipped, depth)?;
                        warnings.push(format!(
                            "Skipped <key> element without name attribute{}",
                            dropped_sessions_note(dropped)
                        ));
                    }
                } else {
                    // Non-key element at this level (shouldn't happen, but be safe).
                    skip_through_end(reader, e.local_name().as_ref())?;
                }
            }
            Ok(Event::End(_)) => break,
            // quick-xml does not report elements left open at the end, and a
            // file cut off before `</key>` must not import as a partial one.
            Ok(Event::Eof) => return Err("Unexpected end of XML".to_string()),
            Err(e) => {
                return Err(format!(
                    "XML parse error at position {}: {e}",
                    reader.error_position()
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Parse a single `<key name="...">...</key>` block.
///
/// Collects typed properties and processes child `<key>` blocks recursively.
/// After reading all children, determines whether this block is a session
/// or a folder based on the `Is Session` property.
// Mutable output vectors must be threaded through the recursive descent.
#[allow(clippy::too_many_arguments)]
fn parse_key_block(
    reader: &mut Reader<&[u8]>,
    key_name: &str,
    parent_temp_id: usize,
    next_temp_id: &mut usize,
    folders: &mut Vec<ImportedFolder>,
    sessions: &mut Vec<ImportedSession>,
    skipped: &mut Vec<SkippedSession>,
    warnings: &mut Vec<String>,
    depth: usize,
) -> Result<(), String> {
    check_depth(depth)?;

    // Allocate a temp_id for this block (used if it turns out to be a folder).
    let my_temp_id = *next_temp_id;
    *next_temp_id += 1;

    let mut props = SessionProps::default();
    let mut has_child_keys = false;

    loop {
        check_parsed_counts(folders.len(), sessions.len(), skipped.len(), warnings.len())?;
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let local = e.local_name();
                match local.as_ref() {
                    b"key" => {
                        // On the first child <key>, this block is definitively a
                        // folder — register it before recursing so children can
                        // resolve their parent temp_id during persistence
                        // (folders are created in vector order).
                        if !has_child_keys {
                            folders.push(ImportedFolder {
                                temp_id: my_temp_id,
                                name: key_name.to_string(),
                                parent_temp_id: Some(parent_temp_id),
                            });
                        }
                        has_child_keys = true;
                        if let Some(child_name) = get_attr(e, "name") {
                            parse_key_block(
                                reader,
                                &child_name,
                                my_temp_id,
                                next_temp_id,
                                folders,
                                sessions,
                                skipped,
                                warnings,
                                depth + 1,
                            )?;
                        } else {
                            let dropped = skip_dropped_key(reader, skipped, depth + 1)?;
                            warnings.push(format!(
                                "In \"{key_name}\": skipped <key> element without name \
                                 attribute{}",
                                dropped_sessions_note(dropped)
                            ));
                        }
                    }
                    b"string" => {
                        let Some(attr_name) = get_attr(e, "name") else {
                            warnings.push(format!(
                                "In \"{key_name}\": ignored <string> element without name attribute"
                            ));
                            skip_through_end(reader, b"string")?;
                            continue;
                        };
                        let value = read_text(reader, b"string")?;
                        props.set_string(&attr_name, &value);
                    }
                    b"dword" => {
                        let Some(attr_name) = get_attr(e, "name") else {
                            skip_through_end(reader, b"dword")?;
                            continue;
                        };
                        let value = read_text(reader, b"dword")?;
                        props.set_dword(&attr_name, &value);
                    }
                    _ => {
                        skip_through_end(reader, local.as_ref())?;
                    }
                }
            }
            Ok(Event::Empty(ref e)) => {
                let local = e.local_name();
                if local.as_ref() == b"string" {
                    if let Some(attr_name) = get_attr(e, "name") {
                        props.set_string(&attr_name, "");
                    } else {
                        warnings.push(format!(
                            "In \"{key_name}\": ignored empty <string> element without name attribute"
                        ));
                    }
                }
            }
            Ok(Event::End(ref e)) if e.local_name().as_ref() == b"key" => {
                break;
            }
            Ok(Event::Eof) => return Err("Unexpected end of XML".to_string()),
            Err(e) => {
                return Err(format!(
                    "XML parse error at position {}: {e}",
                    reader.error_position()
                ));
            }
            _ => {}
        }
    }

    // Decide: session or folder. Folders with children were already pushed
    // eagerly above, under any name: a key with child keys is imported as a
    // folder even directly under `Sessions` with a template's name, and only
    // the template's own session settings are dropped. An empty key with no
    // children and no Is Session flag is silently dropped (matches
    // SecureCRT's own "empty placeholder" behavior), and so is a template.
    let is_template = depth == 0 && SKIP_SESSIONS.contains(&key_name);
    if props.is_session && !is_template {
        match build_session(key_name, &props, parent_temp_id) {
            Ok(s) => sessions.push(s),
            Err(w) => {
                warnings.push(w);
                skipped.push(SkippedSession {
                    name: key_name.to_string(),
                    folder_temp_id: Some(parent_temp_id),
                });
            }
        }
    }

    Ok(())
}

/// Fail once a `<key>` sits `MAX_NESTING_DEPTH` levels below `Sessions`.
fn check_depth(depth: usize) -> Result<(), String> {
    if depth >= MAX_NESTING_DEPTH {
        return Err(format!(
            "Folder nesting exceeds maximum depth of {MAX_NESTING_DEPTH}"
        ));
    }
    Ok(())
}

/// Advance the reader past a `<key>` block, at `depth`, that is dropped with
/// everything in it, and record each session in it as skipped: a jump host
/// reference that also names a session elsewhere then stays ambiguous instead
/// of linking that one as if it were unique. The block's folders are not
/// imported, so its sessions have no folder path. Returns how many sessions
/// the block held.
fn skip_dropped_key(
    reader: &mut Reader<&[u8]>,
    skipped: &mut Vec<SkippedSession>,
    depth: usize,
) -> Result<usize, String> {
    check_depth(depth)?;
    let before = skipped.len();
    // The name and properties of each open `<key>`, starting with the dropped
    // one, which has no name.
    let mut open = vec![(None, SessionProps::default())];
    loop {
        // Only `skipped` grows here; the caller checks the other counts.
        check_parsed_counts(0, 0, skipped.len(), 0)?;
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let local = e.local_name();
                match local.as_ref() {
                    b"key" => {
                        check_depth(depth + open.len())?;
                        open.push((get_attr(e, "name"), SessionProps::default()));
                    }
                    b"dword" => {
                        let attr_name = get_attr(e, "name");
                        let value = read_text(reader, b"dword")?;
                        if let (Some(attr_name), Some((_, props))) = (attr_name, open.last_mut()) {
                            props.set_dword(&attr_name, &value);
                        }
                    }
                    _ => skip_through_end(reader, local.as_ref())?,
                }
            }
            Ok(Event::End(ref e)) if e.local_name().as_ref() == b"key" => {
                if let Some((Some(name), props)) = open.pop() {
                    if props.is_session {
                        skipped.push(SkippedSession {
                            name,
                            folder_temp_id: None,
                        });
                    }
                }
                if open.is_empty() {
                    return Ok(skipped.len() - before);
                }
            }
            Ok(Event::Eof) => return Err("Unexpected end of XML".to_string()),
            Err(e) => {
                return Err(format!(
                    "XML parse error at position {}: {e}",
                    reader.error_position()
                ));
            }
            _ => {}
        }
    }
}

/// Collected session properties from `SecureCRT` `XML`.
#[derive(Default)]
struct SessionProps {
    is_session: bool,
    protocol: String,
    hostname: String,
    ssh_port: Option<i32>,
    telnet_port: Option<i32>,
    username: String,
    firewall_name: String,
}

impl SessionProps {
    fn set_string(&mut self, name: &str, value: &str) {
        match name {
            "Protocol Name" => self.protocol = value.to_string(),
            "Hostname" => self.hostname = value.to_string(),
            "Username" => self.username = value.to_string(),
            "Firewall Name" => self.firewall_name = value.to_string(),
            _ => {}
        }
    }

    fn set_dword(&mut self, name: &str, value: &str) {
        // Only parse the handful of dwords we actually care about. SecureCRT
        // emits dozens of others (e.g. "Printer Quality" = 0xFFFFFFFD) that
        // would overflow i32 — ignoring them avoids log spam on import.
        // Parse as i64 so legitimate u32-range values still fit.
        let Ok(parsed) = value.parse::<i64>() else {
            return;
        };
        match name {
            "Is Session" => self.is_session = parsed == 1,
            "[SSH2] Port" => self.ssh_port = i32::try_from(parsed).ok(),
            "Port" => self.telnet_port = i32::try_from(parsed).ok(),
            _ => {}
        }
    }
}

/// Map `SecureCRT` protocol names to `ShellStation` protocols.
fn map_protocol(protocol: &str) -> Option<&'static str> {
    match protocol {
        "SSH2" | "SSH1" => Some("ssh"),
        "Telnet" => Some("telnet"),
        _ => None,
    }
}

/// Build an `ImportedSession` from collected properties.
fn build_session(
    name: &str,
    props: &SessionProps,
    folder_temp_id: usize,
) -> Result<ImportedSession, String> {
    let Some(protocol) = map_protocol(&props.protocol) else {
        return Err(format!(
            "Skipped \"{name}\" — unsupported protocol: {}",
            props.protocol
        ));
    };

    if props.hostname.is_empty() {
        return Err(format!("Skipped \"{name}\" — no hostname"));
    }

    let port = if protocol == "ssh" {
        props.ssh_port.unwrap_or(22)
    } else {
        props.telnet_port.unwrap_or(23)
    };

    // SecureCRT stores jump hosts as "Firewall Name" = "Session:<name>".
    let jump_host_name = props
        .firewall_name
        .strip_prefix("Session:")
        .map(str::to_string);

    Ok(ImportedSession {
        name: name.to_string(),
        folder_temp_id,
        hostname: props.hostname.clone(),
        port,
        protocol: protocol.to_string(),
        username: if props.username.is_empty() {
            None
        } else {
            Some(props.username.clone())
        },
        jump_host_name,
    })
}

/// Read text content until the matching end tag.
fn read_text(reader: &mut Reader<&[u8]>, end_tag: &[u8]) -> Result<String, String> {
    let mut text = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Text(ref e)) => {
                // SecureCRT exports declare `<?xml version="1.0"?>`, so we apply
                // XML 1.0 EOL normalization (CR/CRLF -> LF). This is a deliberate
                // change from quick-xml 0.39.2's `xml_content()`, which was an
                // alias for the XML 1.1 path that *also* collapses U+0085 (NEL)
                // and U+2028 (LS) to LF. Those code points have no business in
                // SecureCRT session text and never appear in real exports; using
                // XML 1.0 rules matches what the file's own version declaration
                // says it should be parsed under.
                text.push_str(
                    e.xml_content(quick_xml::XmlVersion::Implicit1_0)
                        .map_err(|err| format!("Failed to decode text: {err}"))?
                        .as_ref(),
                );
            }
            Ok(Event::GeneralRef(ref e)) => {
                // quick-xml reports `&amp;`, `&#38;` and the like as separate
                // events between the text chunks around them; ignoring them
                // turned "dom&amp;user" into "domuser". SecureCRT declares no
                // DTD, so only character references and the five predefined
                // entities are well-formed here. Anything else is kept
                // verbatim rather than silently dropped.
                let name = e
                    .decode()
                    .map_err(|err| format!("Failed to decode entity reference: {err}"))?;
                if let Ok(Some(ch)) = e.resolve_char_ref() {
                    text.push(ch);
                } else if let Some(resolved) = quick_xml::escape::resolve_xml_entity(&name) {
                    text.push_str(resolved);
                } else {
                    text.push('&');
                    text.push_str(&name);
                    text.push(';');
                }
            }
            Ok(Event::End(ref e)) if e.local_name().as_ref() == end_tag => return Ok(text),
            Ok(Event::Eof) => return Err("Unexpected end of XML reading text".to_string()),
            Err(e) => {
                return Err(format!(
                    "XML parse error at position {}: {e}",
                    reader.error_position()
                ));
            }
            _ => {}
        }
    }
}

/// Advance the reader through and past the matching end tag, consuming
/// any nested elements with the same tag name along the way.
fn skip_through_end(reader: &mut Reader<&[u8]>, tag: &[u8]) -> Result<(), String> {
    let mut depth = 1u32;
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) if e.local_name().as_ref() == tag => depth += 1,
            Ok(Event::End(ref e)) if e.local_name().as_ref() == tag => {
                depth -= 1;
                if depth == 0 {
                    return Ok(());
                }
            }
            Ok(Event::Eof) => return Err("Unexpected end of XML".to_string()),
            Err(e) => {
                return Err(format!(
                    "XML parse error at position {}: {e}",
                    reader.error_position()
                ));
            }
            _ => {}
        }
    }
}

/// Get an attribute value by name from a `BytesStart` element.
fn get_attr(e: &quick_xml::events::BytesStart<'_>, name: &str) -> Option<String> {
    for attr in e.attributes().flatten() {
        if attr.key.as_ref() == name.as_bytes() {
            return attr
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .ok()
                .map(|v| v.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::MAX_IMPORT_SESSIONS;

    // The SecureCRT parser walks a recursive `<key>` tree where a key is a
    // folder iff it has child keys, and a session iff it has a
    // `<dword name="Is Session">1</dword>` flag plus a `<string name="Hostname">`.
    // The folder-vs-session disambiguation depends on which event we see
    // *first* — child key, or property — and the function commits to the
    // folder interpretation on the first child key. Each branch of that
    // decision tree (built-in sessions skipped, missing hostname, telnet
    // vs SSH ports, jump-host "Session:" prefix) gets a fixture.

    fn wrap(inner: &str) -> String {
        // Minimal SecureCRT-shaped envelope around `<key name="Sessions">`.
        format!(
            r#"<?xml version="1.0"?>
            <VanDyke version="3.0">
                <key name="Sessions">{inner}</key>
            </VanDyke>"#
        )
    }

    #[test]
    fn parse_requires_sessions_key() {
        // Without a `<key name="Sessions">` we cannot determine where
        // sessions live — must error rather than silently return empty.
        let xml = r#"<?xml version="1.0"?><VanDyke version="3.0"/>"#;
        let err = parse(xml).unwrap_err();
        assert!(err.contains("Sessions"));
    }

    #[test]
    fn parse_rejects_a_truncated_file() {
        // A file cut off part way must not import the part before the cut
        // as if it were the whole export.
        let session = r#"<key name="a">
                <dword name="Is Session">1</dword>
                <string name="Hostname">h</string>
                <string name="Protocol Name">SSH2</string>
            </key>"#;
        let head = r#"<?xml version="1.0"?><VanDyke version="3.0"><key name="Sessions">"#;
        for cut in [
            format!(r#"{head}<key name="EU">{session}"#),
            format!("{head}{session}"),
            format!(r#"{head}<key name="b"><dword name="Is Session">1</dword>"#),
        ] {
            assert_eq!(parse(&cut).unwrap_err(), "Unexpected end of XML", "{cut}");
        }
        // Only what follows the `Sessions` block is missing.
        assert!(parse(&format!("{head}{session}</key>")).is_ok());
    }

    #[test]
    fn parse_empty_sessions() {
        let (folders, sessions, _, warnings) = parse(&wrap("")).unwrap();
        assert!(folders.is_empty());
        assert!(sessions.is_empty());
        assert_eq!(warnings, Vec::<String>::new());
    }

    #[test]
    fn parse_ssh_session_minimal() {
        let xml = wrap(
            r#"
            <key name="web1">
                <dword name="Is Session">1</dword>
                <string name="Hostname">web1.example.com</string>
                <string name="Protocol Name">SSH2</string>
                <dword name="[SSH2] Port">22</dword>
            </key>"#,
        );
        let (folders, sessions, _, warnings) = parse(&xml).unwrap();
        assert!(folders.is_empty());
        assert_eq!(sessions.len(), 1);
        assert_eq!(warnings, Vec::<String>::new());
        let s = &sessions[0];
        assert_eq!(s.name, "web1");
        assert_eq!(s.hostname, "web1.example.com");
        assert_eq!(s.port, 22);
        assert_eq!(s.protocol, "ssh");
    }

    #[test]
    fn parse_telnet_session_uses_telnet_port() {
        let xml = wrap(
            r#"
            <key name="switch">
                <dword name="Is Session">1</dword>
                <string name="Hostname">10.0.0.1</string>
                <string name="Protocol Name">Telnet</string>
                <dword name="Port">2323</dword>
            </key>"#,
        );
        let (_, sessions, _, _) = parse(&xml).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].protocol, "telnet");
        assert_eq!(sessions[0].port, 2323);
    }

    #[test]
    fn parse_session_defaults_port() {
        // Omitted [SSH2] Port defaults to 22; omitted telnet Port defaults to 23.
        let ssh = wrap(
            r#"
            <key name="s">
                <dword name="Is Session">1</dword>
                <string name="Hostname">h</string>
                <string name="Protocol Name">SSH2</string>
            </key>"#,
        );
        assert_eq!(parse(&ssh).unwrap().1[0].port, 22);

        let telnet = wrap(
            r#"
            <key name="s">
                <dword name="Is Session">1</dword>
                <string name="Hostname">h</string>
                <string name="Protocol Name">Telnet</string>
            </key>"#,
        );
        assert_eq!(parse(&telnet).unwrap().1[0].port, 23);
    }

    #[test]
    fn parse_session_missing_hostname_warns() {
        let xml = wrap(
            r#"
            <key name="broken">
                <dword name="Is Session">1</dword>
                <string name="Protocol Name">SSH2</string>
            </key>"#,
        );
        let (_, sessions, skipped, warnings) = parse(&xml).unwrap();
        assert!(sessions.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("no hostname"));
        // Still reported by name, so a jump host reference to "broken" counts it.
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].name, "broken");
    }

    #[test]
    fn parse_unsupported_protocol_warns() {
        let xml = wrap(
            r#"
            <key name="Windows">
                <key name="rdp1">
                    <dword name="Is Session">1</dword>
                    <string name="Hostname">srv</string>
                    <string name="Protocol Name">RDP</string>
                </key>
            </key>"#,
        );
        let (folders, sessions, skipped, warnings) = parse(&xml).unwrap();
        assert!(sessions.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].to_lowercase().contains("rdp"));
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].name, "rdp1");
        assert_eq!(skipped[0].folder_temp_id, Some(folders[0].temp_id));
    }

    #[test]
    fn parse_skips_builtin_default_sessions() {
        // The "Default" key would otherwise look like a real session; it must
        // be silently dropped so the user's session tree isn't polluted.
        let xml = wrap(
            r#"
            <key name="Default">
                <dword name="Is Session">1</dword>
                <string name="Hostname">should-not-appear</string>
                <string name="Protocol Name">SSH2</string>
            </key>
            <key name="Default_RDP">
                <dword name="Is Session">1</dword>
                <string name="Protocol Name">RDP</string>
            </key>
            <key name="real">
                <dword name="Is Session">1</dword>
                <string name="Hostname">real.example.com</string>
                <string name="Protocol Name">SSH2</string>
            </key>"#,
        );
        let (_, sessions, skipped, warnings) = parse(&xml).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, "real");
        // A template is no session a jump host can name, and no user error.
        assert!(skipped.is_empty());
        assert_eq!(warnings, Vec::<String>::new());
    }

    #[test]
    fn parse_imports_template_names_below_the_top_level() {
        // Only the keys directly under `Sessions` are templates; the same
        // name further down is the user's own folder or session.
        let xml = wrap(
            r#"
            <key name="Lab">
                <key name="Default">
                    <key name="bastion">
                        <dword name="Is Session">1</dword>
                        <string name="Hostname">bastion.lab</string>
                        <string name="Protocol Name">SSH2</string>
                    </key>
                </key>
                <key name="Default_Serial">
                    <dword name="Is Session">1</dword>
                    <string name="Hostname">console.lab</string>
                    <string name="Protocol Name">SSH2</string>
                </key>
            </key>"#,
        );
        let (folders, sessions, skipped, warnings) = parse(&xml).unwrap();
        let folder_names: Vec<_> = folders.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(folder_names, ["Lab", "Default"]);
        let session_names: Vec<_> = sessions.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(session_names, ["bastion", "Default_Serial"]);
        assert_eq!(sessions[0].folder_temp_id, folders[1].temp_id);
        assert_eq!(sessions[1].folder_temp_id, folders[0].temp_id);
        assert!(skipped.is_empty());
        assert_eq!(warnings, Vec::<String>::new());
    }

    #[test]
    fn parse_imports_a_top_level_template_name_with_child_keys_as_a_folder() {
        // A key with child keys is a folder under any name; only the
        // template's own session settings are dropped.
        let xml = wrap(
            r#"
            <key name="Default">
                <dword name="Is Session">1</dword>
                <string name="Hostname">template</string>
                <string name="Protocol Name">SSH2</string>
                <key name="bastion">
                    <dword name="Is Session">1</dword>
                    <string name="Hostname">bastion.example.com</string>
                    <string name="Protocol Name">SSH2</string>
                </key>
            </key>"#,
        );
        let (folders, sessions, skipped, warnings) = parse(&xml).unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].name, "Default");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, "bastion");
        assert_eq!(sessions[0].folder_temp_id, folders[0].temp_id);
        assert!(skipped.is_empty());
        assert_eq!(warnings, Vec::<String>::new());
    }

    #[test]
    fn parse_nameless_key_warns_and_counts_its_sessions_as_skipped() {
        // A `<key>` without a name is dropped with everything in it. Its
        // sessions still count toward jump host ambiguity, by name only,
        // since none of its folders exist to give them a path.
        let xml = wrap(
            r#"
            <key>
                <key name="EU">
                    <key name="bastion">
                        <dword name="Is Session">1</dword>
                        <string name="Hostname">bastion.eu</string>
                        <string name="Protocol Name">SSH2</string>
                    </key>
                    <key name="notes">
                        <dword name="Is Session">0</dword>
                    </key>
                </key>
                <key name="web">
                    <dword name="Is Session">1</dword>
                </key>
            </key>
            <key>
                <string name="Hostname">no-session-here</string>
            </key>
            <key name="Prod">
                <key>
                    <key name="db">
                        <dword name="Is Session">1</dword>
                    </key>
                </key>
            </key>"#,
        );
        let (folders, sessions, skipped, warnings) = parse(&xml).unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].name, "Prod");
        assert!(sessions.is_empty());
        let skipped: Vec<_> = skipped
            .iter()
            .map(|s| (s.name.as_str(), s.folder_temp_id))
            .collect();
        assert_eq!(skipped, [("bastion", None), ("web", None), ("db", None)]);
        assert_eq!(
            warnings,
            [
                "Skipped <key> element without name attribute (with 2 sessions in it)",
                "Skipped <key> element without name attribute",
                "In \"Prod\": skipped <key> element without name attribute (with 1 session in it)",
            ]
        );
    }

    #[test]
    fn parse_stops_once_past_the_skipped_session_limit() {
        // A nameless key takes one warning for all its sessions, so the
        // sessions it records need a bound of their own.
        let inner = r#"<key name="s"><dword name="Is Session">1</dword></key>"#
            .repeat(MAX_IMPORT_SESSIONS + 1);
        let err = parse(&wrap(&format!("<key>{inner}</key>"))).unwrap_err();
        assert_eq!(
            err,
            format!("Too many skipped sessions in import (more than {MAX_IMPORT_SESSIONS})")
        );
    }

    #[test]
    fn parse_nested_folders() {
        // A `<key>` with child `<key>` elements is a folder, not a session.
        let xml = wrap(
            r#"
            <key name="Production">
                <key name="EU">
                    <key name="web1">
                        <dword name="Is Session">1</dword>
                        <string name="Hostname">web1.eu</string>
                        <string name="Protocol Name">SSH2</string>
                    </key>
                </key>
            </key>"#,
        );
        let (folders, sessions, _, _) = parse(&xml).unwrap();
        assert_eq!(folders.len(), 2);
        assert_eq!(folders[0].name, "Production");
        assert_eq!(folders[1].name, "EU");
        assert_eq!(folders[1].parent_temp_id, Some(folders[0].temp_id));
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].folder_temp_id, folders[1].temp_id);
    }

    #[test]
    fn parse_jump_host_via_firewall_name() {
        // SecureCRT references a jump host via "Firewall Name" = "Session:<name>".
        let xml = wrap(
            r#"
            <key name="bastion">
                <dword name="Is Session">1</dword>
                <string name="Hostname">bastion.example.com</string>
                <string name="Protocol Name">SSH2</string>
            </key>
            <key name="target">
                <dword name="Is Session">1</dword>
                <string name="Hostname">target.example.com</string>
                <string name="Protocol Name">SSH2</string>
                <string name="Firewall Name">Session:bastion</string>
            </key>"#,
        );
        let (_, sessions, _, _) = parse(&xml).unwrap();
        let target = sessions.iter().find(|s| s.name == "target").unwrap();
        assert_eq!(target.jump_host_name.as_deref(), Some("bastion"));
    }

    #[test]
    fn parse_stops_once_past_the_warning_limit() {
        // Each malformed element is a warning, and a crafted file can repeat
        // one at will; the parse must not collect them without bound.
        let junk = "<string>x</string>".repeat(MAX_IMPORT_SESSIONS + 1);
        let xml = wrap(&format!(
            r#"<key name="s">{junk}<string name="Hostname">h</string></key>"#
        ));
        let err = parse(&xml).unwrap_err();
        assert!(
            err.starts_with("Too many skipped or unreadable entries"),
            "{err}"
        );
    }

    #[test]
    fn parse_rejects_excessive_nesting() {
        use std::fmt::Write as _;
        // Build a folder chain deeper than MAX_NESTING_DEPTH (100).
        let mut inner = String::new();
        for i in 0..=MAX_NESTING_DEPTH {
            let _ = write!(inner, r#"<key name="f{i}">"#);
        }
        for _ in 0..=MAX_NESTING_DEPTH {
            inner.push_str("</key>");
        }
        let err = parse(&wrap(&inner)).unwrap_err();
        assert!(err.contains("nesting"));
    }

    #[test]
    fn parse_limits_nesting_under_a_nameless_key_like_any_other() {
        use std::fmt::Write as _;
        // A nameless key followed by `named` levels, closed again.
        let chain = |named: usize| {
            let mut inner = String::from("<key>");
            for i in 0..named {
                let _ = write!(inner, r#"<key name="f{i}">"#);
            }
            inner.push_str(&"</key>".repeat(named + 1));
            wrap(&inner)
        };
        // The nameless key takes the first level, so as with named keys
        // alone, MAX_NESTING_DEPTH levels pass and one more does not.
        assert!(parse(&chain(MAX_NESTING_DEPTH - 1)).is_ok());
        let err = parse(&chain(MAX_NESTING_DEPTH)).unwrap_err();
        assert!(err.contains("nesting"));
    }

    #[test]
    fn parse_oversized_dword_does_not_overflow() {
        // SecureCRT writes large u32 values (e.g. 0xFFFFFFFD for some
        // settings) — these must not break parsing for the dwords we
        // actually care about.
        let xml = wrap(
            r#"
            <key name="s">
                <dword name="Is Session">1</dword>
                <dword name="Printer Quality">4294967293</dword>
                <string name="Hostname">h</string>
                <string name="Protocol Name">SSH2</string>
            </key>"#,
        );
        let (_, sessions, _, _) = parse(&xml).unwrap();
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn parse_resolves_entity_references_in_text() {
        // Element text arrives as text chunks split around separate
        // entity-reference events; every one of them must survive.
        let xml = wrap(
            r#"
            <key name="s">
                <dword name="Is Session">1</dword>
                <string name="Hostname">web&#x2D;1&#46;lan</string>
                <string name="Protocol Name">SSH2</string>
                <string name="Username">dom&amp;user&lt;&gt;&quot;&apos;</string>
            </key>"#,
        );
        let (_, sessions, _, _) = parse(&xml).unwrap();
        assert_eq!(sessions[0].hostname, "web-1.lan");
        assert_eq!(sessions[0].username.as_deref(), Some("dom&user<>\"'"));
    }

    #[test]
    fn parse_keeps_unresolvable_references_verbatim() {
        // Undeclared entities and invalid character references are malformed,
        // but losing them silently would corrupt the value without a trace.
        let xml = wrap(
            r#"
            <key name="s">
                <dword name="Is Session">1</dword>
                <string name="Hostname">h</string>
                <string name="Protocol Name">SSH2</string>
                <string name="Username">a&custom;b&#0;c</string>
            </key>"#,
        );
        let (_, sessions, _, _) = parse(&xml).unwrap();
        assert_eq!(sessions[0].username.as_deref(), Some("a&custom;b&#0;c"));
    }
}
