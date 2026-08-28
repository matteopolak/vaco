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
use vaco_codec_core::{CodecId, CodecParameters, Level, Profile};
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
    let _progressive_sequence = r.get(1);
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

    fn codec_parameters(&self) -> CodecParameters {
        let codec = if self.ext.is_some() {
            CodecId::Mpeg2video
        } else {
            CodecId::Mpeg1video
        };
        let mut params = CodecParameters::video().with_codec(codec);
        if let Some(ext) = self.ext {
            params.profile = Some(match profile_name(ext.profile_and_level_indication) {
                Some(name) => Profile::new(i32::from(ext.profile_and_level_indication), name),
                // Matches `vaco-probe`'s numeric fallback for an unnamed
                // profile: an empty name prints the raw value either way.
                None => Profile::new(i32::from(ext.profile_and_level_indication), ""),
            });
            // The level is the same byte's low nibble, and — measured
            // against four `-level:v` values on a real encode — the
            // reference always prints it as the bare number, never a name.
            params.level = Some(Level(i32::from(ext.profile_and_level_indication & 0x0F)));
        }
        if let Some(v) = params.video.as_mut() {
            v.width = self.full_width();
            v.height = self.full_height();
            v.coded_width = v.width;
            v.coded_height = v.height;
            v.sample_aspect_ratio = aspect_ratio(self.aspect_ratio_information);
            v.frame_rate = self.frame_rate();
            v.format = pixel_format(self.ext.map_or(1, |e| e.chroma_format));
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
        while let Some(i) = annexb::find_start_code(prefix, pos) {
            let Some(&code) = prefix.get(i.saturating_add(3)) else {
                break;
            };
            let Some(body) = prefix.get(i.saturating_add(4)..) else {
                break;
            };
            if code == SEQUENCE_HEADER {
                self.seq = Some(sequence_header(body));
            } else if code == EXTENSION_START {
                let mut r = BitReader::new(body);
                let ext_id = r.get(4);
                if ext_id == SEQUENCE_EXTENSION_ID
                    && let Some(seq) = self.seq.as_mut()
                {
                    seq.ext = Some(sequence_extension(&mut r));
                }
            }
            pos = i.saturating_add(4);
        }
        if let Some(seq) = self.seq {
            let found = seq.codec_parameters();
            if let Some(existing) = &mut self.params {
                existing.fill_from(&found);
            } else {
                let mut found = found;
                found.media_type = Some(MediaType::Video);
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
        assert_eq!(params.level, Some(Level(8)));
        let v = params.video.unwrap();
        assert_eq!((v.width, v.height), (176, 144));
        assert_eq!(v.format, PixFmt::from_name("yuv420p").ok());
        assert_eq!(v.frame_rate, Rational::new(25, 1));
        assert_eq!(v.sample_aspect_ratio, Rational::new(1, 1));
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
        let first_start = data
            .windows(4)
            .position(|w| w == [0, 0, 1, 0])
            .unwrap();
        let second_start = first_start
            + 1
            + data
                .get(first_start + 1..)
                .unwrap()
                .windows(4)
                .position(|w| w == [0, 0, 1, 0])
                .unwrap();
        assert_eq!(used1, second_start);
        assert!(pkt1.flags.contains(PacketFlags::KEY), "picture_coding_type 1 is I");

        let rest = data.get(used1..).unwrap();
        let (pkt2, used2) = p.parse(rest).unwrap();
        let pkt2 = pkt2;
        assert!(pkt2.is_none(), "a P picture with nothing after it is incomplete");
        assert_eq!(used2, 0);

        // End of stream: the buffered P picture flushes.
        let (final_pkt, used3) = p.parse(&[]).unwrap();
        let final_pkt = final_pkt.expect("the trailing picture flushes at EOS");
        assert_eq!(used3, 0);
        assert!(!final_pkt.flags.contains(PacketFlags::KEY), "picture_coding_type 2 is P");
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
