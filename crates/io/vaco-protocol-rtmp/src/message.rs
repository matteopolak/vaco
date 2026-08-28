//! Turning a chunk stream into whole messages, and back.
//!
//! [`Dechunker`] holds one [`ChunkStreamState`] per chunk stream ID, because
//! a `Type1`/`Type2`/`Type3` chunk header only makes sense relative to the
//! last full header seen on *that* chunk stream — two chunk streams
//! interleaved on the same connection (a video stream and an audio stream,
//! say) carry independent delta state. [`chunk_message`] is the inverse:
//! split one message into wire-ready chunks at a given chunk size.

use vaco_limits::Budget;
use vaco_protocol_core::{ProtocolError, Result};

use crate::chunk::{self, EXTENDED_TIMESTAMP, MessageHeader};

const SCHEME: &str = "rtmp";

fn malformed(detail: &'static str) -> ProtocolError {
    ProtocolError::Malformed {
        scheme: SCHEME,
        detail,
    }
}

/// One complete RTMP message, reassembled from however many chunks it took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtmpMessage {
    /// Absolute timestamp, resolved from whatever deltas the wire sent.
    pub timestamp: u32,
    pub message_type_id: u8,
    pub message_stream_id: u32,
    pub payload: Vec<u8>,
}

/// Per-chunk-stream state a [`Dechunker`] needs to resolve the next
/// `Type1`/`Type2`/`Type3` header it sees on that stream.
#[derive(Debug, Default)]
struct ChunkStreamState {
    last_timestamp: u32,
    last_timestamp_delta: u32,
    last_message_length: u32,
    last_message_type_id: u8,
    last_message_stream_id: u32,
    /// `Some` while a message is being assembled across multiple chunks:
    /// the bytes collected so far and how many more are expected. `None`
    /// means the next chunk on this stream starts a new message.
    in_progress: Option<IncompleteMessage>,
    /// Whether the last `Type0`/`Type1`/`Type2` header on this stream used
    /// an extended timestamp, so a following `Type3` knows whether to read
    /// one too — per Adobe's spec, `Type3` repeats the extended-timestamp
    /// field exactly when the header it continues had one.
    extended_timestamp_in_effect: bool,
}

#[derive(Debug)]
struct IncompleteMessage {
    timestamp: u32,
    message_type_id: u8,
    message_stream_id: u32,
    remaining: usize,
    body: vaco_limits::IncrementalVec<u8>,
}

/// Reassembles whole [`RtmpMessage`]s from a byte-oriented chunk stream.
///
/// Feed it bytes as they arrive with [`Dechunker::feed`]; it buffers what it
/// cannot yet interpret and returns every message that becomes complete.
#[derive(Debug)]
pub struct Dechunker {
    chunk_size: u32,
    streams: std::collections::HashMap<u32, ChunkStreamState>,
    /// Bytes not yet consumed into a chunk.
    buf: Vec<u8>,
    budget: Budget,
}

impl Dechunker {
    /// `chunk_size` is the *receive* chunk size in effect — 128 until a
    /// `Set Chunk Size` control message (type 1, chunk stream 2) changes
    /// it; see [`crate::control`].
    #[must_use]
    pub fn new(limits: vaco_limits::Limits) -> Self {
        Self {
            chunk_size: 128,
            streams: std::collections::HashMap::new(),
            buf: Vec::new(),
            budget: Budget::new(limits),
        }
    }

    /// Update the chunk size this side reads with, once a peer's `Set
    /// Chunk Size` control message says so.
    pub const fn set_chunk_size(&mut self, size: u32) {
        self.chunk_size = size;
    }

