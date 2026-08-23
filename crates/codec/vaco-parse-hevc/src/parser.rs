//! The streaming parser: an Annex B byte stream in, access units out.
//!
//! Implements [`vaco_codec_core::Parser`], so
//! [`ParserDriver`](vaco_codec_core::ParserDriver) supplies the reassembly, the
//! end-of-stream convention and the consumed-byte check.
//!
//! # Where an access unit ends
//!
//! Nothing in an Annex B stream marks a picture boundary; it has to be derived
//! from §7.4.2.4.4. HEVC makes that derivation almost trivial:
//!
//! * a VCL NAL unit whose **`first_slice_segment_in_pic_flag` is 1** begins a
//!   new picture, and therefore a new access unit;
//! * an access unit delimiter, parameter set or *prefix* SEI unit that follows a
//!   VCL unit begins a new access unit, because §7.4.2.4.4 requires all of those
//!   to precede the picture they apply to.
//!
//! Compare `vaco-parse-h264`, which has to compare seven slice-header fields
//! for the first clause and needs the parameter sets in hand before it can. Here
//! the flag is bit 16 of the NAL unit and needs nothing at all — see
//! [`peek_first_slice_in_pic`](crate::slice::peek_first_slice_in_pic).
//!
//! # Two entry points, because there are two kinds of source
//!
//! [`Parser::parse`] is the byte-stream path: MPEG-TS and raw elementary
//! streams, where boundaries must be derived. [`HevcParser::push_access_unit`]
//! is the container path: MP4 and Matroska already know where each sample begins
//! and ends, and re-deriving it there would be both wasted work and a chance to
//! disagree with the container.

use std::collections::VecDeque;

use vaco_codec_core::{CodecParameters, FieldOrder, Parser};
use vaco_core::{Error, Result};
use vaco_format_nalu::{Framing, RbspBuf, Scanner, units};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

use crate::nal::{HevcNalHeader, NalUnitType};
use crate::params::{ParameterSets, codec_parameters};
use crate::poc::{PictureOrderCount, PocState, ends_sequence};
use crate::sei::{self, SeiPayload};
use crate::slice::{SliceHeader, peek_first_slice_in_pic, peek_pps_id};

/// The default ceiling on one access unit.
///
/// An access unit larger than this is not a picture, it is a stream that never
/// produces a boundary — the shape a fuzzer finds within seconds. Eight
/// megabytes comfortably exceeds any legitimate HEVC access unit at any level.
pub const DEFAULT_MAX_ACCESS_UNIT: usize = 8 << 20;

/// What an SEI `pic_timing` said about the current picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PicStructHint {
    /// The raw `pic_struct`, Table D.2.
    pub pic_struct: u8,
    /// The field order it implies.
    pub field_order: FieldOrder,
}

/// What the parser learned about the picture it just saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PictureInfo {
    /// The picture order count, from §8.3.1.
    pub poc: PictureOrderCount,
    /// Whether the picture is an IRAP — the HEVC notion of a random-access
    /// point, which is broader than "IDR".
    pub is_irap: bool,
    /// Whether the picture is an IDR specifically.
    pub is_idr: bool,
    /// The first slice's type as a letter — `I`, `P` or `B`.
    pub picture_type: Option<char>,
    /// The field order, when an SEI `pic_timing` stated one.
    pub field_order: Option<FieldOrder>,
    /// `TemporalId` of the first slice's NAL unit.
    pub temporal_id: u8,
}

/// Where the NAL unit currently being assembled sits inside the buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NalPos {
    /// Offset of its start code, `zero_byte` included.
    framed_start: usize,
    /// Offset of its first payload byte, i.e. the first NAL header byte.
    payload_start: usize,
}

