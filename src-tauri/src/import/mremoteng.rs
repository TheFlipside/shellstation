//! `mRemoteNG` `confCons.xml` parser.
//!
//! Implemented as a streaming pull-parser (`quick_xml::Reader`) rather
//! than a DOM walk: production `mRemoteNG` files are routinely tens of
//! megabytes once they accumulate a few thousand sessions, and we cannot
//! afford to materialise the whole tree in memory. The trade-off is that
//! each branch in `parse` corresponds to one *transition* in the `XML`
//! event stream — it's flat by construction. Reorganising it into
//! recursive helpers would just shift the same number of branches into
//! the helpers without simplifying the protocol they decode.
//!
//! The branch table the parser must cover is captured by the test
//! fixtures at the bottom of this file; each fixture exercises one
//! `<Node>` shape the parser commits to a different action on.

use std::collections::HashMap;

use quick_xml::events::Event;
use quick_xml::Reader;

use super::{
    check_parsed_counts, dropped_sessions_note, ImportedFolder, ImportedSession, ParseResult,
    SkippedSession,
};

/// Maximum allowed folder nesting depth to prevent stack exhaustion from
/// pathologically deep `XML` structures.
const MAX_NESTING_DEPTH: usize = 100;

/// Parse an `mRemoteNG` `confCons.xml` file into folders and sessions.
///
/// The format uses `<Node>` elements with attributes:
/// - `Type="Container"` → folder (may nest arbitrarily)
/// - `Type="Connection"` → session (usually self-closing)
///
/// Unsupported protocols (RDP, VNC, etc.) are skipped and reported as warnings,
/// and so is a `<Node>` of any other type, with everything in it.
pub fn parse(xml: &str) -> Result<ParseResult, String> {
    let mut reader = Reader::from_str(xml);
    // Already the default; the parse relies on every end tag matching its
    // start tag to tell a whole file from a truncated one.
    reader.config_mut().check_end_names = true;

    let mut folders = Vec::new();
    let mut sessions = Vec::new();
    let mut skipped = Vec::new();
    let mut warnings = Vec::new();

    // temp_id 0 is reserved for the root import folder (created by caller).
    let root_temp_id: usize = 0;
    let mut folder_stack: Vec<usize> = Vec::new();
    let mut next_temp_id: usize = 1;
    // Elements other than `<Node>` still open, such as the root. quick-xml
    // does not report elements left open at the end, so this and
    // `folder_stack` are what tell a file cut off part way from a whole one.
    let mut open_elements: usize = 0;

    loop {
        check_parsed_counts(folders.len(), sessions.len(), skipped.len(), warnings.len())?;
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                if e.local_name().as_ref() != b"Node" {
                    open_elements += 1;
                    continue;
                }
                let attrs = parse_attributes(e)?;
                let node_type = attrs.get("Type").map_or("", String::as_str);
                let name = attrs.get("Name").cloned().unwrap_or_default();

                match node_type {
                    "Container" => {
                        if folder_stack.len() >= MAX_NESTING_DEPTH {
                            return Err(format!(
                                "Folder nesting exceeds maximum depth of {MAX_NESTING_DEPTH}"
                            ));
                        }
                        let parent = folder_stack.last().copied().unwrap_or(root_temp_id);
                        let temp_id = next_temp_id;
                        next_temp_id += 1;
                        folders.push(ImportedFolder {
                            temp_id,
                            name,
                            parent_temp_id: Some(parent),
                        });
                        folder_stack.push(temp_id);
                    }
                    "Connection" => {
                        let parent = folder_stack.last().copied().unwrap_or(root_temp_id);
                        match build_session(&attrs, parent) {
                            Ok(s) => sessions.push(s),
                            Err(w) => {
                                warnings.push(w);
                                skipped.push(SkippedSession {
                                    name,
                                    folder_temp_id: Some(parent),
                                });
                            }
                        }
                        // Consume until matching </Node>.
                        skip_to_end(&mut reader, b"Node")?;
                    }
                    _ => {
                        let dropped = skip_dropped_node(&mut reader, &mut skipped)?;
                        warnings.push(unsupported_node_warning(&name, node_type, dropped));
                    }
                }
            }
            Ok(Event::Empty(ref e)) => {
                if e.local_name().as_ref() != b"Node" {
                    continue;
                }
                let attrs = parse_attributes(e)?;
                let node_type = attrs.get("Type").map_or("", String::as_str);
                let name = attrs.get("Name").cloned().unwrap_or_default();

                match node_type {
                    "Container" => {
                        let parent = folder_stack.last().copied().unwrap_or(root_temp_id);
                        let temp_id = next_temp_id;
                        next_temp_id += 1;
                        folders.push(ImportedFolder {
                            temp_id,
                            name,
                            parent_temp_id: Some(parent),
                        });
                        // Self-closing — no children, don't push to stack.
                    }
                    "Connection" => {
                        let parent = folder_stack.last().copied().unwrap_or(root_temp_id);
                        match build_session(&attrs, parent) {
                            Ok(s) => sessions.push(s),
                            Err(w) => {
                                warnings.push(w);
                                skipped.push(SkippedSession {
                                    name,
                                    folder_temp_id: Some(parent),
                                });
                            }
                        }
                    }
                    _ => warnings.push(unsupported_node_warning(&name, node_type, 0)),
                }
            }
            Ok(Event::End(ref e)) if e.local_name().as_ref() == b"Node" => {
                folder_stack.pop();
            }
            Ok(Event::End(_)) => open_elements = open_elements.saturating_sub(1),
            Ok(Event::Eof) if folder_stack.is_empty() && open_elements == 0 => break,
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

    Ok((folders, sessions, skipped, warnings))
}

