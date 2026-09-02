//! The parameter-set store, and the stream description an SPS implies.

use vaco_codec_core::{CodecId, CodecParameters, FieldOrder, VideoParameters};
use vaco_core::{Error, Rational, Result};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

use crate::pps::Pps;
use crate::profile;
use crate::sps::{ChromaFormat, Sps};
use crate::vps::Vps;

/// `vps_video_parameter_set_id` runs 0..=15 (§7.4.3.1).
pub const MAX_VPS: usize = 16;

/// `sps_seq_parameter_set_id` runs 0..=15 (§7.4.3.2).
pub const MAX_SPS: usize = 16;

/// `pps_pic_parameter_set_id` runs 0..=63 (§7.4.3.3).
pub const MAX_PPS: usize = 64;

/// Every parameter set seen so far, indexed by id.
///
/// # HEVC's store is simpler than H.264's, in one specific way
///
/// H.264's PPS is sized by fields of the SPS it names, so its store has to
/// resolve the SPS *before* it can finish parsing a PPS, and re-parse if it
/// guessed wrong (`vaco-parse-h264`'s `add_pps` does exactly that). Nothing in
/// an HEVC PPS is sized by an SPS field, so parameter sets can arrive in any
/// order and each parses alone.
#[derive(Debug, Default)]
pub struct ParameterSets {
    vps: Box<[Option<Vps>]>,
    sps: Box<[Option<Sps>]>,
    pps: Box<[Option<Pps>]>,
    /// The id of the most recently activated SPS, which is what a stream
    /// description is derived from.
    active_sps: Option<u8>,
}

impl ParameterSets {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            vps: (0..MAX_VPS).map(|_| None).collect(),
            sps: (0..MAX_SPS).map(|_| None).collect(),
            pps: (0..MAX_PPS).map(|_| None).collect(),
            active_sps: None,
        }
    }

    /// Parse and store a video parameter set.
    ///
    /// # Errors
    ///
    /// Whatever [`Vps::parse`] returns.
    pub fn add_vps(&mut self, rbsp: &[u8], budget: &mut Budget) -> Result<u8> {
        let vps = Vps::parse(rbsp, budget)?;
        let id = vps.id;
        let slot = self.vps.get_mut(id as usize).ok_or(Error::InvalidData(
            "vps_video_parameter_set_id out of range",
        ))?;
        *slot = Some(vps);
        Ok(id)
    }

    /// Parse and store a sequence parameter set.
    ///
    /// A repeated id replaces the previous set, which is what §7.4.2.4.2 permits
    /// between coded video sequences.
    ///
    /// # Errors
    ///
    /// Whatever [`Sps::parse`] returns.
    pub fn add_sps(&mut self, rbsp: &[u8], budget: &mut Budget) -> Result<u8> {
        let sps = Sps::parse(rbsp, budget)?;
        let id = sps.id;
        let slot = self
            .sps
            .get_mut(id as usize)
            .ok_or(Error::InvalidData("sps_seq_parameter_set_id out of range"))?;
        *slot = Some(sps);
        self.active_sps.get_or_insert(id);
        Ok(id)
    }

    /// Parse and store a picture parameter set.
    ///
    /// # Errors
    ///
    /// Whatever [`Pps::parse`] returns.
    pub fn add_pps(&mut self, rbsp: &[u8], budget: &mut Budget) -> Result<u8> {
        let pps = Pps::parse(rbsp, budget)?;
        let id = pps.id;
        let slot = self
            .pps
            .get_mut(id as usize)
            .ok_or(Error::InvalidData("pps_pic_parameter_set_id out of range"))?;
        *slot = Some(pps);
        Ok(id)
    }

    /// The video parameter set with this id.
    #[must_use]
    pub fn get_vps(&self, id: u8) -> Option<&Vps> {
        self.vps.get(id as usize)?.as_ref()
    }

    /// The sequence parameter set with this id.
    #[must_use]
    pub fn get_sps(&self, id: u8) -> Option<&Sps> {
        self.sps.get(id as usize)?.as_ref()
    }

    /// The picture parameter set with this id.
    #[must_use]
    pub fn get_pps(&self, id: u8) -> Option<&Pps> {
        self.pps.get(id as usize)?.as_ref()
    }

    /// The sequence parameter set a picture parameter set refers to.
    #[must_use]
    pub fn sps_for_pps(&self, pps_id: u8) -> Option<(&Pps, &Sps)> {
        let pps = self.get_pps(pps_id)?;
        let sps = self.get_sps(pps.sps_id)?;
        Some((pps, sps))
    }

    /// Mark a sequence parameter set as the active one — what a slice does
    /// implicitly by referring to a PPS that refers to it.
    pub fn activate(&mut self, sps_id: u8) {
        if self.get_sps(sps_id).is_some() {
            self.active_sps = Some(sps_id);
        }
    }

    /// The active sequence parameter set, if any has been seen.
    #[must_use]
    pub fn active(&self) -> Option<&Sps> {
        self.get_sps(self.active_sps?)
    }

    /// Whether any sequence parameter set has been stored.
    #[must_use]
    pub const fn has_sps(&self) -> bool {
        self.active_sps.is_some()
    }

    /// Drop everything.
    pub fn clear(&mut self) {
        for slot in &mut self.vps {
            *slot = None;
        }
        for slot in &mut self.sps {
            *slot = None;
        }
        for slot in &mut self.pps {
            *slot = None;
        }
        self.active_sps = None;
    }
}

