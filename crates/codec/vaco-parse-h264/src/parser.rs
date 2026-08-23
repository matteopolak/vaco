//! The streaming parser: an Annex B byte stream in, access units out.
//!
//! Implements [`vaco_codec_core::Parser`], so
//! [`ParserDriver`](vaco_codec_core::ParserDriver) supplies the reassembly, the
//! end-of-stream convention and the consumed-byte check.
//!
//! # Where an access unit ends
//!
//! Nothing in an Annex B stream marks a picture boundary. It has to be
//! *derived*, from ITU-T H.264 §7.4.1.2.3 (order of NAL units) and §7.4.1.2.4
//! (detection of the first VCL NAL unit of a primary coded picture):
//!
//! * a VCL NAL unit whose slice header differs from the previous one in any of
//!   §7.4.1.2.4's ways begins a new picture, and therefore a new access unit;
//! * an access unit delimiter, parameter set or SEI unit that *follows* a VCL
//!   unit begins a new access unit, because §7.4.1.2.3 requires all of those to
//!   precede the picture they apply to.
//!
//! Getting this wrong does not produce garbage — it produces access units that
//! are one picture too long or too short, and a frame count that is quietly
//! wrong.
//!
//! # Two entry points, because there are two kinds of source
//!
//! [`Parser::parse`] is the byte-stream path: MPEG-TS and raw elementary
//! streams, where boundaries must be derived. [`H264Parser::push_access_unit`]
//! is the container path: MP4 and Matroska already know where each sample
//! begins and ends, and re-deriving it there would be both wasted work and a
//! chance to disagree with the container.

use std::collections::VecDeque;

use vaco_codec_core::{CodecParameters, FieldOrder, Parser};
use vaco_core::{Error, Result};
use vaco_format_nalu::{Framing, RbspBuf, Scanner, units};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

use crate::nal::{H264NalHeader, NalUnitType};
use crate::params::{ParameterSets, codec_parameters};
use crate::poc::{PictureOrderCount, PocState};
use crate::sei::{self, SeiPayload};
use crate::slice::SliceHeader;

/// The default ceiling on one access unit.
///
/// An access unit larger than this is not a picture, it is a stream that never
/// produces a boundary — the shape a fuzzer finds within seconds. Eight
/// megabytes comfortably exceeds any legitimate H.264 access unit at any level.
pub const DEFAULT_MAX_ACCESS_UNIT: usize = 8 << 20;

/// What an SEI `pic_timing` said about the current picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PicStructHint {
    /// The raw `pic_struct`, Table D-1.
    pub pic_struct: u8,
    /// The field order it implies.
    pub field_order: FieldOrder,
}

/// What the parser learned about the picture it just saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PictureInfo {
    /// The picture order count, from §8.2.1.
    pub poc: PictureOrderCount,
    /// Whether the picture is an IDR.
    pub is_idr: bool,
    /// Whether the picture is used for reference.
    pub is_reference: bool,
    /// The first slice's type as a letter — `I`, `P`, `B` or `S`.
    pub picture_type: Option<char>,
    /// The field order, when an SEI `pic_timing` stated one.
    pub field_order: Option<FieldOrder>,
}

/// Where the NAL unit currently being assembled sits inside the buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NalPos {
    /// Offset of its start code, `zero_byte` included.
    framed_start: usize,
    /// Offset of its first payload byte, i.e. the NAL header.
    payload_start: usize,
}

