use quick_xml::events::Event;
use quick_xml::Reader;

use super::{ImportedFolder, ImportedSession, ParseResult};

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
    )?;

    Ok((folders, sessions, warnings))
}

/// Advance the reader past `<key name="Sessions">`, leaving it positioned
/// at the first child element. Returns `false` if the block is not found.
fn find_sessions_key(reader: &mut Reader<&[u8]>) -> Result<bool, String> {
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                if e.local_name().as_ref() == b"key" {
                    if get_attr(e, "name").as_deref() == Some("Sessions") {
                        return Ok(true);
                    }
                    // Not the Sessions key — skip its entire subtree.
                    skip_to_end(reader, b"key")?;
                }
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
fn parse_key_children(
    reader: &mut Reader<&[u8]>,
    parent_temp_id: usize,
    next_temp_id: &mut usize,
    folders: &mut Vec<ImportedFolder>,
    sessions: &mut Vec<ImportedSession>,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                if e.local_name().as_ref() == b"key" {
                    let key_name = get_attr(e, "name").unwrap_or_default();
                    parse_key_block(
                        reader,
                        &key_name,
                        parent_temp_id,
                        next_temp_id,
                        folders,
                        sessions,
                        warnings,
                    )?;
                } else {
                    // Non-key element at this level (shouldn't happen, but be safe).
                    skip_to_end(reader, e.local_name().as_ref())?;
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
fn parse_key_block(
    reader: &mut Reader<&[u8]>,
    key_name: &str,
    parent_temp_id: usize,
    next_temp_id: &mut usize,
    folders: &mut Vec<ImportedFolder>,
    sessions: &mut Vec<ImportedSession>,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    // Skip built-in default sessions early.
    if SKIP_SESSIONS.contains(&key_name) {
        skip_to_end(reader, b"key")?;
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
                        has_child_keys = true;
                        let child_name = get_attr(e, "name").unwrap_or_default();
                        parse_key_block(
                            reader,
                            &child_name,
                            my_temp_id,
                            next_temp_id,
                            folders,
                            sessions,
                            warnings,
                        )?;
                    }
                    b"string" => {
                        let attr_name = get_attr(e, "name").unwrap_or_default();
                        let value = read_text(reader, b"string")?;
                        props.set_string(&attr_name, &value);
                    }
                    b"dword" => {
                        let attr_name = get_attr(e, "name").unwrap_or_default();
                        let value = read_text(reader, b"dword")?;
                        props.set_dword(&attr_name, &value);
                    }
                    _ => {
                        skip_to_end(reader, local.as_ref())?;
                    }
                }
            }
            Ok(Event::Empty(ref e)) => {
                let local = e.local_name();
                if local.as_ref() == b"string" {
                    let attr_name = get_attr(e, "name").unwrap_or_default();
                    props.set_string(&attr_name, "");
                }
            }
            Ok(Event::End(ref e)) => {
                if e.local_name().as_ref() == b"key" {
                    break;
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

    // Decide: session or folder.
    if props.is_session {
        match build_session(key_name, &props, parent_temp_id) {
            Ok(s) => sessions.push(s),
            Err(w) => warnings.push(w),
        }
    } else if has_child_keys {
        folders.push(ImportedFolder {
            temp_id: my_temp_id,
            name: key_name.to_string(),
            parent_temp_id: Some(parent_temp_id),
        });
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
        let parsed: i32 = value.parse().unwrap_or(0);
        match name {
            "Is Session" => self.is_session = parsed == 1,
            "[SSH2] Port" => self.ssh_port = Some(parsed),
            "Port" => self.telnet_port = Some(parsed),
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
    let jump_host_name = if props.firewall_name.starts_with("Session:") {
        Some(props.firewall_name["Session:".len()..].to_string())
    } else {
        None
    };

    Ok(ImportedSession {
        name: name.to_string(),
        folder_temp_id,
        hostname: props.hostname.clone(),
        port,
        protocol: protocol.to_string(),
        username: props.username.clone(),
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
                    e.unescape()
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

/// Skip events until the matching end tag.
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

/// Get an attribute value by name from a `BytesStart` element.
fn get_attr(e: &quick_xml::events::BytesStart<'_>, name: &str) -> Option<String> {
    for attr in e.attributes().flatten() {
        if attr.key.as_ref() == name.as_bytes() {
            return attr.unescape_value().ok().map(|v| v.to_string());
        }
    }
    None
}
