mod database;
mod pty;
mod session;
mod ssh;
mod telnet;

pub use database::*;
pub use pty::*;
pub use session::*;
pub use ssh::*;
pub use telnet::*;

/// Maximum terminal dimensions to prevent resource exhaustion.
const MAX_COLS: u16 = 500;
const MAX_ROWS: u16 = 500;

/// Maximum bytes per single write call.
const MAX_WRITE_SIZE: usize = 65536;

/// Maximum number of jump host hops.
const MAX_JUMP_HOPS: usize = 10;

fn validate_dimensions(cols: u16, rows: u16) -> Result<(), String> {
    if cols == 0 || rows == 0 || cols > MAX_COLS || rows > MAX_ROWS {
        return Err(format!(
            "Invalid terminal dimensions: cols and rows must be 1\u{2013}{MAX_COLS}/{MAX_ROWS}"
        ));
    }
    Ok(())
}

fn validate_port(port: i32) -> Result<(), String> {
    if !(1..=65535).contains(&port) {
        return Err(format!("Invalid port {port}: must be 1\u{2013}65535"));
    }
    Ok(())
}

/// Maximum length for session string fields to prevent resource exhaustion.
const MAX_NAME_LEN: usize = 255;
const MAX_HOSTNAME_LEN: usize = 255;
const MAX_USERNAME_LEN: usize = 128;
const MAX_TAGS_LEN: usize = 1024;

fn validate_session_fields(
    name: Option<&str>,
    hostname: Option<&str>,
    tags: Option<&str>,
) -> Result<(), String> {
    if let Some(v) = name {
        if v.len() > MAX_NAME_LEN {
            return Err(format!("Name too long (max {MAX_NAME_LEN} characters)"));
        }
    }
    if let Some(v) = hostname {
        if v.len() > MAX_HOSTNAME_LEN {
            return Err(format!(
                "Hostname too long (max {MAX_HOSTNAME_LEN} characters)"
            ));
        }
    }
    if let Some(v) = tags {
        if v.len() > MAX_TAGS_LEN {
            return Err(format!("Tags too long (max {MAX_TAGS_LEN} characters)"));
        }
    }
    Ok(())
}