/// The pixel format an SPS implies.
///
/// # Two places HEVC's answer differs from H.264's, both measured
///
/// `// D17:` **monochrome is `gray`, not 4:2:0.** `vaco-parse-h264` records the
/// reference reporting `chroma_format_idc == 0` as `yuv420p`; for HEVC it
/// reports `gray`. Probed:
///
/// ```text
/// ffmpeg -f lavfi -i testsrc2=s=640x360 -pix_fmt gray -c:v libx265 -f hevc mono.265
/// # trace_headers: chroma_format_idc = 0
/// ffprobe -f hevc -show_entries stream=pix_fmt,color_range mono.265
/// # pix_fmt=gray  color_range=pc
/// ```
///
/// (The `pc` is `x265` setting `video_full_range_flag` for a monochrome input,
/// not a rule — patching the flag to 0 in the same stream gives `tv`.)
///
/// `// D17:` **the `yuvj` family is 4:2:0 at 8 bits only.** H.264 gets
/// `yuvj422p` and `yuvj444p` for full-range 8-bit streams; HEVC does not.
/// Probed across the whole matrix of {gray, 420, 422, 444} × {8, 10, 12} ×
/// {tv, pc}: only `yuv420p` + full range becomes `yuvj420p`. Every other
/// full-range combination keeps its plain name and reports `color_range=pc`
/// beside it.
///
/// # Endianness
///
/// High-bit-depth formats are reported little-endian, for the same reason
/// `vaco-parse-h264` gives: the reference reports the *host's* endianness
/// through a compile-time alias, every target this project ships to is
/// little-endian, and a `PixFmt` has to name one.
#[must_use]
pub fn pixel_format(sps: &Sps) -> Option<PixFmt> {
    let depth = sps.bit_depth_luma;
    let full_range = sps
        .vui
        .as_ref()
        .and_then(|v| v.video_full_range)
        .unwrap_or(false);

    // D17: monochrome keeps its own family.
    if sps.chroma_format == ChromaFormat::Monochrome {
        let name = match depth {
            8 => "gray".to_string(),
            9 | 10 | 12 | 14 | 16 => format!("gray{depth}le"),
            _ => return None,
        };
        return PixFmt::from_name(&name).ok();
    }

    let chroma = match sps.chroma_format {
        ChromaFormat::Yuv422 => "422",
        ChromaFormat::Yuv444 => "444",
        _ => "420",
    };
    let name = match depth {
        // D17: `yuvj` exists for 4:2:0 at 8 bits and nowhere else.
        8 if full_range && sps.chroma_format == ChromaFormat::Yuv420 => "yuvj420p".to_string(),
        8 => format!("yuv{chroma}p"),
        // HEVC permits 8 through 16; the reference has a format for every even
        // depth plus 9, and none for 11, 13 or 15.
        9 | 10 | 12 | 14 | 16 => format!("yuv{chroma}p{depth}le"),
        _ => return None,
    };
    PixFmt::from_name(&name).ok()
}

