//! Crate-wide numeric limits shared across modules.
//!
//! Constants live here when they are read by code in a sibling top-level
//! module (e.g. `ssh::` or `telnet::` reading a value that conceptually
//! belongs to IPC argument bounds). Limits that are only used inside one
//! module should stay private to that module.

/// Maximum spawned, not-yet-completed write tasks per session. Bounds the
/// memory a misbehaving or runaway frontend can pin by issuing IPC writes
/// faster than the writer task can drain. Two independent worst-case windows
/// stack: up to `MAX_INFLIGHT_WRITES` spawned tasks each owning up to
/// `MAX_WRITE_SIZE` bytes (= 4 MiB), plus up to a full bounded write channel
/// (64 slots × 64 KiB = another 4 MiB). Theoretical maximum per session is
/// therefore ~8 MiB; across the `MAX_SESSIONS` cap of 100, ~800 MiB.
pub const MAX_INFLIGHT_WRITES: usize = 64;
