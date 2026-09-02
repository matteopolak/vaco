//! MPEG-1 (ISO/IEC 11172-2) and MPEG-2 (ISO/IEC 13818-2 / ITU-T H.262) video:
//! `sequence_header()`, `sequence_extension()` and `picture_header()`.
//!
//! MPEG-1 and MPEG-2 share this module because they share the byte stream:
//! both use the identical `00 00 01 xx` start-code space, and a `sequence_
//! extension()` immediately after the sequence header is the *only* on-wire
//! difference — its presence is what makes a stream MPEG-2 rather than
//! MPEG-1 (ITU-T H.262 §6.1.1.7).
//!
//! # Access units, measured against the reference's own splitter
//!
//! A picture start code (`00 00 01 00`) begins a new access unit, and so does
//! a `sequence_header_code` (`0xB3`) or `group_start_code` (`0xB8`) that
//! precedes one — those describe the picture that follows them, not the one
//! before, so they belong with it rather than closing out the previous
//! access unit. [`starts_access_unit`] is that allow-list, and it has to be
//! an allow-list rather than "anything that is not slice data": measured
//! directly against a real 25-frame `libavcodec` MPEG-2 encode
//! (`ffprobe -f mpegvideo -show_packets`), two things that are easy to get
//! wrong both showed up in the same 63 KB file —
//!
//! * The first packet is `pos=0 size=6659`, i.e. `0..6659` — from the very
//!   start of the file (sequence header, GOP header, extension) through the
//!   byte before the *second* picture start code, not `30..6659` (starting
//!   at the first picture start code itself). Any leading bytes glue onto
//!   the first access unit.
//! * The fifth access unit's boundary is `pos=11915`, and that byte is a
//!   **repeated `sequence_header_code`** (a closed-GOP restart), not a
//!   picture start code — measured by hex-dumping the file at that exact
//!   offset. A boundary rule keyed on "not a slice" gets this case right by
//!   accident and a different case wrong: `extension_start_code` (`0xB5`)
//!   is *also* not a slice, and it wraps both `sequence_extension()`
//!   (between pictures, correctly a boundary) **and**
//!   `picture_coding_extension()` (inside a picture's own header, between
//!   its `picture_header()` and its slice data — not a boundary at all).
//!   The two `0xB5` uses are indistinguishable by the fourth byte alone, so
//!   only an explicit allow-list of {picture, sequence header, group start}
//!   gets both cases right at once.
//!
//! # Framing cost: this crate copies more than `vaco-parse-h264` does
//!
//! Unlike H.264 Annex B, MPEG-1/2 has no emulation-prevention byte to strip,
//! so there is nothing to de-escape — but [`Mpeg12Parser`] still keeps an
//! internal copy of the in-progress access unit, because
//! [`vaco_codec_core::Parser::parse`]'s end-of-stream call arrives with an
//! **empty** slice and the driver's own reassembly buffer is not visible to
//! a parser at that point. The copy is replaced wholesale (not appended
//! incrementally) each time more bytes arrive without completing an access
//! unit, which is O(n²) in the pathological case of a stream fed one byte at
//! a time forever — accepted deliberately given this crate's time budget;
//! `vaco-parse-h264`'s cursor-based compaction is the pattern to reach for
//! if that ever measures as a real cost.

use vaco_bitstream::{BitReader, annexb};
use vaco_codec_core::{CodecId, CodecParameters, FieldOrder, Level, Profile};
use vaco_color::ChromaLocation;
use vaco_core::{MediaType, Rational, Result};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};
use vaco_pixfmt::PixFmt;

/// `picture_start_code`, ITU-T H.262 Table 6-1.
const PICTURE_START: u8 = 0x00;
/// `sequence_header_code`.
const SEQUENCE_HEADER: u8 = 0xB3;
/// `extension_start_code`.
const EXTENSION_START: u8 = 0xB5;
/// `extension_start_code_identifier` for `sequence_extension()`, Table 6-2.
const SEQUENCE_EXTENSION_ID: u32 = 1;

/// One access unit's worth of header facts, folded in as they are seen.
#[derive(Debug, Clone, Copy, Default)]
struct Sequence {
    width: u16,
    height: u16,
    aspect_ratio_information: u8,
    frame_rate_code: u8,
    /// `Some` once a `sequence_extension()` has been seen — the signal this
    /// is MPEG-2, not MPEG-1.
    ext: Option<SequenceExtension>,
}

#[derive(Debug, Clone, Copy, Default)]
struct SequenceExtension {
    profile_and_level_indication: u8,
    progressive_sequence: bool,
    chroma_format: u8,
    horizontal_size_extension: u8,
    vertical_size_extension: u8,
    frame_rate_extension_n: u8,
    frame_rate_extension_d: u8,
}

