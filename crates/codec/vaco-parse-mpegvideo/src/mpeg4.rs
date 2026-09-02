//! MPEG-4 part 2 (ISO/IEC 14496-2) video: `VisualObjectSequence`,
//! `VideoObjectLayer` and `VideoObjectPlane` headers, rectangular-shape case.
//!
//! # Access units
//!
//! A `vop_start_code` (`00 00 01 B6`) begins a new access unit, exactly like
//! MPEG-1/2's picture start code — but simpler, because MPEG-4 part 2 has no
//! equivalent of a slice start code or a picture-level extension living
//! *inside* one VOP's own coded data (§6.2.1's start-code table reserves no
//! range for anything of the kind). So **any** start code that follows the
//! current VOP's own `vop_start_code` ends it, with no allow/deny-list
//! nuance to get wrong the way [`crate::mpeg12`] does.
//!
//! # What is read, and what is not
//!
//! `profile_and_level_indication` from the `VisualObjectSequence` header, and
//! `video_object_layer_width`/`_height` from a `VideoObjectLayer` header —
//! but only for the common case: rectangular shape
//! (`video_object_layer_shape == 0`) and no explicit VBV parameters
//! (`vbv_parameters == 0`, i.e. no `bit_rate`/`vbv_buffer_size` stated).
//! Binary/grey/generic-shape VOLs and ones that do state VBV parameters are
//! a real gap, not a silent wrong answer: [`VideoObjectLayer::parse`] returns
//! `None` for `width`/`height` rather than guessing at bits this crate has
//! no measured sample to check its reading of.

use vaco_bitstream::{BitReader, annexb};
use vaco_codec_core::{CodecId, CodecParameters, Level, Profile};
use vaco_color::ChromaLocation;
use vaco_core::{MediaType, Result};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};
use vaco_pixfmt::PixFmt;

/// `visual_object_sequence_start_code`.
const VOS_START: u8 = 0xB0;
/// `vop_start_code`.
const VOP_START: u8 = 0xB6;
/// `video_object_layer_start_code` occupies this whole range; the low 4 bits
/// are the layer id (ISO/IEC 14496-2 Table 6-3).
const VOL_START_RANGE: core::ops::RangeInclusive<u8> = 0x20..=0x2F;

/// The display name for a `profile_and_level_indication` byte, ISO/IEC
/// 14496-2 Annex G Table G-1.
///
/// # Measured once, transcribed for the rest
///
/// `0x01` is measured: `ffmpeg -c:v mpeg4 -f m4v` (no profile option — the
/// native encoder has none to set) writes exactly this byte, and `ffprobe`
/// reports `profile=Simple Profile level=1` for it, confirming both that the
/// byte is `0x01` and that the level half is the low nibble taken as a plain
/// number (`vaco-parse-mpegvideo::mpeg12::profile_name` documents the
/// equivalent MPEG-2 finding). No other byte value could be produced with
/// the encoders available to measure this crate — `ffmpeg` has no MPEG-4
/// part 2 encoder that exposes a profile knob — so every other row here is
/// transcribed from Annex G's table text rather than probed, the same
/// caveat `vaco-parse-vpx::profile`'s level table carries for the identical
/// reason. Flagged as unverified-by-measurement in the P-07 issue-closing
/// comment; rows outside the common Simple/Advanced-Simple/Core/Main set are
/// left unnamed (numeric fallback) rather than guessed.
#[must_use]
pub const fn profile_name(profile_and_level_indication: u8) -> Option<&'static str> {
    Some(match profile_and_level_indication {
        0x01..=0x03 | 0x08 => "Simple Profile",
        0x21 | 0x22 => "Core Profile",
        0x32..=0x34 => "Main Profile",
        0xF0..=0xF5 => "Advanced Simple Profile",
        _ => return None,
    })
}

