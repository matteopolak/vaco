//! [`Av1Parser`]: an `ObuStream`-framed byte stream in, temporal units out.
//!
//! Implements [`vaco_codec_core::Parser`], so
//! [`ParserDriver`](vaco_codec_core::ParserDriver) supplies reassembly, the
//! end-of-stream convention and the consumed-byte check — the same contract
//! `vaco-parse-h264`/`vaco-parse-hevc` implement.
//!
//! # Where a temporal unit ends
//!
//! Unlike H.264/HEVC, AV1 does not need a field comparison to find an access
//! unit boundary: `OBU_TEMPORAL_DELIMITER` (§5.6, empty payload) is the
//! specification's own marker for "a new temporal unit starts here", and
//! every encoder measured for this crate emits one before every frame. A
//! stream is free to omit them (§7.5's decoding process tolerates it), so
//! this parser does not *require* one — dropping that requirement would mean
//! treating every OBU as its own access unit, which is the conservative
//! fallback when no delimiter ever appears, rather than a hang or a dropped
//! frame.

use std::collections::VecDeque;

use vaco_codec_core::{CodecParameters, Parser};
use vaco_core::{Error, Result};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

use crate::frame_header::FrameHeader;
use crate::obu::{ObuType, next_obu_stream_unit};
use crate::params::codec_parameters;
use crate::seq::SequenceHeader;

/// The default ceiling on one temporal unit.
///
/// A temporal unit larger than this is not a frame, it is a stream that never
/// produces a boundary. Eight megabytes comfortably exceeds any legitimate
/// AV1 access unit at any level (Annex A's largest `MaxLumaPs` is under 36M
/// samples; even 4:4:4 12-bit at the lowest plausible compression ratio does
/// not approach this per access unit).
pub const DEFAULT_MAX_ACCESS_UNIT: usize = 8 << 20;

/// An AV1 elementary-stream parser: OBU framing, the sequence header store,
/// and temporal-unit boundary detection. **It decodes nothing** — no tile is
/// read and no sample is produced (D5, plan 15 §1.6).
#[derive(Debug)]
pub struct Av1Parser {
    seq: Option<SequenceHeader>,
    params: Option<CodecParameters>,
    budget: Budget,
    /// Every byte received and not yet dropped: the emitted prefix
    /// (`..au_base`, kept only until compaction), the in-progress access
    /// unit's bytes (`au_base..cur_start_abs`... — see `cur_start`), and any
    /// OBU bytes received but not yet fully parsed.
    buf: Vec<u8>,
    /// Bytes of `buf` already emitted as a packet; compacted away
    /// periodically. Every other offset below is relative to `buf[au_base..]`
    /// ("the live region"), exactly as `vaco-parse-hevc::parser` does it.
    au_base: usize,
    /// How far into the live region OBUs have been fully parsed.
    scan_pos: usize,
    /// Where the in-progress (not yet emitted) access unit begins, in the
    /// live region.
    cur_start: usize,
    /// Whether the in-progress access unit contains a shown key or intra-only
    /// frame.
    cur_is_key: bool,
    /// High-water mark charged for `buf`.
    buf_charged: u64,
    ready: VecDeque<Packet>,
    max_access_unit: usize,
}

