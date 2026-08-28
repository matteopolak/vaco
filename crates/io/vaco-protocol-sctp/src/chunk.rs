//! RFC 4960 §3.2/§3.3 — the generic chunk header and the twelve chunk
//! types this crate builds.
//!
//! **Scope, stated up front.** Every chunk's *fixed* fields are parsed
//! and built. Variable-length parameters (INIT/INIT ACK's optional
//! parameters beyond INIT ACK's mandatory State Cookie, HEARTBEAT's
//! Heartbeat Info, ERROR/ABORT's error causes) are carried as opaque
//! trailing bytes rather than parsed as the TLV structures §3.2.1/§3.3.5
//! describe in general — this crate's own handshake and DATA/SACK flow
//! never need to interpret their contents, only round-trip them.

use vaco_protocol_core::{ProtocolError, Result};

const SCHEME: &str = "sctp";

fn malformed(detail: &'static str) -> ProtocolError {
    ProtocolError::Malformed { scheme: SCHEME, detail }
}

pub const TYPE_DATA: u8 = 0;
pub const TYPE_INIT: u8 = 1;
pub const TYPE_INIT_ACK: u8 = 2;
pub const TYPE_SACK: u8 = 3;
pub const TYPE_HEARTBEAT: u8 = 4;
pub const TYPE_HEARTBEAT_ACK: u8 = 5;
pub const TYPE_ABORT: u8 = 6;
pub const TYPE_SHUTDOWN: u8 = 7;
pub const TYPE_SHUTDOWN_ACK: u8 = 8;
pub const TYPE_ERROR: u8 = 9;
pub const TYPE_COOKIE_ECHO: u8 = 10;
pub const TYPE_COOKIE_ACK: u8 = 11;
pub const TYPE_SHUTDOWN_COMPLETE: u8 = 14;

/// §3.2's mandatory State Cookie parameter type, the one variable-length
/// parameter this crate does parse (out of INIT ACK's `Vec<Parameter>`
/// worth of optional ones) because the handshake cannot complete without
/// echoing it back verbatim in COOKIE ECHO.
const PARAM_STATE_COOKIE: u16 = 7;

/// Pad `bytes` with zero bytes up to the next multiple of 4 — RFC 4960
/// §3.2's own padding rule, applied by the packet assembler between
/// chunks (padding is not counted in a chunk's own `Chunk Length` field).
pub fn pad_to_4(bytes: &mut Vec<u8>) {
    let padding = (4 - bytes.len() % 4) % 4;
    bytes.extend(std::iter::repeat_n(0u8, padding));
}

/// One parsed chunk, still carrying its own concrete shape rather than
/// being collapsed to a bag of bytes — a caller matches on this the same
/// way [`crate::chunk::Chunk`]'s siblings in this crate's model do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chunk {
    Data(DataChunk),
    Init(InitChunk),
    InitAck(InitAckChunk),
    Sack(SackChunk),
    Heartbeat(Vec<u8>),
    HeartbeatAck(Vec<u8>),
    Abort(Vec<u8>),
    Shutdown { cumulative_tsn_ack: u32 },
    ShutdownAck,
    ShutdownComplete,
    Error(Vec<u8>),
    CookieEcho(Vec<u8>),
    CookieAck,
    /// A recognised-by-nobody-here chunk type — RFC 4960 §3.2 itself
    /// requires an unrecognised chunk to be preserved/reported rather
    /// than silently eaten (the top two bits of the type byte say how);
    /// this variant keeps the raw type and value so a caller can apply
    /// that policy instead of this crate guessing it.
    Unknown { chunk_type: u8, flags: u8, value: Vec<u8> },
}