/// An HEVC elementary-stream parser.
///
/// Parses parameter sets, slice segment headers and SEI; splits the stream into
/// access units; computes picture order counts. **It decodes nothing** — no
/// coding unit is read and no sample is produced (D5, plan 15 §6.2).
#[derive(Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one independent piece of per-access-unit state"
)]
pub struct HevcParser {
    sets: ParameterSets,
    poc: PocState,
    budget: Budget,
    scanner: Scanner,
    rbsp: RbspBuf,
    /// The access unit being assembled, in the framing it arrived in, plus the
    /// trailing NAL unit whose end has not yet arrived.
    ///
    /// One buffer rather than two, for the reason `vaco-parse-h264` records: a
    /// NAL unit's end is only known when the *next* start code appears, and
    /// [`ParserDriver`](vaco_codec_core::ParserDriver) discards whatever a
    /// parser declines to consume once end of stream is reached — so an
    /// incomplete unit left in the driver's buffer is the last unit of every
    /// file, lost.
    au: Vec<u8>,
    /// Bytes at the front of `au` already emitted. The live region is
    /// `au[au_base..]`, and every offset below is relative to *that*.
    ///
    /// A read cursor rather than a `drain` per access unit: `Vec::drain(..n)`
    /// moves the bytes that survive it, which is quadratic in the caller's push
    /// size. Compaction happens once the consumed prefix is at least half the
    /// buffer, which makes the amortised cost per byte constant.
    au_base: usize,
    /// Where in the live region the in-progress NAL unit's start code begins,
    /// and where its payload does. `None` before the first start code.
    nal: Option<NalPos>,
    /// High-water mark charged for `au`, so growth is charged once.
    au_charged: u64,
    /// Whether `au` already holds a VCL NAL unit.
    au_has_vcl: bool,
    /// Whether `au` holds an IRAP picture.
    au_is_irap: bool,
    /// What the current access unit's SEI said about field order.
    au_pic_struct: Option<PicStructHint>,
    /// Whether an end-of-sequence unit has been seen since the last IRAP, which
    /// makes the next CRA a random-access point with `NoRaslOutputFlag` set.
    sequence_ended: bool,
    /// Access units found but not yet handed out.
    ///
    /// One `parse` call can complete several — a megabyte push of an elementary
    /// stream holds dozens — and the trait returns one packet at a time.
    ready: VecDeque<Packet>,
    /// Whether the trailing NAL unit has been folded in at end of stream, so
    /// that a second `parse(&[])` does not apply it twice.
    eos_tail_done: bool,
    max_access_unit: usize,
    params: Option<CodecParameters>,
    last_picture: PictureInfo,
    /// How samples handed to [`Parser::parse`] are framed. Annex B until
    /// [`HevcParser::set_extradata`] reads an `hvcC` and says otherwise; see
    /// `vaco-parse-h264` for why the same parser has to serve both.
    framing: Framing,
}

