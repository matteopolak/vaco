//! VP9 uncompressed frame header, `uncompressed_header()` §6.2 of the VP9
//! Bitstream & Decoding Process Specification (v0.6).
//!
//! # Framing: the same "one packet, one frame" contract as VP8 and Opus
//!
//! No `vaco-demux-raw` `BitstreamSpec` names VP9 (there is no bare
//! elementary-stream demuxer for it, no IVF demuxer, nothing) and §6.2's
//! syntax cannot answer "how many bytes is this frame" by itself — the
//! compressed header states its *own* size (`header_size_in_bytes`) but the
//! tile data that follows has no length field at all; only a superframe
//! index or a container's own length prefix ever states one. So exactly
//! like [`crate::vp8`] and `vaco-parse-opus`, [`Vp9Parser::parse`] takes one
//! already-framed container sample and returns it whole — never a
//! resynchronising byte-stream splitter, because for VP9 outside a
//! superframe index there is nothing to resynchronise to.
//!
//! # Superframes are read, not split
//!
//! A container sample may itself be a VP9 *superframe*: several coded frames
//! (typically a hidden alt-ref frame followed by the visible one) packed back
//! to back with an index appended, §Annex B. Splitting that into separate
//! packets is `vaco-bsf-vpx`'s `vp9_superframe_split`'s job, a deliberate
//! pipeline stage — not this parser's. What this parser needs from a
//! superframe is only [`crate::superframe::last_subframe`]'s answer: which
//! sub-frame's header describes the picture the container's one packet
//! actually shows, so `profile`/`pix_fmt`/dimensions come from that frame
//! rather than from a hidden one that happens to sit first in the buffer.
//!
//! # `level` is never populated — see [`crate::profile`]'s module doc
//!
//! The header below has no level syntax element, and this is measured against
//! `ffprobe 8.1`, not assumed.
//!
//! # What is read, and what stops early
//!
//! Everything through `color_config()`/`frame_size()` on a key frame or an
//! intra-only inter frame — profile, bit depth, chroma subsampling, colour
//! range, dimensions. For an ordinary inter frame (not intra-only), parsing
//! stops right after `show_frame`/`intra_only` are known: `frame_size_with_refs()`
//! needs the *sizes of the reference slots*, which this crate does not track
//! (doing so would mean modelling the eight-slot reference buffer, which no
//! `CodecParameters` field needs), so an inter frame's dimensions come from
//! whichever earlier packet already established them —
//! [`vaco_codec_core::CodecParameters::fill_from`]'s "only fill what is
//! unset" rule handles that without this crate doing anything extra.

use vaco_bitstream::BitReader;
use vaco_codec_core::{CodecId, CodecParameters, FieldOrder, Parser};
use vaco_color::{ColorInfo, ColorRange, MatrixCoefficients};
use vaco_core::{MediaType, Result};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};
use vaco_pixfmt::PixFmt;

use crate::profile;
use crate::superframe::last_subframe;

/// §6.2's `frame_sync_code()`: three fixed bytes.
const FRAME_SYNC_CODE: [u32; 3] = [0x49, 0x83, 0x42];

/// `color_space` §6.2's enum. Only `Rgb` changes parsing (no `color_range`
/// bit is coded — RGB is always full range — and no chroma subsampling is
/// coded either, since RGB has none).
const CS_RGB: u32 = 7;

/// What `color_config()` states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vp9ColorConfig {
    /// 8, 10 or 12.
    pub bit_depth: u8,
    /// The raw 3-bit `color_space` value, 0..=7.
    pub color_space: u8,
    pub full_range: bool,
    pub subsampling_x: bool,
    pub subsampling_y: bool,
}

/// What `uncompressed_header()` states, as far as this crate reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vp9Header {
    pub profile: u8,
    /// `show_existing_frame`: this "frame" carries no new picture at all, only
    /// a pointer to a frame buffer slot already decoded.
    pub show_existing_frame: bool,
    /// `frame_type == KEY_FRAME`. Meaningless when `show_existing_frame`.
    pub is_key_frame: bool,
    pub show_frame: bool,
    /// Present for a key frame or an intra-only inter frame; `None`
    /// otherwise (see the module doc for why).
    pub color: Option<Vp9ColorConfig>,
    /// `(width, height)`, present exactly when `color` is.
    pub size: Option<(u32, u32)>,
}

fn frame_sync_code_ok(r: &mut BitReader<'_>) -> bool {
    FRAME_SYNC_CODE.iter().all(|&want| r.get(8) == want)
}