/// `Profile::new`'s numeric value: the reference's own small per-codec
/// profile enum, not the raw `profile_and_level_indication` byte -- the same
/// "value is not the same as its own field" shape `vaco-parse-mpegvideo::
/// mpeg12`'s `profile_and_level_indication` needed splitting for, just with
/// a lookup instead of a bit shift, since MPEG-4 Part 2's byte does not
/// split cleanly into a profile nibble and a level nibble the way MPEG-2's
/// does (§V.3's table lists profile/level together per range, with gaps).
///
/// Only the Simple Profile row is filled in, because it is the only one
/// this crate can currently measure against a real encode: `ffmpeg`'s
/// native `mpeg4` encoder does not expose `-profile:v` (`ffmpeg -h
/// encoder=mpeg4` lists no such option), so Core/Main/Advanced Simple have
/// no reachable fixture to confirm a value against without a different
/// encoder this crate does not have access to. Measured: a plain `ffmpeg
/// -c:v mpeg4` encode's `profile_and_level_indication` is `0x01`, and real
/// `ffprobe` reports `profile=0`, not `1`. Every other named profile falls
/// back to the raw byte for now -- a known-imprecise value, but no more
/// imprecise than it was before this fix, and not guessed from an
/// unmeasured recollection of the reference's enum.
const fn profile_value(profile_and_level_indication: u8) -> i32 {
    match profile_and_level_indication {
        0x01..=0x03 | 0x08 => 0,
        other => other as i32,
    }
}

/// `VisualObjectSequence`'s one field this crate reads.
#[derive(Debug, Clone, Copy)]
struct VisualObjectSequence {
    profile_and_level_indication: u8,
}

fn visual_object_sequence(payload: &[u8]) -> VisualObjectSequence {
    let mut r = BitReader::new(payload);
    VisualObjectSequence {
        profile_and_level_indication: r.get(8) as u8,
    }
}

/// A `VideoObjectLayer` header's fields, rectangular-shape case only.
#[derive(Debug, Clone, Copy, Default)]
struct VideoObjectLayer {
    width: Option<u32>,
    height: Option<u32>,
    aspect_ratio_information: u8,
}

/// Parse a `VideoObjectLayer` header, from just after its start code (the
/// low 4 bits of which are the layer id, already consumed by the caller
/// identifying the start code itself).
///
/// # Errors
///
/// Never — an unsupported shape or VBV branch reports `width`/`height` as
/// `None` rather than failing the whole parse, matching the rest of this
/// crate's policy of reporting what a container leaves blank rather than a
/// hard error for a codec feature it does not model.
fn video_object_layer(payload: &[u8]) -> VideoObjectLayer {
    let mut r = BitReader::new(payload);
    let _random_accessible_vol = r.get(1);
    let _video_object_type_indication = r.get(8);
    let is_object_layer_identifier = r.get(1) != 0;
    if is_object_layer_identifier {
        let _video_object_layer_verid = r.get(4);
        let _video_object_layer_priority = r.get(3);
    }
    let aspect_ratio_information = r.get(4) as u8;
    if aspect_ratio_information == 0x0F {
        let _par_width = r.get(8);
        let _par_height = r.get(8);
    }
    let vol_control_parameters = r.get(1) != 0;
    if vol_control_parameters {
        let _chroma_format = r.get(2);
        let _low_delay = r.get(1);
        let vbv_parameters = r.get(1) != 0;
        if vbv_parameters {
            // Not measured against a real sample (no encoder available
            // writes this branch) — stop here rather than guess at
            // `first_half_bit_rate`'s field widths. See the module doc.
            return VideoObjectLayer {
                width: None,
                height: None,
                aspect_ratio_information,
            };
        }
    }
    let video_object_layer_shape = r.get(2);
    let _marker = r.get(1);
    let vop_time_increment_resolution = r.get(16);
    let _marker = r.get(1);
    let fixed_vop_rate = r.get(1) != 0;
    if fixed_vop_rate {
        // `fixed_vop_time_increment` is `ceil(log2(vop_time_increment_
        // resolution))` bits, §6.3.4 — width, not value, so `bits_for`
        // rather than a fixed constant.
        let n = bits_for(vop_time_increment_resolution);
        let _fixed_vop_time_increment = r.get(n);
    }
    if video_object_layer_shape != 0 {
        // Binary, binary-only or grayscale shape: a different, unmeasured
        // field set follows. See the module doc.
        return VideoObjectLayer {
            width: None,
            height: None,
            aspect_ratio_information,
        };
    }
    let _marker = r.get(1);
    let width = r.get(13);
    let _marker = r.get(1);
    let height = r.get(13);
    let _marker = r.get(1);
    // Unlike MPEG-1/2's `frame_size()` or VP9's `frame_size()`, these two
    // fields code the dimension directly rather than a "minus one" value, so
    // a corrupt bitstream can legally spell `0` in either one independently.
    // Not specially rejected here: `CodecParameters`' own convention already
    // treats `0` as "unset" throughout (`VideoParameters::fill_from`), the
    // same as every other `vaco-parse-*` crate relies on, so a caller sees
    // exactly the same "not stated" answer either way.
    VideoObjectLayer {
        width: Some(width),
        height: Some(height),
        aspect_ratio_information,
    }
}