impl Av1Parser {
    /// A parser bounded by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            seq: None,
            params: None,
            budget: Budget::new(limits),
            buf: Vec::new(),
            au_base: 0,
            scan_pos: 0,
            cur_start: 0,
            cur_is_key: false,
            buf_charged: 0,
            ready: VecDeque::new(),
            max_access_unit: DEFAULT_MAX_ACCESS_UNIT,
        }
    }

    /// Override the per-access-unit ceiling. Clamped to at least one byte.
    #[must_use]
    pub const fn with_max_access_unit(mut self, bytes: usize) -> Self {
        self.max_access_unit = if bytes == 0 { 1 } else { bytes };
        self
    }

    /// Seed the parser from an `av1C` record, as a container does before the
    /// first sample.
    ///
    /// # Errors
    ///
    /// Whatever [`crate::av1c::Av1CodecConfigurationRecord::parse`] and
    /// [`SequenceHeader::parse`] return.
    pub fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        let record = crate::av1c::Av1CodecConfigurationRecord::parse(extradata, &mut self.budget)?;
        if let Some(sh) = record.sequence_header(&mut self.budget)? {
            self.seq = Some(sh);
            self.refresh_parameters();
        }
        Ok(())
    }

    /// The most recently seen sequence header, if any.
    #[must_use]
    pub const fn sequence_header(&self) -> Option<&SequenceHeader> {
        self.seq.as_ref()
    }

    /// Discard per-access-unit state after a seek. The sequence header
    /// survives: re-acquiring it costs a whole temporal unit of output, and a
    /// stream that redefines it signals a new `OBU_SEQUENCE_HEADER` anyway.
    pub fn flush(&mut self) {
        self.budget.release(self.buf_charged);
        self.buf_charged = 0;
        self.buf.clear();
        self.au_base = 0;
        self.scan_pos = 0;
        self.cur_start = 0;
        self.cur_is_key = false;
        self.ready.clear();
    }

    fn live(&self) -> &[u8] {
        self.buf.get(self.au_base..).unwrap_or(&[])
    }

    fn refresh_parameters(&mut self) {
        if let Some(seq) = &self.seq {
            self.params = Some(codec_parameters(seq));
        }
    }

    fn append_input(&mut self, input: &[u8]) -> Result<()> {
        let would_be = self.live().len().saturating_add(input.len());
        if would_be > self.max_access_unit {
            return Err(Error::LimitExceeded {
                limit: "av1_access_unit",
                requested: would_be as u64,
                cap: self.max_access_unit as u64,
            });
        }
        if would_be as u64 > self.buf_charged {
            self.budget.charge(would_be as u64 - self.buf_charged)?;
            self.buf_charged = would_be as u64;
        }
        self.buf.extend_from_slice(input);
        Ok(())
    }

    /// Emit `live()[cur_start..upto]` as a packet and reset the in-progress
    /// access unit's tracking.
    fn take_access_unit(&mut self, upto: usize) -> Result<Option<Packet>> {
        let cur_start = self.cur_start.min(self.live().len());
        let upto = upto.clamp(cur_start, self.live().len());
        let packet = if upto == cur_start {
            None
        } else {
            let before = self.budget.committed();
            let base = self.au_base;
            let bytes = self.buf.get(base + cur_start..base + upto).unwrap_or(&[]);
            let mut p = Packet::from_slice(&mut self.budget, bytes)?;
            let charged = self.budget.committed().saturating_sub(before);
            self.budget.release(charged);
            if self.cur_is_key {
                p.flags |= PacketFlags::KEY;
            }
            Some(p)
        };
        self.drop_front(upto);
        self.cur_is_key = false;
        Ok(packet)
    }

    /// Drop `n` bytes from the front of the live region, keeping every other
    /// offset consistent, and compact once the dropped prefix is worth it.
    fn drop_front(&mut self, n: usize) {
        let n = n.min(self.live().len());
        if n == 0 {
            return;
        }
        self.au_base += n;
        self.scan_pos = self.scan_pos.saturating_sub(n);
        self.cur_start = self.cur_start.saturating_sub(n);
        if self.au_base * 2 >= self.buf.len() {
            self.buf.drain(..self.au_base);
            self.au_base = 0;
        }
    }

    /// Fold one already-parsed OBU into the parser's state: store a sequence
    /// header, or peek a frame header for its key-frame flag.
    ///
    /// Takes an owned copy of the payload rather than a borrow of `self.buf`:
    /// the two branches below need `&mut self.seq`/`&mut self.budget`, which
    /// cannot coexist with a live borrow of `self.buf` derived from
    /// `self.live()`. The bytes are already inside `buf`'s charged high-water
    /// mark, so this copy is not a new attacker-controlled allocation — just
    /// one already-bounded buffer's bytes read twice.
    fn apply_obu(&mut self, unit: crate::obu::ObuUnit) {
        let payload = unit.payload(self.live()).to_vec();
        let payload = payload.as_slice();
        match unit.header.obu_type {
            ObuType::SEQUENCE_HEADER => {
                if let Ok(sh) = SequenceHeader::parse(payload, &mut self.budget) {
                    self.seq = Some(sh);
                    self.refresh_parameters();
                }
            }
            ObuType::FRAME_HEADER | ObuType::FRAME => {
                let is_key_shown = self.seq.as_ref().is_some_and(|seq| {
                    let Ok(fh) = FrameHeader::parse(
                        payload,
                        seq,
                        unit.header.temporal_id,
                        unit.header.spatial_id,
                    ) else {
                        return false;
                    };
                    matches!(
                        fh,
                        FrameHeader::Intra {
                            frame_type: crate::frame_header::FrameType::Key,
                            show_frame: true,
                            ..
                        }
                    )
                });
                self.cur_is_key |= is_key_shown;
            }
            _ => {}
        }
    }

    /// Scan the live region for as many complete OBUs as are available,
    /// splitting on `OBU_TEMPORAL_DELIMITER` and queueing every temporal unit
    /// that completes.
    fn advance(&mut self) -> Result<()> {
        loop {
            let Some(unit) = next_obu_stream_unit(self.live(), self.scan_pos) else {
                // Either genuinely incomplete (more bytes needed) or the
                // bytes at `scan_pos` are not a valid OBU header at all. A
                // corrupt stream that never yields another parseable OBU
                // would otherwise grow forever; `append_input`'s cap is what
                // actually bounds that, so nothing more is done here.
                return Ok(());
            };
            if unit.header.obu_type == ObuType::TEMPORAL_DELIMITER && self.scan_pos > self.cur_start
            {
                if let Some(p) = self.take_access_unit(self.scan_pos)? {
                    self.ready.push_back(p);
                }
                self.cur_start = self.scan_pos;
            }
            self.apply_obu(unit);
            self.scan_pos += unit.total_len;
        }
    }

    fn finish_stream(&mut self) -> Result<Option<Packet>> {
        self.advance()?;
        if let Some(p) = self.ready.pop_front() {
            return Ok(Some(p));
        }
        // Whatever fully-parsed OBUs remain in the in-progress access unit —
        // up to `scan_pos`, not the whole live region, since anything past
        // `scan_pos` never parsed as a complete OBU and is not a frame this
        // crate can vouch for.
        let upto = self.scan_pos;
        self.take_access_unit(upto)
    }
}

