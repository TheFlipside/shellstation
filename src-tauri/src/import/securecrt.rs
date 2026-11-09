//! `SecureCRT` `XML` export parser: sessions, button bars and the Command
//! Manager.
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
//!
//! Buttons come from two blocks that share one entry format. `ButtonBarV5`
//! holds one `<array>` per button bar, named after the bar. `Commands` is
//! the Command Manager: its folders are nested `<key>`s and each list of
//! commands is an `<array>`. Every `<string>` in an array is one button:
//!
//! ```text
//! SEND,show version \\r,sh ver,,,0,1,,
//! ```
//!
//! The fields are separated by commas, and a backslash escapes the
//! character after it. The first three are the function, its argument and
//! the label; the rest (eight or nine fields in all, depending on the
//! `SecureCRT` version) are not used. Only `SEND` (Send String) has a
//! command bar equivalent; see `convert_send_string` for its argument.

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Serialize;

use super::{
    check_parsed_counts, dropped_sessions_note, ImportedFolder, ImportedSession, ParseResult,
    SkippedSession,
};

/// Maximum allowed folder nesting depth to prevent stack exhaustion from
/// pathologically deep `XML` structures.
const MAX_NESTING_DEPTH: usize = 100;

/// Most button entries kept from one block (the button bars, or the Command
/// Manager): buttons, and entries skipped with a warning. Unassigned entries
/// do not count. The command bar holds at most 1000 buttons in all
/// (`MAX_COMMAND_BUTTONS` in src/stores/settingsStore.ts, enforced by
/// `importCommandButtons` for both blocks together), so a block with more
/// could not be imported whole anyway. Entries past this are counted, not
/// kept.
const MAX_BUTTON_ENTRIES: u32 = 1000;

/// Most characters of a name from the file that a warning quotes: a bar,
/// key or session name, a label, a function or a protocol.
const MAX_QUOTED_LEN: usize = 64;

/// Longest button entry taken apart, in bytes. The longest command a button
/// can have, written as escaped octal codes (`\\101`, five bytes a
/// character), fits about twelve times over. A longer entry cannot become
/// a button, so it is skipped before it is split and copied into fields.
const MAX_BUTTON_ENTRY_BYTES: usize = 64 * 1024;

/// Longest button name, in UTF-16 code units as JavaScript counts them.
/// Matches `MAX_BUTTON_NAME_LENGTH` in src/stores/settingsStore.ts.
const MAX_BUTTON_NAME_LEN: usize = 32;

/// Longest button command, counted the same way. Matches
/// `MAX_BUTTON_COMMAND_LENGTH` in src/stores/settingsStore.ts.
const MAX_BUTTON_COMMAND_LEN: usize = 1024;

/// Names of `SecureCRT`'s built-in session templates. They sit directly under
/// `Sessions`, and are skipped there only: further down, the same name is a
/// user's own folder or session.
const SKIP_SESSIONS: &[&str] = &[
    "Default",
    "Default_LocalShell",
    "Default_RDP",
    "Default_Serial",
];

/// What a `SecureCRT` export holds that `ShellStation` can import. Each part
/// is `None` when the file has no block for it: `SecureCRT` exports only the
/// parts the user picked.
#[derive(Debug, Default)]
pub struct SecureCrtFile {
    /// From `<key name="Sessions">`.
    pub sessions: Option<ParseResult>,
    /// From `<key name="ButtonBarV5">`.
    pub button_bars: Option<ImportedButtons>,
    /// From `<key name="Commands">`.
    pub command_manager: Option<ImportedButtons>,
}

/// A `SecureCRT` button that sends a string, as a command bar button.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct ImportedButton {
    pub name: String,
    pub command: String,
}

/// The buttons read from one block of a `SecureCRT` export.
#[derive(Debug, Default, Serialize)]
pub struct ImportedButtons {
    pub buttons: Vec<ImportedButton>,
    /// Entries that did not become a button. Each has a warning, except
    /// that those past `MAX_BUTTON_ENTRIES` share one.
    pub skipped: u32,
    pub warnings: Vec<String>,
}

/// Parse a `SecureCRT` `XML` export into folders and sessions.
///
/// `SecureCRT` uses a nested `<key>` structure under `<key name="Sessions">`.
/// Each key is either a folder (contains child keys) or a session
/// (has `<dword name="Is Session">1</dword>`).
pub fn parse(xml: &str) -> Result<ParseResult, String> {
    parse_file(xml)?
        .sessions
        .ok_or_else(|| "No <key name=\"Sessions\"> block found in SecureCRT XML".to_string())
}

/// Parse a `SecureCRT` `XML` export: its sessions, button bars and Command
/// Manager, whichever it has.
///
/// The blocks are read where they sit at the top level; a block of the same
/// name nested in another one is not one of them. If a block appears more
/// than once, only the first is read; `SecureCRT` writes each one once. The
/// file must be complete: one cut off inside any block fails.
pub fn parse_file(xml: &str) -> Result<SecureCrtFile, String> {
    let mut reader = Reader::from_str(xml);
    // Already the default; the parse relies on every end tag matching its
    // start tag to tell a whole file from a truncated one.
    reader.config_mut().check_end_names = true;

    let mut file = SecureCrtFile::default();
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) if e.local_name().as_ref() == "key" => {
                match get_attr(e, "name").as_deref() {
                    Some("Sessions") if file.sessions.is_none() => {
                        file.sessions = Some(parse_sessions(&mut reader)?);
                    }
                    Some("ButtonBarV5") if file.button_bars.is_none() => {
                        let mut buttons = ButtonCollector::default();
                        parse_button_bars(&mut reader, &mut buttons)?;
                        file.button_bars = Some(buttons.finish());
                    }
                    Some("Commands") if file.command_manager.is_none() => {
                        let mut buttons = ButtonCollector::default();
                        parse_command_manager(&mut reader, &mut buttons, 0)?;
                        file.command_manager = Some(buttons.finish());
                    }
                    // Global options, keymaps and the like, or a second copy
                    // of a block already read.
                    _ => skip_through_end(&mut reader, "key")?,
                }
            }
            Ok(Event::Eof) => return Ok(file),
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

