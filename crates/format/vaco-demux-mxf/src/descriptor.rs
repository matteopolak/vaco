//! Turning an essence descriptor set into [`CodecParameters`].
//!
//! # What is measured
//!
//! Every property this module reads (`StoredWidth`/`Height`,
//! `SampledWidth`/`Height`, `DisplayWidth`/`Height`, `AspectRatio`,
//! `FrameLayout`, `SampleRate`, `PictureEssenceCoding`, `ComponentDepth`,
//! `Horizontal`/`VerticalSubsampling`) was decoded from a real
//! `MPEGVideoDescriptor` in `out.mxf` (see `ul` module docs) and matches the
//! stream `ffprobe` reports for the same file exactly: `720x576`, SAR
//! `1:1`, DAR `5:4`, 25 fps.
//!
//! # What is not
//!
//! [`PICTURE_ESSENCE_CODING`] maps exactly one Universal Label — MPEG-2
//! Long GOP — to a [`CodecId`], because that is the only one this crate has
//! measured against a real file. `DNxHD`, `ProRes`, uncompressed and JPEG 2000
//! all have well-known RP210 essence-coding labels this crate has **not**
//! verified; guessing one and getting a single byte wrong would silently
//! misidentify a codec, which is worse than reporting `codec_id: None` for a
//! descriptor this table does not recognise (D6/D17: measure, do not
//! recall). See this crate's closing report for the exact gap.

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{CodecId, CodecParameters, FieldOrder};
use vaco_core::Rational;
use vaco_sampfmt::SampleFmt;

use crate::metadata::MetadataSet;
use crate::properties::PropertyId;
use crate::ul::{StructuralClass, Ul};

/// `(PictureEssenceCoding UL, CodecId)`. See the module docs: only the first
/// two rows are measured, both against real files, both `ffprobe`-confirmed
/// `codec_name=mpeg2video`.
const PICTURE_ESSENCE_CODING: &[(Ul, CodecId)] = &[
    (
        Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x03, 0x04, 0x01, 0x02, 0x02, 0x01, 0x01,
            0x11, 0x00,
        ]),
        CodecId::Mpeg2video,
    ),
    (
        // MPEG-2 4:2:2 (the D-10/SMPTE 386M constrained profile) at 50
        // Mbit/s. Measured against a real `ffmpeg -f mxf_d10` file:
        // `ffprobe` reports `codec_name=mpeg2video` for it, the same
        // `CodecId` as the Long GOP UL above — a distinct label from
        // RP210 for a distinct profile, not a distinct codec.
        Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x01, 0x04, 0x01, 0x02, 0x02, 0x01, 0x02,
            0x01, 0x01,
        ]),
        CodecId::Mpeg2video,
    ),
    (
        // The same D-10 profile at 40 Mbit/s — a distinct RP210 label from
        // the 50 Mbit/s one above (differs only in the last byte), measured
        // the same way against a second real `ffmpeg -f mxf_d10` file at
        // `-b:v 40000000`.
        Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x01, 0x04, 0x01, 0x02, 0x02, 0x01, 0x02,
            0x01, 0x03,
        ]),
        CodecId::Mpeg2video,
    ),
    (
        // The same D-10 profile at 30 Mbit/s, measured the same way at
        // `-b:v 30000000`.
        Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x01, 0x04, 0x01, 0x02, 0x02, 0x01, 0x02,
            0x01, 0x05,
        ]),
        CodecId::Mpeg2video,
    ),
];

/// `FrameLayout` values 1..=3 (`SeparateFields`, `SingleField`, `MixedFields`) are
/// interlaced, but the dominant-field property this crate would need to say
/// which field is first was not among the properties measured against a
/// real file (see module docs), so every non-progressive layout — known or
/// not — reports `Unknown` rather than a guess.
fn frame_layout_to_field_order(layout: u8) -> FieldOrder {
    match layout {
        // FullFrame, SegmentedFrame.
        0 | 4 => FieldOrder::Progressive,
        _ => FieldOrder::Unknown,
    }
}