/// The sample aspect ratio as the reference reports it: reduced, and discarded
/// when it is unusable at this picture size.
///
/// # The rule, recovered by probe — and it is H.264's, unchanged
///
/// `// D17:` the specification says nothing about rejecting an aspect ratio.
/// The reference does, and `sample_aspect_ratio` is printed by `-show_streams`,
/// so the rejection is observable and has to be reproduced.
///
/// Measured against `ffmpeg 8.1` by splicing `aspect_ratio_idc = 255` and a
/// 32-bit `sar_width`/`sar_height` pair into the VUI of a 640x360 HEVC stream —
/// a bit-level insertion with the emulation prevention recomputed, because
/// `x265` clamps its own `--sar` and will not emit the extreme values the
/// boundary needs. Thirty-four rows:
///
/// 1. **It is reduced.** `4:2` prints as `2:1`, `6:4` as `3:2`, `100:10` as
///    `10:1`.
/// 2. **It is discarded when the scaled dimension truncates to zero**, with the
///    reduction happening *first*:
///
///    ```text
///    scaled = num < den ? (width  * num) / den
///                       : (height * den) / num      (truncating)
///    usable iff scaled > 0
///    ```
///
///    On a 640x360 picture that admits `360:1` and rejects `361:1`, admits
///    `1:640` and rejects `1:641`. `720:2` is accepted because it reduces to
///    `360:1`; `722:2` is not.
///
/// This is the same rule `vaco-parse-h264` measured for H.264, at a different
/// picture size — so it is the reference's general aspect-ratio handling rather
/// than anything codec-specific.
#[must_use]
pub fn sample_aspect_ratio(sps: &Sps) -> Rational {
    let raw = sps.sample_aspect_ratio();
    if raw.num <= 0 || raw.den <= 0 {
        return Rational::UNDEFINED;
    }
    let (sar, _) = Rational::reduce(i64::from(raw.num), i64::from(raw.den), i64::from(i32::MAX));
    if sar.num <= 0 || sar.den <= 0 {
        return Rational::UNDEFINED;
    }
    if sar.num == sar.den {
        return sar;
    }
    let (w, h) = sps.dimensions().unwrap_or((0, 0));
    let scaled = if sar.num < sar.den {
        u64::from(w)
            .checked_mul(sar.num.unsigned_abs().into())
            .and_then(|n| n.checked_div(sar.den.unsigned_abs().into()))
    } else {
        u64::from(h)
            .checked_mul(sar.den.unsigned_abs().into())
            .and_then(|n| n.checked_div(sar.num.unsigned_abs().into()))
    };
    if scaled.unwrap_or(0) > 0 {
        sar
    } else {
        Rational::UNDEFINED
    }
}