impl Parser for Av1Parser {
    fn parse(&mut self, input: &[u8]) -> Result<(Option<Packet>, usize)> {
        if input.is_empty() {
            return Ok((self.finish_stream()?, 0));
        }
        if let Some(p) = self.ready.pop_front() {
            return Ok((Some(p), 0));
        }
        self.append_input(input)?;
        self.advance()?;
        Ok((self.ready.pop_front(), input.len()))
    }

    fn parameters(&self) -> Option<&CodecParameters> {
        self.params.as_ref()
    }

    /// Read an `AV1CodecConfigurationRecord`.
    ///
    /// Unlike H.264 and HEVC there is no framing to switch: AV1's low-overhead
    /// bitstream format is the same OBU stream in MP4, Matroska and a raw file,
    /// so [`Parser::parse`] needs no adjustment. The record still matters,
    /// because it carries the sequence header and an MP4 sample need not repeat
    /// it.
    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        if extradata.is_empty() {
            return Ok(());
        }
        Self::set_extradata(self, extradata)
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;
    use vaco_codec_core::ParserDriver;

    /// Temporal delimiter, sequence header, then a second temporal delimiter
    /// and a bare (truncated) `OBU_FRAME` — enough to exercise the boundary
    /// split without needing a full tile payload, since this parser never
    /// reads past `frame_header_obu()`'s own fields.
    fn stream() -> Vec<u8> {
        let mut v = vec![0x12, 0x00]; // OBU_TEMPORAL_DELIMITER, size 0
        v.extend_from_slice(&[
            0x0a, 0x0b, // OBU_SEQUENCE_HEADER, size 11
            0x00, 0x00, 0x00, 0x0c, 0xc5, 0x03, 0x65, 0x00, 0xbe, 0x00, 0x10,
        ]);
        v.extend_from_slice(&[0x12, 0x00]); // second temporal delimiter
        // OBU_FRAME (type 6), has_size_field=1, size=2: the same
        // `uncompressed_header()` bit trace as
        // `frame_header::tests::a_key_frame_reports_the_sequence_header_size`
        // (show_existing_frame=0, frame_type=KEY, show_frame=1, ...,
        // render_and_frame_size_different=0) for a shown key frame under the
        // sequence header above.
        v.extend_from_slice(&[0x32, 0x02, 0x10, 0x00]);
        v
    }

