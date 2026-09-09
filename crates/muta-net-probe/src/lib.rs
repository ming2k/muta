//! `muta-net-probe`: the opt-in L2 segment capture of ADR-0200.
//!
//! This is a **separate binary**, not a daemon capability. Reading segments
//! needs `CAP_NET_RAW`, and the always-running process must never carry
//! packet-sniffing authority; an operator who wants segment-level truth runs
//! this tool explicitly, for one 4-tuple, for a bounded window.
//!
//! What it produces is the one thing no HTTP-level library can: a per-segment
//! arrival timeline (`t`, direction, length, TCP flags, sequence number) that
//! can be compared against the syscall tap's read boundaries. When they agree,
//! the tap is faithful; when they diverge, the transport is batching and the
//! derived rates must say so.
//!
//! The parser is pure and unit-tested; only [`capture`] touches a socket, and it
//! compiles out on non-Linux.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod packet;

#[cfg(target_os = "linux")]
pub mod capture;

/// One captured segment, as the probe reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    /// Monotonic nanoseconds since the probe started.
    pub at_ns: u64,
    /// `true` for segments travelling toward the peer.
    pub outbound: bool,
    /// TCP payload length (0 for a bare ACK).
    pub payload_len: u32,
    /// TCP header flags.
    pub flags: u8,
    /// TCP sequence number.
    pub seq: u32,
    /// TCP acknowledgement number.
    pub ack: u32,
}

impl Segment {
    /// Whether this segment carried application data.
    pub const fn carries_data(&self) -> bool {
        self.payload_len > 0
    }

    /// Whether this segment is a bare acknowledgement.
    pub const fn is_bare_ack(&self) -> bool {
        self.payload_len == 0 && self.flags & packet::FLAG_ACK != 0
    }
}
