//! RFC 4960 §5 — the four-way handshake, and basic `DATA`/`SACK` reliable
//! transfer on top of it. Sans-io: [`Association`] owns no socket and no
//! clock, the same shape `vaco-protocol-srt`/`vaco-protocol-rist` use —
//! a caller drives it with received packets and reads generated packets
//! back out.
//!
//! **Scope, stated up front.** Built: the four-way handshake
//! (`INIT`/`INIT ACK`/`COOKIE ECHO`/`COOKIE ACK`) and cumulative-TSN-only
//! `DATA`/`SACK` acknowledgement (no gap-ack-block tracking for
//! out-of-order arrivals, even though [`crate::chunk::SackChunk`] itself
//! can carry them — nothing here fills them in yet). **Not built**:
//! multi-homing, PR-SCTP partial reliability, congestion control, replay
//! protection on the state cookie (§5.1.3's own recommendation is an
//! HMAC-authenticated cookie so an attacker cannot replay or forge one to
//! exhaust server resources — this crate's cookie is the peer's own tag/
//! TSN values with no authentication tag, a real gap for production use,
//! named rather than hidden), and the shutdown sequence's timers/
//! retransmission (the three fixed messages `SHUTDOWN`/`SHUTDOWN ACK`/
//! `SHUTDOWN COMPLETE` are chunk types [`crate::chunk`] can build, but
//! this state machine does not drive a shutdown handshake).

use crate::chunk::{Chunk, DataChunk, DataFlags, GapAckBlock, InitAckChunk, InitChunk, SackChunk, pad_to_4};
use crate::packet::{CommonHeader, build_with_checksum, verify_checksum};
use vaco_protocol_core::{ProtocolError, Result};

const SCHEME: &str = "sctp";

fn malformed(detail: &'static str) -> ProtocolError {
    ProtocolError::Malformed { scheme: SCHEME, detail }
}

/// `IMPLEMENTATION-DEFINED`: RFC 4960 names no required advertised
/// receiver window, only that it "SHOULD" reflect real buffer space.
pub const DEFAULT_ADVERTISED_RECEIVER_WINDOW: u32 = 131_072;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Closed,
    CookieWait,
    CookieEchoed,
    Established,
}

#[derive(Debug, Clone, Copy)]
enum Role {
    Client,
    Server,
}

/// Cumulative-TSN-only receive tracking for one direction — see this
/// module's own scope note on why out-of-order arrivals are not folded
/// into gap-ack blocks.
#[derive(Debug, Default)]
struct ReceiveState {
    cumulative_tsn_ack: Option<u32>,
    received_data: Vec<Vec<u8>>,
}

impl ReceiveState {
    fn on_data(&mut self, tsn: u32, user_data: &[u8]) {
        let is_next = match self.cumulative_tsn_ack {
            None => true,
            Some(cum) => tsn == cum.wrapping_add(1),
        };
        if is_next {
            self.cumulative_tsn_ack = Some(tsn);
            self.received_data.push(user_data.to_vec());
        }
        // An out-of-order or duplicate TSN is silently not acknowledged
        // by this simplified tracker — see the module scope note.
    }
}

/// One SCTP association, driven sans-io.
#[derive(Debug)]
pub struct Association {
    role: Role,
    state: State,
    local_port: u16,
    peer_port: u16,
    local_verification_tag: u32,
    peer_verification_tag: u32,
    local_initial_tsn: u32,
    next_tsn: u32,
    peer_initial_tsn: Option<u32>,
    receive: ReceiveState,
}

impl Association {
    #[must_use]
    pub fn new_client(local_port: u16, peer_port: u16, local_verification_tag: u32, local_initial_tsn: u32) -> Self {
        Self {
            role: Role::Client,
            state: State::Closed,
            local_port,
            peer_port,
            local_verification_tag,
            peer_verification_tag: 0,
            local_initial_tsn,
            next_tsn: local_initial_tsn,
            peer_initial_tsn: None,
            receive: ReceiveState::default(),
        }
    }

    #[must_use]
    pub fn new_server(local_port: u16, peer_port: u16, local_verification_tag: u32, local_initial_tsn: u32) -> Self {
        Self {
            role: Role::Server,
            state: State::Closed,
            local_port,
            peer_port,
            local_verification_tag,
            peer_verification_tag: 0,
            local_initial_tsn,
            next_tsn: local_initial_tsn,
            peer_initial_tsn: None,
            receive: ReceiveState::default(),
        }
    }

