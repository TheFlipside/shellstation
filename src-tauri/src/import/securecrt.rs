//! SecureCRT XML session-store parser.
//!
//! SecureCRT stores sessions as a recursive `<key>` tree where a key is
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

use super::{ImportedFolder, ImportedSession, ParseResult};

/// Maximum allowed folder nesting depth to prevent stack exhaustion from
/// pathologically deep XML structures.
const MAX_NESTING_DEPTH: usize = 100;

/// Names of built-in SecureCRT sessions to skip during import.
const SKIP_SESSIONS: &[&str] = &[
    "Default",
    "Default_LocalShell",
    "Default_RDP",
    "Default_Serial",
];

/// Parse a SecureCRT XML export into folders and sessions.
///
/// SecureCRT uses a nested `<key>` structure under `<key name="Sessions">`.
/// Each key is either a folder (contains child keys) or a session
/// (has `<dword name="Is Session">1</dword>`).
pub fn parse(xml: &str) -> Result<ParseResult, String> {
    let mut reader = Reader::from_str(xml);

    // Navigate to the <key name="Sessions"> block.
    if !find_sessions_key(&mut reader)? {
        return Err("No <key name=\"Sessions\"> block found in SecureCRT XML".to_string());
    }

    let mut folders = Vec::new();
    let mut sessions = Vec::new();
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
        &mut warnings,
        0,
    )?;

    Ok((folders, sessions, warnings))
}

/// Advance the reader past `<key name="Sessions">`, leaving it positioned
/// at the first child element. Returns `false` if the block is not found.
///
/// If the XML contains multiple `<key name="Sessions">` blocks only the
/// first one is processed; SecureCRT exports are expected to have exactly one.
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
    warnings: &mut Vec<String>,
    depth: usize,
) -> Result<(), String> {
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                if e.local_name().as_ref() == b"key" {
                    match get_attr(e, "name") {
                        Some(key_name) => parse_key_block(
                            reader,
                            &key_name,
                            parent_temp_id,
                            next_temp_id,
                            folders,
                            sessions,
                            warnings,
                            depth,
                        )?,
                        None => {
                            warnings.push("Skipped <key> element without name attribute".into());
                            skip_through_end(reader, b"key")?;
                        }
                    }
                } else {
                    // Non-key element at this level (shouldn't happen, but be safe).
                    skip_through_end(reader, e.local_name().as_ref())?;
                }
            }
            Ok(Event::End(_)) | Ok(Event::Eof) => break,
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
    warnings: &mut Vec<String>,
    depth: usize,
) -> Result<(), String> {
    if depth >= MAX_NESTING_DEPTH {
        return Err(format!(
            "Folder nesting exceeds maximum depth of {MAX_NESTING_DEPTH}"
        ));
    }

    // Skip built-in default sessions early.
    if SKIP_SESSIONS.contains(&key_name) {
        skip_through_end(reader, b"key")?;
        return Ok(());
    }

    // Allocate a temp_id for this block (used if it turns out to be a folder).
    let my_temp_id = *next_temp_id;
    *next_temp_id += 1;

    let mut props = SessionProps::default();
    let mut has_child_keys = false;

    loop {
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
                        match get_attr(e, "name") {
                            Some(child_name) => parse_key_block(
                                reader,
                                &child_name,
                                my_temp_id,
                                next_temp_id,
                                folders,
                                sessions,
                                warnings,
                                depth + 1,
                            )?,
                            None => {
                                warnings.push(format!(
                                    "In \"{key_name}\": skipped <key> element without name attribute"
                                ));
                                skip_through_end(reader, b"key")?;
                            }
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
            Ok(Event::Eof) => break,
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
    // eagerly above. An empty key with no children and no Is Session flag is
    // silently dropped (matches SecureCRT's own "empty placeholder" behavior).
    if props.is_session {
        match build_session(key_name, &props, parent_temp_id) {
            Ok(s) => sessions.push(s),
            Err(w) => warnings.push(w),
        }
    }

    Ok(())
}

/// Collected session properties from SecureCRT XML.
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

/// Map SecureCRT protocol names to ShellStation protocols.
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
    let protocol = match map_protocol(&props.protocol) {
        Some(p) => p,
        None => {
            return Err(format!(
                "Skipped \"{name}\" — unsupported protocol: {}",
                props.protocol
            ));
        }
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
                text.push_str(
                    e.xml_content()
                        .map_err(|err| format!("Failed to decode text: {err}"))?
                        .as_ref(),
                );
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
            return attr.unescape_value().ok().map(|v| v.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn parse_empty_sessions() {
        let (folders, sessions, warnings) = parse(&wrap("")).unwrap();
        assert!(folders.is_empty());
        assert!(sessions.is_empty());
        assert!(warnings.is_empty());
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
        let (folders, sessions, warnings) = parse(&xml).unwrap();
        assert!(folders.is_empty());
        assert_eq!(sessions.len(), 1);
        assert!(warnings.is_empty());
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
        let (_, sessions, _) = parse(&xml).unwrap();
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
        let (_, sessions, warnings) = parse(&xml).unwrap();
        assert!(sessions.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("no hostname"));
    }

    #[test]
    fn parse_unsupported_protocol_warns() {
        let xml = wrap(
            r#"
            <key name="rdp1">
                <dword name="Is Session">1</dword>
                <string name="Hostname">srv</string>
                <string name="Protocol Name">RDP</string>
            </key>"#,
        );
        let (_, sessions, warnings) = parse(&xml).unwrap();
        assert!(sessions.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].to_lowercase().contains("rdp"));
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
            <key name="real">
                <dword name="Is Session">1</dword>
                <string name="Hostname">real.example.com</string>
                <string name="Protocol Name">SSH2</string>
            </key>"#,
        );
        let (_, sessions, _) = parse(&xml).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, "real");
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
        let (folders, sessions, _) = parse(&xml).unwrap();
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
        let (_, sessions, _) = parse(&xml).unwrap();
        let target = sessions.iter().find(|s| s.name == "target").unwrap();
        assert_eq!(target.jump_host_name.as_deref(), Some("bastion"));
    }

    #[test]
    fn parse_rejects_excessive_nesting() {
        // Build a folder chain deeper than MAX_NESTING_DEPTH (100).
        let mut inner = String::new();
        for i in 0..=MAX_NESTING_DEPTH {
            inner.push_str(&format!(r#"<key name="f{i}">"#));
        }
        for _ in 0..=MAX_NESTING_DEPTH {
            inner.push_str("</key>");
        }
        let err = parse(&wrap(&inner)).unwrap_err();
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
        let (_, sessions, _) = parse(&xml).unwrap();
        assert_eq!(sessions.len(), 1);
    }
}