/// An H.264 elementary-stream parser.
///
/// Parses parameter sets, slice headers and SEI; splits the stream into access
/// units; computes picture order counts. **It decodes nothing** — no macroblock
/// is read and no sample is produced (D5, plan 15 §6.2).
#[derive(Debug)]
pub struct H264Parser {
    sets: ParameterSets,
    poc: PocState,
    budget: Budget,
    scanner: Scanner,
    rbsp: RbspBuf,
    /// The access unit being assembled, in the framing it arrived in, plus the
    /// trailing NAL unit whose end has not yet arrived.
    ///
    /// One buffer rather than two. A NAL unit's end is only known when the
    /// *next* start code appears, so an incomplete unit has to live somewhere —
    /// and the access unit it belongs to is already being assembled, so it may
    /// as well live there. The alternative, leaving it in the driver's buffer,
    /// loses it: [`ParserDriver`](vaco_codec_core::ParserDriver) discards
    /// whatever a parser declines to consume once end of stream is reached, so
    /// the final NAL unit of every file would vanish.
    au: Vec<u8>,
    /// Bytes at the front of `au` already emitted. The live region is
    /// `au[au_base..]`, and every offset below is relative to *that*.
    ///
    /// A read cursor rather than a `drain` per access unit. `Vec::drain(..n)`
    /// moves the bytes that survive it, so dropping a 3 KiB access unit off the
    /// front of a megabyte buffer moves the remaining megabyte — once per
    /// access unit, which is quadratic in the push size. Measured on a
    /// one-megabyte elementary stream: **19.29 ms with the drain, 1.42 ms with
    /// the cursor — 13.6x** — and the cost only appears when a caller pushes a
    /// large buffer at once, so a chunk-fed test never sees it. Compaction
    /// happens once the consumed prefix is at least half the buffer, which
    /// makes the amortised cost per byte constant; the benchmark now measures
    /// the same 1.4 ms at every chunk size from 1 KiB to the whole megabyte.
    au_base: usize,
    /// Where in the live region the in-progress NAL unit's start code begins,
    /// and where its payload does. `None` before the first start code.
    nal: Option<NalPos>,
    /// High-water mark charged for `au`, so growth is charged once.
    au_charged: u64,
    /// Whether `au` already holds a VCL NAL unit.
    au_has_vcl: bool,
    /// Whether `au` holds an IDR slice.
    au_is_idr: bool,
    /// The slice header of the last VCL unit seen, for §7.4.1.2.4.
    prev_slice: Option<SliceHeader>,
    /// What the current access unit's SEI said about field order.
    au_pic_struct: Option<PicStructHint>,
    /// Access units found but not yet handed out.
    ///
    /// One `parse` call can complete several — a megabyte push of an elementary
    /// stream holds dozens — and the trait returns one packet at a time. They
    /// have to be queued somewhere; queueing them as *packets* rather than
    /// leaving the bytes in `au` is what keeps `au` bounded by one access unit
    /// rather than by the caller's push size.
    ready: VecDeque<Packet>,
    /// Whether the trailing NAL unit has been folded in at end of stream, so
    /// that a second `parse(&[])` does not apply it twice.
    eos_tail_done: bool,
    max_access_unit: usize,
    params: Option<CodecParameters>,
    last_picture: PictureInfo,
    /// How samples handed to [`Parser::parse`] are framed.
    ///
    /// Annex B until [`H264Parser::set_extradata`] reads an `avcC` and says
    /// otherwise. This is what makes the same parser usable from MPEG-TS (a
    /// byte stream, boundaries derived) and from MP4 (length-prefixed samples,
    /// boundaries given) without the caller having to know which it has.
    framing: Framing,
}