/// Build [`CodecParameters`] for a sound (audio) essence descriptor
/// (`AES3PCMDescriptor` or `GenericSoundEssenceDescriptor`).
///
/// # What is measured
///
/// `SampleRate`, `AudioChannelCount` and `AudioQuantizationBits` were
/// decoded from a real `AES3PCMDescriptor` in an `ffmpeg -f mxf` file
/// carrying one `pcm_s16le` track alongside video, and confirmed against
/// what `ffprobe` reports for the same file exactly: `48000 Hz`, `2`
/// channels, `16` bits, with packet `pos`/`len` matching `ffprobe` exactly
/// too — this shape's essence bytes are genuinely tightly-interleaved
/// `pcm_s16le`, verbatim.
///
/// The same descriptor properties were confirmed again on a real
/// `ffmpeg -f mxf_d10` file with audio (class `GenericSoundEssenceDescriptor`,
/// not `AES3PCMDescriptor` — a second, distinct measured class, see
/// `ul.rs`), which answered the open question of whether D-10 audio needed
/// a different path for *metadata*: it does not, the same properties apply.
/// But the essence *bytes* are a different story — see below.
///
/// # The D-10 essence-element layout is not raw PCM
///
/// Measured by comparing this crate's raw KLV length against `ffprobe`'s
/// reported packet size on real `ffmpeg -f mxf_d10` files with audio: they
/// disagree (`61444` raw vs `30720`/`7680` reported), and the disagreement
/// is not a bug in either reader. Byte-level inspection (dumping the raw
/// essence-element value and comparing it word-by-word against `ffmpeg`'s
/// own extracted PCM, via `ffmpeg -c copy -f data`) found a fixed structure,
/// identical in a 2-logical-channel and an 8-logical-channel fixture:
///
/// - A 4-byte element header of undetermined meaning (not a sample count in
///   any encoding tried; skipped rather than guessed at).
/// - Then, per sample instant (`1920` of them at 48 kHz/25 fps), **8** fixed
///   channel slots regardless of the descriptor's own `AudioChannelCount`
///   (measured `2` and `8` both physically occupy all 8 slots) — each slot a
///   4-byte word: 1 tag byte (the slot's 0-based index, constant per slot)
///   followed by a little-endian 24-bit field holding the 16-bit PCM sample
///   left-shifted by 4 bits (confirmed exactly, `raw / 16 == pcm16` on every
///   sample checked). Slots beyond the descriptor's logical channel count
///   carry a zero sample value but keep their real index tag.
/// - `4 + 1920 * 8 * 4 == 61444`, matching the raw KLV length exactly in
///   both fixtures; `ffprobe`'s reported size is the logical channels'
///   worth of unpacked, tightly-interleaved 16-bit samples only
///   (`1920 * channels * 2`).
///
/// This is a real SMPTE-331-style AES3 physical bundle, not a container
/// framing fact this crate's `read_packet` can correct by itself: turning
/// it into playable `pcm_s16le` needs the descriptor's channel count fed
/// back into per-sample unpacking, which is bitstream-level essence-format
/// work, not container demuxing (the same D14.1 line this crate already
/// draws for MPEG-2 timestamp reordering elsewhere). So `read_packet`
/// reports the real, unmodified essence bytes and length (never a fabricated
/// smaller size), and `sound_parameters` reports `codec_id: None` for a
/// `GenericSoundEssenceDescriptor` specifically — `sample_rate`/channel
/// layout/`format` are still accurate descriptor facts, but the packet
/// bytes are not literal `pcm_s16le` and claiming otherwise would be
/// actively wrong, not just incomplete.
///
/// # What is not
///
/// Only 16-bit quantization maps to a `CodecId`
/// (`CodecId::PcmS16le`), and only for `AES3PCMDescriptor` (the class
/// measured to carry raw interleaved PCM) — the only bit depth and shape
/// this crate has measured against a real file. 8/24/32-bit are plausible
/// real shapes (and have existing `CodecId` variants for at least 8 and 32)
/// but guessing the byte order or padding convention MXF's
/// `AES3PCMDescriptor` uses for them would be exactly the kind of
/// unverified guess D6/D17 asks this crate to avoid — they report
/// `codec_id: None` instead, same as an unrecognised `PictureEssenceCoding`.
#[must_use]
pub fn sound_parameters(descriptor: &MetadataSet) -> CodecParameters {
    let mut params = CodecParameters::audio();
    let Some(audio) = params.audio.as_mut() else {
        return params;
    };
    if let Some(r) = descriptor.get_rational(PropertyId::AudioSampleRate) {
        // A rational sample rate is always an integer in every real file
        // this crate has seen (48000/1); `num` alone is what `ffprobe`
        // reports. `AudioSampleRate`, not the generic `SampleRate` — see
        // the `PropertyId::AudioSampleRate` doc comment for why the two
        // are not interchangeable.
        audio.sample_rate = u32::try_from(r.num.max(0)).unwrap_or(0);
    }
    let channels = descriptor.get_u32(PropertyId::AudioChannelCount);
    if let Some(ch) = channels {
        audio.layout = ChannelLayout::default_for(ch);
    }
    // `AudioQuantizationBits` is the primary source; `AudioBlockAlign`
    // (bytes per frame, all channels) is a cross-check this crate can
    // derive the same number from when quantization bits is absent but
    // block align and channel count are both present — measured to agree
    // exactly on a real file (`block_align=4, channels=2` implies 16 bits,
    // matching `AudioQuantizationBits=16` on that same descriptor).
    #[allow(
        clippy::integer_division,
        reason = "channels is checked non-zero immediately above"
    )]
    let bits = descriptor
        .get_u32(PropertyId::AudioQuantizationBits)
        .or_else(|| {
            let block_align = u32::from(descriptor.get_u16(PropertyId::AudioBlockAlign)?);
            let ch = channels?;
            (ch > 0).then(|| (block_align / ch) * 8)
        });
    if bits == Some(16) {
        audio.format = Some(SampleFmt::S16);
        // `AES3PCMDescriptor` (0x47) is the class measured to carry raw,
        // tightly-interleaved `pcm_s16le` verbatim (see above). A
        // `GenericSoundEssenceDescriptor` (0x42) — D-10's audio class in
        // every real fixture this crate has seen — carries the fixed
        // 8-slot AES3 bundle described above instead: the packet bytes are
        // not literal `pcm_s16le`, so `codec_id` stays `None` rather than
        // claiming a shape the bytes do not have.
        if descriptor.class == StructuralClass::Descriptor(0x47) {
            params.codec_id = Some(CodecId::PcmS16le);
        }
    }
    params
}