    #[must_use]
    pub const fn state(&self) -> State {
        self.state
    }

    /// The client's own first move: build the `INIT` packet and enter
    /// `CookieWait`. §5.1's own rule: the Common Header's Verification
    /// Tag is 0 for this one packet, since no peer tag is known yet.
    ///
    /// # Panics
    /// Never in normal use — this method does not touch `peer_*` fields.
    #[must_use]
    pub fn initiate(&mut self) -> Vec<u8> {
        let init = InitChunk { initiate_tag: self.local_verification_tag, advertised_receiver_window_credit: DEFAULT_ADVERTISED_RECEIVER_WINDOW, outbound_streams: 1, inbound_streams: 1, initial_tsn: self.local_initial_tsn };
        let header = CommonHeader { source_port: self.local_port, destination_port: self.peer_port, verification_tag: 0, checksum: 0 };
        self.state = State::CookieWait;
        one_chunk_packet(&header, &Chunk::Init(init))
    }

    /// Feed one received packet, returning zero or more packets to send
    /// in response.
    ///
    /// # Errors
    /// [`ProtocolError::Malformed`] if the packet fails checksum
    /// verification, is too short to contain a common header, or its
    /// verification tag does not match what this association expects for
    /// the chunk type it carries.
    pub fn on_packet(&mut self, packet: &[u8]) -> Result<Vec<Vec<u8>>> {
        if !verify_checksum(packet) {
            return Err(malformed("SCTP packet failed CRC32c verification"));
        }
        let header = CommonHeader::parse(packet)?;
        let mut cursor = crate::packet::COMMON_HEADER_LEN;
        let mut outgoing = Vec::new();
        while cursor < packet.len() {
            let (chunk, consumed) = crate::chunk::parse_one(packet.get(cursor..).ok_or_else(|| malformed("SCTP packet chunk area is truncated"))?)?;
            if let Some(reply) = self.handle_chunk(&header, &chunk)? {
                outgoing.push(reply);
            }
            cursor += consumed;
        }
        Ok(outgoing)
    }

    fn handle_chunk(&mut self, header: &CommonHeader, chunk: &Chunk) -> Result<Option<Vec<u8>>> {
        match (self.role, self.state, chunk) {
            // Server, first contact: INIT arrives with the sender's own
            // tag as `initiate_tag`, Common Header Verification Tag 0.
            (Role::Server, State::Closed, Chunk::Init(init)) => {
                self.peer_verification_tag = init.initiate_tag;
                self.peer_initial_tsn = Some(init.initial_tsn);
                let cookie = build_cookie(self.local_verification_tag, init.initiate_tag, self.local_initial_tsn, init.initial_tsn);
                let init_ack = InitAckChunk {
                    initiate_tag: self.local_verification_tag,
                    advertised_receiver_window_credit: DEFAULT_ADVERTISED_RECEIVER_WINDOW,
                    outbound_streams: 1,
                    inbound_streams: 1,
                    initial_tsn: self.local_initial_tsn,
                    state_cookie: cookie,
                };
                let out_header = CommonHeader { source_port: self.local_port, destination_port: self.peer_port, verification_tag: self.peer_verification_tag, checksum: 0 };
                Ok(Some(one_chunk_packet(&out_header, &Chunk::InitAck(init_ack))))
            }
            // Client, awaiting INIT ACK.
            (Role::Client, State::CookieWait, Chunk::InitAck(init_ack)) => {
                if header.verification_tag != self.local_verification_tag {
                    return Err(malformed("SCTP INIT ACK has the wrong verification tag"));
                }
                self.peer_verification_tag = init_ack.initiate_tag;
                self.peer_initial_tsn = Some(init_ack.initial_tsn);
                self.state = State::CookieEchoed;
                let out_header = CommonHeader { source_port: self.local_port, destination_port: self.peer_port, verification_tag: self.peer_verification_tag, checksum: 0 };
                Ok(Some(one_chunk_packet(&out_header, &Chunk::CookieEcho(init_ack.state_cookie.clone()))))
            }
            // Server, awaiting COOKIE ECHO.
            (Role::Server, State::Closed, Chunk::CookieEcho(_cookie)) if self.peer_verification_tag != 0 => {
                if header.verification_tag != self.local_verification_tag {
                    return Err(malformed("SCTP COOKIE ECHO has the wrong verification tag"));
                }
                self.state = State::Established;
                let out_header = CommonHeader { source_port: self.local_port, destination_port: self.peer_port, verification_tag: self.peer_verification_tag, checksum: 0 };
                Ok(Some(one_chunk_packet(&out_header, &Chunk::CookieAck)))
            }
            // Client, awaiting COOKIE ACK.
            (Role::Client, State::CookieEchoed, Chunk::CookieAck) => {
                if header.verification_tag != self.local_verification_tag {
                    return Err(malformed("SCTP COOKIE ACK has the wrong verification tag"));
                }
                self.state = State::Established;
                Ok(None)
            }
            // Either side, established: DATA -> SACK.
            (_, State::Established, Chunk::Data(data)) => {
                if header.verification_tag != self.local_verification_tag {
                    return Err(malformed("SCTP DATA chunk has the wrong verification tag"));
                }
                self.receive.on_data(data.tsn, &data.user_data);
                let cum = self.receive.cumulative_tsn_ack.unwrap_or(data.tsn.wrapping_sub(1));
                let sack = SackChunk { cumulative_tsn_ack: cum, advertised_receiver_window_credit: DEFAULT_ADVERTISED_RECEIVER_WINDOW, gap_ack_blocks: Vec::<GapAckBlock>::new(), duplicate_tsns: Vec::new() };
                let out_header = CommonHeader { source_port: self.local_port, destination_port: self.peer_port, verification_tag: self.peer_verification_tag, checksum: 0 };
                Ok(Some(one_chunk_packet(&out_header, &Chunk::Sack(sack))))
            }
            (_, State::Established, Chunk::Sack(_sack)) => Ok(None),
            _ => Err(malformed("SCTP chunk is not valid for this association's current state")),
        }
    }

