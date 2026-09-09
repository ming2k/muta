//! Pure Ethernet/IPv4/TCP parsing. No sockets, no unsafe — so it can be tested
//! with hand-built frames.
//!
//! Only what the probe needs is parsed: the 4-tuple (to filter), and the TCP
//! header fields that describe the segment (flags, sequence, payload length).
//! Anything else — IPv6, IP options, VLAN tags, fragmentation — is reported as
//! `None` rather than guessed at.

/// `FIN`.
pub const FLAG_FIN: u8 = 0x01;
/// `SYN`.
pub const FLAG_SYN: u8 = 0x02;
/// `RST`.
pub const FLAG_RST: u8 = 0x04;
/// `PSH`.
pub const FLAG_PSH: u8 = 0x08;
/// `ACK`.
pub const FLAG_ACK: u8 = 0x10;

const ETHERTYPE_IPV4: u16 = 0x0800;
const IPPROTO_TCP: u8 = 6;
const ETH_HEADER_LEN: usize = 14;

/// A parsed TCP segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcpSegment {
    pub source: ([u8; 4], u16),
    pub destination: ([u8; 4], u16),
    pub seq: u32,
    pub ack: u32,
    pub flags: u8,
    pub payload_len: u32,
}

impl TcpSegment {
    /// Whether this segment carried application data.
    pub const fn carries_data(&self) -> bool {
        self.payload_len > 0
    }

    /// Whether this segment is a bare acknowledgement.
    pub const fn is_bare_ack(&self) -> bool {
        self.payload_len == 0 && self.flags & FLAG_ACK != 0
    }
}

/// Parse one Ethernet frame into a TCP segment.
///
/// `None` for anything the probe does not model: a non-IPv4 frame, a
/// non-TCP transport, a fragmented datagram, or a frame too short to contain
/// the headers it claims.
pub fn parse_ethernet_ipv4_tcp(frame: &[u8]) -> Option<TcpSegment> {
    if frame.len() < ETH_HEADER_LEN {
        return None;
    }
    // A VLAN tag shifts the network header by 4 bytes.
    let mut offset = ETH_HEADER_LEN;
    let mut ethertype = u16::from_be_bytes([frame[12], frame[13]]);
    if ethertype == 0x8100 || ethertype == 0x88A8 {
        if frame.len() < offset + 4 {
            return None;
        }
        ethertype = u16::from_be_bytes([frame[16], frame[17]]);
        offset += 4;
    }
    if ethertype != ETHERTYPE_IPV4 {
        return None;
    }
    if frame.len() < offset + 20 {
        return None;
    }
    let ip = &frame[offset..];
    let version = ip[0] >> 4;
    if version != 4 {
        return None;
    }
    let header_len = usize::from(ip[0] & 0x0F) * 4;
    if header_len < 20 || ip.len() < header_len {
        return None;
    }
    // Fragmented datagrams have no complete transport header in this frame.
    let fragment_offset = u16::from_be_bytes([ip[6], ip[7]]) & 0x1FFF;
    if fragment_offset != 0 {
        return None;
    }
    if ip[9] != IPPROTO_TCP {
        return None;
    }
    let total_len = usize::from(u16::from_be_bytes([ip[2], ip[3]]));
    let source_ip = [ip[12], ip[13], ip[14], ip[15]];
    let destination_ip = [ip[16], ip[17], ip[18], ip[19]];

    let tcp = ip.get(header_len..)?;
    if tcp.len() < 20 {
        return None;
    }
    let source_port = u16::from_be_bytes([tcp[0], tcp[1]]);
    let destination_port = u16::from_be_bytes([tcp[2], tcp[3]]);
    let seq = u32::from_be_bytes([tcp[4], tcp[5], tcp[6], tcp[7]]);
    let ack = u32::from_be_bytes([tcp[8], tcp[9], tcp[10], tcp[11]]);
    let data_offset = usize::from(tcp[12] >> 4) * 4;
    if data_offset < 20 || tcp.len() < data_offset {
        return None;
    }
    let flags = tcp[13];

    // Trust the IP total length when it is consistent with the frame; a NIC can
    // pad a frame to the Ethernet minimum, and padding is not payload.
    let ip_available = ip.len().min(total_len.max(header_len));
    let payload_len = ip_available.saturating_sub(header_len + data_offset);

    Some(TcpSegment {
        source: (source_ip, source_port),
        destination: (destination_ip, destination_port),
        seq,
        ack,
        flags,
        payload_len: payload_len as u32,
    })
}