impl HevcParser {
    /// A parser bounded by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            sets: ParameterSets::new(),
            poc: PocState::new(),
            budget: Budget::new(limits),
            scanner: Scanner::new(),
            rbsp: RbspBuf::new(),
            au: Vec::new(),
            au_base: 0,
            nal: None,
            au_charged: 0,
            au_has_vcl: false,
            au_is_irap: false,
            au_pic_struct: None,
            sequence_ended: false,
            ready: VecDeque::new(),
            eos_tail_done: false,
            max_access_unit: DEFAULT_MAX_ACCESS_UNIT,
            params: None,
            last_picture: PictureInfo::default(),
            framing: Framing::AnnexB,
        }
    }

    /// Override the per-access-unit ceiling. Clamped to at least one byte.
    #[must_use]
    pub const fn with_max_access_unit(mut self, bytes: usize) -> Self {
        self.max_access_unit = if bytes == 0 { 1 } else { bytes };
        self
    }

    /// Seed the parser from an `hvcC` record, as a container does before the
    /// first sample. Returns the in-band framing the record declares.
    ///
    /// # Errors
    ///
    /// Whatever [`HevcDecoderConfigurationRecord::parse`](crate::hvcc::HevcDecoderConfigurationRecord::parse)
    /// returns. A parameter set inside the record that fails to parse is
    /// *skipped* rather than fatal: a record carries several, and one bad one
    /// should not lose the rest.
    pub fn set_extradata(&mut self, extradata: &[u8]) -> Result<Framing> {
        let record =
            crate::hvcc::HevcDecoderConfigurationRecord::parse(extradata, &mut self.budget)?;
        // VPS, then SPS, then PPS — a record may list them in any order, and an
        // SPS parsed before the VPS it names is still a complete SPS, but
        // ordering keeps the store's `active` pointing at something sensible.
        for nal in record.vps() {
            self.rbsp.fill(nal, &mut self.budget)?;
            let _ = self.sets.add_vps(self.rbsp.as_slice(), &mut self.budget);
        }
        for nal in record.sps() {
            self.rbsp.fill(nal, &mut self.budget)?;
            let _ = self.sets.add_sps(self.rbsp.as_slice(), &mut self.budget);
        }
        for nal in record.pps() {
            self.rbsp.fill(nal, &mut self.budget)?;
            let _ = self.sets.add_pps(self.rbsp.as_slice(), &mut self.budget);
        }
        self.framing = Framing::LengthPrefixed(record.length_size);
        self.refresh_parameters();
        Ok(self.framing)
    }

    /// The framing [`Parser::parse`] will apply to the next sample.
    #[must_use]
    pub const fn framing(&self) -> Framing {
        self.framing
    }

    /// The parameter sets seen so far.
    #[must_use]
    pub const fn parameter_sets(&self) -> &ParameterSets {
        &self.sets
    }

    /// What the most recently seen picture turned out to be.
    #[must_use]
    pub const fn last_picture(&self) -> PictureInfo {
        self.last_picture
    }

    /// Discard per-picture state after a seek.
    ///
    /// Parameter sets survive: re-acquiring them costs a whole coded video
    /// sequence of output, and a stream that redefines them signals it with a
    /// new SPS anyway.
    pub fn flush(&mut self) {
        self.poc.reset();
        self.scanner.reset();
        self.budget.release(self.au_charged);
        self.au_charged = 0;
        self.au.clear();
        self.au_base = 0;
        self.nal = None;
        self.au_has_vcl = false;
        self.au_is_irap = false;
        self.au_pic_struct = None;
        self.sequence_ended = false;
        self.ready.clear();
        self.eos_tail_done = false;
    }

    /// Feed one complete access unit — a container sample — rather than a byte
    /// stream.
    ///
    /// # Errors
    ///
    /// [`Error::LimitExceeded`] on a budget cap. A slice whose parameter sets
    /// have not been seen is skipped, not an error: that is a stream joined
    /// mid-flight, which is legal and common.
    pub fn push_access_unit(&mut self, data: &[u8], framing: Framing) -> Result<PictureInfo> {
        let mut info = PictureInfo::default();
        let mut first_slice: Option<(SliceHeader, u8)> = None;
        // "The first VCL unit", not "the first one whose header parsed": a
        // slice whose parameter sets have not been seen still *is* the picture,
        // and letting the next one overwrite what this one said reports the
        // access unit's second slice as its first.
        let mut seen_vcl = false;

        for nal in units(data, framing) {
            let Some(header) = HevcNalHeader::parse(nal.data) else {
                continue;
            };
            if !header.is_base_layer() {
                continue;
            }
            self.rbsp.fill(nal.data, &mut self.budget)?;
            match header.nal_unit_type {
                NalUnitType::VPS_NUT => {
                    let _ = self.sets.add_vps(self.rbsp.as_slice(), &mut self.budget);
                }
                NalUnitType::SPS_NUT => {
                    let _ = self.sets.add_sps(self.rbsp.as_slice(), &mut self.budget);
                    self.refresh_parameters();
                }
                NalUnitType::PPS_NUT => {
                    let _ = self.sets.add_pps(self.rbsp.as_slice(), &mut self.budget);
                }
                t if t.is_sei() => {
                    if let Some(hint) = self.read_sei_hint() {
                        info.field_order = Some(hint.field_order);
                        self.au_pic_struct = Some(hint);
                    }
                }
                t if t.is_vcl() && !seen_vcl => {
                    seen_vcl = true;
                    info.is_irap = t.is_irap();
                    info.is_idr = t.is_idr();
                    info.temporal_id = header.temporal_id;
                    if let Some(h) = self.parse_slice() {
                        info.picture_type = Some(h.kind.letter());
                        first_slice = Some((h, header.temporal_id));
                    }
                }
                _ => {}
            }
        }

        if let Some((h, tid)) = first_slice.as_ref()
            && let Some((_, sps)) = self.sets.sps_for_pps(h.pps_id)
        {
            info.poc = self.poc.advance(sps, h, *tid);
        }
        self.apply_field_order(info.field_order);
        self.last_picture = info;
        Ok(info)
    }

    /// Parse the SEI currently in `self.rbsp` and report what it says about
    /// field order.
    fn read_sei_hint(&mut self) -> Option<PicStructHint> {
        let sps = self.sets.active()?;
        let msgs = sei::parse(self.rbsp.as_slice(), Some(sps), &mut self.budget).ok()?;
        msgs.iter().find_map(|m| match &m.payload {
            SeiPayload::PicTiming {
                pic_struct: Some(ps),
                ..
            } => Some(PicStructHint {
                pic_struct: ps.0,
                field_order: ps.field_order(),
            }),
            _ => None,
        })
    }

    /// Parse the slice segment header currently in `self.rbsp`, if its
    /// parameter sets are known.
    fn parse_slice(&mut self) -> Option<SliceHeader> {
        let header = read_slice_header(self.rbsp.as_slice(), &self.sets, &mut self.budget)?;
        // A slice activates the SPS its PPS names (§7.4.2.4.2), which is what
        // makes "the active SPS" well defined for the stream description.
        if let Some(pps) = self.sets.get_pps(header.pps_id) {
            let sps_id = pps.sps_id;
            self.sets.activate(sps_id);
        }
        Some(header)
    }

    /// Recompute the cached stream description from the active SPS.
    fn refresh_parameters(&mut self) {
        let Some(sps) = self.sets.active() else {
            return;
        };
        let mut params = codec_parameters(sps);
        if let (Some(hint), Some(v)) = (self.au_pic_struct, params.video.as_mut())
            && v.field_order == FieldOrder::Unknown
        {
            v.field_order = hint.field_order;
        }
        self.params = Some(params);
    }

    /// Apply an SEI-derived field order to the cached description.
    fn apply_field_order(&mut self, order: Option<FieldOrder>) {
        if let Some(order) = order
            && let Some(params) = self.params.as_mut()
            && let Some(v) = params.video.as_mut()
        {
            v.field_order = order;
        }
    }

    /// The part of the buffer that has not been emitted yet.
    fn live(&self) -> &[u8] {
        self.au.get(self.au_base..).unwrap_or(&[])
    }

    /// Emit `au[..upto]` as a packet, resetting the per-access-unit state and
    /// shifting whatever follows to the front of the buffer.
    fn take_access_unit(&mut self, upto: usize) -> Result<Option<Packet>> {
        let upto = upto.min(self.live().len());
        let packet = if upto == 0 {
            None
        } else {
            // Charge, then release exactly what was charged. The charge enforces
            // the cap at the moment of allocation; the release is because the
            // packet is handed to the caller and this parser no longer owns
            // those bytes.
            let before = self.budget.committed();
            let base = self.au_base;
            let bytes = self.au.get(base..base + upto).unwrap_or(&[]);
            let mut p = Packet::from_slice(&mut self.budget, bytes)?;
            let charged = self.budget.committed().saturating_sub(before);
            self.budget.release(charged);
            if self.au_is_irap {
                p.flags |= PacketFlags::KEY;
            }
            Some(p)
        };
        self.drop_front(upto);
        self.au_has_vcl = false;
        self.au_is_irap = false;
        self.au_pic_struct = None;
        Ok(packet)
    }

    /// Drop `n` bytes from the front of `au`, keeping every offset that survives
    /// it — the scanner's watermark and the in-progress NAL's position —
    /// consistent.
    fn drop_front(&mut self, n: usize) {
        let n = n.min(self.live().len());
        if n == 0 {
            return;
        }
        self.au_base += n;
        self.scanner.consume(n);
        if let Some(pos) = self.nal.as_mut() {
            pos.framed_start = pos.framed_start.saturating_sub(n);
            pos.payload_start = pos.payload_start.saturating_sub(n);
        }
        if self.au_base * 2 >= self.au.len() {
            self.au.drain(..self.au_base);
            self.au_base = 0;
        }
    }

    /// Append incoming bytes to the buffer, bounded and charged.
    fn append_input(&mut self, input: &[u8]) -> Result<()> {
        let would_be = self.live().len().saturating_add(input.len());
        if would_be > self.max_access_unit {
            return Err(Error::LimitExceeded {
                limit: "hevc_access_unit",
                requested: would_be as u64,
                cap: self.max_access_unit as u64,
            });
        }
        // Charge the high-water mark, not every append: the buffer is reused
        // across access units, so what it actually costs is its peak size.
        if would_be as u64 > self.au_charged {
            self.budget.charge(would_be as u64 - self.au_charged)?;
            self.au_charged = would_be as u64;
        }
        self.au.extend_from_slice(input);
        Ok(())
    }

    /// Scan `au` for boundaries, queueing every access unit it completes.
    ///
    /// Runs to exhaustion rather than stopping at the first. A caller that
    /// pushes a megabyte at a time would otherwise fall further and further
    /// behind, and the bytes of every un-emitted unit would pile up in `au`
    /// until they hit the per-access-unit cap — turning a perfectly ordinary
    /// stream into a `LimitExceeded`.
    fn advance(&mut self) -> Result<()> {
        loop {
            let from = self.nal.map_or(0, |p| p.payload_start);
            let base = self.au_base;
            let live = self.au.get(base..).unwrap_or(&[]);
            let Some(sc) = self.scanner.find(live, from) else {
                if self.nal.is_none() {
                    // No start code has ever been found, so nothing in the
                    // buffer is a NAL unit and it can be dropped — which is what
                    // bounds a stream of pure garbage.
                    //
                    // **Three** bytes are kept, not the scanner's two: a
                    // trailing `00 00` may become `00 00 01`, but a *four*-byte
                    // start code is `00 00 00 01` and its leading `zero_byte`
                    // has to survive too, or the same stream fed in one-byte
                    // chunks reports a three-byte start code where a whole-buffer
                    // parse reports four.
                    let keep = self.live().len().saturating_sub(3);
                    self.drop_front(keep);
                }
                return Ok(());
            };
            match self.nal {
                None => {
                    // Leading bytes before the first start code are not part of
                    // any NAL unit; §B.1 calls them `leading_zero_8bits` and
                    // permits nothing else, but a real capture can start
                    // mid-packet.
                    self.drop_front(sc.offset);
                    self.nal = Some(NalPos {
                        framed_start: 0,
                        payload_start: sc.len as usize,
                    });
                }
                Some(pos) => {
                    if let Some(packet) = self.complete_nal(pos, sc)? {
                        self.ready.push_back(packet);
                    }
                }
            }
        }
    }

    /// Handle the NAL unit at `pos`, which the start code `sc` has just ended.
    fn complete_nal(
        &mut self,
        pos: NalPos,
        sc: vaco_format_nalu::StartCode,
    ) -> Result<Option<Packet>> {
        // `trailing_zero_8bits` belong to the framing, not to the unit.
        let base = self.au_base;
        let payload_end = {
            let raw = self
                .au
                .get(base + pos.payload_start..base + sc.offset)
                .unwrap_or(&[]);
            match raw.iter().rposition(|&b| b != 0) {
                Some(last) => pos.payload_start + last + 1,
                None => pos.payload_start,
            }
        };

        if payload_end <= pos.payload_start {
            // An empty unit — two adjacent start codes. Skip it entirely.
            self.nal = Some(NalPos {
                framed_start: sc.offset,
                payload_start: sc.offset + sc.len as usize,
            });
            return Ok(None);
        }

        let header = {
            let payload = self
                .au
                .get(base + pos.payload_start..base + payload_end)
                .unwrap_or(&[]);
            let header = HevcNalHeader::parse(payload);
            if header.is_some() {
                self.rbsp.fill(payload, &mut self.budget)?;
            }
            header
        };
        let Some(header) = header else {
            self.nal = Some(NalPos {
                framed_start: sc.offset,
                payload_start: sc.offset + sc.len as usize,
            });
            return Ok(None);
        };

        let boundary = self.starts_access_unit(header);
        let mut completed = None;
        let mut shift = 0usize;
        if boundary {
            shift = pos.framed_start;
            completed = self.take_access_unit(shift)?;
        }
        let next = NalPos {
            framed_start: sc.offset - shift,
            payload_start: sc.offset + sc.len as usize - shift,
        };

        self.apply_nal(header);
        self.nal = Some(next);
        Ok(completed)
    }

    /// Fold the NAL unit currently in `self.rbsp` into the parser's state.
    fn apply_nal(&mut self, header: HevcNalHeader) {
        if !header.is_base_layer() {
            return;
        }
        match header.nal_unit_type {
            NalUnitType::VPS_NUT => {
                let _ = self.sets.add_vps(self.rbsp.as_slice(), &mut self.budget);
            }
            NalUnitType::SPS_NUT => {
                let _ = self.sets.add_sps(self.rbsp.as_slice(), &mut self.budget);
                self.refresh_parameters();
            }
            NalUnitType::PPS_NUT => {
                let _ = self.sets.add_pps(self.rbsp.as_slice(), &mut self.budget);
            }
            t if t.is_sei() => {
                if let Some(hint) = self.read_sei_hint() {
                    self.au_pic_struct = Some(hint);
                    self.apply_field_order(Some(hint.field_order));
                }
            }
            t if t.is_vcl() => {
                if !self.au_has_vcl {
                    self.begin_picture(header);
                }
                self.au_has_vcl = true;
                self.au_is_irap |= t.is_irap();
                self.sequence_ended = false;
            }
            t => {
                if ends_sequence(t) {
                    // §8.1: the next IRAP has `NoRaslOutputFlag` set again.
                    self.poc.reset();
                    self.sequence_ended = true;
                }
            }
        }
    }

    /// End of stream: drain what is queued, fold in the trailing NAL unit, and
    /// emit what is left.
    ///
    /// Called repeatedly — once per `next_unit` after `finish` — until it yields
    /// nothing, which is how a buffer holding several access units is drained.
    /// `eos_tail_done` is what stops the trailing unit from being folded in
    /// twice, which would advance the picture order count an extra time.
    fn finish_stream(&mut self) -> Result<Option<Packet>> {
        // Boundaries the last `parse` call found but was not asked for.
        self.advance()?;
        if let Some(p) = self.ready.pop_front() {
            return Ok(Some(p));
        }
        if !self.eos_tail_done {
            self.eos_tail_done = true;
            if let Some(p) = self.finish_tail()? {
                return Ok(Some(p));
            }
        }
        let all = self.live().len();
        self.take_access_unit(all)
    }

    /// The last NAL unit of the stream, whose end no start code marks.
    fn finish_tail(&mut self) -> Result<Option<Packet>> {
        let Some(pos) = self.nal else {
            return Ok(None);
        };
        let base = self.au_base;
        let end = self.live().len();
        let payload_end = {
            let raw = self
                .au
                .get(base + pos.payload_start..base + end)
                .unwrap_or(&[]);
            match raw.iter().rposition(|&b| b != 0) {
                Some(last) => pos.payload_start + last + 1,
                None => pos.payload_start,
            }
        };
        if payload_end <= pos.payload_start {
            return Ok(None);
        }
        let header = {
            let payload = self
                .au
                .get(base + pos.payload_start..base + payload_end)
                .unwrap_or(&[]);
            let header = HevcNalHeader::parse(payload);
            if header.is_some() {
                self.rbsp.fill(payload, &mut self.budget)?;
            }
            header
        };
        let Some(header) = header else {
            return Ok(None);
        };
        let completed = if self.starts_access_unit(header) {
            let shift = pos.framed_start;
            let packet = self.take_access_unit(shift)?;
            self.nal = Some(NalPos {
                framed_start: 0,
                payload_start: pos.payload_start.saturating_sub(shift),
            });
            packet
        } else {
            None
        };
        self.apply_nal(header);
        Ok(completed)
    }

    /// Record what the first slice of a new picture says, and advance the POC.
    fn begin_picture(&mut self, header: HevcNalHeader) {
        let mut info = PictureInfo {
            is_irap: header.nal_unit_type.is_irap(),
            is_idr: header.nal_unit_type.is_idr(),
            temporal_id: header.temporal_id,
            field_order: self.au_pic_struct.map(|p| p.field_order),
            ..PictureInfo::default()
        };
        if let Some(h) = self.parse_slice() {
            info.picture_type = Some(h.kind.letter());
            if let Some((_, sps)) = self.sets.sps_for_pps(h.pps_id) {
                let t = h.nal_unit_type;
                let no_rasl = t.is_idr()
                    || t.is_bla()
                    || (t.is_cra() && (self.sequence_ended || !self.poc.started()));
                info.poc = self.poc.advance_with(sps, &h, header.temporal_id, no_rasl);
            }
        }
        self.last_picture = info;
    }

    /// Whether the NAL unit currently in `self.rbsp` begins a new access unit.
    /// §7.4.2.4.4.
    fn starts_access_unit(&mut self, header: HevcNalHeader) -> bool {
        if !header.is_base_layer() {
            return false;
        }
        let t = header.nal_unit_type;
        if t.is_vcl() {
            if !self.au_has_vcl {
                return false;
            }
            // The whole rule, in one bit. A unit too short to hold the flag is
            // treated as a boundary, which splits where one might be rather
            // than merging two pictures into one.
            return peek_first_slice_in_pic(self.rbsp.as_slice()).unwrap_or(true);
        }
        // §7.4.2.4.4: these always precede the picture they apply to, so one
        // that follows a VCL unit belongs to the *next* access unit. Suffix SEI
        // and filler deliberately do not.
        t.precedes_slice_data() && self.au_has_vcl
    }
}

