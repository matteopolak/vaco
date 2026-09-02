//! The parameter-set store, and the stream description an SPS implies.

use vaco_codec_core::{CodecId, CodecParameters, FieldOrder, VideoParameters};
use vaco_core::{Error, Rational, Result};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

use crate::pps::Pps;
use crate::profile::{self, LEVELS};
use crate::sps::{ChromaFormat, Sps, SpsExtension, VuiParameters};

/// `seq_parameter_set_id` runs 0..=31 (§7.4.2.1.1), so the store is a fixed
/// array rather than a map: 32 slots is smaller than a hash map's header, and a
/// fixed array cannot be grown by a hostile stream that sends 32 distinct ids.
pub const MAX_SPS: usize = 32;

/// `pic_parameter_set_id` runs 0..=255 (§7.4.2.2).
///
/// 256 `Option<Box<Pps>>` slots is 2 KiB of pointers, which is the price of
/// making "a stream with 256 distinct PPS ids" cost the same as any other.
pub const MAX_PPS: usize = 256;

/// Every parameter set seen so far, indexed by id.
///
/// # Why the sets are boxed and cloned rather than shared
///
/// A `Box` rather than an `Arc` because nothing in a parser is concurrent: the
/// header stage is sequential by construction (plan 15 §1.8.1). If frame
/// threading ever needs to hand a parameter set to a task, the change is
/// `Box` to `Arc` in this one file.
#[derive(Debug, Default)]
pub struct ParameterSets {
    sps: Box<[Option<Sps>]>,
    pps: Box<[Option<Pps>]>,
    /// Keyed the same way as `sps`: `seq_parameter_set_id`. Most streams
    /// have none — the auxiliary-picture extension (§7.3.2.1.2) is Annex G
    /// syntax that only a handful of alpha/depth-coded streams ever carry.
    sps_ext: Box<[Option<SpsExtension>]>,
    /// The id of the most recently activated SPS, which is what a stream
    /// description is derived from.
    active_sps: Option<u8>,
}