/// Parse the children of `<key name="Sessions">`, with the reader just past
/// its start tag, through its end tag.
fn parse_sessions(reader: &mut Reader<&[u8]>) -> Result<ParseResult, String> {
    let mut folders = Vec::new();
    let mut sessions = Vec::new();
    let mut skipped = Vec::new();
    let mut warnings = Vec::new();

    let root_temp_id: usize = 0;
    let mut next_temp_id: usize = 1;

    parse_key_children(
        reader,
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
                if e.local_name().as_ref() == "key" {
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
                    "key" => {
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
                                "In \"{}\": skipped <key> element without name \
                                 attribute{}",
                                quoted(key_name),
                                dropped_sessions_note(dropped)
                            ));
                        }
                    }
                    "string" => {
                        let Some(attr_name) = get_attr(e, "name") else {
                            warnings.push(format!(
                                "In \"{}\": ignored <string> element without name attribute",
                                quoted(key_name)
                            ));
                            skip_through_end(reader, "string")?;
                            continue;
                        };
                        let value = read_text(reader, "string")?;
                        props.set_string(&attr_name, &value);
                    }
                    "dword" => {
                        let Some(attr_name) = get_attr(e, "name") else {
                            skip_through_end(reader, "dword")?;
                            continue;
                        };
                        let value = read_text(reader, "dword")?;
                        props.set_dword(&attr_name, &value);
                    }
                    _ => {
                        skip_through_end(reader, local.as_ref())?;
                    }
                }
            }
            Ok(Event::Empty(ref e)) => {
                let local = e.local_name();
                if local.as_ref() == "string" {
                    if let Some(attr_name) = get_attr(e, "name") {
                        props.set_string(&attr_name, "");
                    } else {
                        warnings.push(format!(
                            "In \"{}\": ignored empty <string> element without name attribute",
                            quoted(key_name)
                        ));
                    }
                }
            }
            Ok(Event::End(ref e)) if e.local_name().as_ref() == "key" => {
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
                    "key" => {
                        check_depth(depth + open.len())?;
                        open.push((get_attr(e, "name"), SessionProps::default()));
                    }
                    "dword" => {
                        let attr_name = get_attr(e, "name");
                        let value = read_text(reader, "dword")?;
                        if let (Some(attr_name), Some((_, props))) = (attr_name, open.last_mut()) {
                            props.set_dword(&attr_name, &value);
                        }
                    }
                    _ => skip_through_end(reader, local.as_ref())?,
                }
            }
            Ok(Event::End(ref e)) if e.local_name().as_ref() == "key" => {
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
            "Skipped \"{}\" — unsupported protocol: {}",
            quoted(name),
            quoted(&props.protocol)
        ));
    };

    if props.hostname.is_empty() {
        return Err(format!("Skipped \"{}\" — no hostname", quoted(name)));
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

// ── Buttons ─────────────────────────────────────────────────────────

/// Collects the buttons of one block, up to `MAX_BUTTON_ENTRIES` entries.
#[derive(Default)]
struct ButtonCollector {
    result: ImportedButtons,
    /// Entries kept so far: buttons, and entries skipped with a warning.
    kept: u32,
    /// Entries past `MAX_BUTTON_ENTRIES`, counted but not kept.
    over_limit: u32,
}

impl ButtonCollector {
    /// Read the `<string>` entry the reader has just entered, in `place`
    /// (as a warning names it), through its end tag. An unassigned entry is
    /// passed over and does not count toward the limit.
    fn read_entry(&mut self, reader: &mut Reader<&[u8]>, place: &str) -> Result<(), String> {
        let entry = read_text(reader, "string")?;
        let outcome = if entry.len() > MAX_BUTTON_ENTRY_BYTES {
            Some(Err(format!(
                "Skipped an entry in {place}: it is longer than any button that can be imported"
            )))
        } else {
            parse_button_entry(&entry, place)
        };
        let Some(outcome) = outcome else {
            return Ok(());
        };
        if self.kept >= MAX_BUTTON_ENTRIES {
            self.over_limit = self.over_limit.saturating_add(1);
            return Ok(());
        }
        self.kept += 1;
        match outcome {
            Ok(button) => self.result.buttons.push(button),
            Err(warning) => {
                self.result.skipped += 1;
                self.result.warnings.push(warning);
            }
        }
        Ok(())
    }

    fn finish(mut self) -> ImportedButtons {
        if self.over_limit > 0 {
            self.result.skipped = self.result.skipped.saturating_add(self.over_limit);
            self.result.warnings.push(format!(
                "Skipped {} more buttons: an import takes at most {MAX_BUTTON_ENTRIES} \
                 from the button bars, and as many from the Command Manager",
                self.over_limit
            ));
        }
        self.result
    }
}

/// Read the children of `<key name="ButtonBarV5">` through its end tag: one
/// `<array>` per button bar.
fn parse_button_bars(
    reader: &mut Reader<&[u8]>,
    buttons: &mut ButtonCollector,
) -> Result<(), String> {
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let local = e.local_name();
                if local.as_ref() == "array" {
                    // Named once per bar, cut: the name goes into the warning
                    // of every entry on the bar that is skipped.
                    let bar = quoted(&get_attr(e, "name").unwrap_or_default());
                    parse_button_array(reader, &format!("button bar \"{bar}\""), buttons)?;
                } else {
                    skip_through_end(reader, local.as_ref())?;
                }
            }
            // Every child is read through its own end tag, so this is the
            // block's: `check_end_names` fails a file where it would not be.
            Ok(Event::End(_)) => return Ok(()),
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

/// Read the children of `<key name="Commands">`, or of a folder in it,
/// through its end tag. `depth` is the folder's depth below `Commands`.
fn parse_command_manager(
    reader: &mut Reader<&[u8]>,
    buttons: &mut ButtonCollector,
    depth: usize,
) -> Result<(), String> {
    check_depth(depth)?;
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let local = e.local_name();
                match local.as_ref() {
                    "key" => parse_command_manager(reader, buttons, depth + 1)?,
                    "array" => parse_button_array(reader, "the Command Manager", buttons)?,
                    other => skip_through_end(reader, other)?,
                }
            }
            // The block's or folder's own end tag, as in `parse_button_bars`.
            Ok(Event::End(_)) => return Ok(()),
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

/// Read the `<string>` entries of an `<array>` through its end tag. An
/// empty `<string/>` holds no button and is passed over.
fn parse_button_array(
    reader: &mut Reader<&[u8]>,
    place: &str,
    buttons: &mut ButtonCollector,
) -> Result<(), String> {
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let local = e.local_name();
                if local.as_ref() == "string" {
                    buttons.read_entry(reader, place)?;
                } else {
                    skip_through_end(reader, local.as_ref())?;
                }
            }
            // The array's own end tag, as in `parse_button_bars`.
            Ok(Event::End(_)) => return Ok(()),
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

/// Turn one button entry from `place` into a command bar button. `None`
/// for an unassigned button, which has neither a function nor a label;
/// `Err` with a warning for one that cannot become a command bar button.
fn parse_button_entry(entry: &str, place: &str) -> Option<Result<ImportedButton, String>> {
    let [function, argument, label] = leading_fields(entry);
    let function = function.trim();
    let label = decode_label(&label);
    if function.is_empty() && label.is_empty() {
        return None;
    }
    // Built only for an entry that is skipped, and from cut names: a warning
    // is made for each one, and the file decides how long the names are.
    let skip = |reason: &str| {
        let button = if label.is_empty() {
            format!("a button without a label in {place}")
        } else {
            format!("\"{}\" in {place}", quoted(&label))
        };
        Some(Err(format!("Skipped {button}: {reason}")))
    };
    if function != "SEND" {
        return skip(&if function.is_empty() {
            "it has no function; only buttons that send a string can be imported".to_string()
        } else {
            format!(
                "its function, {}, has no command bar equivalent; only buttons that send a \
                 string can be imported",
                quoted(function)
            )
        });
    }
    if label.is_empty() {
        return skip("a command bar button needs one");
    }
    if argument.is_empty() {
        return skip("it sends nothing");
    }
    let command = match convert_send_string(&argument) {
        Ok(command) => command,
        Err(Unsendable::RawByte(code)) => {
            return skip(&format!(
                "it sends character code \\{code:03o}, which a command bar button cannot send"
            ));
        }
        Err(Unsendable::Ambiguous) => {
            return skip(
                "its octal codes, written for the command bar, would run into the characters \
                 around them and send something else",
            );
        }
    };
    if utf16_len(&command) > MAX_BUTTON_COMMAND_LEN {
        return skip(&format!(
            "its command is longer than {MAX_BUTTON_COMMAND_LEN} characters"
        ));
    }
    Some(Ok(ImportedButton {
        name: truncate_utf16(&label, MAX_BUTTON_NAME_LEN),
        command,
    }))
}

/// The first three comma-separated fields of a button entry: its function,
/// its argument and its label. A backslash escapes the character after it,
/// so `\,` is a comma inside a field and `\\` one backslash. A field the
/// entry does not have is empty, and the fields after the third are not
/// read, so an entry made of commas costs no more than three strings.
fn leading_fields(entry: &str) -> [String; 3] {
    let mut fields: [String; 3] = Default::default();
    let mut index = 0;
    let mut chars = entry.chars();
    while let Some(c) = chars.next() {
        match c {
            ',' => {
                index += 1;
                if index == fields.len() {
                    break;
                }
            }
            '\\' => fields[index].push(chars.next().unwrap_or('\\')),
            _ => fields[index].push(c),
        }
    }
    fields
}

/// A button label as `SecureCRT` shows it: `&&` stands for `&`, and a single
/// `&` marks the letter after it as the button's access key (a Windows
/// mnemonic), so it is dropped.
fn decode_label(raw: &str) -> String {
    let mut label = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '&' {
            label.push(c);
        } else if chars.next_if_eq(&'&').is_some() {
            label.push('&');
        }
    }
    label.trim().to_string()
}

