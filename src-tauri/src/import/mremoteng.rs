use std::collections::HashMap;

use quick_xml::events::Event;
use quick_xml::Reader;

use super::{ImportedFolder, ImportedSession, ParseResult};

/// Parse an mRemoteNG `confCons.xml` file into folders and sessions.
///
/// The format uses `<Node>` elements with attributes:
/// - `Type="Container"` → folder (may nest arbitrarily)
/// - `Type="Connection"` → session (usually self-closing)
///
/// Unsupported protocols (RDP, VNC, etc.) are skipped and reported as warnings.
pub fn parse(xml: &str) -> Result<ParseResult, String> {
    let mut reader = Reader::from_str(xml);

    let mut folders = Vec::new();
    let mut sessions = Vec::new();
    let mut warnings = Vec::new();

    // temp_id 0 is reserved for the root import folder (created by caller).
    let root_temp_id: usize = 0;
    let mut folder_stack: Vec<usize> = Vec::new();
    let mut next_temp_id: usize = 1;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                if e.local_name().as_ref() != b"Node" {
                    continue;
                }
                let attrs = parse_attributes(e)?;
                let node_type = attrs.get("Type").map(String::as_str).unwrap_or("");
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
                        folder_stack.push(temp_id);
                    }
                    "Connection" => {
                        let parent = folder_stack.last().copied().unwrap_or(root_temp_id);
                        match build_session(&attrs, parent) {
                            Ok(s) => sessions.push(s),
                            Err(w) => warnings.push(w),
                        }
                        // Consume until matching </Node>.
                        skip_to_end(&mut reader, b"Node")?;
                    }
                    _ => {
                        skip_to_end(&mut reader, b"Node")?;
                    }
                }
            }
            Ok(Event::Empty(ref e)) => {
                if e.local_name().as_ref() != b"Node" {
                    continue;
                }
                let attrs = parse_attributes(e)?;
                let node_type = attrs.get("Type").map(String::as_str).unwrap_or("");
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
                            Err(w) => warnings.push(w),
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                if e.local_name().as_ref() == b"Node" {
                    folder_stack.pop();
                }
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

    Ok((folders, sessions, warnings))
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

/// Extract all attributes from a `<Node>` element into a HashMap.
fn parse_attributes(
    e: &quick_xml::events::BytesStart<'_>,
) -> Result<HashMap<String, String>, String> {
    let mut map = HashMap::new();
    for attr in e.attributes() {
        let attr = attr.map_err(|err| format!("Failed to parse XML attribute: {err}"))?;
        let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
        let value = attr
            .unescape_value()
            .map_err(|err| format!("Failed to decode attribute '{key}': {err}"))?
            .to_string();
        map.insert(key, value);
    }
    Ok(map)
}

/// Map mRemoteNG protocol names to ShellStation protocol strings.
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
    let raw_protocol = attrs.get("Protocol").map(String::as_str).unwrap_or("SSH2");

    let protocol = match map_protocol(raw_protocol) {
        Some(p) => p,
        None => {
            return Err(format!(
                "Skipped \"{name}\" — unsupported protocol: {raw_protocol}"
            ));
        }
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