/// Build [`CodecParameters`] for a picture (video) essence descriptor.
///
/// Returns `None` if the set carries no `PictureEssenceCoding` this crate
/// recognises — the caller's stream still gets built, just with
/// `codec_id: None`, exactly like an unrecognised codec anywhere else in the
/// workspace.
#[must_use]
pub fn picture_parameters(descriptor: &MetadataSet) -> CodecParameters {
    let mut params = CodecParameters::video();
    let Some(video) = params.video.as_mut() else {
        return params;
    };
    if let Some(w) = descriptor.get_u32(PropertyId::StoredWidth) {
        video.coded_width = w;
        video.width = w;
    }
    if let Some(h) = descriptor.get_u32(PropertyId::StoredHeight) {
        video.coded_height = h;
        video.height = h;
    }
    // `Sampled*` (the region actually carrying picture, excluding VBI) wins
    // over `Stored*` for the *display* dimensions when both are present and
    // narrower — measured: `out.mxf` states identical Stored/Sampled/Display
    // dimensions (720x576), so this crate has not observed a file where they
    // differ, and documents the fallback order rather than asserting one is
    // definitely right.
    if let Some(w) = descriptor
        .get_u32(PropertyId::DisplayWidth)
        .or_else(|| descriptor.get_u32(PropertyId::SampledWidth))
    {
        video.width = w;
    }
    if let Some(h) = descriptor
        .get_u32(PropertyId::DisplayHeight)
        .or_else(|| descriptor.get_u32(PropertyId::SampledHeight))
    {
        video.height = h;
    }
    let frame_layout = descriptor.get_u8(PropertyId::FrameLayout);
    // `FrameLayout == 1` ("Separate Fields") states every height property in
    // terms of one field, not the frame — measured against a real D-10 file
    // (`ffmpeg -f mxf_d10`): `Stored`/`Sampled`/`DisplayHeight` all read
    // `288`, while `ffprobe` reports the frame itself as `576`, exactly
    // double. Applied only to the one value actually measured — layouts `2`
    // ("Single Field") and `3` ("Mixed Fields") are also interlaced (see
    // `frame_layout_to_field_order` below) but this crate has not measured
    // a real file in either shape, so their height is reported as-is rather
    // than doubled on a guess.
    if frame_layout == Some(1) {
        video.coded_height = video.coded_height.saturating_mul(2);
        video.height = video.height.saturating_mul(2);
    }
    if let Some(r) = descriptor.get_rational(PropertyId::AspectRatio) {
        video.sample_aspect_ratio = display_to_sample_aspect(r, video.width, video.height);
    }
    if let Some(r) = descriptor.get_rational(PropertyId::SampleRate) {
        video.frame_rate = r;
    }
    if let Some(layout) = frame_layout {
        video.field_order = frame_layout_to_field_order(layout);
    }
    if let Some(depth) = descriptor.get_u32(PropertyId::ComponentDepth) {
        // `ComponentDepth` is a raw, attacker-controlled `u32` property; a
        // sample depth of 0 or anything past 64 bits is not a real pixel
        // format and would reach probe output as fabricated metadata — the
        // same class of bug `fuzz/fuzz_targets/registry_discovery.rs` found
        // in JPEG's unchecked `precision`
        // (crash-b105f0b6cfac5b713adef84be6cdd3c1d57599a0). `u8::try_from`
        // alone only rejected values above 255, not the 65..=255 range or 0.
        video.bits_per_raw_sample = u8::try_from(depth).ok().filter(|&b| (1..=64).contains(&b));
    }
    if let Some(coding) = descriptor.get_ul(PropertyId::PictureEssenceCoding) {
        params.codec_id = PICTURE_ESSENCE_CODING
            .iter()
            .find(|&&(ul, _)| ul == coding)
            .map(|&(_, id)| id);
    }
    params
}