/// A `SecureCRT` send string as a command bar command.
///
/// Both read the same backslash escapes (`\r`, `\n`, `\t`, `\b`, `\e`, `\p`
/// for a one-second pause, `\v` to paste the clipboard, `\\` for a
/// backslash), and the command bar reads `SecureCRT`'s three-digit octal
/// codes for control characters, `\000` to `\037` and `\177` (`\003` is
/// Ctrl+C; see `parseEscapes` in src/components/CommandBar.tsx, which scans
/// the same way). Those carry over as written.
///
/// A printable character written as a code, `\040` to `\176`, becomes that
/// character, with `\134` becoming `\\`. The command bar sends such codes as
/// written, since a command spelled in them would show as digits in the
/// import dialog while typing text: written out, it reads as what it types.
///
/// The result is checked to send what the send string sends, read as each
/// side reads it (see `sent`), and refused if it does not.
fn convert_send_string(raw: &str) -> Result<String, Unsendable> {
    let command = write_out_codes(raw)?;
    if sent(&command, CodesRead::Control) == sent(raw, CodesRead::Ascii) {
        Ok(command)
    } else {
        Err(Unsendable::Ambiguous)
    }
}

/// Why a send string cannot become a command bar command.
#[derive(Debug, PartialEq, Eq)]
enum Unsendable {
    /// The first octal code above `\177`: `SecureCRT` sends it as a single
    /// raw byte, which the command bar, sending text, cannot do.
    RawByte(u32),
    /// Written out, a code runs together with the characters around it: in
    /// `\0\061\062`, `\0` is no code, but with the codes written out it
    /// would start `\012`, which the command bar reads as a line feed.
    Ambiguous,
}