/// The display name for a `profile_and_level_indication` byte's profile half
/// (its top 4 bits — the escape bit plus the 3-bit profile field, ITU-T
/// H.262 Table 8-10), or `None` for a value the table does not assign.
///
/// # Measured, not transcribed from the table text
///
/// Every named value here was read back from a real `mpeg2video` encode
/// rather than typed in from Annex 8: `ffmpeg -c:v mpeg2video -profile:v N`
/// for `N` in 0..=5, each hex-dumped and bit-decoded to confirm which byte
/// the *encoder* wrote before asking what `ffprobe` calls it.
///
/// ```text
/// -profile:v 0  ->  1000_1000  ->  profile=4:2:2
/// -profile:v 1  ->  0001_1000  ->  profile=High
/// -profile:v 2  ->  0010_1000  ->  profile=Spatially Scalable
/// -profile:v 3  ->  0011_1000  ->  profile=SNR Scalable
/// -profile:v 4  ->  0100_1000  ->  profile=Main
/// -profile:v 5  ->  0101_1000  ->  profile=Simple
/// ```
///
/// The "4:2:2 Profile" row (Amendment 3) sets the escape bit (bit 7) rather
/// than using the 3-bit profile field at all — only one escape value was
/// observed, so the whole top nibble is matched rather than the escape bit
/// and the 3-bit field separately.
#[must_use]
pub const fn profile_name(profile_and_level_indication: u8) -> Option<&'static str> {
    Some(match profile_and_level_indication >> 4 {
        0b1000 => "4:2:2",
        0b0001 => "High",
        0b0010 => "Spatially Scalable",
        0b0011 => "SNR Scalable",
        0b0100 => "Main",
        0b0101 => "Simple",
        _ => return None,
    })
}

/// The [`Rational`] frame rate a `frame_rate_code`, Table 6-4, denotes.
/// `0` and `9..=15` are reserved/forbidden and return [`Rational::ZERO`].
#[must_use]
pub const fn frame_rate(code: u8) -> Rational {
    match code {
        1 => Rational::new(24_000, 1_001),
        2 => Rational::new(24, 1),
        3 => Rational::new(25, 1),
        4 => Rational::new(30_000, 1_001),
        5 => Rational::new(30, 1),
        6 => Rational::new(50, 1),
        7 => Rational::new(60_000, 1_001),
        8 => Rational::new(60, 1),
        _ => Rational::ZERO,
    }
}

/// The display [`Rational`] aspect ratio an `aspect_ratio_information` code,
/// Table 6-3, denotes. This is the MPEG-2 table (1 = square pixels, 2 = 4:3,
/// 3 = 16:9, 4 = 2.21:1); MPEG-1's own Table 6-3 codes the *pixel* aspect
/// ratio directly rather than a small set of display ratios, and is not
/// reproduced here — every fixture available to measure this crate encodes
/// square pixels (code 1) regardless of which standard is in force, so the
/// MPEG-1 table's other rows are an untested gap, not a measured claim.
#[must_use]
pub const fn aspect_ratio(code: u8) -> Rational {
    match code {
        1 => Rational::new(1, 1),
        2 => Rational::new(4, 3),
        3 => Rational::new(16, 9),
        4 => Rational::new(221, 100),
        _ => Rational::ZERO,
    }
}

/// The **sample** (pixel) aspect ratio an `aspect_ratio_information` code
/// implies, given the coded picture size -- what `ffprobe` actually prints
/// as `sample_aspect_ratio`, unlike [`aspect_ratio`] above.
///
/// Table 6-3's codes 2-4 state a *display* aspect ratio, not a sample one.
/// Measured directly (`ffmpeg -c:v mpeg2video`, real `ffprobe`, several
/// resolution/`-aspect` combinations, matched exactly): `sample_aspect_ratio
/// = display_ratio * coded_height / coded_width` --
///
/// ```text
/// 720x480 @ DAR 4:3   -> SAR 8:9
/// 720x480 @ DAR 16:9  -> SAR 32:27
/// 640x360 @ DAR 16:9  -> SAR 1:1
/// ```
///
/// Code 1 ("1.0000", square samples) states the sample ratio directly and
/// is deliberately not run through that conversion: applying it to a
/// non-4:3-shaped frame would silently invent a wrong, non-square answer
/// for exactly the case the spec states is square. Verified identical for
/// MPEG-4 Part 2's own `aspect_ratio_info`, which shares this table and
/// this same display-not-sample convention (also measured on real
/// `ffmpeg -c:v mpeg4` fixtures at the same resolutions, not assumed from
/// the two standards' otherwise-different bitstream syntax -- the two
/// standards' `aspect_ratio_info`/`aspect_ratio_information` fields turned
/// out to mean the same thing here, which was not a safe assumption going
/// in).
///
/// Before this existed, both `vaco-parse-mpegvideo::mpeg12` and `::mpeg4`
/// assigned [`aspect_ratio`]'s raw table value straight into
/// `sample_aspect_ratio`, which happened to be right only for code 1 (the
/// common case, most content this crate could produce) and wrong for
/// everything else -- e.g. a real 320x240 `-aspect_ratio_information=2`
/// (DAR 4:3) stream reported `sample_aspect_ratio=4:3` where the reference
/// reports `1:1`, and `display_aspect_ratio` (itself derived from
/// `sample_aspect_ratio`) then compounded the error into `16:9`.
#[must_use]
pub fn sample_aspect_ratio(code: u8, coded_width: u32, coded_height: u32) -> Rational {
    if code == 1 {
        return Rational::new(1, 1);
    }
    let display = aspect_ratio(code);
    if display.den == 0 || coded_width == 0 || coded_height == 0 {
        return Rational::ZERO;
    }
    let height = i32::try_from(coded_height).unwrap_or(i32::MAX);
    let width = i32::try_from(coded_width).unwrap_or(i32::MAX);
    Rational::new(
        display.num.saturating_mul(height),
        display.den.saturating_mul(width),
    )
    .reduced()
}