    /// Feed newly-received bytes and drain every message that is now
    /// complete. Bytes belonging to a message still in progress are held
    /// internally, not returned.
    ///
    /// # Errors
    /// [`ProtocolError::Malformed`] for a header this crate cannot make
    /// sense of (`fmt` 1/2/3 as the very first thing ever seen on a chunk
    /// stream, since there is no prior header to extend); propagates a
    /// [`vaco_limits::LimitError`] (as [`ProtocolError::Io`]) if a
    /// message's declared length would exceed the budget.
    pub fn feed(&mut self, data: &[u8]) -> Result<Vec<RtmpMessage>> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        loop {
            match self.try_take_one()? {
                ChunkOutcome::NeedMoreData => break,
                ChunkOutcome::Consumed => {}
                ChunkOutcome::Message(msg) => out.push(msg),
            }
        }
        Ok(out)
    }

    /// Try to consume exactly one chunk from the front of `self.buf`.
    fn try_take_one(&mut self) -> Result<ChunkOutcome> {
        let Some((basic, basic_len)) = chunk::decode_basic_header(&self.buf)? else {
            return Ok(ChunkOutcome::NeedMoreData);
        };
        let after_basic = self.buf.get(basic_len..).unwrap_or(&[]);
        let Some((header, header_len)) = MessageHeader::decode(basic.fmt, after_basic)? else {
            return Ok(ChunkOutcome::NeedMoreData);
        };
        let after_header = after_basic.get(header_len..).unwrap_or(&[]);

        // Peek the raw (possibly-sentinel) timestamp field this header
        // carries, to decide whether an extended timestamp follows. Type3
        // carries none of its own; it inherits the previous header's flag.
        let raw_ts = match header {
            MessageHeader::Type0 { timestamp, .. } => Some(timestamp),
            MessageHeader::Type1 {
                timestamp_delta, ..
            }
            | MessageHeader::Type2 { timestamp_delta } => Some(timestamp_delta),
            MessageHeader::Type3 => None,
        };

        let state_has_extended = self
            .streams
            .get(&basic.csid)
            .is_some_and(|s| s.extended_timestamp_in_effect);
        let reads_extended = match raw_ts {
            Some(EXTENDED_TIMESTAMP) => true,
            Some(_) => false,
            None => state_has_extended,
        };

        let (extended, after_extended) = if reads_extended {
            let Some(bytes) = after_header.get(..4) else {
                return Ok(ChunkOutcome::NeedMoreData);
            };
            let Ok(arr) = <[u8; 4]>::try_from(bytes) else {
                return Ok(ChunkOutcome::NeedMoreData);
            };
            (
                Some(u32::from_be_bytes(arr)),
                after_header.get(4..).unwrap_or(&[]),
            )
        } else {
            (None, after_header)
        };

        let state = self.streams.entry(basic.csid).or_default();

        // Resolve this header against `state`, and how many payload bytes
        // this chunk should carry (bounded by both the message's remaining
        // length and the negotiated chunk size).
        let (timestamp, message_type_id, message_stream_id, message_length, is_continuation) =
            match header {
                MessageHeader::Type0 {
                    message_length,
                    message_type_id,
                    message_stream_id,
                    ..
                } => {
                    let ts = extended.unwrap_or(raw_ts.unwrap_or(0));
                    (
                        ts,
                        message_type_id,
                        message_stream_id,
                        message_length,
                        false,
                    )
                }
                MessageHeader::Type1 {
                    message_length,
                    message_type_id,
                    ..
                } => {
                    let delta = extended.unwrap_or(raw_ts.unwrap_or(0));
                    let ts = state.last_timestamp.wrapping_add(delta);
                    (
                        ts,
                        message_type_id,
                        state.last_message_stream_id,
                        message_length,
                        false,
                    )
                }
                MessageHeader::Type2 { .. } => {
                    let delta = extended.unwrap_or(raw_ts.unwrap_or(0));
                    let ts = state.last_timestamp.wrapping_add(delta);
                    (
                        ts,
                        state.last_message_type_id,
                        state.last_message_stream_id,
                        state.last_message_length,
                        false,
                    )
                }
                MessageHeader::Type3 => {
                    if state.in_progress.is_some() {
                        // Continuing a message already in flight: no new
                        // timestamp at all, this chunk is pure payload.
                        (
                            state.last_timestamp,
                            state.last_message_type_id,
                            state.last_message_stream_id,
                            state.last_message_length,
                            true,
                        )
                    } else {
                        // Starting a new message that repeats every field,
                        // including the delta, from the previous one.
                        let ts = state
                            .last_timestamp
                            .wrapping_add(state.last_timestamp_delta);
                        (
                            ts,
                            state.last_message_type_id,
                            state.last_message_stream_id,
                            state.last_message_length,
                            false,
                        )
                    }
                }
            };

        if !matches!(header, MessageHeader::Type3) {
            state.last_timestamp_delta = match header {
                MessageHeader::Type0 { .. } => 0,
                _ => extended.unwrap_or(raw_ts.unwrap_or(0)),
            };
        }
        state.last_timestamp = timestamp;
        state.last_message_type_id = message_type_id;
        state.last_message_stream_id = message_stream_id;
        state.last_message_length = message_length;
        state.extended_timestamp_in_effect = reads_extended;

        let total_len = usize::try_from(message_length).unwrap_or(usize::MAX);
        let remaining_before = if is_continuation {
            state
                .in_progress
                .as_ref()
                .map_or(total_len, |m| m.remaining)
        } else {
            total_len
        };
        let take = remaining_before.min(usize::try_from(self.chunk_size).unwrap_or(usize::MAX));
        let Some(payload_chunk) = after_extended.get(..take) else {
            return Ok(ChunkOutcome::NeedMoreData);
        };
        // Copy out before draining `self.buf` below, so the slices borrowed
        // from it (`after_extended`/`payload_chunk`) do not outlive that
        // mutable borrow.
        let payload_owned = payload_chunk.to_vec();
        let after_extended_len = after_extended.len();

        // Only now, once the whole chunk is confirmed present, remove it
        // from the front of the buffer.
        let consumed = self.buf.len() - after_extended_len + take;
        self.buf.drain(..consumed.min(self.buf.len()));

        let mut msg_state = if is_continuation {
            state
                .in_progress
                .take()
                .ok_or_else(|| malformed("chunk continues a message that was never started"))?
        } else {
            // Phase 1: is the declared length even plausible, before a
            // single byte of it is spent? A non-committing check, not a
            // reservation — `IncrementalVec::push_slice` below does the
            // real (phase 2) accounting as bytes actually arrive, so a
            // huge declared length with a small real payload never costs
            // more than the real payload did.
            self.budget
                .check(message_length.into())
                .map_err(vaco_core::Error::from)?;
            IncompleteMessage {
                timestamp,
                message_type_id,
                message_stream_id,
                remaining: total_len,
                body: self.budget.incremental(total_len),
            }
        };
        msg_state
            .body
            .push_slice(&mut self.budget, &payload_owned)
            .map_err(vaco_core::Error::from)?;
        msg_state.remaining = msg_state.remaining.saturating_sub(take);

        if msg_state.remaining == 0 {
            self.budget.release(msg_state.body.charged());
            Ok(ChunkOutcome::Message(RtmpMessage {
                timestamp: msg_state.timestamp,
                message_type_id: msg_state.message_type_id,
                message_stream_id: msg_state.message_stream_id,
                payload: msg_state.body.into_vec(),
            }))
        } else {
            self.streams.entry(basic.csid).or_default().in_progress = Some(msg_state);
            Ok(ChunkOutcome::Consumed)
        }
    }
}