/// `raw` with its printable octal codes written out, as `convert_send_string`
/// describes.
fn write_out_codes(raw: &str) -> Result<String, Unsendable> {
    let mut command = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(backslash) = rest.find('\\') {
        command.push_str(&rest[..backslash]);
        let after = &rest[backslash + 1..];
        if let Some(code) = octal_code(after) {
            match code {
                0..=0o37 | 0o177 => {
                    command.push('\\');
                    command.push_str(&after[..3]);
                }
                0o134 => command.push_str(r"\\"),
                _ => match char::from_u32(code) {
                    Some(c @ ' '..='~') => command.push(c),
                    _ => return Err(Unsendable::RawByte(code)),
                },
            }
            rest = &after[3..];
        } else if let Some(next) = after.chars().next() {
            // An escape both read alike, or a backslash both send as it is.
            // Taken as a pair, as the command bar takes it, so the `\` of
            // `\\` never starts another escape.
            command.push('\\');
            command.push(next);
            rest = &after[next.len_utf8()..];
        } else {
            command.push('\\');
            rest = "";
        }
    }
    command.push_str(rest);
    Ok(command)
}

/// Which octal codes a reading of a send string takes as characters.
#[derive(Clone, Copy)]
enum CodesRead {
    /// Control characters only, `\000` to `\037` and `\177`, as the command
    /// bar reads a command.
    Control,
    /// Every ASCII character, `\000` to `\177`, as `SecureCRT` reads a send
    /// string.
    Ascii,
}

/// One thing a command sends.
#[derive(Debug, PartialEq, Eq)]
enum Sent {
    Char(char),
    /// `\p`, a one-second pause.
    Pause,
    /// `\v`, the clipboard's text.
    Paste,
}

/// What `s` sends, read the way `parseEscapes` in
/// src/components/CommandBar.tsx reads a command, taking the octal codes
/// `codes` says. An escape it does not know sends its backslash, and the
/// character after it is read on its own. Control characters are named by
/// their codes.
fn sent(s: &str, codes: CodesRead) -> Vec<Sent> {
    let ascii = |code: u8| Sent::Char(char::from(code));
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::with_capacity(chars.len());
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        i += 1;
        if c != '\\' {
            out.push(Sent::Char(c));
            continue;
        }
        let named = match chars.get(i) {
            Some('r') => Some(ascii(13)),
            Some('n') => Some(ascii(10)),
            Some('t') => Some(ascii(9)),
            Some('b') => Some(ascii(8)),
            Some('e') => Some(ascii(27)),
            Some('\\') => Some(ascii(92)),
            Some('p') => Some(Sent::Pause),
            Some('v') => Some(Sent::Paste),
            _ => None,
        };
        if let Some(named) = named {
            out.push(named);
            i += 1;
            continue;
        }
        let code = chars
            .get(i..i + 3)
            .filter(|digits| digits.iter().all(|d| matches!(d, '0'..='7')))
            .map(|digits| {
                digits
                    .iter()
                    .fold(0, |n, d| n * 8 + d.to_digit(8).unwrap_or(0))
            });
        let read = match (code, codes) {
            (Some(code), CodesRead::Control) => code <= 0o37 || code == 0o177,
            (Some(code), CodesRead::Ascii) => code <= 0o177,
            (None, _) => false,
        };
        match code.and_then(char::from_u32) {
            Some(character) if read => {
                out.push(Sent::Char(character));
                i += 3;
            }
            _ => out.push(Sent::Char('\\')),
        }
    }
    out
}

/// The value of the three octal digits `s` starts with, if it does.
fn octal_code(s: &str) -> Option<u32> {
    let digits = s.get(..3)?;
    if digits.bytes().all(|b| matches!(b, b'0'..=b'7')) {
        u32::from_str_radix(digits, 8).ok()
    } else {
        None
    }
}

/// `s` as a warning quotes it: at most `MAX_QUOTED_LEN` characters, with
/// `…` when it was cut.
fn quoted(s: &str) -> String {
    let mut chars = s.chars();
    let mut cut: String = chars.by_ref().take(MAX_QUOTED_LEN).collect();
    if chars.next().is_some() {
        cut.push('…');
    }
    cut
}

/// The length of `s` in UTF-16 code units, as JavaScript counts it.
fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// `s` cut to at most `max` UTF-16 code units, never inside a character,
/// and without the spaces the cut may leave at its end.
fn truncate_utf16(s: &str, max: usize) -> String {
    let mut len = 0;
    let end = s
        .char_indices()
        .find(|&(_, c)| {
            len += c.len_utf16();
            len > max
        })
        .map_or(s.len(), |(i, _)| i);
    s[..end].trim_end().to_string()
}

