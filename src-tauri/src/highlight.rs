use serde::{Deserialize, Serialize};

/// A single keyword highlight rule within a profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HighlightRule {
    pub pattern: String,
    pub color: String,
    pub case_sensitive: bool,
    pub bold: bool,
}

/// Parsed result from a SecureCRT keyword highlight INI file.
pub struct ParsedHighlightProfile {
    pub name: String,
    pub rules: Vec<HighlightRule>,
}

/// Maximum size for imported INI files (10 MB).
const MAX_INI_SIZE: usize = 10 * 1024 * 1024;

/// Parse a SecureCRT keyword highlight INI file.
///
/// The format is:
/// ```text
/// D:"Match Case"=00000001
/// S:"List Name"=Cisco
/// Z:"Keyword List V2"=00000033
///  "regex_pattern",BBGGRR,00000001
/// ```
pub fn parse_securecrt_highlight_ini(content: &str) -> Result<Vec<ParsedHighlightProfile>, String> {
    if content.len() > MAX_INI_SIZE {
        return Err("File too large (max 10 MB)".to_string());
    }

    let mut profiles = Vec::new();
    let mut name = String::new();
    let mut case_sensitive = true;
    let mut rules = Vec::new();
    let mut in_keyword_list = false;
    let mut remaining_rules: usize = 0;

    for line in content.lines() {
        let trimmed = line.trim();

        // Profile name
        if let Some(val) = extract_string_field(trimmed, "List Name") {
            // If we already accumulated rules, save the previous profile.
            if !name.is_empty() && !rules.is_empty() {
                profiles.push(ParsedHighlightProfile {
                    name: std::mem::take(&mut name),
                    rules: std::mem::take(&mut rules),
                });
            }
            name = val;
            in_keyword_list = false;
            continue;
        }

        // Match case flag (per-profile)
        if let Some(val) = extract_dword_field(trimmed, "Match Case") {
            case_sensitive = val != 0;
            continue;
        }

        // Start of keyword list (V2 or V3)
        if trimmed.starts_with("Z:\"Keyword List V") {
            if let Some(eq_pos) = trimmed.find('=') {
                let hex = &trimmed[eq_pos + 1..];
                remaining_rules = usize::from_str_radix(hex.trim(), 16).unwrap_or(0);
                in_keyword_list = true;
            }
            continue;
        }

        // Parse rule lines (indented with a space)
        if in_keyword_list && remaining_rules > 0 && line.starts_with(' ') {
            if let Some(rule) = parse_rule_line(trimmed, case_sensitive) {
                rules.push(rule);
            }
            remaining_rules -= 1;
            if remaining_rules == 0 {
                in_keyword_list = false;
            }
            continue;
        }
    }

    // Save the last profile.
    if !name.is_empty() && !rules.is_empty() {
        profiles.push(ParsedHighlightProfile { name, rules });
    }

    if profiles.is_empty() {
        return Err("No highlight profiles found in file".to_string());
    }

    Ok(profiles)
}

/// Extract a `S:"<key>"=<value>` string field.
fn extract_string_field(line: &str, key: &str) -> Option<String> {
    let prefix = format!("S:\"{key}\"=");
    if line.starts_with(&prefix) {
        Some(line[prefix.len()..].to_string())
    } else {
        None
    }
}

/// Extract a `D:"<key>"=<hex>` dword field.
fn extract_dword_field(line: &str, key: &str) -> Option<u32> {
    let prefix = format!("D:\"{key}\"=");
    if line.starts_with(&prefix) {
        let hex = &line[prefix.len()..];
        u32::from_str_radix(hex.trim(), 16).ok()
    } else {
        None
    }
}

/// Parse a single rule line: `"pattern",BBGGRR,flags[,flags2]`
fn parse_rule_line(line: &str, case_sensitive: bool) -> Option<HighlightRule> {
    // Find the pattern between quotes.
    let start = line.find('"')?;
    let end = line[start + 1..].rfind('"')? + start + 1;
    let pattern = &line[start + 1..end];

    // Remaining after closing quote: ,BBGGRR,flags[,flags2]
    let rest = &line[end + 1..];
    let parts: Vec<&str> = rest.split(',').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }

    let color_bgr = parts[0].trim();
    let color = bgr_to_rgb_hex(color_bgr)?;

    Some(HighlightRule {
        pattern: pattern.to_string(),
        color,
        case_sensitive,
        bold: false,
    })
}

/// Convert BBGGRR hex string to #RRGGBB.
fn bgr_to_rgb_hex(bgr: &str) -> Option<String> {
    if bgr.len() < 6 {
        return None;
    }
    // Pad to 8 chars (some values may have leading zeros removed).
    let padded = format!("{:0>8}", bgr);
    // Format is 00BBGGRR (Windows COLORREF).
    let bb = &padded[2..4];
    let gg = &padded[4..6];
    let rr = &padded[6..8];
    Some(format!("#{rr}{gg}{bb}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgr_conversion() {
        assert_eq!(bgr_to_rgb_hex("0000ff00").as_deref(), Some("#00ff00"));
        assert_eq!(bgr_to_rgb_hex("001a1aff").as_deref(), Some("#ff1a1a"));
        assert_eq!(bgr_to_rgb_hex("00ffff00").as_deref(), Some("#00ffff"));
        assert_eq!(bgr_to_rgb_hex("00ff8000").as_deref(), Some("#0080ff"));
    }

    #[test]
    fn parse_rule() {
        let rule = parse_rule_line(r#""(?:up|UP|FULL)",0000ff00,00000001"#, true).unwrap();
        assert_eq!(rule.pattern, "(?:up|UP|FULL)");
        assert_eq!(rule.color, "#00ff00");
        assert!(rule.case_sensitive);
    }

    #[test]
    fn parse_ini_basic() {
        let ini = r#"D:"Match Case"=00000001
D:"Regex Line Mode"=00000001
S:"List Name"=Test
Z:"Keyword List V2"=00000002
 "up|UP",0000ff00,00000001
 "down|DOWN",001a1aff,00000001
"#;
        let profiles = parse_securecrt_highlight_ini(ini).unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "Test");
        assert_eq!(profiles[0].rules.len(), 2);
        assert_eq!(profiles[0].rules[0].color, "#00ff00");
        assert_eq!(profiles[0].rules[1].color, "#ff1a1a");
    }
}