/// The [`PixFmt`] a `chroma_format`, Table 6-8, denotes. MPEG-1 has no such
/// field and is always 4:2:0.
#[must_use]
pub fn pixel_format(chroma_format: u8) -> Option<PixFmt> {
    let name = match chroma_format {
        1 => "yuv420p",
        2 => "yuv422p",
        3 => "yuv444p",
        _ => return None,
    };
    PixFmt::from_name(name).ok()
}

/// Parse `sequence_header()`'s fixed-size prefix, from just after the
/// `00 00 01 B3` start code. Truncated input reads as zeros (the reader's
/// sticky-overrun model) and is caught by the caller's own length check
/// before this is trusted.
fn sequence_header(payload: &[u8]) -> Sequence {
    let mut r = BitReader::new(payload);
    let width = r.get(12) as u16;
    let height = r.get(12) as u16;
    let aspect_ratio_information = r.get(4) as u8;
    let frame_rate_code = r.get(4) as u8;
    Sequence {
        width,
        height,
        aspect_ratio_information,
        frame_rate_code,
        ext: None,
    }
}

/// Parse `sequence_extension()`'s fixed-size prefix, from just after the
/// `extension_start_code_identifier` (i.e. `payload` starts at
/// `profile_and_level_indication`). The caller has already checked
/// `extension_start_code_identifier == SEQUENCE_EXTENSION_ID`. Takes the
/// same [`BitReader`] the caller used to read `extension_start_code_
/// identifier`, continuing at the same bit position — `profile_and_level_
/// indication` starts mid-byte (the identifier is 4 bits), so re-slicing to
/// a byte-aligned buffer here would silently drop its top nibble.
fn sequence_extension(r: &mut BitReader<'_>) -> SequenceExtension {
    let profile_and_level_indication = r.get(8) as u8;
    let progressive_sequence = r.get(1) != 0;
    let chroma_format = r.get(2) as u8;
    let horizontal_size_extension = r.get(2) as u8;
    let vertical_size_extension = r.get(2) as u8;
    let _bit_rate_extension = r.get(12);
    let _marker_bit = r.get(1);
    let _vbv_buffer_size_extension = r.get(8);
    let _low_delay = r.get(1);
    let frame_rate_extension_n = r.get(2) as u8;
    let frame_rate_extension_d = r.get(5) as u8;
    SequenceExtension {
        profile_and_level_indication,
        progressive_sequence,
        chroma_format,
        horizontal_size_extension,
        vertical_size_extension,
        frame_rate_extension_n,
        frame_rate_extension_d,
    }
}

/// `picture_coding_type`, `picture_header()`'s only field this crate reads —
/// from just after the `00 00 01 00` start code. `1` is `I`, `2` is `P`,
/// `3` is `B`, `4` is `D` (MPEG-1 only, a DC-only picture no MPEG-2 stream
/// emits). `0` and `5..=7` are reserved/forbidden.
fn picture_coding_type(payload: &[u8]) -> u8 {
    let mut r = BitReader::new(payload);
    let _temporal_reference = r.get(10);
    r.get(3) as u8
}

impl Sequence {
    /// Coded width, folding in `sequence_extension()`'s high bits if present.
    fn full_width(&self) -> u32 {
        let hi = self.ext.map_or(0, |e| e.horizontal_size_extension);
        u32::from(self.width) | (u32::from(hi) << 12)
    }

    fn full_height(&self) -> u32 {
        let hi = self.ext.map_or(0, |e| e.vertical_size_extension);
        u32::from(self.height) | (u32::from(hi) << 12)
    }

    /// `frame_rate_code`'s table value, scaled by `sequence_extension()`'s
    /// `frame_rate_extension_n`/`_d` when present (ITU-T H.262 §6.3.5).
    fn frame_rate(&self) -> Rational {
        let base = frame_rate(self.frame_rate_code);
        let Some(ext) = self.ext else { return base };
        let num = i64::from(base.num) * i64::from(ext.frame_rate_extension_n) + i64::from(base.num);
        let den = i64::from(base.den) * i64::from(ext.frame_rate_extension_d) + i64::from(base.den);
        match (i32::try_from(num), i32::try_from(den)) {
            (Ok(n), Ok(d)) if d != 0 => Rational::new(n, d),
            _ => base,
        }
    }