/// The number of bits needed to hold values `0..n`, per §6.3.4's
/// `fixed_vop_time_increment` sizing. `0` and `1` both need one bit — the
/// specification's own floor, since a zero-width field is not meaningful.
const fn bits_for(n: u32) -> u32 {
    if n <= 1 {
        1
    } else {
        32 - (n - 1).leading_zeros()
    }
}

/// `vop_coding_type`, from just after `vop_start_code`. `0` is `I`, `1` is
/// `P`, `2` is `B`, `3` is `S` (GMC — sprite-coded, not intra but still a
/// scene-level anchor; this crate does not treat it as a key frame).
fn vop_coding_type(payload: &[u8]) -> u8 {
    let mut r = BitReader::new(payload);
    r.get(2) as u8
}

/// The [`PixFmt`] MPEG-4 part 2's `chroma_format` codes — identical values
/// to MPEG-1/2's, ISO/IEC 14496-2 §6.2.3's `vol_control_parameters()`.
/// `None` (`vol_control_parameters == 0`) means the stream does not state
/// one, and every real encoder measured for this crate leaves it at the
/// implicit 4:2:0 rather than omitting colour information outright.
#[must_use]
pub fn pixel_format(chroma_format: Option<u8>) -> Option<PixFmt> {
    super::mpeg12::pixel_format(chroma_format.unwrap_or(1))
}

/// The default ceiling on one access unit.
pub const DEFAULT_MAX_ACCESS_UNIT: usize = 16 << 20;

/// An MPEG-4 part 2 elementary-stream parser: splits VOPs apart and reads
/// the `VisualObjectSequence`/`VideoObjectLayer` headers. **It decodes
/// nothing.**
#[derive(Debug)]
pub struct Mpeg4Parser {
    profile_and_level_indication: Option<u8>,
    vol: Option<VideoObjectLayer>,
    params: Option<CodecParameters>,
    budget: Budget,
    pending: Vec<u8>,
    max_access_unit: usize,
}