/// Parse a slice segment header out of an RBSP, resolving its parameter sets
/// first.
///
/// A free function rather than a method because it needs `&ParameterSets` and
/// `&mut Budget` at once, which a `&mut self` method cannot express without
/// borrowing the whole parser.
fn read_slice_header(
    rbsp: &[u8],
    sets: &ParameterSets,
    budget: &mut Budget,
) -> Option<SliceHeader> {
    let nal = HevcNalHeader::parse(rbsp)?;
    if !nal.nal_unit_type.has_slice_header() {
        return None;
    }
    let pps_id = peek_pps_id(rbsp)?;
    let (pps, sps) = sets.sps_for_pps(pps_id)?;
    let mut reader = vaco_bitstream::BitReader::new(rbsp);
    reader.skip(16);
    let header = SliceHeader::parse_data(&mut reader, nal, sps, pps, budget).ok()?;
    reader.check().ok()?;
    Some(header)
}

impl Parser for HevcParser {
    fn parse(&mut self, input: &[u8]) -> Result<(Option<Packet>, usize)> {
        // A length-prefixed sample is already one access unit and contains no
        // start codes, so the byte-stream scanner would find nothing in it. See
        // `vaco-parse-h264`'s equivalent.
        if let Framing::LengthPrefixed(size) = self.framing
            && !input.is_empty()
        {
            self.push_access_unit(input, Framing::LengthPrefixed(size))?;
            let mut packet = Packet::from_slice(&mut self.budget, input)?;
            if self.last_picture.is_irap {
                packet.flags = PacketFlags::KEY;
            }
            return Ok((Some(packet), input.len()));
        }
        if input.is_empty() {
            // End of stream. Called repeatedly until it yields nothing, which is
            // how the driver drains a buffer holding more than one unit.
            return Ok((self.finish_stream()?, 0));
        }
        // Hand back a queued unit before taking more input. `used == 0` with a
        // packet is fine: the driver returns the packet and resets its progress
        // guard before it ever looks at the byte count.
        if let Some(p) = self.ready.pop_front() {
            return Ok((Some(p), 0));
        }
        // Otherwise every byte is consumed. That is not an optimisation choice:
        // `ParserDriver` discards whatever a parser declines to consume once end
        // of stream is reached, so a parser that leaves its trailing NAL unit in
        // the driver's buffer loses the last unit of every file.
        self.append_input(input)?;
        self.advance()?;
        Ok((self.ready.pop_front(), input.len()))
    }