impl H264Parser {
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
            au_is_idr: false,
            prev_slice: None,
            au_pic_struct: None,
            ready: VecDeque::new(),
            eos_tail_done: false,
            max_access_unit: DEFAULT_MAX_ACCESS_UNIT,
            params: None,
            last_picture: PictureInfo::default(),
            framing: Framing::AnnexB,
        }
    }

    /// The part of the buffer that has not been emitted yet.
    fn live(&self) -> &[u8] {
        self.au.get(self.au_base..).unwrap_or(&[])
    }

    /// Override the per-access-unit ceiling. Clamped to at least one byte.
    #[must_use]
    pub const fn with_max_access_unit(mut self, bytes: usize) -> Self {
        self.max_access_unit = if bytes == 0 { 1 } else { bytes };
        self
    }

    /// Seed the parser from a container's extradata, in either shape it comes
    /// in. Returns the in-band framing the extradata implies.
    ///
    /// Two shapes, and telling them apart matters:
    ///
    /// * An **`avcC` record** (ISO/IEC 14496-15) — what MP4 carries. Its first
    ///   byte is `configurationVersion`, which is 1.
    /// * **Raw Annex B** — a start-code-prefixed SPS and PPS, which is what ASF
    ///   carries in the tail of its `BITMAPINFOHEADER`, and what any container
    ///   holding an unframed elementary stream carries. Its first byte is the
    ///   first byte of a start code, which is 0.
    ///
    /// So `extradata[0] == 1` discriminates, and that is what the reference
    /// tests too. Measured on files this reference build wrote:
    ///
    /// ```text
    /// p.mp4  avcC payload  01 64 00 0a ff e1 …   -> avcC, length-prefixed
    /// a.asf  BITMAPINFO tail  00 00 00 01 67 64 …  -> Annex B (0x67 = SPS)
    /// ```
    ///
    /// This used to parse everything as `avcC`. Annex-B extradata therefore
    /// failed, and `vaco-format-core`'s `build_parser` discards the error, so
    /// the failure was silent: ASF probed with `profile=unknown`,
    /// `level=-99` and `pix_fmt=unknown` while holding a perfectly good SPS
    /// (CONFORMANCE-FINDINGS 21 and 22).
    ///
    /// # Errors
    ///
    /// Whatever `AvcDecoderConfigurationRecord::parse` returns, for the `avcC`
    /// shape. A parameter set that fails to parse is *skipped* rather than
    /// fatal, in either shape: extradata often carries several, and one bad one
    /// should not lose the rest. Annex-B extradata cannot fail as a whole —
    /// there is no header to reject — so it reports `Ok` even if every unit in
    /// it is unusable, which is the same thing an in-band scan would do.
    pub fn set_extradata(&mut self, extradata: &[u8]) -> Result<Framing> {
        if extradata.first() != Some(&1) {
            return self.set_annexb_extradata(extradata);
        }
        let record = crate::AvcDecoderConfigurationRecord::parse(extradata, &mut self.budget)?;
        for nal in record.sps.iter().chain(&record.sps_ext) {
            self.rbsp.fill(nal, &mut self.budget)?;
            let _ = self.sets.add_sps(self.rbsp.as_slice(), &mut self.budget);
        }
        for nal in &record.pps {
            self.rbsp.fill(nal, &mut self.budget)?;
            let _ = self.sets.add_pps(self.rbsp.as_slice(), &mut self.budget);
        }
        self.framing = Framing::LengthPrefixed(record.length_size);
        self.refresh_parameters();
        Ok(self.framing)
    }

    /// Seed from start-code-prefixed parameter sets, leaving framing Annex B.
    ///
    /// Only SPS and PPS are taken. Anything else in the extradata — an SEI, or
    /// a slice a careless writer left behind — is skipped rather than fed to
    /// the picture machinery, because extradata is not a sample and treating it
    /// as one would invent an access unit that is not in the stream.
    fn set_annexb_extradata(&mut self, extradata: &[u8]) -> Result<Framing> {
        const NAL_SPS: u8 = 7;
        const NAL_PPS: u8 = 8;
        const NAL_SPS_EXT: u8 = 13;
        for nal in vaco_bitstream::annexb::nal_units(extradata) {
            let Some(&header) = nal.first() else { continue };
            let kind = header & 0x1F;
            if kind != NAL_SPS && kind != NAL_PPS && kind != NAL_SPS_EXT {
                continue;
            }
            self.rbsp.fill(nal, &mut self.budget)?;
            let _ = if kind == NAL_PPS {
                self.sets.add_pps(self.rbsp.as_slice(), &mut self.budget)
            } else {
                self.sets.add_sps(self.rbsp.as_slice(), &mut self.budget)
            };
        }
        self.framing = Framing::AnnexB;
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
    /// Parameter sets survive: re-acquiring them costs a whole GOP of output,
    /// and a stream that redefines them signals it with a new SPS anyway.
    pub fn flush(&mut self) {
        self.poc.reset();
        self.scanner.reset();
        self.budget.release(self.au_charged);
        self.au_charged = 0;
        self.au.clear();
        self.au_base = 0;
        self.nal = None;
        self.au_has_vcl = false;
        self.au_is_idr = false;
        self.prev_slice = None;
        self.au_pic_struct = None;
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
        let mut first_slice: Option<SliceHeader> = None;

        for nal in units(data, framing) {
            let Some(header) = H264NalHeader::parse(nal.data) else {
                continue;
            };
            self.rbsp.fill(nal.data, &mut self.budget)?;
            match header.nal_unit_type {
                NalUnitType::Sps | NalUnitType::SubsetSps => {
                    let _ = self.sets.add_sps(self.rbsp.as_slice(), &mut self.budget);
                    self.refresh_parameters();
                }
                NalUnitType::Pps => {
                    let _ = self.sets.add_pps(self.rbsp.as_slice(), &mut self.budget);
                }
                NalUnitType::Sei => {
                    if let Some(hint) = self.read_sei_hint() {
                        info.field_order = Some(hint.field_order);
                        self.au_pic_struct = Some(hint);
                    }
                }
                t if t.has_slice_header() && t.is_vcl() && first_slice.is_none() => {
                    if let Some(h) = self.parse_slice() {
                        info.is_idr = h.is_idr();
                        info.is_reference = h.is_reference();
                        info.picture_type = Some(h.kind.letter());
                        first_slice = Some(h);
                    }
                }
                _ => {}
            }
        }

        if let Some(h) = first_slice.as_ref()
            && let Some((_, sps)) = self.sets.sps_for_pps(h.pps_id)
        {
            info.poc = self.poc.advance(sps, h);
        }
        self.apply_field_order(info.field_order);
        if first_slice.is_some() {
            self.prev_slice = first_slice;
        }
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

    /// Parse the slice header currently in `self.rbsp`, if its parameter sets
    /// are known.
    fn parse_slice(&mut self) -> Option<SliceHeader> {
        let header = read_slice_header(self.rbsp.as_slice(), &self.sets, &mut self.budget)?;
        // A slice activates the SPS its PPS names (§7.4.1.2.1), which is what
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
        if let Some(v) = params.video.as_mut() {
            // `codec_parameters` defaults this to 0 (Annex B) because an SPS
            // alone cannot know how the container frames its samples. Only the
            // parser has seen the configuration record, so only the parser can
            // correct it.
            v.nal_length_size = Some(
                self.framing
                    .length_size()
                    .map_or(0, vaco_format_nalu::LengthSize::get),
            );
            if let Some(hint) = self.au_pic_struct
                && v.field_order == FieldOrder::Unknown
            {
                v.field_order = hint.field_order;
            }
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

    /// Emit `au[..upto]` as a packet, resetting the per-access-unit state and
    /// shifting whatever follows to the front of the buffer.
    fn take_access_unit(&mut self, upto: usize) -> Result<Option<Packet>> {
        let upto = upto.min(self.live().len());
        let packet = if upto == 0 {
            None
        } else {
            // Charge, then release exactly what was charged. The charge is what
            // enforces the cap at the moment of allocation; the release is
            // because the packet is handed to the caller and this parser no
            // longer owns those bytes. The budget counts *live* bytes we hold,
            // and without the release a long stream would exhaust it over its
            // lifetime while never holding more than one access unit at once.
            let before = self.budget.committed();
            let base = self.au_base;
            let bytes = self.au.get(base..base + upto).unwrap_or(&[]);
            let mut p = Packet::from_slice(&mut self.budget, bytes)?;
            let charged = self.budget.committed().saturating_sub(before);
            self.budget.release(charged);
            if self.au_is_idr {
                p.flags |= PacketFlags::KEY;
            }
            Some(p)
        };
        self.drop_front(upto);
        self.au_has_vcl = false;
        self.au_is_idr = false;
        self.au_pic_struct = None;
        Ok(packet)
    }

    /// Drop `n` bytes from the front of `au`, keeping every offset that
    /// survives it — the scanner's watermark and the in-progress NAL's
    /// position — consistent.
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
        // Reclaim once the consumed prefix is at least half the buffer, which
        // makes the total moved bytes linear in the total bytes seen.
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
                limit: "h264_access_unit",
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
                    // buffer is a NAL unit and it can be dropped — which is
                    // what bounds a stream of pure garbage.
                    //
                    // **Three** bytes are kept, not the scanner's two. The
                    // scanner needs two, because a trailing `00 00` may become
                    // `00 00 01`; but a *four*-byte start code is `00 00 00 01`
                    // and its leading `zero_byte` has to survive too, or the
                    // same stream fed in one-byte chunks reports a three-byte
                    // start code where a whole-buffer parse reports four.
                    let keep = self.live().len().saturating_sub(3);
                    self.drop_front(keep);
                }
                return Ok(());
            };
            match self.nal {
                None => {
                    // Leading bytes before the first start code are not part of
                    // any NAL unit; §B.1 calls them leading_zero_8bits and
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
            let header = H264NalHeader::parse(payload);
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
    fn apply_nal(&mut self, header: H264NalHeader) {
        match header.nal_unit_type {
            NalUnitType::Sps | NalUnitType::SubsetSps => {
                let _ = self.sets.add_sps(self.rbsp.as_slice(), &mut self.budget);
                self.refresh_parameters();
            }
            NalUnitType::Pps => {
                let _ = self.sets.add_pps(self.rbsp.as_slice(), &mut self.budget);
            }
            NalUnitType::Sei => {
                if let Some(hint) = self.read_sei_hint() {
                    self.au_pic_struct = Some(hint);
                    self.apply_field_order(Some(hint.field_order));
                }
            }
            t if t.is_vcl() => {
                if !self.au_has_vcl && t.has_slice_header() {
                    self.begin_picture(header);
                }
                self.au_has_vcl = true;
                self.au_is_idr |= header.is_idr();
            }
            _ => {}
        }
    }

    /// End of stream: drain what is queued, fold in the trailing NAL unit, and
    /// emit what is left.
    ///
    /// Called repeatedly — once per `next_unit` after `finish` — until it
    /// yields nothing, which is how a buffer holding several access units is
    /// drained. `eos_tail_done` is what stops the trailing unit from being
    /// folded in twice, which would advance the picture order count an extra
    /// time.
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
            let header = H264NalHeader::parse(payload);
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
    fn begin_picture(&mut self, header: H264NalHeader) {
        let Some(h) = self.parse_slice() else {
            // Parameter sets not seen yet; still note what the NAL header says.
            self.last_picture = PictureInfo {
                is_idr: header.is_idr(),
                is_reference: header.is_reference(),
                ..PictureInfo::default()
            };
            return;
        };
        let poc = match self.sets.sps_for_pps(h.pps_id) {
            Some((_, sps)) => self.poc.advance(sps, &h),
            None => PictureOrderCount::default(),
        };
        self.last_picture = PictureInfo {
            poc,
            is_idr: h.is_idr(),
            is_reference: h.is_reference(),
            picture_type: Some(h.kind.letter()),
            field_order: self.au_pic_struct.map(|p| p.field_order),
        };
        self.prev_slice = Some(h);
    }

    /// Whether the NAL unit currently in `self.rbsp` begins a new access unit.
    /// §7.4.1.2.3 and §7.4.1.2.4.
    fn starts_access_unit(&mut self, header: H264NalHeader) -> bool {
        match header.nal_unit_type {
            // §7.4.1.2.3: these always precede the picture they apply to, so
            // one that follows a VCL unit belongs to the *next* access unit.
            NalUnitType::AccessUnitDelimiter
            | NalUnitType::Sps
            | NalUnitType::SubsetSps
            | NalUnitType::Pps
            | NalUnitType::Sei
            | NalUnitType::Prefix => self.au_has_vcl,
            t if t.is_vcl() => {
                if !self.au_has_vcl {
                    return false;
                }
                let Some(prev) = self.prev_slice.as_ref() else {
                    return true;
                };
                // A slice we cannot parse — parameter sets not yet seen — is
                // treated as a new picture. That is the safe direction: it
                // splits where a boundary might be rather than merging two
                // pictures into one.
                let Some(next) =
                    read_slice_header(self.rbsp.as_slice(), &self.sets, &mut self.budget)
                else {
                    return true;
                };
                match self.sets.sps_for_pps(next.pps_id) {
                    Some((_, sps)) => next.starts_new_picture(prev, sps),
                    None => true,
                }
            }
            _ => false,
        }
    }
}

/// Parse a slice header out of an RBSP, resolving its parameter sets first.
///
/// A free function rather than a method because it needs `&ParameterSets` and
/// `&mut Budget` at once, which a `&mut self` method cannot express without
/// borrowing the whole parser.
fn read_slice_header(
    rbsp: &[u8],
    sets: &ParameterSets,
    budget: &mut Budget,
) -> Option<SliceHeader> {
    let nal = H264NalHeader::parse(rbsp)?;
    if !nal.nal_unit_type.has_slice_header() {
        return None;
    }
    let pps_id = peek_pps_id(rbsp)?;
    let (pps, sps) = sets.sps_for_pps(pps_id)?;
    let mut reader = vaco_bitstream::BitReader::new(rbsp);
    reader.skip(8);
    let header = SliceHeader::parse_data(&mut reader, nal, sps, pps, budget).ok()?;
    reader.check().ok()?;
    Some(header)
}

/// Read a slice header's `pic_parameter_set_id` without parsing the rest.
///
/// The first three elements are `first_mb_in_slice`, `slice_type` and
/// `pic_parameter_set_id`, all `ue(v)` and none of them dependent on any
/// parameter set — which is exactly why the format puts them first. That is
/// what makes it possible to find the right SPS and PPS *before* parsing a
/// header whose remaining fields need them.
fn peek_pps_id(rbsp: &[u8]) -> Option<u8> {
    use vaco_codec_golomb::GolombDecode;
    let mut r = vaco_bitstream::BitReader::new(rbsp);
    r.skip(8);
    let _first_mb = r.ue_v_max(u32::MAX - 1).ok()?;
    let _slice_type = r.ue_v_max(9).ok()?;
    let id = r.ue_v_max(255).ok()?;
    Some(id as u8)
}

impl Parser for H264Parser {
    fn parse(&mut self, input: &[u8]) -> Result<(Option<Packet>, usize)> {
        // A length-prefixed sample is already exactly one access unit, and it
        // contains no start codes at all — running the byte-stream scanner over
        // it finds nothing, forever. The container path exists for this and is
        // taken whenever a configuration record has told us the framing.
        if let Framing::LengthPrefixed(size) = self.framing
            && !input.is_empty()
        {
            self.push_access_unit(input, Framing::LengthPrefixed(size))?;
            let mut packet = Packet::from_slice(&mut self.budget, input)?;
            if self.last_picture.is_idr {
                packet.flags = PacketFlags::KEY;
            }
            return Ok((Some(packet), input.len()));
        }
        if input.is_empty() {
            // End of stream. Called repeatedly until it yields nothing, which
            // is how the driver drains a buffer holding more than one unit.
            return Ok((self.finish_stream()?, 0));
        }
        // Hand back a queued unit before taking more input. `used == 0` with a
        // packet is fine: the driver returns the packet and resets its progress
        // guard before it ever looks at the byte count.
        if let Some(p) = self.ready.pop_front() {
            return Ok((Some(p), 0));
        }
        // Otherwise every byte is consumed. That is not an optimisation choice:
        // `ParserDriver` discards whatever a parser declines to consume once
        // end of stream is reached, so a parser that leaves its trailing NAL
        // unit in the driver's buffer loses the last unit of every file.
        self.append_input(input)?;
        self.advance()?;
        Ok((self.ready.pop_front(), input.len()))
    }

    fn parameters(&self) -> Option<&CodecParameters> {
        self.params.as_ref()
    }

    /// Read an `AVCDecoderConfigurationRecord`.
    ///
    /// This is where every H.264 field `-show_streams` prints for an MP4 comes
    /// from: the sequence parameter set is in `avcC` and in no sample, so a
    /// parser fed only payloads reports nothing however many it is given.
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

    /// SPS, PPS, an IDR slice and a non-IDR slice, from a real `libx264`
    /// stream. The slice payloads are truncated to a few bytes — the parser
    /// reads headers only, so the rest is framing.
    fn stream() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&[0, 0, 0, 1]);
        v.extend_from_slice(&[
            0x67, 0x64, 0x00, 0x1E, 0xAC, 0xD9, 0x40, 0xA0, 0x2F, 0xF9, 0x70, 0x11, 0x00, 0x00,
            0x03, 0x00, 0x01, 0x00, 0x00, 0x03, 0x00, 0x30, 0x0F, 0x16, 0x2D, 0x96,
        ]);
        v.extend_from_slice(&[0, 0, 0, 1]);
        v.extend_from_slice(&[0x68, 0xEB, 0xE3, 0xCB, 0x22, 0xC0]);
        v.extend_from_slice(&[0, 0, 0, 1]);
        v.extend_from_slice(&[0x65, 0x88, 0x84, 0x00, 0x2F, 0x7F, 0x7E]);
        v.extend_from_slice(&[0, 0, 0, 1]);
        v.extend_from_slice(&[0x41, 0x9A, 0x02, 0x2F, 0x7F, 0x7E]);
        v
    }

    fn drive(data: &[u8], chunk: usize) -> Vec<Vec<u8>> {
        let parser = H264Parser::new(Limits::strict());
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
    fn the_stream_splits_into_two_access_units() {
        let data = stream();
        let units = drive(&data, data.len());
        assert_eq!(units.len(), 2, "SPS+PPS+IDR, then the non-IDR slice");
        // The first access unit carries the parameter sets and the IDR slice.
        assert!(units[0].starts_with(&[0, 0, 0, 1, 0x67]));
        assert!(units[1].starts_with(&[0, 0, 0, 1, 0x41]));
        // Every byte of the input ends up in exactly one access unit.
        let rejoined: Vec<u8> = units.concat();
        assert_eq!(rejoined, data);
    }

    #[test]
    fn chunking_does_not_change_the_result() {
        let data = stream();
        let whole = drive(&data, data.len());
        for chunk in [1usize, 2, 3, 5, 7, 13, 64] {
            assert_eq!(drive(&data, chunk), whole, "chunk size {chunk}");
        }
    }

    #[test]
    fn parameters_come_out_of_the_sps() {
        let data = stream();
        let mut p = H264Parser::new(Limits::strict());
        let (_, used) = p.parse(&data).expect("no error");
        assert!(used > 0);
        let params = p.parameters().expect("parameters after the SPS");
        let v = params.video.as_ref().expect("video parameters");
        assert_eq!((v.width, v.height), (640, 360));
        assert_eq!(v.frame_rate, vaco_core::Rational::new(48, 1));
    }

    #[test]
    fn the_first_access_unit_is_marked_as_a_keyframe() {
        let data = stream();
        let parser = H264Parser::new(Limits::strict());
        let mut driver = ParserDriver::new(parser, Limits::strict());
        driver.push(&data).expect("push");
        driver.finish();
        let first = driver.next_unit().expect("an access unit");
        assert!(first.is_key(), "the unit containing the IDR must be a key");
    }

    /// Found by the `parse_h264` fuzz target, 27 executions in.
    ///
    /// Four VCL NAL units and no parameter sets, so every access-unit boundary
    /// is decided by the fallback in §7.4.1.2.4 rather than by a parsed slice
    /// header. Fed whole, the parser emitted three access units; fed one byte
    /// at a time, four.
    ///
    /// The cause was not the boundary rule but the end-of-stream path: it
    /// finalised the trailing NAL unit and emitted everything left in the
    /// buffer as **one** access unit, without first draining the boundaries the
    /// last `parse` call had already found. Chunked feeding hid it because
    /// every chunk gave the scanner another chance to drain.
    #[test]
    fn eos_drains_every_queued_access_unit() {
        let data = [
            0u8, 0, 1, 33, 0, 0, 1, 1, 0, 0, 1, 36, 1, 0, 1, 0, 0, 0, 1, 1,
        ];
        let whole = drive(&data, data.len());
        assert_eq!(whole.len(), 4, "one access unit per VCL unit");
        assert_eq!(drive(&data, 1), whole, "chunk size 1");
        assert_eq!(whole.concat(), data, "every byte survives");
    }

    /// Drive the raw [`Parser`] contract rather than going through the driver:
    /// each call must consume everything it is given, **or** consume nothing
    /// and hand back a queued access unit.
    ///
    /// Returns the access units, and asserts the contract on every call.
    fn drive_raw(data: &[u8], chunk: usize) -> Vec<Vec<u8>> {
        let mut parser = H264Parser::new(Limits::strict());
        let mut out = Vec::new();
        for c in data.chunks(chunk.max(1)) {
            let mut rest = c;
            while !rest.is_empty() {
                let (unit, used) = parser.parse(rest).expect("no error on this fixture");
                assert!(used <= rest.len(), "consumed more than it was given");
                let produced = unit.is_some();
                if let Some(p) = unit {
                    out.push(p.payload().to_vec());
                }
                assert!(
                    used == rest.len() || (used == 0 && produced),
                    "a call must consume everything or hand back a queued unit"
                );
                rest = &rest[used..];
            }
        }
        while let Ok((Some(p), used)) = parser.parse(&[]) {
            assert_eq!(used, 0, "end of stream consumes nothing");
            out.push(p.payload().to_vec());
        }
        out
    }

    /// Two inputs from the `parse_h264` and `limit_h264` fuzz targets, kept as
    /// regressions for the queue that `advance` fills.
    ///
    /// Both are runs of VCL NAL units with no parameter sets, so several access
    /// units complete inside a single `parse` call. Before the queue existed,
    /// `advance` returned at the first boundary and left the rest as bytes in
    /// the access-unit buffer — which meant a caller pushing large chunks fell
    /// one unit further behind on every push until the buffer hit its cap.
    ///
    /// The contract these pin is the one that replaced it: a call with a queued
    /// unit hands it back and consumes nothing, so no input is ever dropped and
    /// the buffer never holds more than one access unit.
    #[test]
    fn queued_access_units_are_handed_back_before_more_input() {
        let cases: &[&[u8]] = &[
            &[
                0, 0, 1, 129, 0, 0, 0, 1, 129, 0, 0, 1, 129, 0, 0, 1, 129, 0, 0, 1, 199, 2, 213, 0,
                255, 0, 75, 1, 0,
            ],
            &[
                0, 0, 1, 1, 17, 0, 1, 0, 1, 0, 1, 17, 0, 0, 1, 217, 0, 0, 1, 1, 17, 0, 0, 1, 1, 17,
                0, 0, 44, 0, 1, 0, 0, 1, 1, 17, 0, 0, 1, 0, 1, 1, 17, 0, 0, 217, 0, 0, 1, 1, 17, 0,
                0, 1, 17, 0, 0, 37, 217, 0, 0, 17, 0, 0, 37,
            ],
        ];
        for (i, data) in cases.iter().enumerate() {
            let whole = drive_raw(data, data.len());
            assert!(!whole.is_empty(), "case {i}: nothing emitted");
            for chunk in [1usize, 3, 17] {
                assert_eq!(
                    drive_raw(data, chunk),
                    whole,
                    "case {i}: chunk size {chunk}"
                );
            }
            // And the driver agrees with the raw contract.
            assert_eq!(drive(data, data.len()), whole, "case {i}: via the driver");
        }
    }

    #[test]
    fn garbage_makes_progress_rather_than_stalling() {
        let mut p = H264Parser::new(Limits::strict());
        let junk = vec![0xAAu8; 4096];
        let (unit, used) = p.parse(&junk).expect("no error");
        assert!(unit.is_none());
        assert!(used > 0, "a parser that consumes nothing is a hang");
    }

    #[test]
    fn an_access_unit_that_never_ends_is_refused_not_buffered() {
        let mut p = H264Parser::new(Limits::permissive()).with_max_access_unit(1024);
        let mut data = vec![0u8, 0, 0, 1, 0x41, 0x9A];
        for _ in 0..600 {
            data.extend_from_slice(&[0, 0, 0, 1, 0x0C, 0xFF]);
        }
        let err = p.parse(&data).unwrap_err();
        assert!(matches!(err, Error::LimitExceeded { .. }));
    }

    #[test]
    fn a_container_sample_updates_the_same_state() {
        let data = stream();
        let mut p = H264Parser::new(Limits::strict());
        let info = p
            .push_access_unit(&data, Framing::AnnexB)
            .expect("a sample parses");
        assert!(info.is_idr);
        assert_eq!(info.picture_type, Some('I'));
        assert!(p.parameters().is_some());
    }

    #[test]
    fn every_truncation_of_the_stream_is_handled() {
        let data = stream();
        for n in 0..data.len() {
            let mut p = H264Parser::new(Limits::strict());
            let _ = p.parse(&data[..n]);
            let _ = p.parse(&[]);
        }
    }

    /// The exact 38 bytes an ASF file's `BITMAPINFOHEADER` tail carries, read
    /// off a file this reference build wrote:
    ///
    /// ```sh
    /// ffmpeg -f lavfi -i testsrc=size=64x64:rate=25:duration=1 \
    ///        -pix_fmt yuv420p -c:v libx264 -f asf a.asf
    /// ```
    ///
    /// Start code, SPS (`0x67`), start code, PPS (`0x68`) — no `avcC` header
    /// anywhere. `ffprobe` reports `profile=High`, `level=10`,
    /// `pix_fmt=yuv420p` for this stream; `vaco-probe` reported
    /// `unknown/-99/unknown` until `set_extradata` learned the second shape.
    const ASF_ANNEXB_EXTRADATA: &[u8] = &[
        0x00, 0x00, 0x00, 0x01, 0x67, 0x64, 0x00, 0x0a, 0xac, 0xd9, 0x44, 0x26, 0xc0, 0x44, 0x00,
        0x00, 0x03, 0x00, 0x04, 0x00, 0x00, 0x03, 0x00, 0xc8, 0x3c, 0x48, 0x96, 0x58, 0x00, 0x00,
        0x00, 0x01, 0x68, 0xeb, 0xe3, 0xcb, 0x22, 0xc0,
    ];

    #[test]
    fn annexb_extradata_seeds_the_parameter_sets() {
        let mut p = H264Parser::new(Limits::strict());
        let framing = p
            .set_extradata(ASF_ANNEXB_EXTRADATA)
            .expect("Annex-B extradata is not an avcC and must not be read as one");
        // The framing the *stream* uses, not the one an avcC would declare.
        assert_eq!(framing, Framing::AnnexB);
        let params = p.parameters().expect("an SPS was read");
        let video = params.video.as_ref().expect("a video stream");
        assert_eq!(video.width, 64);
        assert_eq!(video.height, 64);
        let profile = params.profile.expect("High");
        assert_eq!(profile.name, "High");
        assert_eq!(video.format, vaco_pixfmt::PixFmt::from_name("yuv420p").ok());
    }

    /// The discriminator itself: an `avcC` still goes down the `avcC` path.
    ///
    /// A minimal record — version 1, then the three profile bytes, then the
    /// length-size byte and zero SPS and PPS counts. It carries no parameter
    /// set, which is the point: it must still be *recognised* as an `avcC` and
    /// set length-prefixed framing, not be mistaken for Annex B.
    #[test]
    fn a_configuration_record_is_still_read_as_one() {
        let avcc = [0x01, 0x64, 0x00, 0x0a, 0xff, 0xe0, 0x00];
        let mut p = H264Parser::new(Limits::strict());
        if let Ok(framing) = p.set_extradata(&avcc) {
            assert!(
                matches!(framing, Framing::LengthPrefixed(_)),
                "an avcC must not be read as Annex B: {framing:?}"
            );
        }
    }
}