    /// Data this association has received (in the order its cumulative
    /// TSN tracker accepted it), for a caller to drain.
    #[must_use]
    pub fn received_data(&self) -> &[Vec<u8>] {
        &self.receive.received_data
    }

    /// Send one `DATA` chunk (single-fragment, unordered=false), once
    /// `Established`.
    ///
    /// # Errors
    /// [`ProtocolError::Unsupported`] if this association is not yet
    /// `Established`.
    pub fn send_data(&mut self, stream_id: u16, payload_protocol_id: u32, payload: &[u8]) -> Result<Vec<u8>> {
        if self.state != State::Established {
            return Err(ProtocolError::Unsupported { scheme: SCHEME, operation: "send_data before the association is Established" });
        }
        let tsn = self.next_tsn;
        self.next_tsn = self.next_tsn.wrapping_add(1);
        let data = DataChunk {
            flags: DataFlags { unordered: false, beginning_fragment: true, ending_fragment: true },
            tsn,
            stream_id,
            stream_sequence_number: 0,
            payload_protocol_id,
            user_data: payload.to_vec(),
        };
        let header = CommonHeader { source_port: self.local_port, destination_port: self.peer_port, verification_tag: self.peer_verification_tag, checksum: 0 };
        Ok(one_chunk_packet(&header, &Chunk::Data(data)))
    }
}

/// A cookie carrying just enough state for the server to resume the
/// handshake statelessly on `COOKIE ECHO` (in a real server, without
/// keeping the half-open association around) — but **not
/// authenticated**, see this module's own top-level scope note.
fn build_cookie(server_tag: u32, client_tag: u32, server_initial_tsn: u32, client_initial_tsn: u32) -> Vec<u8> {
    let mut cookie = Vec::new();
    cookie.extend_from_slice(&server_tag.to_be_bytes());
    cookie.extend_from_slice(&client_tag.to_be_bytes());
    cookie.extend_from_slice(&server_initial_tsn.to_be_bytes());
    cookie.extend_from_slice(&client_initial_tsn.to_be_bytes());
    cookie
}