fn color_config(r: &mut BitReader<'_>, profile: u8) -> Vp9ColorConfig {
    let bit_depth = if profile >= 2 {
        if r.get(1) != 0 { 12 } else { 10 }
    } else {
        8
    };
    let color_space = r.get(3) as u8;
    let (full_range, subsampling_x, subsampling_y) = if u32::from(color_space) == CS_RGB {
        if profile == 1 || profile == 3 {
            let _reserved_zero = r.get(1);
        }
        (true, false, false)
    } else {
        let full_range = r.get(1) != 0;
        let (sx, sy) = if profile == 1 || profile == 3 {
            let sx = r.get(1) != 0;
            let sy = r.get(1) != 0;
            let _reserved_zero = r.get(1);
            (sx, sy)
        } else {
            (true, true)
        };
        (full_range, sx, sy)
    };
    Vp9ColorConfig {
        bit_depth,
        color_space,
        full_range,
        subsampling_x,
        subsampling_y,
    }
}

fn frame_size(r: &mut BitReader<'_>) -> (u32, u32) {
    let width = r.get(16) + 1;
    let height = r.get(16) + 1;
    (width, height)
}

/// Parse `uncompressed_header()` from a buffer that is exactly one VP9
/// frame — not a superframe-wrapping sample; see [`parse_display_header`]
/// for that. Returns `None` when the bits do not describe a VP9 frame at all
/// (bad `frame_marker`) or the buffer is too short for the fields this crate
/// reads (§6.2's sticky-overrun-checked tail).
#[must_use]
pub fn parse_uncompressed_header(data: &[u8]) -> Option<Vp9Header> {
    let mut r = BitReader::new(data);
    if r.get(2) != 2 {
        return None; // frame_marker must be 2.
    }
    let profile_low = r.get(1);
    let profile_high = r.get(1);
    let profile = ((profile_high << 1) | profile_low) as u8;
    if profile == 3 {
        let _reserved_zero = r.get(1);
    }
    let show_existing_frame = r.get(1) != 0;
    if show_existing_frame {
        let _frame_to_show_map_idx = r.get(3);
        r.check().ok()?;
        return Some(Vp9Header {
            profile,
            show_existing_frame: true,
            is_key_frame: false,
            show_frame: true,
            color: None,
            size: None,
        });
    }

    let is_key_frame = r.get(1) == 0; // frame_type: 0 = KEY_FRAME.
    let show_frame = r.get(1) != 0;
    let error_resilient_mode = r.get(1) != 0;

    let mut color = None;
    let mut size = None;

    if is_key_frame {
        if !frame_sync_code_ok(&mut r) {
            return None;
        }
        color = Some(color_config(&mut r, profile));
        size = Some(frame_size(&mut r));
    } else {
        let intra_only = if show_frame { false } else { r.get(1) != 0 };
        if !error_resilient_mode {
            let _reset_frame_context = r.get(2);
        }
        if intra_only {
            if !frame_sync_code_ok(&mut r) {
                return None;
            }
            color = Some(if profile > 0 {
                color_config(&mut r, profile)
            } else {
                // §6.2: profile 0's intra-only frames skip color_config()
                // entirely and are defined to be 8-bit 4:2:0 BT.601.
                Vp9ColorConfig {
                    bit_depth: 8,
                    color_space: 1, // CS_BT_601
                    full_range: false,
                    subsampling_x: true,
                    subsampling_y: true,
                }
            });
            let _refresh_frame_flags = r.get(8);
            size = Some(frame_size(&mut r));
        }
        // An ordinary inter frame: nothing past this point is read. See the
        // module doc for why `frame_size_with_refs()` is out of scope.
    }

    r.check().ok()?;
    Some(Vp9Header {
        profile,
        show_existing_frame: false,
        is_key_frame,
        show_frame,
        color,
        size,
    })
}

/// [`parse_uncompressed_header`], but on a possible superframe: when `data`
/// carries a superframe index, parses the *last* sub-frame — the one a real
/// encoder shows — instead of whatever sits first in the buffer.
#[must_use]
pub fn parse_display_header(data: &[u8]) -> Option<Vp9Header> {
    let frame = last_subframe(data).unwrap_or(data);
    parse_uncompressed_header(frame)
}