impl ParameterSets {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sps: (0..MAX_SPS).map(|_| None).collect(),
            pps: (0..MAX_PPS).map(|_| None).collect(),
            sps_ext: (0..MAX_SPS).map(|_| None).collect(),
            active_sps: None,
        }
    }

    /// Parse and store a sequence parameter set from a NAL unit's RBSP.
    ///
    /// Returns the id it was stored under. A repeated id replaces the previous
    /// set, which is what §7.4.1.2.1 permits between coded video sequences.
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
            .ok_or(Error::InvalidData("seq_parameter_set_id out of range"))?;
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
        // The PPS's own tail is sized by its SPS, so look it up first. The
        // id is at a fixed position, but reading it here would mean parsing
        // twice; instead the PPS parser is given whatever SPS is active, and
        // re-parsed against the right one if that guess was wrong.
        let guess = self.active_sps.and_then(|id| self.get_sps(id));
        let pps = match Pps::parse(rbsp, guess, budget) {
            Ok(p) if guess.is_none_or(|s| s.id == p.sps_id) => p,
            Ok(p) => {
                // The guess was the wrong SPS. It only matters when the tail
                // has scaling lists, and re-parsing is cheaper than reasoning
                // about when it does.
                match self.get_sps(p.sps_id) {
                    Some(real) => Pps::parse(rbsp, Some(real), budget)?,
                    None => p,
                }
            }
            Err(e) => return Err(e),
        };
        let id = pps.id;
        let slot = self
            .pps
            .get_mut(id as usize)
            .ok_or(Error::InvalidData("pic_parameter_set_id out of range"))?;
        *slot = Some(pps);
        Ok(id)
    }

    /// Parse and store a sequence parameter set extension (§7.3.2.1.2).
    ///
    /// Returns the `seq_parameter_set_id` it was stored under. Unlike
    /// [`Self::add_sps`], nothing here requires the base SPS to already be
    /// present — the extension is validated on its own syntax, and pairing
    /// it with a base SPS is a caller concern (§7.4.1.2.1 does not require
    /// the extension to immediately follow its SPS, only to follow it
    /// somewhere in the same access unit or earlier).
    ///
    /// # Errors
    ///
    /// Whatever [`SpsExtension::parse`] returns.
    pub fn add_sps_extension(&mut self, rbsp: &[u8], budget: &mut Budget) -> Result<u8> {
        let ext = SpsExtension::parse(rbsp, budget)?;
        let id = ext.seq_parameter_set_id;
        let slot = self
            .sps_ext
            .get_mut(id as usize)
            .ok_or(Error::InvalidData("seq_parameter_set_id out of range"))?;
        *slot = Some(ext);
        Ok(id)
    }

    /// The sequence parameter set with this id.
    #[must_use]
    pub fn get_sps(&self, id: u8) -> Option<&Sps> {
        self.sps.get(id as usize)?.as_ref()
    }

    /// The auxiliary-picture extension for the sequence parameter set with
    /// this id, if the stream carried one.
    #[must_use]
    pub fn get_sps_extension(&self, id: u8) -> Option<&SpsExtension> {
        self.sps_ext.get(id as usize)?.as_ref()
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
    pub fn has_sps(&self) -> bool {
        self.active_sps.is_some()
    }

    /// Drop everything. Parameter sets do **not** survive a seek in general —
    /// a stream may redefine them — but a parser normally keeps them, because
    /// re-acquiring them costs a whole GOP of output.
    pub fn clear(&mut self) {
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
/// # Two places the reference does not follow the specification
///
/// `// D17:` **monochrome is reported as 4:2:0.** `chroma_format_idc == 0` is
/// Table 6-1's monochrome format and has no chroma arrays at all, so `gray`
/// (or `gray10le`, …) is the faithful answer. `ffmpeg 8.1` reports
/// `yuv420p` — or `yuvj420p`, since `libx264` also sets the full-range flag for
/// such a stream. Probed:
///
/// ```text
/// ffmpeg -f lavfi -i testsrc2=s=640x360 -pix_fmt gray -c:v libx264 -f h264 mono.264
/// # trace_headers: chroma_format_idc = 0
/// ffprobe -f h264 -show_entries stream=pix_fmt,color_range mono.264
/// # pix_fmt=yuvj420p  color_range=pc
/// ```
///
/// The same answer comes back from an actual decode, so it is not a
/// parse-only shortcut on the reference's part. D6 makes `pix_fmt` part of the
/// output contract, so it is reproduced. [`Sps::chroma_format`] still says
/// `Monochrome`, so nothing is lost.
///
/// `// D17:` **the `yuvj` family is 8-bit only.** Full range at 8 bits gives
/// `yuvj420p`/`yuvj422p`/`yuvj444p`; full range at 10 bits gives plain
/// `yuv420p10le` with `color_range=pc` beside it, because no `yuvj` format
/// exists above 8 bits. Probed at both depths.
///
/// # Endianness
///
/// High-bit-depth formats are reported little-endian. The reference reports the
/// *host's* endianness — its `AV_PIX_FMT_YUV420P10` is an alias resolved at
/// compile time — so on a big-endian host it would print `be`. Every target
/// this project ships to is little-endian, and a `PixFmt` has to name one, so
/// `le` it is.
#[must_use]
pub fn pixel_format(sps: &Sps) -> Option<PixFmt> {
    let depth = sps.bit_depth_luma;
    let full_range = sps
        .vui
        .as_ref()
        .and_then(|v| v.video_full_range)
        .unwrap_or(false);
    let chroma = match sps.chroma_format {
        // D17: the reference reports monochrome as 4:2:0.
        ChromaFormat::Monochrome | ChromaFormat::Yuv420 => "420",
        ChromaFormat::Yuv422 => "422",
        ChromaFormat::Yuv444 => "444",
    };
    let name = match depth {
        8 if full_range => format!("yuvj{chroma}p"),
        8 => format!("yuv{chroma}p"),
        // H.264 permits 9 through 14; the reference has a format for every
        // even depth plus 9, and none for 11 or 13.
        9 | 10 | 12 | 14 => format!("yuv{chroma}p{depth}le"),
        _ => return None,
    };
    PixFmt::from_name(&name).ok()
}

/// The sample aspect ratio as the reference reports it: reduced, and discarded
/// when it is unusable at this picture size.
///
/// # The rule, recovered by probe
///
/// `// D17:` the specification says nothing about rejecting an aspect ratio.
/// The reference does, and `sample_aspect_ratio` is printed by
/// `-show_streams`, so the rejection is observable and has to be reproduced.
///
/// Two behaviours, both measured against `ffmpeg 8.1` by patching `sar_width`
/// and `sar_height` in the VUI of a 320x240 stream and reading back
/// `-show_entries stream=sample_aspect_ratio`:
///
/// 1. **It is reduced.** `4:2` is printed as `2:1`, `6:4` as `3:2`, `100:10`
///    as `10:1`.
/// 2. **It is discarded when the scaled dimension truncates to zero.** The
///    reference scales the *shorter* axis by the ratio and requires the result
///    to stay above zero:
///
///    ```text
///    scaled = num < den ? (width  * num) / den
///                       : (height * den) / num      (truncating)
///    usable iff scaled > 0
///    ```
///
///    On a 320x240 picture that admits everything from `1:320` to `240:1` and
///    rejects `1:321` and `241:1`. Twelve rows were probed either side of both
///    boundaries and all twelve agree, including that the reduction happens
///    *first*: `480:2` is accepted (it reduces to `240:1`) and `482:2` is not.
///
/// A ratio of `0:x` or `x:0` is unusable; the specification calls the first
/// "unspecified" and the second is malformed.
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
/// This is what `ffprobe -show_streams` prints for an H.264 stream, and the
/// three fields most easily got wrong are called out here rather than left to
/// be discovered:
///
/// * `width` / `height` are the **cropped** dimensions (§7.4.2.1.1). A 1080-line
///   stream is coded as 1088 and cropped; reporting 1088 is the classic bug.
/// * `coded_width` / `coded_height` are set **equal to** the cropped ones.
///   `// D17:` that is not what the names suggest, and it is what the reference
///   does: `ffprobe` on a 1918x1078 stream prints `coded_width=1918`, not the
///   macroblock-aligned 1920. [`Sps::coded_width`] and [`Sps::coded_height`]
///   still give the aligned values for a caller that wants them.
/// * `frame_rate` is the **tick rate**, `time_scale / num_units_in_tick`, which
///   is twice the picture rate §E.2.1 defines. `// D17:` that is the number the
///   reference prints as `r_frame_rate` for a raw Annex B stream — 48/1 for a
///   24 fps file. [`Sps::frame_rate`] gives the halved, specification-defined
///   one.
#[must_use]
pub fn codec_parameters(sps: &Sps) -> CodecParameters {
    let (width, height) = sps.dimensions().unwrap_or((0, 0));
    let video = VideoParameters {
        width,
        height,
        // D17: equal to the cropped dimensions, per the reference.
        coded_width: width,
        coded_height: height,
        format: pixel_format(sps),
        sample_aspect_ratio: sample_aspect_ratio(sps),
        // D17: the tick rate, which is what `r_frame_rate` carries.
        frame_rate: sps
            .vui
            .as_ref()
            .map_or(Rational::UNDEFINED, VuiParameters::tick_rate),
        color: sps.color_info(),
        field_order: if sps.frame_mbs_only {
            FieldOrder::Progressive
        } else {
            // Without an SEI `pic_struct` the SPS alone cannot say which field
            // comes first; only that the stream may be field-coded. The parser
            // refines this from `pic_timing` when one arrives.
            FieldOrder::Unknown
        },
        has_b_frames: sps.max_num_reorder_frames().unwrap_or(0).min(255) as u8,
        // D17: measured, and it does NOT transfer to the other codecs. The
        // reference prints `bits_per_raw_sample=8` on an 8-bit H.264 stream
        // and `10` on a 10-bit one, but prints `N/A` for HEVC, AV1 and VP9 at
        // the same depth. So this is set here and deliberately left unset in
        // `vaco-parse-hevc` and `vaco-parse-av1`.
        //
        //   1918x1080 yuv420p10le h264 -> bits_per_raw_sample="10"
        //   1918x1080 yuv420p     hevc -> bits_per_raw_sample="N/A"
        bits_per_raw_sample: Some(sps.bit_depth_luma),
        // Annex B until a configuration record says otherwise. `parser::
        // H264Parser::set_extradata` overwrites this with the record's own
        // length size; a stream that never gets one is a byte stream, which is
        // what `nal_length_size=0` means.
        nal_length_size: Some(0),
        // `quarter_sample`/`divx_packed` are MPEG-4 Part 2 concepts (see
        // `VideoParameters`'s own doc); `None` for every other codec.
        ..VideoParameters::default()
    };

    let mut params = CodecParameters::video().with_codec(CodecId::H264);
    params.profile = Some(profile::profile(sps.profile_idc, sps.constraint_flags));
    params.level = Some(profile::level(sps.level_idc));
    params.video = Some(video);
    params
}

/// The level table for H.264, from Annex A Table A-1.
///
/// Re-exported here so a caller that has `CodecParameters` in hand can resolve
/// `level` to a name without reaching into [`crate::profile`].
pub const LEVEL_TABLE: vaco_codec_core::LevelTable = LEVELS;

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

    /// The SPS `libx264` writes for `testsrc2=s=640x360:r=24`, byte for byte
    /// from a real Annex B stream, emulation prevention still in place.
    const SD_SPS_EBSP: &[u8] = &[
        0x67, 0x64, 0x00, 0x1E, 0xAC, 0xD9, 0x40, 0xA0, 0x2F, 0xF9, 0x70, 0x11, 0x00, 0x00, 0x03,
        0x00, 0x01, 0x00, 0x00, 0x03, 0x00, 0x30, 0x0F, 0x16, 0x2D, 0x96,
    ];

    fn parse_sd() -> Sps {
        let mut scratch = Vec::new();
        let rbsp = vaco_bitstream::annexb::to_rbsp(SD_SPS_EBSP, &mut scratch);
        let mut budget = Budget::new(Limits::strict());
        Sps::parse(rbsp, &mut budget).expect("a real SPS parses")
    }

    #[test]
    fn the_store_rejects_nothing_it_can_hold() {
        let mut sets = ParameterSets::new();
        let mut scratch = Vec::new();
        let rbsp = vaco_bitstream::annexb::to_rbsp(SD_SPS_EBSP, &mut scratch).to_vec();
        let mut budget = Budget::new(Limits::strict());
        assert_eq!(sets.add_sps(&rbsp, &mut budget).unwrap(), 0);
        assert!(sets.has_sps());
        assert!(sets.get_sps(0).is_some());
        assert!(sets.get_sps(31).is_none());
    }

    #[test]
    fn add_sps_extension_stores_it_under_its_own_id() {
        let mut sets = ParameterSets::new();
        let mut w = vaco_bitstream::BitWriter::new();
        w.ue(0); // seq_parameter_set_id
        w.ue(0); // aux_format_idc == 0
        w.put(1, 0); // additional_extension_flag
        w.rbsp_trailing();
        let mut rbsp = vec![0x0Du8]; // nal_unit_type 13
        rbsp.extend_from_slice(&w.finish());

        let mut budget = Budget::new(Limits::permissive());
        assert_eq!(sets.add_sps_extension(&rbsp, &mut budget).unwrap(), 0);
        let ext = sets.get_sps_extension(0).expect("stored");
        assert!(ext.aux_format.is_none());
        assert!(sets.get_sps_extension(1).is_none());
    }

    #[test]
    fn pixel_formats_across_the_matrix() {
        let mut sps = parse_sd();
        for (chroma, depth, full, expected) in [
            (ChromaFormat::Yuv420, 8u8, false, Some("yuv420p")),
            (ChromaFormat::Yuv420, 8, true, Some("yuvj420p")),
            (ChromaFormat::Yuv422, 8, false, Some("yuv422p")),
            (ChromaFormat::Yuv422, 8, true, Some("yuvj422p")),
            (ChromaFormat::Yuv444, 8, false, Some("yuv444p")),
            (ChromaFormat::Yuv444, 8, true, Some("yuvj444p")),
            // D17: monochrome reports as 4:2:0.
            (ChromaFormat::Monochrome, 8, false, Some("yuv420p")),
            (ChromaFormat::Monochrome, 8, true, Some("yuvj420p")),
            // D17: there is no `yuvj` above 8 bits.
            (ChromaFormat::Yuv420, 10, true, Some("yuv420p10le")),
            (ChromaFormat::Yuv420, 10, false, Some("yuv420p10le")),
            (ChromaFormat::Yuv444, 12, false, Some("yuv444p12le")),
            (ChromaFormat::Yuv422, 14, false, Some("yuv422p14le")),
            (ChromaFormat::Yuv420, 9, false, Some("yuv420p9le")),
            // No format exists for these depths.
            (ChromaFormat::Yuv420, 11, false, None),
            (ChromaFormat::Yuv420, 13, false, None),
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
    fn codec_parameters_report_the_cropped_size() {
        let sps = parse_sd();
        let params = codec_parameters(&sps);
        let v = params.video.expect("video parameters");
        assert_eq!((v.width, v.height), (640, 360));
        // D17: coded_* equals the cropped size, which is what ffprobe prints.
        assert_eq!((v.coded_width, v.coded_height), (640, 360));
        // ...while the SPS itself still knows the macroblock-aligned height.
        assert_eq!(sps.coded_height(), 368);
        assert_eq!(v.frame_rate, vaco_core::Rational::new(48, 1));
        assert_eq!(v.has_b_frames, 2);
        assert_eq!(params.profile.map(|p| p.name), Some("High"));
        assert_eq!(params.level.map(vaco_codec_core::Level::raw), Some(30));
    }
}