    fn parameters(&self) -> Option<&CodecParameters> {
        self.params.as_ref()
    }

    /// Read an `HEVCDecoderConfigurationRecord`. In MP4 and Matroska the
    /// sequence parameter set is in `hvcC` and in no sample, so without this a
    /// parser fed only payloads reports nothing.
    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        if extradata.is_empty() {
            return Ok(());
        }
        Self::set_extradata(self, extradata).map(|_| ())
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

    /// VPS, SPS, PPS, an IDR slice and a trailing slice, from a real `x265`
    /// stream. The slice payloads are truncated to a few bytes — the parser
    /// reads headers only, so the rest is framing.
    fn stream() -> Vec<u8> {
        let mut v = Vec::new();
        for nal in [
            &[
                0x40u8, 0x01, 0x0c, 0x01, 0xff, 0xff, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90,
                0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x3f, 0x95, 0x98, 0x09,
            ][..],
            &[
                0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00,
                0x00, 0x03, 0x00, 0x3f, 0xa0, 0x05, 0x02, 0x01, 0x69, 0x65, 0x95, 0x9a, 0x49, 0x32,
                0xbc, 0x05, 0xa0, 0x20, 0x00, 0x00, 0x03, 0x00, 0x20, 0x00, 0x00, 0x03, 0x03, 0x01,
            ][..],
            &[0x44, 0x01, 0xc1, 0x72, 0xb4, 0x62, 0x40][..],
            &[
                0x28, 0x01, 0xaf, 0x1d, 0x30, 0xc6, 0x23, 0x40, 0xf2, 0xcd, 0x58, 0xb9, 0x5a, 0x80,
                0x62, 0x7c, 0x25, 0xcc, 0x46, 0x65,
            ][..],
            &[
                0x02, 0x01, 0xd0, 0x29, 0x4b, 0xe1, 0x0c, 0x63, 0x86, 0x16, 0xd0, 0x1e, 0x32, 0xc3,
                0xc2, 0x99, 0xee, 0x5f, 0x65, 0x1f,
            ][..],
        ] {
            v.extend_from_slice(&[0, 0, 0, 1]);
            v.extend_from_slice(nal);
        }
        v
    }