/// The warning for a `<Node>` whose type is neither `Container` nor
/// `Connection`, dropped with the `dropped` sessions in it.
fn unsupported_node_warning(name: &str, node_type: &str, dropped: usize) -> String {
    format!(
        "Skipped \"{name}\" — unsupported node type \"{node_type}\"{}",
        dropped_sessions_note(dropped)
    )
}

/// Skip events until the matching end tag of a `<Node>` that is dropped with
/// everything in it, and record each connection in it as skipped: a jump host
/// reference that also names a session elsewhere then stays ambiguous instead
/// of linking that one as if it were unique. The node's folders are not
/// imported, so its sessions have no folder path. Returns how many sessions
/// the node held.
fn skip_dropped_node(
    reader: &mut Reader<&[u8]>,
    skipped: &mut Vec<SkippedSession>,
) -> Result<usize, String> {
    let before = skipped.len();
    let mut depth = 1u32;
    loop {
        // Only `skipped` grows here; the caller checks the other counts.
        check_parsed_counts(0, 0, skipped.len(), 0)?;
        match reader.read_event() {
            Ok(Event::Start(ref e)) if e.local_name().as_ref() == b"Node" => {
                depth += 1;
                record_dropped_connection(e, skipped)?;
            }
            Ok(Event::Empty(ref e)) if e.local_name().as_ref() == b"Node" => {
                record_dropped_connection(e, skipped)?;
            }
            Ok(Event::End(ref e)) if e.local_name().as_ref() == b"Node" => {
                depth -= 1;
                if depth == 0 {
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

/// Record a `<Node>` inside a dropped one as skipped if it is a connection.
fn record_dropped_connection(
    e: &quick_xml::events::BytesStart<'_>,
    skipped: &mut Vec<SkippedSession>,
) -> Result<(), String> {
    let mut attrs = parse_attributes(e)?;
    if attrs.get("Type").map(String::as_str) == Some("Connection") {
        skipped.push(SkippedSession {
            name: attrs.remove("Name").unwrap_or_default(),
            folder_temp_id: None,
        });
    }
    Ok(())
}

/// Skip events until the matching end tag for `tag` is consumed.
fn skip_to_end(reader: &mut Reader<&[u8]>, tag: &[u8]) -> Result<(), String> {
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

/// Extract all attributes from a `<Node>` element into a `HashMap`.
fn parse_attributes(
    e: &quick_xml::events::BytesStart<'_>,
) -> Result<HashMap<String, String>, String> {
    let mut map = HashMap::new();
    for attr in e.attributes() {
        let attr = attr.map_err(|err| format!("Failed to parse XML attribute: {err}"))?;
        let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
        let value = attr
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map_err(|err| format!("Failed to decode attribute '{key}': {err}"))?
            .to_string();
        map.insert(key, value);
    }
    Ok(map)
}

/// Map `mRemoteNG` protocol names to `ShellStation` protocol strings.
fn map_protocol(protocol: &str) -> Option<&'static str> {
    match protocol {
        "SSH2" | "SSH1" => Some("ssh"),
        "Telnet" => Some("telnet"),
        _ => None,
    }
}

/// Build an `ImportedSession` from node attributes, or return a skip warning.
fn build_session(
    attrs: &HashMap<String, String>,
    folder_temp_id: usize,
) -> Result<ImportedSession, String> {
    let name = attrs.get("Name").cloned().unwrap_or_default();
    let hostname = attrs.get("Hostname").cloned().unwrap_or_default();
    let raw_protocol = attrs.get("Protocol").map_or("SSH2", String::as_str);

    let Some(protocol) = map_protocol(raw_protocol) else {
        return Err(format!(
            "Skipped \"{name}\" — unsupported protocol: {raw_protocol}"
        ));
    };

    if hostname.is_empty() {
        return Err(format!("Skipped \"{name}\" — no hostname"));
    }

    let port: i32 = attrs
        .get("Port")
        .and_then(|p| p.parse().ok())
        .unwrap_or(if protocol == "ssh" { 22 } else { 23 });

    let username = attrs.get("Username").filter(|s| !s.is_empty()).cloned();

    // mRemoteNG stores jump host references in SSHTunnelConnectionName.
    let jump_host_name = attrs
        .get("SSHTunnelConnectionName")
        .filter(|s| !s.is_empty())
        .cloned();

    Ok(ImportedSession {
        name,
        folder_temp_id,
        hostname,
        port,
        protocol: protocol.to_string(),
        username,
        jump_host_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{MAX_IMPORT_FOLDERS, MAX_IMPORT_SESSIONS};

    // The mRemoteNG parser is a stateful pull-parser over an XML stream:
    // tag-by-tag input, a folder-stack mutated on each `<Node Type="…">`,
    // and skip-ahead logic for unsupported subtrees. Branches that only
    // appear under specific tag sequences (deep nesting, RDP skip,
    // unclosed Connection nodes) can't be covered by reading the function
    // in isolation; each behaviour gets a minimal XML fixture below.

    #[test]
    fn parse_empty_root() {
        let xml = r#"<?xml version="1.0"?><Confcons></Confcons>"#;
        let (folders, sessions, _, warnings) = parse(xml).unwrap();
        assert!(folders.is_empty());
        assert!(sessions.is_empty());
        assert!(warnings.is_empty());
    }

    #[test]
    fn parse_single_ssh_session() {
        let xml = r#"<?xml version="1.0"?>
            <Confcons>
                <Node Type="Connection" Name="web1" Hostname="web1.example.com"
                      Port="22" Protocol="SSH2" Username="root"/>
            </Confcons>"#;
        let (folders, sessions, _, warnings) = parse(xml).unwrap();
        assert!(folders.is_empty());
        assert_eq!(sessions.len(), 1);
        assert!(warnings.is_empty());
        let s = &sessions[0];
        assert_eq!(s.name, "web1");
        assert_eq!(s.hostname, "web1.example.com");
        assert_eq!(s.port, 22);
        assert_eq!(s.protocol, "ssh");
        assert_eq!(s.username.as_deref(), Some("root"));
    }

    #[test]
    fn parse_telnet_session() {
        let xml = r#"<?xml version="1.0"?>
            <Confcons>
                <Node Type="Connection" Name="switch" Hostname="10.0.0.1"
                      Port="23" Protocol="Telnet"/>
            </Confcons>"#;
        let (_, sessions, _, warnings) = parse(xml).unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(warnings.is_empty());
        assert_eq!(sessions[0].protocol, "telnet");
    }

    #[test]
    fn parse_unsupported_protocol_warns_but_continues() {
        let xml = r#"<?xml version="1.0"?>
            <Confcons>
                <Node Type="Connection" Name="rdp1" Hostname="srv" Protocol="RDP"/>
                <Node Type="Connection" Name="ssh1" Hostname="web" Port="22" Protocol="SSH2"/>
            </Confcons>"#;
        let (_, sessions, skipped, warnings) = parse(xml).unwrap();
        // Unsupported entry produces a warning, supported entry still imports.
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, "ssh1");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].to_lowercase().contains("rdp"));
        // Still reported by name, so a jump host reference to "rdp1" counts it.
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].name, "rdp1");
        assert_eq!(skipped[0].folder_temp_id, Some(0));
    }

    #[test]
    fn parse_unknown_node_type_warns_and_counts_its_sessions_as_skipped() {
        // A node that is neither a folder nor a connection is dropped with
        // everything in it. Its connections still count toward jump host
        // ambiguity, by name only, since none of its folders exist to give
        // them a path.
        let xml = r#"<?xml version="1.0"?>
            <Confcons>
                <Node Type="Group" Name="EU">
                    <Node Type="Container" Name="Web">
                        <Node Type="Connection" Name="bastion" Hostname="b" Protocol="SSH2"/>
                    </Node>
                    <Node Type="Connection" Name="db" Hostname="d" Protocol="SSH2"></Node>
                    <Node Type="Note" Name="readme"/>
                </Node>
                <Node Type="Note" Name="todo"/>
                <Node Type="Connection" Name="web" Hostname="w" Port="22" Protocol="SSH2"/>
            </Confcons>"#;
        let (folders, sessions, skipped, warnings) = parse(xml).unwrap();
        assert!(folders.is_empty());
        // The folder stack is intact past the dropped node.
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, "web");
        assert_eq!(sessions[0].folder_temp_id, 0);
        let skipped: Vec<_> = skipped
            .iter()
            .map(|s| (s.name.as_str(), s.folder_temp_id))
            .collect();
        assert_eq!(skipped, [("bastion", None), ("db", None)]);
        assert_eq!(
            warnings,
            [
                "Skipped \"EU\" — unsupported node type \"Group\" (with 2 sessions in it)",
                "Skipped \"todo\" — unsupported node type \"Note\"",
            ]
        );
    }

    #[test]
    fn parse_stops_once_past_the_skipped_session_limit() {
        // A dropped node takes one warning for all its connections, so the
        // ones it records need a bound of their own.
        let nodes = r#"<Node Type="Connection" Name="c"/>"#.repeat(MAX_IMPORT_SESSIONS + 1);
        let xml = format!(r#"<Confcons><Node Type="Group" Name="g">{nodes}</Node></Confcons>"#);
        assert_eq!(
            parse(&xml).unwrap_err(),
            format!("Too many skipped sessions in import (more than {MAX_IMPORT_SESSIONS})")
        );
    }

    #[test]
    fn parse_nested_folders_track_parent() {
        let xml = r#"<?xml version="1.0"?>
            <Confcons>
                <Node Type="Container" Name="Production">
                    <Node Type="Container" Name="EU">
                        <Node Type="Connection" Name="eu1" Hostname="h" Port="22" Protocol="SSH2"/>
                    </Node>
                </Node>
            </Confcons>"#;
        let (folders, sessions, _, _) = parse(xml).unwrap();
        assert_eq!(folders.len(), 2);
        assert_eq!(folders[0].name, "Production");
        assert_eq!(folders[0].parent_temp_id, Some(0)); // root
        assert_eq!(folders[1].name, "EU");
        assert_eq!(folders[1].parent_temp_id, Some(folders[0].temp_id));
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].folder_temp_id, folders[1].temp_id);
    }

    #[test]
    fn parse_stops_once_past_the_folder_limit() {
        // A file of tiny entries is rejected during the parse, not after
        // every entry has been built.
        let nodes = r#"<Node Type="Container" Name="f"/>"#.repeat(MAX_IMPORT_FOLDERS + 1);
        let xml = format!("<Confcons>{nodes}<Node Type=\"Container\" Name=\"last\"/></Confcons>");
        let err = parse(&xml).unwrap_err();
        assert_eq!(
            err,
            format!("Too many folders in import (more than {MAX_IMPORT_FOLDERS})")
        );
    }

    #[test]
    fn parse_rejects_excessive_nesting() {
        use std::fmt::Write as _;
        // Build a folder chain deeper than MAX_NESTING_DEPTH (100).
        let mut xml = String::from(r#"<?xml version="1.0"?><Confcons>"#);
        for i in 0..=MAX_NESTING_DEPTH {
            let _ = write!(xml, r#"<Node Type="Container" Name="f{i}">"#);
        }
        for _ in 0..=MAX_NESTING_DEPTH {
            xml.push_str("</Node>");
        }
        xml.push_str("</Confcons>");
        let err = parse(&xml).unwrap_err();
        assert!(err.contains("nesting"));
    }

    #[test]
    fn parse_rejects_a_truncated_file() {
        // A file cut off part way must not import the part before the cut
        // as if it were the whole export.
        let connection = r#"<Node Type="Connection" Name="a" Hostname="h" Protocol="SSH2"/>"#;
        for cut in [
            format!(r#"<Confcons><Node Type="Container" Name="EU">{connection}"#),
            format!("<Confcons>{connection}"),
        ] {
            assert_eq!(parse(&cut).unwrap_err(), "Unexpected end of XML", "{cut}");
        }
        let whole =
            format!(r#"<Confcons><Node Type="Container" Name="EU">{connection}</Node></Confcons>"#);
        assert!(parse(&whole).is_ok());
    }

    #[test]
    fn parse_malformed_xml_returns_error() {
        let xml = r#"<?xml version="1.0"?><Confcons><Node Type="Container" Name="x"#;
        let result = parse(xml);
        assert!(result.is_err());
    }

    #[test]
    fn parse_jump_host_reference() {
        // mRemoteNG uses "Inheritance" / SSHOptions block — emulate the
        // form the parser actually understands.
        let xml = r#"<?xml version="1.0"?>
            <Confcons>
                <Node Type="Connection" Name="bastion" Hostname="bastion.example.com"
                      Port="22" Protocol="SSH2"/>
                <Node Type="Connection" Name="target" Hostname="target.example.com"
                      Port="22" Protocol="SSH2" SSHTunnelConnectionName="bastion"/>
            </Confcons>"#;
        let (_, sessions, _, _) = parse(xml).unwrap();
        assert_eq!(sessions.len(), 2);
        let target = sessions.iter().find(|s| s.name == "target").unwrap();
        assert_eq!(target.jump_host_name.as_deref(), Some("bastion"));
    }
}