/// The [`CodecParameters`] a sequence parameter set implies.
///
/// This is what `ffprobe -show_streams` prints for an HEVC stream. Three fields
/// are easy to get wrong and **all three differ from H.264's answer**, so they
/// are called out here rather than left to be discovered:
///
/// * `width` / `height` are the picture after the **conformance window**
///   (§7.4.3.2). The offsets are in chroma units, so a `conf_win_right_offset`
///   of 1 removes two luma columns from a 4:2:0 stream.
/// * `coded_width` / `coded_height` are `pic_width_in_luma_samples` and
///   `pic_height_in_luma_samples` — the coded size, **not** the cropped one.
///   `// D17:` this is the opposite of what the reference does for H.264, where
///   `vaco-parse-h264` measured `coded_width` equal to the cropped width.
///   Probed on a 1918x1078 HEVC stream, which `ffprobe` reports as
///   `width=1918 height=1078 coded_width=1920 coded_height=1080`; and on a
///   66x34 one, reported as `66x34` cropped from `72x40`.
/// * `frame_rate` is `vui_time_scale / vui_num_units_in_tick`, **not halved**.
///   `// D17:` H.264's `r_frame_rate` is twice its picture rate because
///   `num_units_in_tick` counts field durations there; HEVC's does not. A 24 fps
///   HEVC stream prints `r_frame_rate=24/1` where the H.264 encode of the same
///   source prints `48/1`.
#[must_use]
pub fn codec_parameters(sps: &Sps) -> CodecParameters {
    let (width, height) = sps.dimensions().unwrap_or((0, 0));
    let video = VideoParameters {
        width,
        height,
        // D17: the coded size, unlike H.264's.
        coded_width: sps.coded_width(),
        coded_height: sps.coded_height(),
        format: pixel_format(sps),
        sample_aspect_ratio: sample_aspect_ratio(sps),
        // D17: not halved.
        frame_rate: sps.frame_rate(),
        color: sps.color_info(),
        field_order: field_order(sps),
        has_b_frames: sps.max_num_reorder_pics().min(255) as u8,
        // D17: **not** set, unlike H.264's. Measured on the same 1918x1080
        // source encoded twice: `bits_per_raw_sample="8"` for H.264 and
        // `"N/A"` for HEVC, and `"10"` for 10-bit H.264. AV1 and VP9 report
        // `N/A` as well, so H.264 is the exception rather than the rule and
        // nothing about the bit depth transfers between them.
        bits_per_raw_sample: None,
        // D17: `is_avc`/`nal_length_size` are H.264 decoder private options and
        // the reference prints neither for an HEVC stream, in *any* container —
        // probed on an MP4 whose `hvcC` declares a four-byte length prefix.
        // Leaving this `None` is what keeps `vaco-probe` from printing them.
        nal_length_size: None,
        // `quarter_sample`/`divx_packed` are MPEG-4 Part 2 concepts; `None`
        // for every other codec, HEVC included.
        ..VideoParameters::default()
    };

    let mut params = CodecParameters::video().with_codec(CodecId::Hevc);
    params.profile = sps.ptl.general.as_ref().map(profile::profile);
    params.level = Some(profile::level(sps.ptl.general_level_idc));
    params.video = Some(video);
    params
}

/// The field order an SPS alone can state, which is **none**.
///
/// `// D17:` the reference prints `field_order=unknown` for every HEVC stream
/// whose `pic_timing` SEI does not carry a `pic_struct` — including plainly
/// progressive ones. Nineteen `x265` streams across every chroma format, bit
/// depth and frame rate in the corpus all report `unknown`, and patching
/// `field_seq_flag` to 1 in one of them changes nothing.
///
/// That is the opposite of the H.264 answer: `vaco-parse-h264` measured
/// `field_order=progressive` from `frame_mbs_only_flag` alone. HEVC has no
/// equivalent flag — `field_seq_flag` says a stream codes *fields* but not
/// which comes first — and the reference declines to infer one. The
/// `pic_timing` SEI is the only source, and
/// [`HevcParser`](crate::parser::HevcParser) applies it when one arrives.
///
/// The parsed `field_seq_flag` is still available at
/// [`VuiParameters::field_seq`](crate::sps::VuiParameters::field_seq).
#[must_use]
pub const fn field_order(_sps: &Sps) -> FieldOrder {
    FieldOrder::Unknown
}