/// Convert a display aspect ratio (`AspectRatio`, e.g. `5/4`) into the
/// per-pixel sample aspect ratio `ffprobe` reports, given the frame's pixel
/// dimensions: `sar = dar * height / width`. Measured against `out.mxf`:
/// `AspectRatio = 5/4`, `720x576` → `sar = 1/1`, which is exactly what
/// `ffprobe` prints for that file.
fn display_to_sample_aspect(dar: Rational, width: u32, height: u32) -> Rational {
    if width == 0 || height == 0 || dar.den == 0 {
        return Rational::UNDEFINED;
    }
    // sar = dar * height / width. `Rational::reduce` is `vaco-core`'s own
    // "demuxer reducing a container-supplied 64-bit pair" entry point, so
    // this delegates to already-vetted arithmetic rather than writing a
    // second gcd/overflow story here.
    let num = i64::from(dar.num) * i64::from(height);
    let den = i64::from(dar.den) * i64::from(width);
    Rational::reduce(num, den, i64::from(i32::MAX)).0
}

impl MetadataSet {
    pub(crate) fn get_u32(&self, p: PropertyId) -> Option<u32> {
        crate::localset::u32_be(self.props.get(&p)?)
    }
    pub(crate) fn get_u16(&self, p: PropertyId) -> Option<u16> {
        crate::localset::u16_be(self.props.get(&p)?)
    }
    pub(crate) fn get_u8(&self, p: PropertyId) -> Option<u8> {
        crate::localset::u8_(self.props.get(&p)?)
    }
    pub(crate) fn get_rational(&self, p: PropertyId) -> Option<Rational> {
        crate::localset::rational_be(self.props.get(&p)?)
    }
    pub(crate) fn get_ul(&self, p: PropertyId) -> Option<Ul> {
        self.props.get(&p).and_then(|v| Ul::parse(v))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn descriptor_with(props: Vec<(PropertyId, Vec<u8>)>) -> MetadataSet {
        MetadataSet {
            class: StructuralClass::Descriptor(0x51),
            instance_uid: None,
            props: props.into_iter().collect::<HashMap<_, _>>(),
        }
    }

    #[test]
    fn measured_mpeg2_descriptor_matches_ffprobes_reported_stream() {
        let d = descriptor_with(vec![
            (PropertyId::StoredWidth, 720u32.to_be_bytes().to_vec()),
            (PropertyId::StoredHeight, 576u32.to_be_bytes().to_vec()),
            (PropertyId::DisplayWidth, 720u32.to_be_bytes().to_vec()),
            (PropertyId::DisplayHeight, 576u32.to_be_bytes().to_vec()),
            (PropertyId::AspectRatio, {
                let mut v = 5i32.to_be_bytes().to_vec();
                v.extend_from_slice(&4i32.to_be_bytes());
                v
            }),
            (PropertyId::SampleRate, {
                let mut v = 25i32.to_be_bytes().to_vec();
                v.extend_from_slice(&1i32.to_be_bytes());
                v
            }),
            (PropertyId::FrameLayout, vec![0]),
            (
                PropertyId::PictureEssenceCoding,
                vec![
                    0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x03, 0x04, 0x01, 0x02, 0x02, 0x01,
                    0x01, 0x11, 0x00,
                ],
            ),
        ]);
        let params = picture_parameters(&d);
        assert_eq!(params.codec_id, Some(CodecId::Mpeg2video));
        let v = params.video.unwrap();
        assert_eq!((v.width, v.height), (720, 576));
        assert_eq!(v.sample_aspect_ratio, Rational { num: 1, den: 1 });
        assert_eq!(v.frame_rate, Rational { num: 25, den: 1 });
        assert_eq!(v.field_order, FieldOrder::Progressive);
    }

    #[test]
    fn an_unrecognised_essence_coding_ul_leaves_codec_id_none() {
        let d = descriptor_with(vec![(PropertyId::PictureEssenceCoding, vec![0xFF; 16])]);
        let params = picture_parameters(&d);
        assert_eq!(params.codec_id, None);
    }

    /// `ComponentDepth` is a raw, attacker-controlled `u32` property. Before
    /// this, `u8::try_from(depth).ok()` only rejected values above 255 —
    /// 65..=255 and 0 all passed straight through to `bits_per_raw_sample`.
    /// Same class of bug `fuzz/fuzz_targets/registry_discovery.rs` found in
    /// JPEG's `precision` (crash-b105f0b6cfac5b713adef84be6cdd3c1d57599a0),
    /// audited here rather than found by the fuzzer directly.
    #[test]
    fn an_out_of_range_component_depth_is_not_reported() {
        for depth in [0u32, 65, 255, 4096] {
            let d = descriptor_with(vec![(
                PropertyId::ComponentDepth,
                depth.to_be_bytes().to_vec(),
            )]);
            let params = picture_parameters(&d);
            let v = params.video.unwrap();
            assert_eq!(v.bits_per_raw_sample, None, "depth={depth}");
        }
    }

    #[test]
    fn a_plausible_component_depth_is_reported() {
        let d = descriptor_with(vec![(
            PropertyId::ComponentDepth,
            10u32.to_be_bytes().to_vec(),
        )]);
        let params = picture_parameters(&d);
        let v = params.video.unwrap();
        assert_eq!(v.bits_per_raw_sample, Some(10));
    }
}