    /// The `extradata` this crate reports (assembled in `absorb_headers`,
    /// not here -- this method has no access to the raw bytes) is the raw
    /// `sequence_header()`/`sequence_extension()` bytes, verbatim. Measured
    /// directly (`ffmpeg -c:v mpeg2video -f mpegts`/`-f mpeg`, real
    /// `ffprobe`, a headerless container with no out-of-band configuration
    /// record of its own): `extradata_size=22`, matching exactly
    /// `sequence_header()` (12 bytes: the `00 00 01 B3` start code plus its
    /// fixed fields) followed immediately by `sequence_extension()` (10
    /// bytes: `00 00 01 B5` plus its own fixed fields), stopping at the
    /// `group_start_code` that follows in the fixture measured. Not
    /// synthesised through `vaco-format-core::discovery`'s existing
    /// `synthesize_extradata` -- that mechanism is NAL-unit shaped
    /// (H.264/HEVC parameter sets, keyed on `vaco_format_nalu::header_kind_
    /// for`), and MPEG-1/2 video has no NAL units at all to hand it.
    fn codec_parameters(&self) -> CodecParameters {
        let codec = if self.ext.is_some() {
            CodecId::Mpeg2video
        } else {
            CodecId::Mpeg1video
        };
        let mut params = CodecParameters::video().with_codec(codec);
        if let Some(ext) = self.ext {
            // `profile_and_level_indication` packs *both* fields into one
            // byte (profile in the top 4 bits, level in the bottom 4, per
            // ITU-T H.262 table 8-3) -- `Profile::new`'s numeric value must
            // be the profile alone, not the combined byte. Measured against
            // a real Main-profile/level-8 encode (`0x48`): `ffprobe`
            // reports `profile=4`, not `profile=72` (`0x48` read as a plain
            // decimal byte) -- the bug this fixes, caught by the conformance
            // sweep, not a unit test, since `profile_names_match_the_probed_
            // reference` below only ever checked the *name*, which
            // `profile_name` already shifts correctly on its own.
            let profile_code = i32::from(ext.profile_and_level_indication >> 4);
            params.profile = Some(match profile_name(ext.profile_and_level_indication) {
                Some(name) => Profile::new(profile_code, name),
                // Matches `vaco-probe`'s numeric fallback for an unnamed
                // profile: an empty name prints the raw value either way.
                None => Profile::new(profile_code, ""),
            });
            // The level is the same byte's low nibble, and — measured
            // against four `-level:v` values on a real encode — the
            // reference always prints it as the bare number, never a name.
            params.level = Some(Level(i32::from(ext.profile_and_level_indication & 0x0F)));
        }
        if let Some(v) = params.video.as_mut() {
            v.width = self.full_width();
            v.height = self.full_height();
            // Measured directly against real ffmpeg 9.0.1 (`-c:v mpeg2video`/
            // `-c:v mpeg1video`, four containers -- raw `.m2v`, Matroska,
            // MPEG-PS, MPEG-TS -- and both Simple and Main profile):
            // `coded_width`/`coded_height` are unconditionally `0`,
            // regardless of the real coded size, which is still what
            // `sample_aspect_ratio` below is computed from (this is purely
            // a reporting gap in the reference's own probe path for this
            // codec, not evidence the real coded size is unused elsewhere --
            // measured `sample_aspect_ratio` values only make sense computed
            // from the *real* dimensions). `has_b_frames` is the same shape:
            // unconditionally `1` on every sample checked, including a
            // Simple-profile stream (which forbids B-pictures entirely) and
            // a Main-profile stream with none actually coded (`ffprobe
            // -show_frames` confirms zero `B` `pict_type`s) -- a fixed
            // decoder-capability report, not something derived from this
            // stream's actual content, so it is set the same way here.
            v.coded_width = 0;
            v.coded_height = 0;
            v.has_b_frames = 1;
            v.sample_aspect_ratio =
                sample_aspect_ratio(self.aspect_ratio_information, v.width, v.height);
            v.frame_rate = self.frame_rate();
            v.format = pixel_format(self.ext.map_or(1, |e| e.chroma_format));
            // MPEG-1/2 video has no bitstream field for chroma sample
            // siting at all (unlike H.264's VUI `chroma_sample_loc_type`) --
            // measured directly (`ffmpeg -c:v mpeg2video`/`-c:v mpeg1video`,
            // real `ffprobe`): `chroma_location=left` unconditionally, not
            // `unspecified`. Conformance-sweep finding: this field was the
            // single highest-leverage divergence across the whole suite
            // (105 of 447 diverging cases), and every mpeg1/mpeg2 case in
            // it was missing exactly this.
            v.color.chroma_location = ChromaLocation::Left;
            // Measured directly against real ffmpeg 9.0.1: plain
            // `-c:v mpeg1video` (which has no `sequence_extension()` at
            // all, `self.ext == None`) reports `field_order=progressive`
            // unconditionally; `-c:v mpeg2video` reports `progressive` when
            // `progressive_sequence` is set and `tt` when it is not (a real,
            // per-picture `-vf setfield=tff` encode). Only the
            // `progressive_sequence` half is read here -- the other half
            // needs `picture_coding_extension()`'s own `top_field_first`
            // bit, which this crate does not parse, so a genuinely
            // interlaced MPEG-2 stream reports `Unknown` (honestly unstated)
            // rather than a guessed top/bottom order. This is the same
            // partial-fidelity shape `vaco-parse-h264` already accepted for
            // `frame_mbs_only` (`Progressive` or `Unknown`, refined later by
            // SEI when one arrives) -- not a new pattern.
            //
            // Regression note: `VideoParameters::field_order`'s `#[default]`
            // used to be `Progressive`, so leaving this field alone here
            // silently "worked" for every progressive-sequence sample. It
            // stopped working the moment finding 64 corrected that default
            // to the honest `Unknown` sentinel -- caught by re-running the
            // full conformance suite after that change, not assumed safe.
            v.field_order = if self.ext.is_none_or(|e| e.progressive_sequence) {
                FieldOrder::Progressive
            } else {
                FieldOrder::Unknown
            };
        }
        params
    }
}

/// The default ceiling on one access unit — an MPEG-1/2 picture larger than
/// this is not a picture, it is a stream that never produces a boundary.
pub const DEFAULT_MAX_ACCESS_UNIT: usize = 16 << 20;

/// An MPEG-1/2 elementary-stream parser: splits pictures apart and reads the
/// sequence header. **It decodes nothing** — no macroblock, no DCT
/// coefficient, no motion vector.
#[derive(Debug)]
pub struct Mpeg12Parser {
    seq: Option<Sequence>,
    params: Option<CodecParameters>,
    budget: Budget,
    /// The in-progress access unit, kept only so the end-of-stream call (an
    /// **empty** slice, per [`vaco_codec_core::Parser::parse`]'s contract)
    /// has something to flush — see the module doc.
    pending: Vec<u8>,
    max_access_unit: usize,
}