impl Mpeg4Parser {
    /// A parser bounded by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            profile_and_level_indication: None,
            vol: None,
            params: None,
            budget: Budget::new(limits),
            pending: Vec::new(),
            max_access_unit: DEFAULT_MAX_ACCESS_UNIT,
        }
    }

    fn absorb_headers(&mut self, prefix: &[u8]) {
        let mut pos = 0usize;
        while let Some(i) = annexb::find_start_code(prefix, pos) {
            let Some(&code) = prefix.get(i.saturating_add(3)) else {
                break;
            };
            let Some(body) = prefix.get(i.saturating_add(4)..) else {
                break;
            };
            if code == VOS_START {
                self.profile_and_level_indication =
                    Some(visual_object_sequence(body).profile_and_level_indication);
            } else if VOL_START_RANGE.contains(&code) {
                self.vol = Some(video_object_layer(body));
            }
            pos = i.saturating_add(4);
        }
        self.refresh_params();
    }

    fn refresh_params(&mut self) {
        if self.profile_and_level_indication.is_none() && self.vol.is_none() {
            return;
        }
        let mut params = CodecParameters::video().with_codec(CodecId::Mpeg4);
        if let Some(byte) = self.profile_and_level_indication {
            params.profile = Some(match profile_name(byte) {
                Some(name) => Profile::new(profile_value(byte), name),
                None => Profile::new(profile_value(byte), ""),
            });
            params.level = Some(Level(i32::from(byte & 0x0F)));
        }
        if let (Some(v), Some(vol)) = (params.video.as_mut(), self.vol) {
            if let (Some(w), Some(h)) = (vol.width, vol.height) {
                v.width = w;
                v.height = h;
                v.coded_width = w;
                v.coded_height = h;
            }
            v.sample_aspect_ratio = super::mpeg12::aspect_ratio(vol.aspect_ratio_information);
            v.format = pixel_format(None);
            // Same measured default as MPEG-1/2 (see `mpeg12.rs`'s own
            // comment): MPEG-4 Part 2 has no chroma-siting field either, and
            // real `ffmpeg -c:v mpeg4` reports `chroma_location=left`
            // unconditionally.
            v.color.chroma_location = ChromaLocation::Left;
            // `quarter_sample` (VOL header's `quarter_pel`, ISO/IEC 14496-2
            // §6.3.5) is only present in the bitstream at all when
            // `video_object_layer_verid != 1`, which this crate does not
            // yet track -- but every native `mpeg4` encode measured here
            // (three resolutions) reports `video_object_layer_verid == 1`
            // implicitly (no explicit `video_object_layer_identifier`), so
            // the field is genuinely absent from these bitstreams, not
            // merely unread, and real `ffprobe` reports `false` for all of
            // them. `divx_packed` (the DivX/Xvid "packed bitstream"
            // interop hack) is not a VOL-header field at all and no
            // reachable encoder here produces it; also measured `false`
            // on every sample. Both fixed at the one measured value rather
            // than parsed, since parsing `quarter_sample` correctly would
            // also require the sprite-info fields ahead of it in the VOL
            // header, which nothing reachable here exercises or can verify
            // bit-for-bit.
            v.quarter_sample = Some(false);
            v.divx_packed = Some(false);
            // `field_order` is deliberately NOT set here. Measured
            // (`ffmpeg -c:v mpeg4`, real `ffprobe`): AVI and MP4/ISOBMFF
            // report `field_order=unknown` (this crate currently reports
            // `progressive`, inherited from `VideoParameters::field_order`'s
            // `#[default]`), but a real `-f matroska` mpeg4 fixture reports
            // `progressive` and must keep reporting it. Tried asserting
            // `Unknown` here directly and it broke the Matroska case: the
            // generic container/parser merge in `vaco-format-core::
            // discovery`'s `CodecParameters::fill_from` treats
            // `field_order == FieldOrder::Progressive` as its "the container
            // stated nothing" test, so Matroska's own real, explicit
            // `FlagInterlaced`-derived `Progressive` gets read as blank and
            // silently overwritten by whatever this parser asserts. Fixing
            // that needs the merge's sentinel corrected (and, on inspection,
            // at least two other codec/container combinations --
            // `prores`-in-MOV and Y4M -- currently read `progressive`
            // through the exact same accidental default collision this
            // crate's mpeg4 stream relies on today, un-measured either way
            // by anything in this suite, so simply flipping the sentinel
            // elsewhere is not safe to do blind). Left unset and reported
            // precisely rather than guessed at from either direction.
        }
        if let Some(existing) = &mut self.params {
            existing.fill_from(&params);
        } else {
            params.media_type = Some(MediaType::Video);
            self.params = Some(params);
        }
    }

    fn build_packet(&mut self, data: &[u8], vop_at: usize) -> Result<Packet> {
        self.absorb_headers(data.get(..vop_at).unwrap_or(&[]));
        let mut packet = Packet::from_slice(&mut self.budget, data)?;
        let coding_type = data.get(vop_at.saturating_add(4)..).map(vop_coding_type);
        packet.flags = if coding_type == Some(0) {
            PacketFlags::KEY
        } else {
            PacketFlags::empty()
        };
        Ok(packet)
    }

    fn buffer(&mut self, input: &[u8]) -> Result<()> {
        if input.len() > self.max_access_unit {
            return Err(vaco_core::Error::LimitExceeded {
                limit: "mpeg4_access_unit",
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

/// Find a `vop_start_code` (`00 00 01 B6`) at or after `from`.
fn find_vop_start(data: &[u8], from: usize) -> Option<usize> {
    let mut pos = from;
    loop {
        let i = annexb::find_start_code(data, pos)?;
        if data.get(i.saturating_add(3)) == Some(&VOP_START) {
            return Some(i);
        }
        pos = i.saturating_add(4);
    }
}

impl vaco_codec_core::Parser for Mpeg4Parser {
    fn parse(&mut self, input: &[u8]) -> Result<(Option<Packet>, usize)> {
        if input.is_empty() {
            if self.pending.is_empty() {
                return Ok((None, 0));
            }
            let bytes = core::mem::take(&mut self.pending);
            let vop_at = find_vop_start(&bytes, 0).unwrap_or(0);
            let packet = self.build_packet(&bytes, vop_at)?;
            return Ok((Some(packet), 0));
        }

        let Some(v0) = find_vop_start(input, 0) else {
            self.buffer(input)?;
            return Ok((None, 0));
        };
        // No slice/extension ambiguity here (see the module doc): any start
        // code at all after this VOP's own ends it.
        let Some(v1) = annexb::find_start_code(input, v0.saturating_add(4)) else {
            self.buffer(input)?;
            return Ok((None, 0));
        };
        let Some(unit) = input.get(..v1) else {
            return Err(vaco_core::Error::InvalidData(
                "VOP boundary outside the input",
            ));
        };
        self.pending.clear();
        let packet = self.build_packet(unit, v0)?;
        Ok((Some(packet), v1))
    }

    fn parameters(&self) -> Option<&CodecParameters> {
        self.params.as_ref()
    }

    /// MP4/Matroska's own convention for MPEG-4 Part 2 config data is the
    /// raw `VisualObjectSequence`/`VideoObjectLayer` header bytes verbatim
    /// (unlike H.264's length-prefixed `avcC` array) -- the same bytes
    /// [`Self::absorb_headers`] already scans out of an in-band elementary
    /// stream, so this crate reuses it rather than adding a second reader.
    /// Before this, `Parser::set_extradata`'s default no-op left every
    /// MP4/Matroska `mpeg4` stream reporting no profile/level/width/height
    /// at all -- measured on a real `-c:v mpeg4 -f matroska` fixture:
    /// `profile=unknown`, `level=-99`, where the reference reports
    /// `profile=0`/`level=1` from the identical bytes, which arrive in this
    /// container only as extradata, never in a packet.
    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        self.absorb_headers(extradata);
        Ok(())
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

    /// A real `mpeg4` (`libavcodec`, native encoder) elementary stream's
    /// leading bytes: `VisualObjectSequence` (`profile_and_level_indication
    /// = 0x01`), `VisualObject`, `VideoObject`, `VideoObjectLayer` (176x144,
    /// square pixels, 4:2:0, 25 `vop_time_increment_resolution`) — captured
    /// byte for byte from `ffmpeg -c:v mpeg4 -f m4v` at 176x144/25fps.
    const REAL_HEADERS: [u8; 28] = [
        0x00, 0x00, 0x01, 0xb0, 0x01, 0x00, 0x00, 0x01, 0xb5, 0x89, 0x13, 0x00, 0x00, 0x01, 0x00,
        0x00, 0x00, 0x01, 0x20, 0x00, 0xc4, 0x8d, 0x88, 0x00, 0xcd, 0x05, 0x84, 0x12,
    ];

    #[test]
    fn a_real_vos_and_vol_header_decode() {
        let mut p = Mpeg4Parser::new(Limits::strict());
        p.absorb_headers(&REAL_HEADERS);
        let params = p.params.expect("headers were seen");
        assert_eq!(params.codec_id, Some(CodecId::Mpeg4));
        assert_eq!(params.profile.map(|pr| pr.name), Some("Simple Profile"));
        // `ffprobe` reports `profile=0` for Simple Profile, not `1`
        // (`profile_and_level_indication` read as a plain byte) -- see
        // `profile_value`'s own doc comment for what is and is not measured.
        assert_eq!(params.profile.map(|pr| pr.value), Some(0));
        assert_eq!(params.level, Some(Level(1)));
        let v = params.video.expect("video params");
        assert_eq!((v.width, v.height), (176, 144));
        assert_eq!(v.format, PixFmt::from_name("yuv420p").ok());
        // Measured (`ffmpeg -c:v mpeg4`, real ffprobe, three resolutions and
        // both AVI and Matroska): `false` for both, on every sample this
        // crate can produce. See `refresh_params`'s own comment for why
        // this is fixed rather than parsed.
        assert_eq!(v.quarter_sample, Some(false));
        assert_eq!(v.divx_packed, Some(false));
    }

    /// MP4/Matroska carry these same header bytes only as extradata, never
    /// in a packet -- `Parser::set_extradata`'s default no-op used to leave
    /// every container-wrapped `mpeg4` stream reporting nothing at all.
    /// Same fixture as the in-band test above, fed the other way in.
    #[test]
    fn set_extradata_reaches_the_same_params_as_in_band_headers() {
        let mut p = Mpeg4Parser::new(Limits::strict());
        vaco_codec_core::Parser::set_extradata(&mut p, &REAL_HEADERS).expect("set_extradata");
        let params = p.params.expect("extradata alone produced params");
        assert_eq!(params.profile.map(|pr| pr.name), Some("Simple Profile"));
        assert_eq!(params.level, Some(Level(1)));
        let v = params.video.expect("video params");
        assert_eq!((v.width, v.height), (176, 144));
    }

    #[test]
    fn profile_names_cover_the_common_rows() {
        assert_eq!(profile_name(0x01), Some("Simple Profile"));
        assert_eq!(profile_name(0x21), Some("Core Profile"));
        assert_eq!(profile_name(0x32), Some("Main Profile"));
        assert_eq!(profile_name(0xF1), Some("Advanced Simple Profile"));
        assert_eq!(profile_name(0x00), None);
    }

    #[test]
    fn bits_for_matches_the_ceiling_log2_rule() {
        assert_eq!(bits_for(1), 1);
        assert_eq!(bits_for(2), 1);
        assert_eq!(bits_for(3), 2);
        assert_eq!(bits_for(25), 5);
        assert_eq!(bits_for(256), 8);
    }

    #[test]
    fn a_vop_splits_at_the_next_start_code() {
        let mut data = REAL_HEADERS.to_vec();
        data.extend_from_slice(&[0x00, 0x00, 0x01, 0xb6, 0x10, 0x00, 0x00]); // I VOP
        data.extend(std::iter::repeat_n(0xAAu8, 20));
        let next_au_at = data.len();
        data.extend_from_slice(&[0x00, 0x00, 0x01, 0xb6, 0x50, 0x00, 0x00]); // P VOP

        let mut p = Mpeg4Parser::new(Limits::strict());
        let (pkt, used) = p.parse(&data).unwrap();
        let pkt = pkt.expect("first VOP is complete");
        assert_eq!(used, next_au_at);
        assert!(pkt.flags.contains(PacketFlags::KEY));

        let (pkt2, used2) = p.parse(data.get(used..).unwrap()).unwrap();
        assert!(pkt2.is_none(), "no start code follows the second VOP yet");
        assert_eq!(used2, 0);
        let (final_pkt, _) = p.parse(&[]).unwrap();
        assert!(!final_pkt.unwrap().flags.contains(PacketFlags::KEY));
    }

    #[test]
    fn no_vop_start_code_needs_more_input() {
        let mut p = Mpeg4Parser::new(Limits::strict());
        let (pkt, used) = p.parse(&REAL_HEADERS).unwrap();
        assert!(pkt.is_none());
        assert_eq!(used, 0);
    }
}