/// What one call to [`Dechunker::try_take_one`] did.
enum ChunkOutcome {
    /// `self.buf` does not yet hold a whole chunk; wait for more bytes.
    NeedMoreData,
    /// A whole chunk was consumed but it did not complete a message.
    Consumed,
    /// A whole chunk was consumed and it completed this message.
    Message(RtmpMessage),
}

/// Split `payload` into wire-ready chunks on chunk stream `csid`, each at
/// most `chunk_size` bytes, using a `Type0` header for the first chunk and
/// `Type3` for every continuation (the simplest correct encoding — it never
/// relies on the peer already knowing a previous header to delta against).
///
/// # Errors
/// [`ProtocolError::Malformed`] if `csid`/`chunk_size` are out of range (see
/// [`chunk::encode_basic_header`]).
pub fn chunk_message(msg: &RtmpMessage, csid: u32, chunk_size: u32) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let message_length = u32::try_from(msg.payload.len())
        .map_err(|_| malformed("message payload does not fit RTMP's 24-bit length field"))?;
    let chunk_size = chunk_size.max(1) as usize;

    let mut offset = 0usize;
    let mut first = true;
    while offset < msg.payload.len() || (first && msg.payload.is_empty()) {
        let fmt = if first { 0 } else { 3 };
        chunk::encode_basic_header(fmt, csid, &mut out)?;
        if first {
            let header = MessageHeader::Type0 {
                timestamp: msg.timestamp.min(EXTENDED_TIMESTAMP),
                message_length,
                message_type_id: msg.message_type_id,
                message_stream_id: msg.message_stream_id,
            };
            header.encode(&mut out);
            if msg.timestamp >= EXTENDED_TIMESTAMP {
                out.extend_from_slice(&msg.timestamp.to_be_bytes());
            }
        } else if msg.timestamp >= EXTENDED_TIMESTAMP {
            // Type3 continuations repeat the extended timestamp field.
            out.extend_from_slice(&msg.timestamp.to_be_bytes());
        }
        let end = (offset + chunk_size).min(msg.payload.len());
        if let Some(slice) = msg.payload.get(offset..end) {
            out.extend_from_slice(slice);
        }
        offset = end;
        first = false;
        if msg.payload.is_empty() {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "tests")]
mod tests {
    use super::*;

    fn limits() -> vaco_limits::Limits {
        vaco_limits::Limits::strict()
    }