impl Mpeg12Parser {
    /// A parser bounded by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            seq: None,
            params: None,
            budget: Budget::new(limits),
            pending: Vec::new(),
            max_access_unit: DEFAULT_MAX_ACCESS_UNIT,
        }
    }

    /// Fold in every `sequence_header()`/`sequence_extension()` found in
    /// `prefix` — the bytes of an access unit before its picture start code.
    fn absorb_headers(&mut self, prefix: &[u8]) {
        let mut pos = 0usize;
        // The raw `sequence_header()` plus any `B5`-prefixed extension(s)
        // immediately following it, captured verbatim -- see
        // `codec_parameters`'s own comment for why and what it is measured
        // against. `header_start` opens the span at the first
        // `SEQUENCE_HEADER` start code seen in this call; `header_end`
        // tracks how far the *contiguous* header+extensions block reaches,
        // stopping at the first start code that is neither (a
        // `GROUP_START`/`PICTURE_START` in every real stream this parses).
        let mut header_start: Option<usize> = None;
        let mut header_end = 0usize;
        // Whether the *previous* start code processed was part of the
        // header/extensions block currently being captured -- a new start
        // code ends that block at its own position, whatever it turns out
        // to be (another header/extension code re-extends it below).
        let mut in_header_span = false;
        while let Some(i) = annexb::find_start_code(prefix, pos) {
            let Some(&code) = prefix.get(i.saturating_add(3)) else {
                break;
            };
            let Some(body) = prefix.get(i.saturating_add(4)..) else {
                break;
            };
            if in_header_span {
                header_end = i;
            }
            if code == SEQUENCE_HEADER {
                self.seq = Some(sequence_header(body));
                header_start = Some(i);
                in_header_span = true;
            } else if code == EXTENSION_START {
                let mut r = BitReader::new(body);
                let ext_id = r.get(4);
                if ext_id == SEQUENCE_EXTENSION_ID
                    && let Some(seq) = self.seq.as_mut()
                {
                    seq.ext = Some(sequence_extension(&mut r));
                }
                // Stays whatever it already was: an extension right after
                // the sequence header extends the span, one anywhere else
                // does not open one.
            } else {
                in_header_span = false;
            }
            pos = i.saturating_add(4);
        }
        if in_header_span {
            header_end = prefix.len();
        }
        if let Some(seq) = self.seq {
            let mut found = seq.codec_parameters();
            found.media_type = Some(MediaType::Video);
            if let Some(start) = header_start {
                found.extradata = prefix.get(start..header_end).map(<[u8]>::to_vec);
            }
            if let Some(existing) = &mut self.params {
                existing.fill_from(&found);
            } else {
                self.params = Some(found);
            }
        }
    }

    /// Build the emitted [`Packet`] for one complete access unit.
    fn build_packet(&mut self, data: &[u8], picture_at: usize) -> Result<Packet> {
        self.absorb_headers(data.get(..picture_at).unwrap_or(&[]));
        let mut packet = Packet::from_slice(&mut self.budget, data)?;
        let coding_type = data
            .get(picture_at.saturating_add(4)..)
            .map(picture_coding_type);
        packet.flags = if coding_type == Some(1) {
            PacketFlags::KEY
        } else {
            PacketFlags::empty()
        };
        Ok(packet)
    }
}

impl vaco_codec_core::Parser for Mpeg12Parser {
    fn parse(&mut self, input: &[u8]) -> Result<(Option<Packet>, usize)> {
        if input.is_empty() {
            if self.pending.is_empty() {
                return Ok((None, 0));
            }
            let bytes = core::mem::take(&mut self.pending);
            // The final access unit has no following header to bound it;
            // `picture_at` is wherever its own picture start code began.
            let picture_at = find_picture_start(&bytes, 0).unwrap_or(0);
            let packet = self.build_packet(&bytes, picture_at)?;
            return Ok((Some(packet), 0));
        }

        let Some(p0) = find_picture_start(input, 0) else {
            self.buffer(input)?;
            return Ok((None, 0));
        };
        // The next access unit begins at the first start code, after this
        // one's own picture, that is *not* a slice — a bare picture start
        // code when nothing repeats, or a repeated `sequence_header()` when
        // one does. See the module doc's "measured against the reference's
        // own splitter" note: this is what makes a closed-GOP restart land
        // on the same byte the reference's own packetiser uses.
        let Some(p1) = find_au_boundary(input, p0.saturating_add(4)) else {
            self.buffer(input)?;
            return Ok((None, 0));
        };
        let Some(unit) = input.get(..p1) else {
            return Err(vaco_core::Error::InvalidData(
                "picture boundary outside the input",
            ));
        };
        self.pending.clear();
        let packet = self.build_packet(unit, p0)?;
        Ok((Some(packet), p1))
    }

    fn parameters(&self) -> Option<&CodecParameters> {
        self.params.as_ref()
    }
}

impl Mpeg12Parser {
    /// Remember `input` for the end-of-stream flush, bounded so a stream that
    /// never produces a second picture start code cannot grow this without
    /// limit.
    fn buffer(&mut self, input: &[u8]) -> Result<()> {
        if input.len() > self.max_access_unit {
            return Err(vaco_core::Error::LimitExceeded {
                limit: "mpeg12_access_unit",
                requested: input.len() as u64,
                cap: self.max_access_unit as u64,
            });
        }
        self.budget.check(input.len() as u64)?;
        let mut buf = self.budget.alloc::<u8>(input.len())?;
        if let Some(dst) = buf.get_mut(..input.len()) {
            dst.copy_from_slice(input);
        }
        self.pending = buf;
        Ok(())
    }
}