    fn drive(data: &[u8], chunk: usize) -> Vec<Vec<u8>> {
        let parser = Av1Parser::new(Limits::strict());
        let mut driver = ParserDriver::new(parser, Limits::strict());
        let mut out = Vec::new();
        for c in data.chunks(chunk.max(1)) {
            driver.push(c).expect("push");
            loop {
                match driver.next_unit() {
                    Ok(pkt) => out.push(pkt.payload().to_vec()),
                    Err(Error::NeedMoreInput | Error::Eof) => break,
                    Err(e) => panic!("{e}"),
                }
            }
        }
        driver.finish();
        loop {
            match driver.next_unit() {
                Ok(pkt) => out.push(pkt.payload().to_vec()),
                Err(Error::NeedMoreInput | Error::Eof) => break,
                Err(e) => panic!("{e}"),
            }
        }
        out
    }

    #[test]
    fn a_stream_splits_at_each_temporal_delimiter() {
        let data = stream();
        let units = drive(&data, usize::MAX);
        assert_eq!(units.len(), 2, "one access unit per temporal delimiter");
        assert_eq!(
            units.concat(),
            data,
            "every byte is accounted for, in order"
        );
    }

    #[test]
    fn chunking_is_invisible() {
        let data = stream();
        let whole = drive(&data, usize::MAX);
        for chunk in [1usize, 2, 3, 5, 17, 64] {
            assert_eq!(
                drive(&data, chunk),
                whole,
                "chunk size {chunk} changed the access-unit sequence"
            );
        }
    }

    #[test]
    fn the_parameters_come_out_of_the_sequence_header() {
        let data = stream();
        let mut parser = Av1Parser::new(Limits::strict());
        let _ = parser.parse(&data).expect("parse");
        let params = parser.parameters().expect("a description");
        let v = params.video.as_ref().expect("video");
        assert_eq!((v.width, v.height), (642, 358));
        assert_eq!(params.codec_id, Some(vaco_codec_core::CodecId::Av1));
    }

    #[test]
    fn a_stream_of_pure_garbage_does_not_grow_the_buffer() {
        let mut parser = Av1Parser::new(Limits::strict()).with_max_access_unit(4096);
        for _ in 0..1000 {
            let _ = parser.parse(&[0x80u8; 1024]);
        }
    }

    #[test]
    fn a_flush_forgets_the_partial_access_unit_but_keeps_the_sequence_header() {
        let data = stream();
        let mut parser = Av1Parser::new(Limits::strict());
        let _ = parser.parse(&data[..10]).expect("parse");
        parser.flush();
        assert!(parser.live().is_empty());
        let _ = parser.parse(&data).expect("parse");
        assert!(parser.sequence_header().is_some());
        parser.flush();
        assert!(parser.sequence_header().is_some());
    }
}
