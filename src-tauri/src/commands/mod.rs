mod pty;
mod session;
mod ssh;

pub use pty::*;
pub use session::*;
pub use ssh::*;

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