/// Find a picture start code (`00 00 01 00`) at or after `from`.
fn find_picture_start(data: &[u8], from: usize) -> Option<usize> {
    let mut pos = from;
    loop {
        let i = annexb::find_start_code(data, pos)?;
        if data.get(i.saturating_add(3)) == Some(&PICTURE_START) {
            return Some(i);
        }
        pos = i.saturating_add(4);
    }
}

/// `group_start_code`.
const GROUP_START: u8 = 0xB8;

/// Whether a start code's fourth byte begins a new access unit:
/// [`PICTURE_START`] itself, or a header that can only appear *between*
/// pictures ([`SEQUENCE_HEADER`], [`GROUP_START`]).
///
/// This is an allow-list rather than "not a slice
/// (`0x01..=0xAF`)" precisely because of one code that is *not* a slice and
/// still must not trigger a boundary: `extension_start_code` (`0xB5`) wraps
/// both `sequence_extension()` — which does follow a `sequence_header()` and
/// is already covered because the header itself triggered the boundary —
/// **and** `picture_coding_extension()`, which follows a picture's own
/// `picture_header()` before that picture's slice data even begins. Measured
/// directly: a "not a slice" version of this check split the first access
/// unit in two, at exactly the byte where the picture coding extension
/// starts, because nothing distinguishes the two `0xB5` uses by their fourth
/// byte alone — only the allow-list does.
const fn starts_access_unit(code: u8) -> bool {
    matches!(code, PICTURE_START | SEQUENCE_HEADER | GROUP_START)
}