/// Whether `segment` belongs to the connection identified by `local` and
/// `remote` (either direction).
pub fn matches_flow(
    segment: &TcpSegment,
    local: ([u8; 4], u16),
    remote: ([u8; 4], u16),
) -> Option<bool> {
    if segment.source == local && segment.destination == remote {
        Some(true)
    } else if segment.source == remote && segment.destination == local {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an Ethernet + IPv4 + TCP frame with the given payload.
    fn frame(
        source: ([u8; 4], u16),
        destination: ([u8; 4], u16),
        flags: u8,
        payload: &[u8],
    ) -> Vec<u8> {
        let tcp_len = 20 + payload.len();
        let ip_len = 20 + tcp_len;
        let mut frame = vec![0u8; ETH_HEADER_LEN + ip_len];
        // Ethernet
        frame[0..6].copy_from_slice(&[0x02, 0, 0, 0, 0, 1]);
        frame[6..12].copy_from_slice(&[0x02, 0, 0, 0, 0, 2]);
        frame[12..14].copy_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
        // IPv4
        let ip = &mut frame[ETH_HEADER_LEN..];
        ip[0] = 0x45;
        ip[2..4].copy_from_slice(&(ip_len as u16).to_be_bytes());
        ip[8] = 64;
        ip[9] = IPPROTO_TCP;
        ip[12..16].copy_from_slice(&source.0);
        ip[16..20].copy_from_slice(&destination.0);
        // TCP
        let tcp = &mut ip[20..];
        tcp[0..2].copy_from_slice(&source.1.to_be_bytes());
        tcp[2..4].copy_from_slice(&destination.1.to_be_bytes());
        tcp[4..8].copy_from_slice(&1234u32.to_be_bytes());
        tcp[8..12].copy_from_slice(&5678u32.to_be_bytes());
        tcp[12] = 0x50; // 20-byte data offset
        tcp[13] = flags;
        tcp[20..].copy_from_slice(payload);
        frame
    }

    #[test]
    fn a_data_segment_parses_with_its_flow_and_payload_length() {
        let bytes = frame(
            ([127, 0, 0, 1], 51_234),
            ([127, 0, 0, 1], 443),
            FLAG_PSH | FLAG_ACK,
            b"data: hello\n\n",
        );
        let segment = parse_ethernet_ipv4_tcp(&bytes).expect("parse");
        assert_eq!(segment.source, ([127, 0, 0, 1], 51_234));
        assert_eq!(segment.destination, ([127, 0, 0, 1], 443));
        assert_eq!(segment.flags, FLAG_PSH | FLAG_ACK);
        assert_eq!(segment.payload_len, 13);
        assert_eq!(
            matches_flow(&segment, ([127, 0, 0, 1], 51_234), ([127, 0, 0, 1], 443)),
            Some(true)
        );
        assert_eq!(
            matches_flow(&segment, ([127, 0, 0, 1], 443), ([127, 0, 0, 1], 51_234)),
            Some(false)
        );
        assert_eq!(
            matches_flow(&segment, ([127, 0, 0, 1], 9), ([127, 0, 0, 1], 443)),
            None
        );
    }

    #[test]
    fn a_bare_ack_has_no_payload() {
        let bytes = frame(([10, 0, 0, 1], 443), ([10, 0, 0, 2], 51_234), FLAG_ACK, &[]);
        let segment = parse_ethernet_ipv4_tcp(&bytes).expect("parse");
        assert_eq!(segment.payload_len, 0);
        assert!(segment.is_bare_ack());
        assert!(!segment.carries_data());
    }

    #[test]
    fn ethernet_padding_is_not_counted_as_payload() {
        // A 60-byte frame: the NIC pads the tail, the IP length does not.
        let mut bytes = frame(
            ([10, 0, 0, 1], 443),
            ([10, 0, 0, 2], 51_234),
            FLAG_ACK,
            b"hi",
        );
        bytes.resize(60, 0);
        let segment = parse_ethernet_ipv4_tcp(&bytes).expect("parse");
        assert_eq!(
            segment.payload_len, 2,
            "padding must not inflate the payload"
        );
    }

    #[test]
    fn ip_options_and_tcp_options_shift_the_payload_correctly() {
        let mut bytes = frame(
            ([10, 0, 0, 1], 443),
            ([10, 0, 0, 2], 51_234),
            FLAG_ACK,
            b"payload",
        );
        // Give the TCP header 4 bytes of options and adjust both lengths.
        let tcp_start = ETH_HEADER_LEN + 20;
        let mut new = Vec::new();
        new.extend_from_slice(&bytes[..tcp_start]);
        new.extend_from_slice(&bytes[tcp_start..tcp_start + 12]);
        new.push(0x60); // data offset 6 words = 24 bytes
        new.extend_from_slice(&bytes[tcp_start + 13..tcp_start + 20]);
        new.extend_from_slice(&[0x01, 0x01, 0x01, 0x01]); // NOP padding
        new.extend_from_slice(&bytes[tcp_start + 20..]);
        bytes = new;
        let ip_len = (bytes.len() - ETH_HEADER_LEN) as u16;
        bytes[ETH_HEADER_LEN + 2..ETH_HEADER_LEN + 4].copy_from_slice(&ip_len.to_be_bytes());
        let segment = parse_ethernet_ipv4_tcp(&bytes).expect("parse");
        assert_eq!(segment.payload_len, 7);
    }

    #[test]
    fn a_vlan_tagged_frame_is_unwrapped() {
        let plain = frame(
            ([10, 0, 0, 1], 443),
            ([10, 0, 0, 2], 51_234),
            FLAG_ACK,
            b"x",
        );
        let mut tagged = Vec::new();
        tagged.extend_from_slice(&plain[..12]);
        tagged.extend_from_slice(&0x8100u16.to_be_bytes());
        tagged.extend_from_slice(&[0x00, 0x64]); // VLAN id 100
        tagged.extend_from_slice(&plain[12..]);
        let segment = parse_ethernet_ipv4_tcp(&tagged).expect("parse");
        assert_eq!(segment.payload_len, 1);
    }

    #[test]
    fn frames_we_do_not_model_are_refused_rather_than_guessed() {
        // Truncated Ethernet.
        assert_eq!(parse_ethernet_ipv4_tcp(&[0u8; 10]), None);
        // IPv6.
        let mut ipv6 = frame(([10, 0, 0, 1], 443), ([10, 0, 0, 2], 51_234), FLAG_ACK, b"");
        ipv6[12..14].copy_from_slice(&0x86DDu16.to_be_bytes());
        assert_eq!(parse_ethernet_ipv4_tcp(&ipv6), None);
        // Non-TCP.
        let mut udp = frame(([10, 0, 0, 1], 443), ([10, 0, 0, 2], 51_234), FLAG_ACK, b"");
        udp[ETH_HEADER_LEN + 9] = 17;
        assert_eq!(parse_ethernet_ipv4_tcp(&udp), None);
        // A non-first fragment carries no transport header.
        let mut fragmented = frame(([10, 0, 0, 1], 443), ([10, 0, 0, 2], 51_234), FLAG_ACK, b"");
        fragmented[ETH_HEADER_LEN + 6] = 0x00;
        fragmented[ETH_HEADER_LEN + 7] = 0x08;
        assert_eq!(parse_ethernet_ipv4_tcp(&fragmented), None);
    }
}