/// Parse one chunk from the front of `buf`, returning it and how many
/// bytes were consumed **including padding** (so a caller can advance
/// straight to the next chunk).
///
/// # Errors
/// [`ProtocolError::Malformed`] on a truncated header, a declared length
/// shorter than the fixed header, or a length that runs past `buf`.
pub fn parse_one(buf: &[u8]) -> Result<(Chunk, usize)> {
    let chunk_type = *buf.first().ok_or_else(|| malformed("SCTP chunk header is truncated"))?;
    let flags = *buf.get(1).ok_or_else(|| malformed("SCTP chunk header is truncated"))?;
    let length_bytes: [u8; 2] = buf.get(2..4).ok_or_else(|| malformed("SCTP chunk header is truncated"))?.try_into().unwrap_or([0; 2]);
    let length = usize::from(u16::from_be_bytes(length_bytes));
    if length < 4 {
        return Err(malformed("SCTP chunk length is shorter than its own header"));
    }
    let value = buf.get(4..length).ok_or_else(|| malformed("SCTP chunk runs past the end of the packet"))?;
    // The padding itself is allowed to be missing on the very last chunk
    // in a packet (nothing follows it to need separating from), so
    // `consumed` only claims padding bytes that are actually present.
    let padded_len = length + (4 - length % 4) % 4;
    let consumed = padded_len.min(buf.len()).max(length);

    let chunk = match chunk_type {
        TYPE_DATA => Chunk::Data(DataChunk::parse(flags, value)?),
        TYPE_INIT => Chunk::Init(InitChunk::parse(value)?),
        TYPE_INIT_ACK => Chunk::InitAck(InitAckChunk::parse(value)?),
        TYPE_SACK => Chunk::Sack(SackChunk::parse(value)?),
        TYPE_HEARTBEAT => Chunk::Heartbeat(value.to_vec()),
        TYPE_HEARTBEAT_ACK => Chunk::HeartbeatAck(value.to_vec()),
        TYPE_ABORT => Chunk::Abort(value.to_vec()),
        TYPE_SHUTDOWN => {
            let tsn_bytes: [u8; 4] = value.get(..4).ok_or_else(|| malformed("SCTP SHUTDOWN chunk is truncated"))?.try_into().unwrap_or([0; 4]);
            Chunk::Shutdown { cumulative_tsn_ack: u32::from_be_bytes(tsn_bytes) }
        }
        TYPE_SHUTDOWN_ACK => Chunk::ShutdownAck,
        TYPE_SHUTDOWN_COMPLETE => Chunk::ShutdownComplete,
        TYPE_ERROR => Chunk::Error(value.to_vec()),
        TYPE_COOKIE_ECHO => Chunk::CookieEcho(value.to_vec()),
        TYPE_COOKIE_ACK => Chunk::CookieAck,
        other => Chunk::Unknown { chunk_type: other, flags, value: value.to_vec() },
    };
    Ok((chunk, consumed))
}

/// Encode one chunk, **unpadded** — the packet assembler pads between
/// chunks via [`pad_to_4`].
#[must_use]
pub fn encode(chunk: &Chunk) -> Vec<u8> {
    let (chunk_type, flags, value): (u8, u8, Vec<u8>) = match chunk {
        Chunk::Data(d) => (TYPE_DATA, d.flags(), d.encode_value()),
        Chunk::Init(i) => (TYPE_INIT, 0, i.encode_value()),
        Chunk::InitAck(i) => (TYPE_INIT_ACK, 0, i.encode_value()),
        Chunk::Sack(s) => (TYPE_SACK, 0, s.encode_value()),
        Chunk::Heartbeat(v) => (TYPE_HEARTBEAT, 0, v.clone()),
        Chunk::HeartbeatAck(v) => (TYPE_HEARTBEAT_ACK, 0, v.clone()),
        Chunk::Abort(v) => (TYPE_ABORT, 0, v.clone()),
        Chunk::Shutdown { cumulative_tsn_ack } => (TYPE_SHUTDOWN, 0, cumulative_tsn_ack.to_be_bytes().to_vec()),
        Chunk::ShutdownAck => (TYPE_SHUTDOWN_ACK, 0, Vec::new()),
        Chunk::ShutdownComplete => (TYPE_SHUTDOWN_COMPLETE, 0, Vec::new()),
        Chunk::Error(v) => (TYPE_ERROR, 0, v.clone()),
        Chunk::CookieEcho(v) => (TYPE_COOKIE_ECHO, 0, v.clone()),
        Chunk::CookieAck => (TYPE_COOKIE_ACK, 0, Vec::new()),
        Chunk::Unknown { chunk_type, flags, value } => (*chunk_type, *flags, value.clone()),
    };
    let mut out = Vec::new();
    out.push(chunk_type);
    out.push(flags);
    #[allow(clippy::cast_possible_truncation, reason = "an SCTP chunk's own length field is 16 bits; nothing in this crate builds a chunk anywhere near u16::MAX")]
    let length = (4 + value.len()) as u16;
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(&value);
    out
}