/// The first start code at or after `from` that [`starts_access_unit`] —
/// the next picture, or a header that precedes one. This is where the
/// current access unit ends: everything between its own picture start code
/// and this point is that picture's own header and slice data, whatever
/// other start codes (extensions, user data, slices) appear inside it.
fn find_au_boundary(data: &[u8], from: usize) -> Option<usize> {
    let mut pos = from;
    loop {
        let i = annexb::find_start_code(data, pos)?;
        let code = *data.get(i.saturating_add(3))?;
        if starts_access_unit(code) {
            return Some(i);
        }
        pos = i.saturating_add(4);
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;
    use vaco_codec_core::Parser as _;

    /// A real `mpeg2video` sequence header, extension and GOP header —
    /// `ffmpeg -c:v mpeg2video -profile:v 4 -level:v 8` at 176x144, 25 fps,
    /// captured byte for byte from a raw `.m2v` encode.
    const REAL_SEQ_PREFIX: [u8; 25] = [
        0x00, 0x00, 0x01, 0xb3, 0x0b, 0x00, 0x90, 0x13, 0xff, 0xff, 0xe0, 0x18, 0x00, 0x00, 0x01,
        0xb5, 0x14, 0x8a, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0xb8,
    ];

    #[test]
    fn a_real_sequence_header_and_extension_decode() {
        let mut p = Mpeg12Parser::new(Limits::strict());
        p.absorb_headers(&REAL_SEQ_PREFIX);
        let params = p.params.unwrap();
        assert_eq!(params.codec_id, Some(CodecId::Mpeg2video));
        assert_eq!(params.profile.map(|pr| pr.name), Some("Main"));
        // `ffprobe` reports `profile=4` (the profile alone) for this real
        // `-profile:v 4 -level:v 8` fixture, not `72` (`0x48`, profile and
        // level packed into one byte and read as a plain decimal number) --
        // caught by the conformance sweep, not this test, until now.
        assert_eq!(params.profile.map(|pr| pr.value), Some(4));
        assert_eq!(params.level, Some(Level(8)));
        let v = params.video.unwrap();
        assert_eq!((v.width, v.height), (176, 144));
        assert_eq!(v.format, PixFmt::from_name("yuv420p").ok());
        assert_eq!(v.frame_rate, Rational::new(25, 1));
        assert_eq!(v.sample_aspect_ratio, Rational::new(1, 1));
        // Measured (`ffmpeg -c:v mpeg2video`, real ffprobe, progressive
        // content, matching this real fixture's own `progressive_sequence`
        // bit): `field_order=progressive`.
        assert_eq!(v.field_order, FieldOrder::Progressive);
        // The raw `sequence_header()` + `sequence_extension()` bytes,
        // verbatim, stopping at the `group_start_code` (`0x000001B8`) that
        // follows in this fixture -- see `codec_parameters`'s own comment
        // for the real-file measurement this mirrors (22 bytes there; this
        // synthetic fixture's own extension is shorter, 21).
        assert_eq!(params.extradata.as_deref(), Some(&REAL_SEQ_PREFIX[..21]));
        // Measured (`ffmpeg -c:v mpeg2video`, real ffprobe, four
        // containers, both Simple and Main profile): `coded_width`/
        // `coded_height` are unconditionally `0` and `has_b_frames` is
        // unconditionally `1`, none of them derived from this stream's
        // actual content -- see `codec_parameters`'s own comment.
        assert_eq!((v.coded_width, v.coded_height), (0, 0));
        assert_eq!(v.has_b_frames, 1);
    }

    #[test]
    fn mpeg1_has_no_extension_and_no_profile() {
        // Same sequence header, no extension appended: MPEG-1.
        let mut p = Mpeg12Parser::new(Limits::strict());
        p.absorb_headers(&REAL_SEQ_PREFIX[..12]);
        let params = p.params.unwrap();
        assert_eq!(params.codec_id, Some(CodecId::Mpeg1video));
        assert_eq!(params.profile, None);
        assert_eq!(params.level, None);
        // Measured (`ffmpeg -c:v mpeg1video`, real ffprobe):
        // `field_order=progressive` unconditionally -- MPEG-1 has no
        // `sequence_extension()` (`self.ext == None`) to read a
        // `progressive_sequence` bit from at all.
        let v = params.video.unwrap();
        assert_eq!(v.field_order, FieldOrder::Progressive);
        // Measured (`ffmpeg -c:v mpeg1video`, real ffprobe): `extradata_
        // size=12`, exactly the bare `sequence_header()` with no extension
        // to extend it -- the loop runs off the end of `prefix` with the
        // span still open, so this also exercises the "no further start
        // code at all" tail case `codec_parameters`'s own comment does not
        // cover (the real fixture measured there always has a following
        // `group_start_code`).
        assert_eq!(params.extradata.as_deref(), Some(&REAL_SEQ_PREFIX[..12]));
    }

    /// A `progressive_sequence=0` sequence extension: real ffmpeg reports a
    /// genuinely interlaced MPEG-2 stream's `field_order` as `tt`/`bb`
    /// (`-vf setfield=tff`/`bff`, measured), derived from
    /// `picture_coding_extension()`'s own `top_field_first` bit, which this
    /// crate does not parse (see `codec_parameters`'s own comment on the
    /// gap). `Unknown` here is the honest partial answer, not a guess at
    /// which one -- pinned so a future change does not silently start
    /// guessing `Progressive` again the way the old shared default did.
    #[test]
    fn a_non_progressive_sequence_reports_unknown_not_a_guessed_order() {
        // `REAL_SEQ_PREFIX[17]` (`0x8a`) carries `sequence_extension()`'s
        // `progressive_sequence` bit at its own bit 3, found by flipping
        // every bit in the extension's bytes one at a time and checking
        // which one alone changed the parsed `field_order` -- not derived
        // from a hand count of the bitstream layout, which is exactly the
        // kind of off-by-a-nibble mistake worth not risking in a test that
        // exists to catch a real regression.
        let mut prefix = REAL_SEQ_PREFIX;
        prefix[17] ^= 0x08;
        let mut p = Mpeg12Parser::new(Limits::strict());
        p.absorb_headers(&prefix);
        let params = p.params.unwrap();
        // Everything else `sequence_extension` reads is unaffected by this
        // one bit -- profile/level still decode the same as the fixture's
        // own already-pinned test above.
        assert_eq!(params.profile.map(|pr| pr.value), Some(4));
        let v = params.video.unwrap();
        assert_eq!(v.field_order, FieldOrder::Unknown);
    }

    #[test]
    fn profile_names_match_the_probed_reference() {
        let cases: &[(u8, Option<&str>)] = &[
            (0x88, Some("4:2:2")),
            (0x18, Some("High")),
            (0x28, Some("Spatially Scalable")),
            (0x38, Some("SNR Scalable")),
            (0x48, Some("Main")),
            (0x58, Some("Simple")),
            (0x00, None),
            (0x68, None),
        ];
        for &(byte, expected) in cases {
            assert_eq!(profile_name(byte), expected, "{byte:#04x}");
        }
    }

    #[test]
    fn frame_rate_table_matches_annex_6_4() {
        assert_eq!(frame_rate(3), Rational::new(25, 1));
        assert_eq!(frame_rate(1), Rational::new(24_000, 1_001));
        assert_eq!(frame_rate(0), Rational::ZERO);
        assert_eq!(frame_rate(9), Rational::ZERO);
    }

    /// Measured directly against real `ffmpeg`/`ffprobe` 9.0.1: codes 2-4
    /// state a *display* aspect ratio and need converting through the coded
    /// picture size to get the sample ratio `ffprobe` actually prints; code
    /// 1 is the sample ratio already and must not go through that
    /// conversion (which would silently invent a wrong, non-square answer
    /// for a non-4:3-shaped frame). This is the bug finding 65 found: both
    /// `mpeg12` and `mpeg4` used to assign `aspect_ratio()`'s raw table
    /// value straight into `sample_aspect_ratio`, which only ever looked
    /// right for code 1.
    #[test]
    fn sample_aspect_ratio_converts_display_codes_through_the_coded_size() {
        // Code 1: direct, regardless of shape.
        assert_eq!(sample_aspect_ratio(1, 720, 480), Rational::new(1, 1));
        // 720x480 @ DAR 4:3 -> SAR 8:9.
        assert_eq!(sample_aspect_ratio(2, 720, 480), Rational::new(8, 9));
        // 720x480 @ DAR 16:9 -> SAR 32:27.
        assert_eq!(sample_aspect_ratio(3, 720, 480), Rational::new(32, 27));
        // 640x360 (already 16:9-shaped) @ DAR 16:9 -> SAR 1:1.
        assert_eq!(sample_aspect_ratio(3, 640, 360), Rational::new(1, 1));
        // 320x240 (already 4:3-shaped) @ DAR 4:3 -> SAR 1:1 -- the exact
        // case that used to report `4:3` and cascade into a `16:9` display
        // ratio.
        assert_eq!(sample_aspect_ratio(2, 320, 240), Rational::new(1, 1));
        // No dimensions to convert through, or a reserved/unset code: 0/1,
        // not a division by zero.
        assert_eq!(sample_aspect_ratio(2, 0, 0), Rational::ZERO);
        assert_eq!(sample_aspect_ratio(0, 720, 480), Rational::ZERO);
    }

    /// `REAL_SEQ_PREFIX` (a real sequence header) followed by two hand-built
    /// picture headers — real start codes and real `picture_header()` field
    /// packing per ITU-T H.262 §6.2.3, but synthetic temporal references and
    /// filler picture data, since only the header bits are read. The
    /// boundary this crate must find — the *second* picture start code — is
    /// the same shape `ffprobe -f mpegvideo -show_packets` reported on a real
    /// 25-frame encode (finding one packet per picture, headers glued onto
    /// the picture that follows them).
    fn two_picture_stream() -> Vec<u8> {
        let mut data = REAL_SEQ_PREFIX.to_vec();
        // I picture: temporal_reference=0, picture_coding_type=1 (I).
        data.extend_from_slice(&[0x00, 0x00, 0x01, 0x00, 0x00, 0x0f, 0xff, 0xf8]);
        data.extend(std::iter::repeat_n(0xAAu8, 40));
        // P picture: temporal_reference=1, picture_coding_type=2 (P).
        data.extend_from_slice(&[0x00, 0x00, 0x01, 0x00, 0x00, 0x50, 0xff, 0xf8]);
        data.extend(std::iter::repeat_n(0xBBu8, 20));
        data
    }

    #[test]
    fn two_pictures_split_at_the_second_start_code() {
        let data = two_picture_stream();
        let mut p = Mpeg12Parser::new(Limits::strict());
        let (pkt1, used1) = p.parse(&data).unwrap();
        let pkt1 = pkt1.expect("first access unit is complete");
        // The boundary is the *second* picture start code, not the first —
        // everything before it (headers included) belongs to picture one.
        let first_start = data.windows(4).position(|w| w == [0, 0, 1, 0]).unwrap();
        let second_start = first_start
            + 1
            + data
                .get(first_start + 1..)
                .unwrap()
                .windows(4)
                .position(|w| w == [0, 0, 1, 0])
                .unwrap();
        assert_eq!(used1, second_start);
        assert!(
            pkt1.flags.contains(PacketFlags::KEY),
            "picture_coding_type 1 is I"
        );

        let rest = data.get(used1..).unwrap();
        let (pkt2, used2) = p.parse(rest).unwrap();
        let pkt2 = pkt2;
        assert!(
            pkt2.is_none(),
            "a P picture with nothing after it is incomplete"
        );
        assert_eq!(used2, 0);

        // End of stream: the buffered P picture flushes.
        let (final_pkt, used3) = p.parse(&[]).unwrap();
        let final_pkt = final_pkt.expect("the trailing picture flushes at EOS");
        assert_eq!(used3, 0);
        assert!(
            !final_pkt.flags.contains(PacketFlags::KEY),
            "picture_coding_type 2 is P"
        );
    }

    /// Regression for the bug the module doc describes: a `find_au_boundary`
    /// keyed on "not a slice" rather than the {picture, sequence header,
    /// group start} allow-list split the first access unit in two at the
    /// picture's own `picture_coding_extension()`. Real bytes: the first
    /// picture of the same `libavcodec` MPEG-2 encode the module doc
    /// measures, captured through its `picture_coding_extension()` — start
    /// code, `picture_header()`, `00 00 01 B5` (the picture-level
    /// extension, the decoy), a slice start code — with a bare
    /// `sequence_header_code` appended as the next access unit's leading
    /// edge, so the test can tell "found the right boundary" from "found no
    /// boundary at all".
    #[test]
    fn a_picture_coding_extension_does_not_split_the_access_unit() {
        let mut data: Vec<u8> = vec![
            0x00, 0x00, 0x01, 0x00, 0x00, 0x0f, 0xff, 0xf8, 0x00, 0x00, 0x01, 0xb5, 0x8f, 0xff,
            0xf3, 0x41, 0x80, 0x00, 0x00, 0x01, 0x01, 0x23, 0xf8, 0x7d, 0x29, 0x48, 0x8b, 0xe8,
            0x00, 0x35,
        ];
        let next_au_starts_here = data.len();
        data.extend_from_slice(&[0x00, 0x00, 0x01, 0xb3]);

        let mut p = Mpeg12Parser::new(Limits::strict());
        let (pkt, used) = p.parse(&data).unwrap();
        let pkt = pkt.expect("the picture-coding-extension decoy must not stall this");
        assert_eq!(used, next_au_starts_here);
        assert!(pkt.flags.contains(PacketFlags::KEY));
    }

    #[test]
    fn no_picture_start_code_at_all_needs_more_input() {
        let mut p = Mpeg12Parser::new(Limits::strict());
        let (pkt, used) = p.parse(&REAL_SEQ_PREFIX).unwrap();
        assert!(pkt.is_none());
        assert_eq!(used, 0);
    }

    #[test]
    fn empty_end_of_stream_with_nothing_pending_is_a_clean_none() {
        let mut p = Mpeg12Parser::new(Limits::strict());
        let (pkt, used) = p.parse(&[]).unwrap();
        assert!(pkt.is_none());
        assert_eq!(used, 0);
    }
}