/// Read text content until the matching end tag. An element nested in it is
/// skipped with everything in it: read as text, a nested element of the same
/// name would end the read at its own end tag, leaving the outer end tag to
/// be taken for the end of whatever encloses it.
fn read_text(reader: &mut Reader<&[u8]>, end_tag: &str) -> Result<String, String> {
    let mut text = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => skip_through_end(reader, e.local_name().as_ref())?,
            Ok(Event::Text(ref e)) => {
                // SecureCRT exports declare `<?xml version="1.0"?>`, so we apply
                // XML 1.0 EOL normalization (CR/CRLF -> LF). This is a deliberate
                // change from quick-xml 0.39.2's `xml_content()`, which was an
                // alias for the XML 1.1 path that *also* collapses U+0085 (NEL)
                // and U+2028 (LS) to LF. Those code points have no business in
                // SecureCRT session text and never appear in real exports; using
                // XML 1.0 rules matches what the file's own version declaration
                // says it should be parsed under.
                text.push_str(&e.xml_content(quick_xml::XmlVersion::Implicit1_0));
            }
            Ok(Event::GeneralRef(ref e)) => {
                // quick-xml reports `&amp;`, `&#38;` and the like as separate
                // events between the text chunks around them; ignoring them
                // turned "dom&amp;user" into "domuser". SecureCRT declares no
                // DTD, so only character references and the five predefined
                // entities are well-formed here. Anything else is kept
                // verbatim rather than silently dropped.
                let name: &str = e;
                if let Ok(Some(ch)) = e.resolve_char_ref() {
                    text.push(ch);
                } else if let Some(resolved) = quick_xml::escape::resolve_xml_entity(name) {
                    text.push_str(resolved);
                } else {
                    text.push('&');
                    text.push_str(name);
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
fn skip_through_end(reader: &mut Reader<&[u8]>, tag: &str) -> Result<(), String> {
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
        if attr.key.as_ref() == name {
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

    // ── Buttons ─────────────────────────────────────────────────────

    /// A file with `blocks` at its top level.
    fn export(blocks: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><VanDyke version="3.0">{blocks}</VanDyke>"#
        )
    }

    fn button(name: &str, command: &str) -> ImportedButton {
        ImportedButton {
            name: name.to_string(),
            command: command.to_string(),
        }
    }

    /// The button an entry becomes, or the warning it is skipped with.
    fn entry(raw: &str) -> Result<ImportedButton, String> {
        parse_button_entry(raw, "button bar \"Default\"").expect("an assigned button")
    }

    #[test]
    fn parse_file_reads_sessions_button_bars_and_the_command_manager() {
        // Laid out as SecureCRT 9.7 exports them, with the blocks around them.
        let xml = export(
            r#"
            <string name="Product Version">Version 9.7.3</string>
            <key name="Global"><dword name="User Button Bar">1</dword></key>
            <key name="Sessions">
                <key name="host1">
                    <dword name="Is Session">1</dword>
                    <string name="Hostname">10.0.0.1</string>
                    <string name="Protocol Name">SSH2</string>
                    <string name="User Button Bar Name">Default</string>
                </key>
            </key>
            <key name="ButtonBarV5">
                <array name="Default">
                    <string>SEND,show version \\r,sh ver,,,0,1,,</string>
                    <string>SEND,show running-config \\r,sh run,,,0,3,,</string>
                </array>
                <array name="Empty"/>
                <array name="Cisco">
                    <string>SEND,sh ip int br\\r,ip br,,,0,4,</string>
                </array>
            </key>
            <key name="Commands">
                <key name="__Commands__">
                    <array name="Default">
                        <string>SEND,show version \\r,sh vers,,,0,1,,</string>
                    </array>
                </key>
            </key>
            <key name="SSH2"><string name="Identity Filename V2"/></key>"#,
        );
        let file = parse_file(&xml).unwrap();
        let (_, sessions, _, _) = file.sessions.unwrap();
        assert_eq!(sessions.len(), 1);
        let bars = file.button_bars.unwrap();
        assert_eq!(
            bars.buttons,
            [
                button("sh ver", "show version \\r"),
                button("sh run", "show running-config \\r"),
                button("ip br", "sh ip int br\\r"),
            ]
        );
        assert_eq!(bars.skipped, 0);
        assert_eq!(bars.warnings, Vec::<String>::new());
        let commands = file.command_manager.unwrap();
        assert_eq!(commands.buttons, [button("sh vers", "show version \\r")]);
    }

    #[test]
    fn parse_file_leaves_out_the_blocks_a_file_does_not_have() {
        // SecureCRT exports only what the user picked, so a file may hold
        // buttons and no sessions.
        let xml = export(
            r#"<key name="ButtonBarV5"><array name="Default">
                <string>SEND,uptime\\r,up,,,0,1,,</string>
            </array></key>"#,
        );
        let file = parse_file(&xml).unwrap();
        assert!(file.sessions.is_none());
        assert!(file.command_manager.is_none());
        assert_eq!(
            file.button_bars.unwrap().buttons,
            [button("up", "uptime\\r")]
        );
        // The sessions import still needs a Sessions block.
        assert!(parse(&xml).unwrap_err().contains("Sessions"));

        let empty = parse_file(&export("")).unwrap();
        assert!(empty.sessions.is_none() && empty.button_bars.is_none());
    }

    #[test]
    fn parse_file_reads_command_manager_folders() {
        // As SecureCRT 9.7 exports a folder: a key with a command list of its
        // own, written before the top-level list. Commands are taken in file
        // order, folders included, at any depth.
        let xml = export(
            r#"<key name="Commands">
                <key name="more commands">
                    <key name="__Commands__">
                        <array name="Default">
                            <string>SEND,show inventory \\r,sh inv,,,0,4,,</string>
                        </array>
                    </key>
                    <key name="deeper">
                        <dword name="Is Expanded">1</dword>
                        <key name="__Commands__">
                            <array name="Default"><string>SEND,b\\r,b,,,0,1,,</string></array>
                        </key>
                    </key>
                </key>
                <key name="__Commands__">
                    <array name="Default">
                        <string>SEND,show version \\r,sh vers,,,0,1,,</string>
                    </array>
                </key>
            </key>"#,
        );
        let commands = parse_file(&xml).unwrap().command_manager.unwrap();
        assert_eq!(
            commands.buttons,
            [
                button("sh inv", r"show inventory \r"),
                button("b", r"b\r"),
                button("sh vers", r"show version \r"),
            ]
        );
    }

    #[test]
    fn parse_file_reads_each_block_once_and_only_at_the_top_level() {
        let xml = export(
            r#"<key name="Global">
                <key name="ButtonBarV5"><array name="Nested">
                    <string>SEND,nested\\r,nested,,,0,1,,</string>
                </array></key>
            </key>
            <key name="ButtonBarV5"><array name="Default">
                <string>SEND,first\\r,first,,,0,1,,</string>
            </array></key>
            <key name="ButtonBarV5"><array name="Default">
                <string>SEND,second\\r,second,,,0,1,,</string>
            </array></key>"#,
        );
        let bars = parse_file(&xml).unwrap().button_bars.unwrap();
        assert_eq!(bars.buttons, [button("first", "first\\r")]);
    }

    #[test]
    fn parse_file_rejects_a_file_cut_off_in_a_button_block() {
        let head = r#"<?xml version="1.0"?><VanDyke version="3.0">"#;
        for cut in [
            format!(r#"{head}<key name="ButtonBarV5"><array name="Default">"#),
            format!(r#"{head}<key name="ButtonBarV5"><array name="D"><string>SEND,a"#),
            format!(r#"{head}<key name="Commands"><key name="__Commands__">"#),
        ] {
            assert!(parse_file(&cut).is_err(), "{cut}");
        }
    }

    #[test]
    fn parse_file_limits_the_entries_read_from_each_block() {
        let entries =
            r"<string>SEND,x\\r,x,,,0,1,,</string>".repeat(MAX_BUTTON_ENTRIES as usize + 5);
        let xml = export(&format!(
            r#"<key name="ButtonBarV5"><array name="Default">{entries}</array></key>
            <key name="Commands"><key name="__Commands__"><array name="Default">
                <string>SEND,y\\r,y,,,0,1,,</string>
            </array></key></key>"#
        ));
        let file = parse_file(&xml).unwrap();
        let bars = file.button_bars.unwrap();
        assert_eq!(bars.buttons.len(), MAX_BUTTON_ENTRIES as usize);
        assert_eq!(bars.skipped, 5);
        assert_eq!(bars.warnings.len(), 1);
        assert!(
            bars.warnings[0].starts_with("Skipped 5 more buttons"),
            "{bars:?}"
        );
        // The other block has a limit of its own.
        assert_eq!(file.command_manager.unwrap().buttons, [button("y", "y\\r")]);
    }

    #[test]
    fn button_fields_unescape_commas_and_backslashes() {
        // As SecureCRT 9.7 writes a comma in a command.
        assert_eq!(
            entry(r"SEND,show interface range g1/0/1 - 2 \, g1/0/5 \\r,sh range,,,0,6,,"),
            Ok(button(
                "sh range",
                r"show interface range g1/0/1 - 2 , g1/0/5 \r"
            ))
        );
        // And in a label, written the same way.
        assert_eq!(
            entry(r"SEND,a,int\, range,,,0,1,,").unwrap().name,
            "int, range"
        );
        // SecureCRT 8.x writes one field fewer; only the first three count.
        assert_eq!(
            entry(r"SEND,exit\\r,exit,,,0,10,"),
            Ok(button("exit", "exit\\r"))
        );
        // A dangling backslash is kept.
        assert_eq!(entry(r"SEND,a\\,b\"), Ok(button("b\\", "a\\")));
    }

    #[test]
    fn leading_fields_stops_after_the_third() {
        let [function, argument, label] = leading_fields(&format!("SEND,a,b{}", ",".repeat(10)));
        assert_eq!((&*function, &*argument, &*label), ("SEND", "a", "b"));
        let [function, argument, label] = leading_fields("SEND");
        assert_eq!((&*function, &*argument, &*label), ("SEND", "", ""));
    }

    #[test]
    fn button_labels_drop_access_key_markers() {
        assert_eq!(entry(r"SEND,a,&Reload,,,0,1,,").unwrap().name, "Reload");
        assert_eq!(entry(r"SEND,a,R&&D &,,,0,1,,").unwrap().name, "R&D");
        assert_eq!(entry(r"SEND,a,  padded  ,,,0,1,,").unwrap().name, "padded");
    }

    #[test]
    fn button_names_are_cut_to_the_command_bar_limit() {
        // 31 units and an emoji of two: the emoji does not fit, and is not split.
        let label = format!("{}😀tail", "x".repeat(MAX_BUTTON_NAME_LEN - 1));
        let name = entry(&format!("SEND,a,{label},,,0,1,,")).unwrap().name;
        assert_eq!(name, "x".repeat(MAX_BUTTON_NAME_LEN - 1));
        // A cut that ends on a space does not keep it.
        let label = format!("{} tail", "x".repeat(MAX_BUTTON_NAME_LEN - 1));
        let name = entry(&format!("SEND,a,{label},,,0,1,,")).unwrap().name;
        assert_eq!(name, "x".repeat(MAX_BUTTON_NAME_LEN - 1));
    }

    #[test]
    fn button_commands_over_the_limit_are_skipped_not_cut() {
        // Cutting a command would send something other than the button did.
        let at_limit = "x".repeat(MAX_BUTTON_COMMAND_LEN);
        assert!(entry(&format!("SEND,{at_limit},x,,,0,1,,")).is_ok());
        let over = "é".repeat(MAX_BUTTON_COMMAND_LEN + 1);
        let warning = entry(&format!("SEND,{over},x,,,0,1,,")).unwrap_err();
        assert!(warning.contains("longer than 1024"), "{warning}");
    }

    #[test]
    fn buttons_without_a_command_bar_equivalent_are_skipped_with_a_warning() {
        assert_eq!(
            entry("MENU_TOGGLE_KEYWORD_HIGHLIGHTING,,Highlight on/off,,,0,0,"),
            Err(
                "Skipped \"Highlight on/off\" in button bar \"Default\": its function, \
                 MENU_TOGGLE_KEYWORD_HIGHLIGHTING, has no command bar equivalent; only \
                 buttons that send a string can be imported"
                    .into()
            )
        );
        assert!(entry(r"RUN_SCRIPT,C:\\scripts\\a.py,script,,,0,0,")
            .unwrap_err()
            .contains("RUN_SCRIPT"));
        assert!(entry(",,label only,,,0,0,,")
            .unwrap_err()
            .contains("it has no function"));
        assert!(entry("SEND,,nothing,,,0,0,,")
            .unwrap_err()
            .ends_with("it sends nothing"));
        assert_eq!(
            entry("SEND,uptime,,,,0,0,,"),
            Err(
                "Skipped a button without a label in button bar \"Default\": a command bar \
                 button needs one"
                    .into()
            )
        );
    }

    #[test]
    fn warnings_quote_names_from_the_file_cut() {
        // A bar name goes into the warning of every skipped entry on the bar;
        // uncut, a long one would be copied once per entry.
        let long = "n".repeat(100_000);
        // Long, but short enough for the entry to be read.
        let field = "n".repeat(10_000);
        let entries = format!("<string>{field},,{field},,,0,0,</string>").repeat(3);
        let xml = export(&format!(
            r#"<key name="ButtonBarV5"><array name="{long}">{entries}</array></key>"#
        ));
        let bars = parse_file(&xml).unwrap().button_bars.unwrap();
        assert_eq!(bars.skipped, 3);
        let cut = format!("{}…", "n".repeat(MAX_QUOTED_LEN));
        for warning in &bars.warnings {
            let len = warning.chars().count();
            assert!(len < 400, "{len} characters");
            assert_eq!(warning.matches(&cut).count(), 3, "{warning}");
        }
        // A name at the limit is quoted whole.
        let at_limit = "n".repeat(MAX_QUOTED_LEN);
        assert_eq!(quoted(&at_limit), at_limit);
    }

    #[test]
    fn session_warnings_quote_key_names_cut() {
        // Each nameless element in a key is a warning naming the key, and the
        // parse allows tens of thousands of warnings.
        let long = "k".repeat(100_000);
        let xml = wrap(&format!(
            r#"<key name="{long}">
                {}<key><key name="s"/></key>
                <dword name="Is Session">1</dword>
                <string name="Protocol Name">{long}</string>
                <string name="Hostname">h</string>
            </key>"#,
            "<string>x</string><string/>".repeat(3)
        ));
        let (_, sessions, skipped, warnings) = parse(&xml).unwrap();
        assert!(sessions.is_empty());
        assert_eq!(skipped.len(), 1);
        // Three of each nameless string, the nameless key, the protocol.
        assert_eq!(warnings.len(), 8);
        let cut = format!("{}…", "k".repeat(MAX_QUOTED_LEN));
        for warning in &warnings {
            let len = warning.chars().count();
            assert!(len < 300, "{len} characters");
            assert!(warning.contains(&cut), "{warning}");
        }
    }

    #[test]
    fn a_nested_string_cannot_end_its_block_early() {
        // Read as text, the inner `</string>` ended the entry and the outer
        // one the array, so the rest of the block was read as top level, and
        // a `Sessions` key inside it as the file's sessions.
        let xml = export(
            r#"<key name="ButtonBarV5">
                <array name="Default">
                    <string><string>SEND,x,inner,,,0,1,,</string>SEND,b\\r,b,,,0,1,,</string>
                    <string>SEND,c\\r,c,,,0,1,,</string>
                </array>
                <key name="Sessions">
                    <key name="s">
                        <dword name="Is Session">1</dword>
                        <string name="Hostname">h</string>
                        <string name="Protocol Name">SSH2</string>
                    </key>
                </key>
            </key>"#,
        );
        let file = parse_file(&xml).unwrap();
        assert!(file.sessions.is_none());
        assert_eq!(
            file.button_bars.unwrap().buttons,
            [button("b", "b\\r"), button("c", "c\\r")]
        );
    }

    #[test]
    fn button_entries_too_long_to_import_are_skipped_whole() {
        let label = "x".repeat(MAX_BUTTON_ENTRY_BYTES);
        let xml = export(&format!(
            r#"<key name="ButtonBarV5"><array name="Default">
                <string>SEND,a\\r,{label},,,0,1,,</string>
            </array></key>"#
        ));
        let bars = parse_file(&xml).unwrap().button_bars.unwrap();
        assert_eq!(bars.buttons, Vec::<ImportedButton>::new());
        assert_eq!(bars.skipped, 1);
        assert_eq!(
            bars.warnings,
            [
                "Skipped an entry in button bar \"Default\": it is longer than any button that \
              can be imported"
            ]
        );
        // Just under the limit, the label is cut instead, as any long one is.
        let label = "x".repeat(MAX_BUTTON_ENTRY_BYTES - 20);
        let fits = entry(&format!("SEND,a\\\\r,{label},,,0,1,,"));
        assert_eq!(fits, Ok(button(&"x".repeat(MAX_BUTTON_NAME_LEN), "a\\r")));
    }

    #[test]
    fn parse_file_limits_command_manager_nesting() {
        // `Commands` is depth 0; each folder in it one more.
        let nested = |depth: usize| {
            export(&format!(
                r#"<key name="Commands">{}{}</key>"#,
                r#"<key name="f">"#.repeat(depth),
                "</key>".repeat(depth)
            ))
        };
        assert!(parse_file(&nested(MAX_NESTING_DEPTH - 1)).is_ok());
        let err = parse_file(&nested(MAX_NESTING_DEPTH)).unwrap_err();
        assert!(err.contains("nesting"), "{err}");
    }

    #[test]
    fn unassigned_entries_do_not_count_toward_the_limit() {
        let padding = "<string>,,,,,0,0,,</string><string/>".repeat(MAX_BUTTON_ENTRIES as usize);
        let xml = export(&format!(
            r#"<key name="ButtonBarV5"><array name="Default">{padding}
                <string>SEND,x\\r,x,,,0,1,,</string>
            </array></key>"#
        ));
        let bars = parse_file(&xml).unwrap().button_bars.unwrap();
        assert_eq!(bars.buttons, [button("x", "x\\r")]);
        assert_eq!(bars.skipped, 0);
    }

    #[test]
    fn unassigned_buttons_are_passed_over() {
        assert_eq!(parse_button_entry("", "here"), None);
        assert_eq!(parse_button_entry(",,,,,0,0,,", "here"), None);
        assert_eq!(parse_button_entry(" ,, & ,,,0,0,,", "here"), None);
    }

    #[test]
    fn send_strings_keep_the_escapes_the_command_bar_shares() {
        let shared = r"ena\rpass\p\v\t\b\e\n\\x";
        assert_eq!(convert_send_string(shared), Ok(shared.to_string()));
        // Escapes neither knows are sent as they are, by both.
        assert_eq!(convert_send_string(r"a\qb\"), Ok(r"a\qb\".to_string()));
        assert_eq!(convert_send_string("überall"), Ok("überall".to_string()));
    }

    #[test]
    fn send_string_control_codes_are_kept_as_written() {
        // The command bar reads \000 to \037 and \177 as SecureCRT does. The
        // inputs match "reads the octal range's edges as the SecureCRT import
        // checks them" in src/components/CommandBar.test.tsx.
        for raw in [
            r"\033[A\015\012\011\010",
            r"\000\003\004\037\177",
            r"\000\177\\\003\8\0é\",
            // An escaped backslash before digits is no octal code, nor are
            // fewer than three digits.
            r"\\012\\\033\01x\8",
        ] {
            assert_eq!(convert_send_string(raw), Ok(raw.to_string()));
        }
        assert_eq!(
            entry(r"SEND,\\003,Ctrl-C,,,0,1,,"),
            Ok(button("Ctrl-C", r"\003"))
        );
    }

    #[test]
    fn send_string_printable_codes_are_written_out() {
        // A command spelled in codes would show as digits in the import
        // dialog while it types text; written out, it reads as what it types.
        assert_eq!(
            convert_send_string(r"\162\155\040\055\162\146\040\057\015"),
            Ok(r"rm -rf /\015".to_string())
        );
        assert_eq!(
            convert_send_string(r"\101\176\040x"),
            Ok("A~ x".to_string())
        );
        // A backslash stays one: written out, it is escaped, so it neither
        // starts an escape nor joins the next character into one.
        assert_eq!(
            convert_send_string(r"\134r\134\134"),
            Ok(r"\\r\\\\".to_string())
        );
        // Written-out digits are fine where they cannot complete a code.
        assert_eq!(
            convert_send_string(r"\\0\061\062"),
            Ok(r"\\012".to_string())
        );
        assert_eq!(convert_send_string(r"\x\061\062"), Ok(r"\x12".to_string()));
    }

    #[test]
    fn send_string_codes_that_would_run_together_skip_the_button() {
        // `\0` and `\01` are no codes, but with the digits after them written
        // out they would become `\012`, a line feed SecureCRT never sends.
        // `\1\067\067` would likewise become `\177`, DEL.
        for raw in [r"\0\061\062", r"\01\062", r"\0\0612", r"\1\067\067"] {
            assert_eq!(
                convert_send_string(raw),
                Err(Unsendable::Ambiguous),
                "{raw}"
            );
        }
        // Completing a code the command bar does not read is no harm: it
        // sends the code as written, and SecureCRT, reading the backslash
        // before it as no code, sends the same text.
        assert_eq!(convert_send_string(r"\1\061\061"), Ok(r"\111".to_string()));
        assert_eq!(
            convert_send_string(r"\7\067\067x"),
            Ok(r"\777x".to_string())
        );
        assert!(entry(r"SEND,\\0\\061\\062,lf,,,0,1,,")
            .unwrap_err()
            .ends_with("would run into the characters around them and send something else"));
    }

    #[test]
    fn sent_reads_a_command_as_the_command_bar_or_secure_crt_does() {
        let ascii = |code: u8| Sent::Char(char::from(code));
        assert_eq!(
            sent(r"a\r\p\v\\\003\101\q\", CodesRead::Control),
            [
                Sent::Char('a'),
                ascii(13),
                Sent::Pause,
                Sent::Paste,
                ascii(92),
                ascii(3),
                ascii(92),
                Sent::Char('1'),
                Sent::Char('0'),
                Sent::Char('1'),
                ascii(92),
                Sent::Char('q'),
                ascii(92),
            ]
        );
        assert_eq!(sent(r"\101", CodesRead::Ascii), [Sent::Char('A')]);
    }

    #[test]
    fn send_string_raw_bytes_the_command_bar_cannot_send_skip_the_button() {
        // Above \177, SecureCRT sends one raw byte; text cannot carry it.
        for (raw, code) in [(r"\200", 0o200), (r"ok\003\240", 0o240), (r"\777", 0o777)] {
            assert_eq!(convert_send_string(raw), Err(Unsendable::RawByte(code)));
        }
        // Escaped, it is plain text.
        assert_eq!(convert_send_string(r"\\240"), Ok(r"\\240".to_string()));
        assert_eq!(
            entry(r"SEND,\\240,nbsp,,,0,1,,"),
            Err(
                "Skipped \"nbsp\" in button bar \"Default\": it sends character code \
                 \\240, which a command bar button cannot send"
                    .into()
            )
        );
    }
}