/// §3.3.1's DATA chunk flags: only the low 3 bits are defined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataFlags {
    pub unordered: bool,
    pub beginning_fragment: bool,
    pub ending_fragment: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataChunk {
    pub flags: DataFlags,
    pub tsn: u32,
    pub stream_id: u16,
    pub stream_sequence_number: u16,
    pub payload_protocol_id: u32,
    pub user_data: Vec<u8>,
}

impl DataChunk {
    fn flags(&self) -> u8 {
        (u8::from(self.flags.unordered) << 2) | (u8::from(self.flags.beginning_fragment) << 1) | u8::from(self.flags.ending_fragment)
    }

    fn encode_value(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&self.tsn.to_be_bytes());
        out.extend_from_slice(&self.stream_id.to_be_bytes());
        out.extend_from_slice(&self.stream_sequence_number.to_be_bytes());
        out.extend_from_slice(&self.payload_protocol_id.to_be_bytes());
        out.extend_from_slice(&self.user_data);
        out
    }

    fn parse(flags_byte: u8, value: &[u8]) -> Result<Self> {
        let tsn = u32::from_be_bytes(value.get(0..4).ok_or_else(|| malformed("SCTP DATA chunk is truncated"))?.try_into().unwrap_or([0; 4]));
        let stream_id = u16::from_be_bytes(value.get(4..6).ok_or_else(|| malformed("SCTP DATA chunk is truncated"))?.try_into().unwrap_or([0; 2]));
        let stream_sequence_number = u16::from_be_bytes(value.get(6..8).ok_or_else(|| malformed("SCTP DATA chunk is truncated"))?.try_into().unwrap_or([0; 2]));
        let payload_protocol_id = u32::from_be_bytes(value.get(8..12).ok_or_else(|| malformed("SCTP DATA chunk is truncated"))?.try_into().unwrap_or([0; 4]));
        let user_data = value.get(12..).ok_or_else(|| malformed("SCTP DATA chunk is truncated"))?.to_vec();
        let flags = DataFlags {
            unordered: flags_byte & 0b100 != 0,
            beginning_fragment: flags_byte & 0b010 != 0,
            ending_fragment: flags_byte & 0b001 != 0,
        };
        Ok(Self { flags, tsn, stream_id, stream_sequence_number, payload_protocol_id, user_data })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitChunk {
    pub initiate_tag: u32,
    pub advertised_receiver_window_credit: u32,
    pub outbound_streams: u16,
    pub inbound_streams: u16,
    pub initial_tsn: u32,
}

const INIT_FIXED_LEN: usize = 16;

impl InitChunk {
    fn encode_value(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&self.initiate_tag.to_be_bytes());
        out.extend_from_slice(&self.advertised_receiver_window_credit.to_be_bytes());
        out.extend_from_slice(&self.outbound_streams.to_be_bytes());
        out.extend_from_slice(&self.inbound_streams.to_be_bytes());
        out.extend_from_slice(&self.initial_tsn.to_be_bytes());
        out
    }

    fn parse(value: &[u8]) -> Result<Self> {
        let fixed = value.get(..INIT_FIXED_LEN).ok_or_else(|| malformed("SCTP INIT chunk is truncated"))?;
        Ok(Self {
            initiate_tag: u32::from_be_bytes(fixed.get(0..4).and_then(|s| s.try_into().ok()).unwrap_or([0; 4])),
            advertised_receiver_window_credit: u32::from_be_bytes(fixed.get(4..8).and_then(|s| s.try_into().ok()).unwrap_or([0; 4])),
            outbound_streams: u16::from_be_bytes(fixed.get(8..10).and_then(|s| s.try_into().ok()).unwrap_or([0; 2])),
            inbound_streams: u16::from_be_bytes(fixed.get(10..12).and_then(|s| s.try_into().ok()).unwrap_or([0; 2])),
            initial_tsn: u32::from_be_bytes(fixed.get(12..16).and_then(|s| s.try_into().ok()).unwrap_or([0; 4])),
        })
        // Optional parameters after the fixed part (e.g. IPv4/IPv6
        // Address, Cookie Preservative) are deliberately not parsed —
        // see this module's own top-level scope note.
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitAckChunk {
    pub initiate_tag: u32,
    pub advertised_receiver_window_credit: u32,
    pub outbound_streams: u16,
    pub inbound_streams: u16,
    pub initial_tsn: u32,
    /// The mandatory State Cookie parameter's value — opaque as far as
    /// this crate is concerned (only the server that issued it needs to
    /// interpret it; the client only has to echo it back verbatim in
    /// COOKIE ECHO, §5.1's own "MUST NOT change" requirement).
    pub state_cookie: Vec<u8>,
}

impl InitAckChunk {
    fn encode_value(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&self.initiate_tag.to_be_bytes());
        out.extend_from_slice(&self.advertised_receiver_window_credit.to_be_bytes());
        out.extend_from_slice(&self.outbound_streams.to_be_bytes());
        out.extend_from_slice(&self.inbound_streams.to_be_bytes());
        out.extend_from_slice(&self.initial_tsn.to_be_bytes());
        // The one parameter this crate does encode: State Cookie,
        // type/length/value, padded to a 4-byte boundary within the
        // chunk's own value (RFC 4960 §3.2.1's parameter padding rule is
        // the same shape as the outer chunk padding rule).
        out.extend_from_slice(&PARAM_STATE_COOKIE.to_be_bytes());
        #[allow(clippy::cast_possible_truncation, reason = "a state cookie is never anywhere near u16::MAX bytes in this crate's own use")]
        let param_len = (4 + self.state_cookie.len()) as u16;
        out.extend_from_slice(&param_len.to_be_bytes());
        out.extend_from_slice(&self.state_cookie);
        pad_to_4(&mut out);
        out
    }

    fn parse(value: &[u8]) -> Result<Self> {
        let fixed = value.get(..INIT_FIXED_LEN).ok_or_else(|| malformed("SCTP INIT ACK chunk is truncated"))?;
        let initiate_tag = u32::from_be_bytes(fixed.get(0..4).and_then(|s| s.try_into().ok()).unwrap_or([0; 4]));
        let advertised_receiver_window_credit = u32::from_be_bytes(fixed.get(4..8).and_then(|s| s.try_into().ok()).unwrap_or([0; 4]));
        let outbound_streams = u16::from_be_bytes(fixed.get(8..10).and_then(|s| s.try_into().ok()).unwrap_or([0; 2]));
        let inbound_streams = u16::from_be_bytes(fixed.get(10..12).and_then(|s| s.try_into().ok()).unwrap_or([0; 2]));
        let initial_tsn = u32::from_be_bytes(fixed.get(12..16).and_then(|s| s.try_into().ok()).unwrap_or([0; 4]));

        let params = value.get(INIT_FIXED_LEN..).unwrap_or(&[]);
        let state_cookie = find_state_cookie(params)?;

        Ok(Self { initiate_tag, advertised_receiver_window_credit, outbound_streams, inbound_streams, initial_tsn, state_cookie })
    }
}

/// Scan INIT ACK's parameter list for the mandatory State Cookie
/// (§3.3.3's own requirement: "The parameter part... MUST contain... the
/// State Cookie parameter"). Other parameters are skipped over by their
/// own declared length rather than interpreted.
fn find_state_cookie(mut params: &[u8]) -> Result<Vec<u8>> {
    while params.len() >= 4 {
        let param_type = u16::from_be_bytes(params.get(0..2).ok_or_else(|| malformed("SCTP INIT ACK parameter is truncated"))?.try_into().unwrap_or([0; 2]));
        let param_len = usize::from(u16::from_be_bytes(params.get(2..4).ok_or_else(|| malformed("SCTP INIT ACK parameter is truncated"))?.try_into().unwrap_or([0; 2])));
        if param_len < 4 {
            return Err(malformed("SCTP INIT ACK parameter length is shorter than its own header"));
        }
        let param_value = params.get(4..param_len).ok_or_else(|| malformed("SCTP INIT ACK parameter runs past the chunk"))?;
        if param_type == PARAM_STATE_COOKIE {
            return Ok(param_value.to_vec());
        }
        let padded = param_len + (4 - param_len % 4) % 4;
        params = params.get(padded..).unwrap_or(&[]);
    }
    Err(malformed("SCTP INIT ACK has no State Cookie parameter"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GapAckBlock {
    pub start: u16,
    pub end: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SackChunk {
    pub cumulative_tsn_ack: u32,
    pub advertised_receiver_window_credit: u32,
    pub gap_ack_blocks: Vec<GapAckBlock>,
    pub duplicate_tsns: Vec<u32>,
}

impl SackChunk {
    fn encode_value(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&self.cumulative_tsn_ack.to_be_bytes());
        out.extend_from_slice(&self.advertised_receiver_window_credit.to_be_bytes());
        #[allow(clippy::cast_possible_truncation, reason = "SCTP's own gap-ack-block/duplicate-TSN counts are 16 bits; not exceeded by anything this crate builds")]
        out.extend_from_slice(&(self.gap_ack_blocks.len() as u16).to_be_bytes());
        #[allow(clippy::cast_possible_truncation, reason = "see gap_ack_blocks above")]
        out.extend_from_slice(&(self.duplicate_tsns.len() as u16).to_be_bytes());
        for block in &self.gap_ack_blocks {
            out.extend_from_slice(&block.start.to_be_bytes());
            out.extend_from_slice(&block.end.to_be_bytes());
        }
        for tsn in &self.duplicate_tsns {
            out.extend_from_slice(&tsn.to_be_bytes());
        }
        out
    }

    fn parse(value: &[u8]) -> Result<Self> {
        let cumulative_tsn_ack = u32::from_be_bytes(value.get(0..4).ok_or_else(|| malformed("SCTP SACK chunk is truncated"))?.try_into().unwrap_or([0; 4]));
        let advertised_receiver_window_credit = u32::from_be_bytes(value.get(4..8).ok_or_else(|| malformed("SCTP SACK chunk is truncated"))?.try_into().unwrap_or([0; 4]));
        let gap_count = usize::from(u16::from_be_bytes(value.get(8..10).ok_or_else(|| malformed("SCTP SACK chunk is truncated"))?.try_into().unwrap_or([0; 2])));
        let dup_count = usize::from(u16::from_be_bytes(value.get(10..12).ok_or_else(|| malformed("SCTP SACK chunk is truncated"))?.try_into().unwrap_or([0; 2])));

        let mut cursor = 12usize;
        let mut gap_ack_blocks = Vec::new();
        for _ in 0..gap_count {
            let start = u16::from_be_bytes(value.get(cursor..cursor + 2).ok_or_else(|| malformed("SCTP SACK gap ack block is truncated"))?.try_into().unwrap_or([0; 2]));
            let end = u16::from_be_bytes(value.get(cursor + 2..cursor + 4).ok_or_else(|| malformed("SCTP SACK gap ack block is truncated"))?.try_into().unwrap_or([0; 2]));
            gap_ack_blocks.push(GapAckBlock { start, end });
            cursor += 4;
        }
        let mut duplicate_tsns = Vec::new();
        for _ in 0..dup_count {
            let tsn = u32::from_be_bytes(value.get(cursor..cursor + 4).ok_or_else(|| malformed("SCTP SACK duplicate TSN is truncated"))?.try_into().unwrap_or([0; 4]));
            duplicate_tsns.push(tsn);
            cursor += 4;
        }
        Ok(Self { cumulative_tsn_ack, advertised_receiver_window_credit, gap_ack_blocks, duplicate_tsns })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn round_trip(chunk: &Chunk) -> Chunk {
        let encoded = encode(chunk);
        let (decoded, consumed) = parse_one(&encoded).unwrap();
        assert_eq!(consumed, encoded.len());
        decoded
    }

    #[test]
    fn init_chunk_round_trips() {
        let chunk = Chunk::Init(InitChunk { initiate_tag: 1, advertised_receiver_window_credit: 65536, outbound_streams: 10, inbound_streams: 10, initial_tsn: 42 });
        assert_eq!(round_trip(&chunk), chunk);
    }

    #[test]
    fn init_ack_chunk_round_trips_including_the_state_cookie() {
        let chunk = Chunk::InitAck(InitAckChunk { initiate_tag: 2, advertised_receiver_window_credit: 65536, outbound_streams: 5, inbound_streams: 5, initial_tsn: 100, state_cookie: b"opaque-cookie-bytes".to_vec() });
        assert_eq!(round_trip(&chunk), chunk);
    }

    #[test]
    fn init_ack_with_an_odd_length_cookie_still_round_trips_through_padding() {
        let chunk = Chunk::InitAck(InitAckChunk { initiate_tag: 2, advertised_receiver_window_credit: 65536, outbound_streams: 5, inbound_streams: 5, initial_tsn: 100, state_cookie: b"x".to_vec() });
        assert_eq!(round_trip(&chunk), chunk);
    }

    #[test]
    fn cookie_echo_and_cookie_ack_round_trip() {
        assert_eq!(round_trip(&Chunk::CookieEcho(b"cookie".to_vec())), Chunk::CookieEcho(b"cookie".to_vec()));
        assert_eq!(round_trip(&Chunk::CookieAck), Chunk::CookieAck);
    }

    #[test]
    fn data_chunk_round_trips_its_fixed_fields() {
        let chunk = Chunk::Data(DataChunk {
            flags: DataFlags { unordered: false, beginning_fragment: false, ending_fragment: false },
            tsn: 7,
            stream_id: 1,
            stream_sequence_number: 0,
            payload_protocol_id: 0,
            user_data: b"hello sctp".to_vec(),
        });
        assert_eq!(round_trip(&chunk), chunk);
    }

    #[test]
    fn sack_chunk_round_trips_with_gap_blocks_and_duplicates() {
        let chunk = Chunk::Sack(SackChunk {
            cumulative_tsn_ack: 10,
            advertised_receiver_window_credit: 65536,
            gap_ack_blocks: vec![GapAckBlock { start: 2, end: 3 }],
            duplicate_tsns: vec![9],
        });
        assert_eq!(round_trip(&chunk), chunk);
    }

    #[test]
    fn shutdown_and_shutdown_ack_and_shutdown_complete_round_trip() {
        assert_eq!(round_trip(&Chunk::Shutdown { cumulative_tsn_ack: 5 }), Chunk::Shutdown { cumulative_tsn_ack: 5 });
        assert_eq!(round_trip(&Chunk::ShutdownAck), Chunk::ShutdownAck);
        assert_eq!(round_trip(&Chunk::ShutdownComplete), Chunk::ShutdownComplete);
    }

    #[test]
    fn heartbeat_and_heartbeat_ack_carry_opaque_info_unchanged() {
        assert_eq!(round_trip(&Chunk::Heartbeat(b"info".to_vec())), Chunk::Heartbeat(b"info".to_vec()));
        assert_eq!(round_trip(&Chunk::HeartbeatAck(b"info".to_vec())), Chunk::HeartbeatAck(b"info".to_vec()));
    }

    #[test]
    fn an_unrecognised_chunk_type_is_preserved_not_rejected() {
        let chunk = Chunk::Unknown { chunk_type: 200, flags: 0x5, value: b"???".to_vec() };
        assert_eq!(round_trip(&chunk), chunk);
    }

    #[test]
    fn truncated_chunk_header_is_rejected_not_panicking() {
        assert!(parse_one(&[]).is_err());
        assert!(parse_one(&[TYPE_INIT, 0, 0]).is_err());
    }

    #[test]
    fn a_declared_length_shorter_than_the_fixed_header_is_rejected() {
        assert!(parse_one(&[TYPE_INIT, 0, 0, 2]).is_err());
    }

    #[test]
    fn a_declared_length_past_the_buffer_is_rejected() {
        assert!(parse_one(&[TYPE_INIT, 0, 0, 200]).is_err());
    }

    #[test]
    fn init_ack_missing_its_mandatory_state_cookie_is_rejected() {
        let mut value = Vec::new();
        value.extend_from_slice(&1u32.to_be_bytes());
        value.extend_from_slice(&65536u32.to_be_bytes());
        value.extend_from_slice(&1u16.to_be_bytes());
        value.extend_from_slice(&1u16.to_be_bytes());
        value.extend_from_slice(&1u32.to_be_bytes());
        // no State Cookie parameter appended
        assert!(InitAckChunk::parse(&value).is_err());
    }

    #[test]
    fn multiple_chunks_parse_sequentially_using_the_padded_consumed_length() {
        let a = Chunk::CookieAck;
        let b = Chunk::Shutdown { cumulative_tsn_ack: 1 };
        let mut buf = Vec::new();
        let mut a_bytes = encode(&a);
        pad_to_4(&mut a_bytes);
        buf.extend_from_slice(&a_bytes);
        buf.extend_from_slice(&encode(&b));

        let (first, consumed1) = parse_one(&buf).unwrap();
        assert_eq!(first, a);
        let (second, _consumed2) = parse_one(&buf[consumed1..]).unwrap();
        assert_eq!(second, b);
    }
}