fn one_chunk_packet(header: &CommonHeader, chunk: &Chunk) -> Vec<u8> {
    let mut chunk_bytes = crate::chunk::encode(chunk);
    pad_to_4(&mut chunk_bytes);
    build_with_checksum(header, &chunk_bytes)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    /// #561's own Acceptance Criterion, replayed as a replacement bar: a
    /// full four-way handshake to `Established`, both sides this crate's
    /// own code (self-consistency, not a differential check — see the
    /// crate's own docs for why no reference peer is reachable here).
    #[test]
    fn four_way_handshake_reaches_established_on_both_sides() {
        let mut client = Association::new_client(10000, 20000, 0x1111_1111, 1000);
        let mut server = Association::new_server(20000, 10000, 0x2222_2222, 5000);

        let init = client.initiate();
        assert_eq!(client.state(), State::CookieWait);

        let init_ack_packets = server.on_packet(&init).unwrap();
        assert_eq!(init_ack_packets.len(), 1);

        let cookie_echo_packets = client.on_packet(&init_ack_packets[0]).unwrap();
        assert_eq!(client.state(), State::CookieEchoed);
        assert_eq!(cookie_echo_packets.len(), 1);

        let cookie_ack_packets = server.on_packet(&cookie_echo_packets[0]).unwrap();
        assert_eq!(server.state(), State::Established);
        assert_eq!(cookie_ack_packets.len(), 1);

        let none = client.on_packet(&cookie_ack_packets[0]).unwrap();
        assert_eq!(client.state(), State::Established);
        assert!(none.is_empty());
    }

    fn handshaken_pair() -> (Association, Association) {
        let mut client = Association::new_client(10000, 20000, 0x1111_1111, 1000);
        let mut server = Association::new_server(20000, 10000, 0x2222_2222, 5000);
        let init = client.initiate();
        let init_ack = server.on_packet(&init).unwrap();
        let cookie_echo = client.on_packet(&init_ack[0]).unwrap();
        let cookie_ack = server.on_packet(&cookie_echo[0]).unwrap();
        client.on_packet(&cookie_ack[0]).unwrap();
        (client, server)
    }

    #[test]
    fn data_sent_after_the_handshake_is_received_and_acknowledged() {
        let (mut client, mut server) = handshaken_pair();
        let data_packet = client.send_data(1, 0, b"hello sctp").unwrap();

        let sack_packets = server.on_packet(&data_packet).unwrap();
        assert_eq!(server.received_data(), &[b"hello sctp".to_vec()]);
        assert_eq!(sack_packets.len(), 1);

        let none = client.on_packet(&sack_packets[0]).unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn multiple_in_order_data_chunks_all_arrive() {
        let (mut client, mut server) = handshaken_pair();
        for i in 0..5u8 {
            let packet = client.send_data(1, 0, &[i]).unwrap();
            server.on_packet(&packet).unwrap();
        }
        assert_eq!(server.received_data(), &[vec![0], vec![1], vec![2], vec![3], vec![4]]);
    }

    #[test]
    fn send_data_before_established_is_rejected() {
        let mut client = Association::new_client(1, 2, 3, 4);
        assert!(client.send_data(1, 0, b"too early").is_err());
    }

    #[test]
    fn a_bit_flip_anywhere_in_the_packet_fails_checksum_verification() {
        let mut client = Association::new_client(10000, 20000, 0x1111_1111, 1000);
        let mut init = client.initiate();
        let last = init.len() - 1;
        init[last] ^= 0xFF;
        let mut server = Association::new_server(20000, 10000, 0x2222_2222, 5000);
        assert!(server.on_packet(&init).is_err());
    }

    #[test]
    fn a_wrong_verification_tag_after_the_handshake_is_rejected() {
        let (_client, mut server) = handshaken_pair();
        let mut wrong_client = Association::new_client(10000, 20000, 0xBAD_BAD, 1000);
        let _ = wrong_client.initiate();
        // Forge a DATA-shaped packet under the wrong tag directly: skip
        // straight to Established-shaped state to isolate the tag check
        // from the handshake sequencing check.
        let header = CommonHeader { source_port: 10000, destination_port: 20000, verification_tag: 0xFFFF_FFFF, checksum: 0 };
        let data = Chunk::Data(DataChunk { flags: DataFlags { unordered: false, beginning_fragment: true, ending_fragment: true }, tsn: 1, stream_id: 0, stream_sequence_number: 0, payload_protocol_id: 0, user_data: vec![1] });
        let packet = one_chunk_packet(&header, &data);
        assert!(server.on_packet(&packet).is_err());
    }
}