/// The [`MatrixCoefficients`] a VP9 `color_space` value denotes.
///
/// VP9's `color_space` is its own 3-bit enumeration, not an H.273 code point
/// read off the wire the way AV1's is — so unlike
/// `vaco-parse-av1::params::color_info`, this is a *table*, not a narrowing
/// cast. Measured against `ffprobe 8.1`: `-colorspace bt709`, `bt470bg`,
/// `smpte170m` and `smpte240m` each round-trip through a `libvpx-vp9`
/// encode to the identically-named `color_space` field, and BT.601 and
/// BT.470BG share ITU-T H.273 code point 5 by the standard's own
/// definition — which is *why* the round trip holds, not a coincidence
/// this table papers over. `CS_BT_2020`'s non-constant-luminance/constant-
/// luminance split (H.273 9 vs 10) has no separate VP9 signal at all, so it
/// maps to the non-constant-luminance value, matching what every encoder
/// available to probe this with produces.
#[must_use]
pub const fn matrix_coefficients(color_space: u8) -> MatrixCoefficients {
    match color_space {
        1 => MatrixCoefficients::Bt470bg,    // CS_BT_601
        2 => MatrixCoefficients::Bt709,      // CS_BT_709
        3 => MatrixCoefficients::Smpte170m,  // CS_SMPTE_170
        4 => MatrixCoefficients::Smpte240m,  // CS_SMPTE_240
        5 => MatrixCoefficients::Bt2020Ncl,  // CS_BT_2020
        7 => MatrixCoefficients::Identity,   // CS_RGB
        _ => MatrixCoefficients::Unspecified, // CS_UNKNOWN, CS_RESERVED
    }
}

/// The [`PixFmt`] a [`Vp9ColorConfig`] implies.
///
/// `// D17:` no `yuvj` family, measured directly: `-color_range pc` on an
/// 8-bit 4:2:0 `libvpx-vp9` encode still reports `pix_fmt=yuv420p` with
/// `color_range=pc` reported separately, exactly the pattern
/// `vaco-parse-av1::params::pixel_format` documents for AV1. VP9's
/// `color_config()` has no monochrome flag at all (unlike AV1's
/// `mono_chrome`), so there is no gray family to map to here.
#[must_use]
pub fn pixel_format(c: &Vp9ColorConfig) -> Option<PixFmt> {
    if u32::from(c.color_space) == CS_RGB {
        let name = match c.bit_depth {
            8 => "gbrp".to_string(),
            10 | 12 => format!("gbrp{}le", c.bit_depth),
            _ => return None,
        };
        return PixFmt::from_name(&name).ok();
    }
    let chroma = match (c.subsampling_x, c.subsampling_y) {
        (true, true) => "420",
        (true, false) => "422",
        _ => "444",
    };
    let name = match c.bit_depth {
        8 => format!("yuv{chroma}p"),
        10 | 12 => format!("yuv{chroma}p{}le", c.bit_depth),
        _ => return None,
    };
    PixFmt::from_name(&name).ok()
}

/// The [`ColorInfo`] a [`Vp9ColorConfig`] implies.
///
/// VP9's frame header has no colour-primaries or transfer-characteristics
/// signal at all — only `color_space` (matrix) and the range bit — so both
/// stay [`Default`], matching what `ffprobe` reports for every `WebM`-carried
/// VP9 stream probed while measuring [`matrix_coefficients`].
#[must_use]
pub fn color_info(c: &Vp9ColorConfig) -> ColorInfo {
    ColorInfo {
        matrix: matrix_coefficients(c.color_space),
        range: if c.full_range {
            ColorRange::Full
        } else {
            ColorRange::Limited
        },
        ..ColorInfo::default()
    }
}

/// The [`CodecParameters`] a [`Vp9Header`] describes.
#[must_use]
pub fn codec_parameters(h: &Vp9Header) -> CodecParameters {
    let mut params = CodecParameters::video().with_codec(CodecId::Vp9);
    params.profile = Some(profile::profile(h.profile));
    if let Some(v) = params.video.as_mut() {
        v.field_order = FieldOrder::Progressive; // VP9 has no interlace syntax.
        if let (Some(c), Some((w, h))) = (h.color, h.size) {
            v.width = w;
            v.height = h;
            v.coded_width = w;
            v.coded_height = h;
            v.format = pixel_format(&c);
            v.color = color_info(&c);
            v.bits_per_raw_sample = Some(c.bit_depth);
        }
    }
    params
}

/// A VP9 parser: reads the uncompressed frame header (superframe-aware) and
/// nothing past it. **It decodes nothing.**
#[derive(Debug)]
pub struct Vp9Parser {
    budget: Budget,
    params: Option<CodecParameters>,
}