/// The level table for HEVC, from Annex A Table A.6.
///
/// Re-exported here so a caller that has [`CodecParameters`] in hand can resolve
/// `level` to a name without reaching into [`crate::profile`].
pub const LEVEL_TABLE: vaco_codec_core::LevelTable = profile::LEVELS;

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
    use vaco_limits::Limits;

    /// The SPS `x265` writes for `testsrc2=s=640x360:r=24`, byte for byte from
    /// a real Annex B stream, emulation prevention still in place.
    const SD_SPS_EBSP: &[u8] = &[
        0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00,
        0x03, 0x00, 0x3f, 0xa0, 0x05, 0x02, 0x01, 0x69, 0x65, 0x95, 0x9a, 0x49, 0x32, 0xbc, 0x05,
        0xa0, 0x20, 0x00, 0x00, 0x03, 0x00, 0x20, 0x00, 0x00, 0x03, 0x03, 0x01,
    ];
    const SD_PPS_EBSP: &[u8] = &[0x44, 0x01, 0xc1, 0x72, 0xb4, 0x62, 0x40];
    const SD_VPS_EBSP: &[u8] = &[
        0x40, 0x01, 0x0c, 0x01, 0xff, 0xff, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00,
        0x03, 0x00, 0x00, 0x03, 0x00, 0x3f, 0x95, 0x98, 0x09,
    ];

    fn rbsp(ebsp: &[u8]) -> Vec<u8> {
        let mut scratch = Vec::new();
        vaco_bitstream::annexb::to_rbsp(ebsp, &mut scratch).to_vec()
    }

    fn parse_sd() -> Sps {
        let mut budget = Budget::new(Limits::strict());
        Sps::parse(&rbsp(SD_SPS_EBSP), &mut budget).expect("a real SPS parses")
    }

    #[test]
    fn the_store_holds_all_three_kinds_and_needs_no_ordering() {
        let mut sets = ParameterSets::new();
        let mut budget = Budget::new(Limits::strict());
        // PPS first, which H.264's store could not do.
        assert_eq!(sets.add_pps(&rbsp(SD_PPS_EBSP), &mut budget).unwrap(), 0);
        assert_eq!(sets.add_sps(&rbsp(SD_SPS_EBSP), &mut budget).unwrap(), 0);
        assert_eq!(sets.add_vps(&rbsp(SD_VPS_EBSP), &mut budget).unwrap(), 0);
        assert!(sets.has_sps());
        assert!(sets.sps_for_pps(0).is_some());
        assert!(sets.get_vps(0).is_some());
        assert!(sets.get_sps(15).is_none());
        sets.clear();
        assert!(!sets.has_sps());
    }

    #[test]
    fn pixel_formats_across_the_matrix() {
        let mut sps = parse_sd();
        for (chroma, depth, full, expected) in [
            (ChromaFormat::Yuv420, 8u8, false, Some("yuv420p")),
            // D17: the only `yuvj` HEVC produces.
            (ChromaFormat::Yuv420, 8, true, Some("yuvj420p")),
            (ChromaFormat::Yuv422, 8, false, Some("yuv422p")),
            (ChromaFormat::Yuv422, 8, true, Some("yuv422p")),
            (ChromaFormat::Yuv444, 8, false, Some("yuv444p")),
            (ChromaFormat::Yuv444, 8, true, Some("yuv444p")),
            // D17: monochrome is gray, not 4:2:0.
            (ChromaFormat::Monochrome, 8, false, Some("gray")),
            (ChromaFormat::Monochrome, 8, true, Some("gray")),
            (ChromaFormat::Monochrome, 10, false, Some("gray10le")),
            (ChromaFormat::Monochrome, 12, false, Some("gray12le")),
            (ChromaFormat::Yuv420, 10, true, Some("yuv420p10le")),
            (ChromaFormat::Yuv420, 10, false, Some("yuv420p10le")),
            (ChromaFormat::Yuv420, 12, false, Some("yuv420p12le")),
            (ChromaFormat::Yuv422, 10, false, Some("yuv422p10le")),
            (ChromaFormat::Yuv422, 12, false, Some("yuv422p12le")),
            (ChromaFormat::Yuv444, 10, false, Some("yuv444p10le")),
            (ChromaFormat::Yuv444, 12, false, Some("yuv444p12le")),
            (ChromaFormat::Yuv444, 16, false, Some("yuv444p16le")),
            // No format exists for these depths.
            (ChromaFormat::Yuv420, 11, false, None),
            (ChromaFormat::Yuv420, 13, false, None),
            (ChromaFormat::Yuv420, 15, false, None),
        ] {
            sps.chroma_format = chroma;
            sps.bit_depth_luma = depth;
            if let Some(v) = sps.vui.as_mut() {
                v.video_full_range = Some(full);
            }
            let got = pixel_format(&sps).map(PixFmt::name);
            assert_eq!(
                got, expected,
                "chroma {chroma:?} depth {depth} full_range {full}"
            );
        }
    }

    #[test]
    fn codec_parameters_report_the_windowed_size_and_the_coded_one() {
        let sps = parse_sd();
        let params = codec_parameters(&sps);
        let v = params.video.expect("video parameters");
        assert_eq!((v.width, v.height), (640, 360));
        // D17: coded_* is the CODED size for HEVC, unlike H.264.
        assert_eq!((v.coded_width, v.coded_height), (640, 360));
        // D17: the frame rate is not halved.
        assert_eq!(v.frame_rate, Rational::new(24, 1));
        assert_eq!(v.has_b_frames, 2);
        assert_eq!(params.profile.map(|p| p.name), Some("Main"));
        assert_eq!(params.level.map(vaco_codec_core::Level::raw), Some(63));
        assert_eq!(v.field_order, FieldOrder::Unknown);
    }

    /// The sample-aspect-ratio rejection rule, at both boundaries, on a 640x360
    /// picture. Every row was read back from `ffprobe 8.1`.
    #[test]
    fn the_sar_rejection_boundary() {
        let cases: &[(u16, u16, Option<(i32, i32)>)] = &[
            (1, 1, Some((1, 1))),
            (4, 2, Some((2, 1))),
            (6, 4, Some((3, 2))),
            (100, 10, Some((10, 1))),
            (3, 1, Some((3, 1))),
            (11, 1, Some((11, 1))),
            (358, 1, Some((358, 1))),
            (359, 1, Some((359, 1))),
            (360, 1, Some((360, 1))), // 360*1/360 = 1, just usable
            (361, 1, None),           // 360*1/361 = 0, discarded
            (362, 1, None),
            (720, 2, Some((360, 1))), // reduces to 360:1 first
            (722, 2, None),           // reduces to 361:1
            (1, 639, Some((1, 639))),
            (1, 640, Some((1, 640))), // 640*1/640 = 1
            (1, 641, None),           // 640*1/641 = 0
            (1, 1280, None),
            (65_535, 65_535, Some((1, 1))),
            (65_534, 65_533, Some((65_534, 65_533))),
            (0, 1, None),
            (1, 0, None),
        ];
        let base = parse_sd();
        for &(w, h, expected) in cases {
            let mut sps = base.clone();
            let vui = sps.vui.as_mut().expect("a VUI");
            vui.aspect_ratio_idc = Some(crate::sps::EXTENDED_SAR);
            vui.sar = Some((w, h));
            let got = sample_aspect_ratio(&sps);
            match expected {
                Some((n, d)) => assert_eq!((got.num, got.den), (n, d), "sar {w}:{h}"),
                None => assert!(got.is_undefined(), "sar {w}:{h} should be discarded"),
            }
        }
    }

    /// An SPS alone never states a field order, whatever `field_seq_flag`
    /// says — which is what the reference prints.
    #[test]
    fn an_sps_alone_never_states_a_field_order() {
        let mut sps = parse_sd();
        assert_eq!(field_order(&sps), FieldOrder::Unknown);
        sps.vui.as_mut().expect("a VUI").field_seq = true;
        assert_eq!(field_order(&sps), FieldOrder::Unknown);
    }
}