    #[test]
    fn single_chunk_message_round_trips() {
        let msg = RtmpMessage {
            timestamp: 100,
            message_type_id: 20,
            message_stream_id: 1,
            payload: b"hello rtmp".to_vec(),
        };
        let wire = chunk_message(&msg, 3, 128).unwrap();
        let mut d = Dechunker::new(limits());
        let out = d.feed(&wire).unwrap();
        assert_eq!(out, vec![msg]);
    }

    #[test]
    fn message_larger_than_chunk_size_splits_and_reassembles() {
        let payload: Vec<u8> = (0..500u32).map(|i| (i % 251) as u8).collect();
        let msg = RtmpMessage {
            timestamp: 42,
            message_type_id: 9,
            message_stream_id: 1,
            payload: payload.clone(),
        };
        let wire = chunk_message(&msg, 4, 128).unwrap();
        // 500 bytes at chunk size 128: one Type0 (128 bytes) + three Type3
        // continuations (128+128+116), so more than one chunk boundary was
        // actually exercised.
        let mut d = Dechunker::new(limits());
        let out = d.feed(&wire).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].payload, payload);
        assert_eq!(out[0].timestamp, 42);
    }

    #[test]
    fn feed_byte_at_a_time_still_reassembles() {
        let msg = RtmpMessage {
            timestamp: 7,
            message_type_id: 18,
            message_stream_id: 1,
            payload: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
        };
        let wire = chunk_message(&msg, 5, 128).unwrap();
        let mut d = Dechunker::new(limits());
        let mut got = Vec::new();
        for b in &wire {
            got.extend(d.feed(&[*b]).unwrap());
        }
        assert_eq!(got, vec![msg]);
    }

    #[test]
    fn two_interleaved_chunk_streams_keep_independent_state() {
        let a = RtmpMessage {
            timestamp: 10,
            message_type_id: 8,
            message_stream_id: 1,
            payload: vec![0xaa; 10],
        };
        let b = RtmpMessage {
            timestamp: 20,
            message_type_id: 9,
            message_stream_id: 1,
            payload: vec![0xbb; 10],
        };
        let mut wire = chunk_message(&a, 4, 128).unwrap();
        wire.extend(chunk_message(&b, 5, 128).unwrap());
        let mut d = Dechunker::new(limits());
        let out = d.feed(&wire).unwrap();
        assert_eq!(out, vec![a, b]);
    }

    #[test]
    fn extended_timestamp_round_trips() {
        let msg = RtmpMessage {
            timestamp: EXTENDED_TIMESTAMP + 12345,
            message_type_id: 8,
            message_stream_id: 1,
            payload: vec![1, 2, 3],
        };
        let wire = chunk_message(&msg, 4, 128).unwrap();
        let mut d = Dechunker::new(limits());
        let out = d.feed(&wire).unwrap();
        assert_eq!(out, vec![msg]);
    }

    #[test]
    fn empty_payload_message_round_trips() {
        let msg = RtmpMessage {
            timestamp: 0,
            message_type_id: 4,
            message_stream_id: 0,
            payload: Vec::new(),
        };
        let wire = chunk_message(&msg, 2, 128).unwrap();
        let mut d = Dechunker::new(limits());
        let out = d.feed(&wire).unwrap();
        assert_eq!(out, vec![msg]);
    }

    #[test]
    fn declared_length_over_the_budget_is_rejected_not_allocated() {
        // Type0 header claiming a message far larger than a tiny budget
        // allows (`Limits::tiny`'s max_alloc_single is 16 KiB). `feed` must
        // refuse up front, from the declared length alone — the wire never
        // carries anywhere near 100_000 real bytes in this test, which is
        // the point: a huge header must not cost more than checking it.
        let mut wire = Vec::new();
        chunk::encode_basic_header(0, 3, &mut wire).unwrap();
        let header = MessageHeader::Type0 {
            timestamp: 0,
            message_length: 100_000,
            message_type_id: 9,
            message_stream_id: 1,
        };
        header.encode(&mut wire);
        wire.extend_from_slice(&[0u8; 128]);
        let mut d = Dechunker::new(vaco_limits::Limits::tiny());
        assert!(d.feed(&wire).is_err());
    }

    #[test]
    fn a_declared_length_within_budget_but_larger_than_one_chunk_still_completes() {
        // Guards against the fast-fail check above becoming a blanket
        // rejection of any message bigger than one chunk.
        let payload = vec![7u8; 40_000];
        let msg = RtmpMessage {
            timestamp: 0,
            message_type_id: 9,
            message_stream_id: 1,
            payload,
        };
        let wire = chunk_message(&msg, 3, 128).unwrap();
        let mut d = Dechunker::new(vaco_limits::Limits::permissive());
        let out = d.feed(&wire).unwrap();
        assert_eq!(out, vec![msg]);
    }
}