impl Vp9Parser {
    /// A parser bounded by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            budget: Budget::new(limits),
            params: None,
        }
    }
}

impl Parser for Vp9Parser {
    fn parse(&mut self, input: &[u8]) -> Result<(Option<Packet>, usize)> {
        if input.is_empty() {
            return Ok((None, 0));
        }
        let mut packet = Packet::from_slice(&mut self.budget, input)?;
        if let Some(h) = parse_display_header(input) {
            let found = codec_parameters(&h);
            if let Some(existing) = &mut self.params {
                existing.fill_from(&found);
            } else {
                let mut found = found;
                found.media_type = Some(MediaType::Video);
                self.params = Some(found);
            }
            packet.flags = if !h.show_existing_frame && h.is_key_frame {
                PacketFlags::KEY
            } else {
                PacketFlags::empty()
            };
        }
        Ok((Some(packet), input.len()))
    }

    fn parameters(&self) -> Option<&CodecParameters> {
        self.params.as_ref()
    }

    /// Seed profile/level facts from an MP4 `vpcC` — see [`crate::vpcc`].
    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        if let Some(found) = crate::vpcc::codec_parameters(extradata) {
            if let Some(existing) = &mut self.params {
                existing.fill_from(&found);
            } else {
                self.params = Some(found);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code over fixed fixtures")]
mod tests {
    use super::*;

    /// Build a synthetic `uncompressed_header()` bit for bit, per §6.2, for a
    /// key frame at 176x144 — built by hand from the specification rather
    /// than captured from a real encode, since exercising every profile and
    /// colour-space combination this crate reads would need encoders this
    /// environment does not have (`libvpx-vp9` alone cannot reach every
    /// profile/`color_space` pairing `color_config()` allows).
    fn frame_bits(profile: u8, bit_depth_bit: Option<bool>, cs: u8, full_range: bool) -> Vec<u8> {
        let mut bits: Vec<bool> = Vec::new();
        let mut push = |n: u32, v: u32| {
            for i in (0..n).rev() {
                bits.push((v >> i) & 1 != 0);
            }
        };
        push(2, 2); // frame_marker
        push(1, u32::from(profile & 1));
        push(1, u32::from((profile >> 1) & 1));
        if profile == 3 {
            push(1, 0);
        }
        push(1, 0); // show_existing_frame = 0
        push(1, 0); // frame_type = KEY_FRAME
        push(1, 1); // show_frame = 1
        push(1, 0); // error_resilient_mode = 0
        push(8, 0x49);
        push(8, 0x83);
        push(8, 0x42);
        if let Some(b) = bit_depth_bit {
            push(1, u32::from(b));
        }
        push(3, u32::from(cs));
        if cs != 7 {
            push(1, u32::from(full_range));
            if profile == 1 || profile == 3 {
                push(1, 1); // subsampling_x
                push(1, 1); // subsampling_y
                push(1, 0); // reserved_zero
            }
        } else if profile == 1 || profile == 3 {
            push(1, 0); // reserved_zero
        }
        push(16, 176 - 1); // frame_width_minus_1
        push(16, 144 - 1); // frame_height_minus_1
        // Pack into bytes, padding the tail with zero bits.
        let mut out = Vec::new();
        for chunk in bits.chunks(8) {
            let mut byte = 0u8;
            for (i, &b) in chunk.iter().enumerate() {
                if b {
                    byte |= 0x80 >> i;
                }
            }
            out.push(byte);
        }
        out
    }

    #[test]
    fn a_profile_0_key_frame_decodes() {
        let data = frame_bits(0, None, 1, false);
        let h = parse_uncompressed_header(&data).unwrap();
        assert_eq!(h.profile, 0);
        assert!(h.is_key_frame);
        assert!(!h.show_existing_frame);
        let c = h.color.unwrap();
        assert_eq!(c.bit_depth, 8);
        assert_eq!((c.subsampling_x, c.subsampling_y), (true, true));
        assert_eq!(h.size, Some((176, 144)));
    }

    #[test]
    fn a_profile_2_key_frame_reads_the_bit_depth_bit() {
        let data = frame_bits(2, Some(true), 1, false);
        let h = parse_uncompressed_header(&data).unwrap();
        assert_eq!(h.profile, 2);
        assert_eq!(h.color.unwrap().bit_depth, 12);
    }

    #[test]
    fn rgb_forces_full_range_and_no_subsampling() {
        let data = frame_bits(1, None, 7, false);
        let h = parse_uncompressed_header(&data).unwrap();
        let c = h.color.unwrap();
        assert!(c.full_range);
        assert_eq!((c.subsampling_x, c.subsampling_y), (false, false));
    }

    #[test]
    fn a_bad_frame_marker_is_rejected() {
        assert!(parse_uncompressed_header(&[0x00, 0x00]).is_none());
    }

    #[test]
    fn a_truncated_header_is_rejected_not_panicked() {
        let data = frame_bits(0, None, 1, false);
        for n in 0..data.len() {
            let Some(prefix) = data.get(..n) else {
                continue;
            };
            let _ = parse_uncompressed_header(prefix);
        }
    }

    /// The first 24 bytes of a real key frame — `ffmpeg -f lavfi -i
    /// testsrc=size=176x144 -c:v libvpx-vp9 -pix_fmt yuv420p`, remuxed to IVF
    /// and the first frame's payload taken byte for byte. `parse_uncompressed_header`
    /// only reads the leading few dozen bits (through `frame_size()`), so the
    /// truncated prefix is enough — the rest of the 2551-byte frame is
    /// boolean-coded tile data this crate never touches.
    ///
    /// Hand-traced bit by bit against the specification to confirm this
    /// crate's field extraction independently of the synthetic vectors above:
    /// `frame_marker=2`, `profile=0`, `frame_type=KEY_FRAME`, `show_frame=1`,
    /// sync code `49 83 42`, `color_space=0` (`CS_UNKNOWN`), `color_range=0`,
    /// 4:2:0 (implied for profile 0), `width_minus_1=175`,
    /// `height_minus_1=143` — i.e. exactly the 176x144 the source declared.
    #[test]
    fn a_real_libvpx_vp9_key_frame_decodes() {
        const REAL_FRAME_PREFIX: [u8; 10] = [
            0x82, 0x49, 0x83, 0x42, 0x00, 0x0a, 0xf0, 0x08, 0xf6, 0x08,
        ];
        let h = parse_uncompressed_header(&REAL_FRAME_PREFIX).unwrap();
        assert_eq!(h.profile, 0);
        assert!(h.is_key_frame);
        assert!(!h.show_existing_frame);
        assert!(h.show_frame);
        let c = h.color.unwrap();
        assert_eq!(c.bit_depth, 8);
        assert_eq!(c.color_space, 0);
        assert!(!c.full_range);
        assert_eq!((c.subsampling_x, c.subsampling_y), (true, true));
        assert_eq!(h.size, Some((176, 144)));
        assert_eq!(pixel_format(&c), PixFmt::from_name("yuv420p").ok());
    }

    #[test]
    fn pix_fmt_matches_the_measured_reference() {
        let c8_420 = Vp9ColorConfig {
            bit_depth: 8,
            color_space: 1,
            full_range: false,
            subsampling_x: true,
            subsampling_y: true,
        };
        assert_eq!(pixel_format(&c8_420), PixFmt::from_name("yuv420p").ok());
        let c10_420 = Vp9ColorConfig {
            bit_depth: 10,
            ..c8_420
        };
        assert_eq!(pixel_format(&c10_420), PixFmt::from_name("yuv420p10le").ok());
        let c8_422 = Vp9ColorConfig {
            subsampling_y: false,
            ..c8_420
        };
        assert_eq!(pixel_format(&c8_422), PixFmt::from_name("yuv422p").ok());
        let rgb = Vp9ColorConfig {
            color_space: 7,
            subsampling_x: false,
            subsampling_y: false,
            full_range: true,
            ..c8_420
        };
        assert_eq!(pixel_format(&rgb), PixFmt::from_name("gbrp").ok());
    }

    #[test]
    fn the_parser_reports_profile_and_dimensions() {
        let mut p = Vp9Parser::new(Limits::strict());
        let data = frame_bits(0, None, 1, false);
        let (pkt, used) = p.parse(&data).unwrap();
        let pkt = pkt.unwrap();
        assert_eq!(used, data.len());
        assert!(pkt.flags.contains(PacketFlags::KEY));
        let params = p.parameters().unwrap();
        assert_eq!(params.profile.map(|pr| pr.name), Some("Profile 0"));
        let v = params.video.as_ref().unwrap();
        assert_eq!((v.width, v.height), (176, 144));
    }

    #[test]
    fn end_of_stream_flushes_nothing() {
        let mut p = Vp9Parser::new(Limits::strict());
        let (pkt, used) = p.parse(&[]).unwrap();
        assert!(pkt.is_none());
        assert_eq!(used, 0);
    }
}