    fn drive(data: &[u8], chunk: usize) -> Vec<Vec<u8>> {
        let parser = HevcParser::new(Limits::strict());
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

    /// Two access units: the parameter sets and the IDR slice, then the trailing
    /// slice.
    #[test]
    fn a_real_stream_splits_into_two_access_units() {
        let data = stream();
        let units = drive(&data, usize::MAX);
        assert_eq!(units.len(), 2, "one per picture");
        // Every byte of the input is accounted for, in order.
        assert_eq!(units.concat(), data);
        // The first unit holds the parameter sets and the IDR.
        assert!(units[0].len() > units[1].len());
    }

    /// The property the H.264 fuzzer found three separate bugs against: the
    /// chunk size must not change the answer.
    #[test]
    fn chunking_is_invisible() {
        let data = stream();
        let whole = drive(&data, usize::MAX);
        for chunk in [1usize, 2, 3, 5, 17, 64, 4096] {
            assert_eq!(
                drive(&data, chunk),
                whole,
                "chunk size {chunk} changed the access-unit sequence"
            );
        }
    }

    /// The key frame flag follows IRAP, not IDR — a CRA is a random access
    /// point too.
    #[test]
    fn the_irap_access_unit_is_flagged_as_a_key_frame() {
        let data = stream();
        let parser = HevcParser::new(Limits::strict());
        let mut driver = ParserDriver::new(parser, Limits::strict());
        driver.push(&data).expect("push");
        driver.finish();
        let first = driver.next_unit().expect("one unit");
        assert!(first.flags.contains(PacketFlags::KEY));
        let second = driver.next_unit().expect("two units");
        assert!(!second.flags.contains(PacketFlags::KEY));
    }

    #[test]
    fn the_parameters_come_out_of_the_sps() {
        let data = stream();
        let mut parser = HevcParser::new(Limits::strict());
        let _ = parser.parse(&data).expect("parse");
        let params = parser.parameters().expect("a description");
        let v = params.video.as_ref().expect("video");
        assert_eq!((v.width, v.height), (640, 360));
        assert_eq!(params.codec_id, Some(vaco_codec_core::CodecId::Hevc));
        assert_eq!(params.profile.map(|p| p.name), Some("Main"));
    }

    #[test]
    fn a_container_sample_needs_no_boundary_derivation() {
        let data = stream();
        let mut parser = HevcParser::new(Limits::strict());
        let info = parser
            .push_access_unit(&data, Framing::AnnexB)
            .expect("push");
        assert!(info.is_irap);
        assert!(info.is_idr);
        assert_eq!(info.picture_type, Some('I'));
    }

    #[test]
    fn a_stream_of_pure_garbage_does_not_grow_the_buffer() {
        let mut parser = HevcParser::new(Limits::strict()).with_max_access_unit(4096);
        for _ in 0..1000 {
            let _ = parser.parse(&[0x55u8; 1024]);
        }
        // Nothing was emitted, and the buffer stayed bounded rather than
        // accumulating a megabyte of non-NAL bytes.
        assert!(parser.live().len() <= 8);
    }

    #[test]
    fn a_flush_forgets_the_partial_access_unit() {
        let data = stream();
        let mut parser = HevcParser::new(Limits::strict());
        let _ = parser.parse(&data[..20]).expect("parse");
        parser.flush();
        assert!(parser.live().is_empty());
        // ...and the parameter sets survive it.
        let _ = parser.parse(&data).expect("parse");
        assert!(parser.parameter_sets().has_sps());
        parser.flush();
        assert!(parser.parameter_sets().has_sps());
    }
}
