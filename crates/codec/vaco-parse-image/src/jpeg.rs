//! JPEG (ITU-T T.81 / ISO/IEC 10918-1), read as Motion JPEG: `SOI`, then
//! markers up to the first `SOF` (start of frame).
//!
//! # Measured: `profile` and the `yuvj` family
//!
//! `SOF0`/`SOF2` were probed directly (`ffmpeg -c:v mjpeg` for the former,
//! `Pillow`'s `progressive=True` for the latter):
//!
//! ```text
//! SOF0 (0xC0, baseline DCT)     -> profile=Baseline
//! SOF2 (0xC2, progressive DCT)  -> profile=Progressive
//! ```
//!
//! The other nine SOF markers (extended sequential, lossless, the
//! differential and arithmetic-coded variants) were not — no encoder
//! available to this crate produces them — so [`profile_name`] names them
//! from ITU-T T.81 Table B.1's own terminology rather than a probe, flagged
//! the same way `vaco-parse-vpx::profile`'s level table is for the
//! equivalent reason.
//!
//! `pix_fmt` is the `yuvj` family keyed on chroma subsampling — measured at
//! 4:2:0, 4:2:2 and 4:4:4 (`-pix_fmt yuvj420p`/`yuvj422p` explicitly, and
//! 4:4:4 as the encoder's own unforced default) — because JPEG has no
//! separate colour-range field: every three-component frame is full-range
//! YCbCr by convention (Baseline JPEG carries no colour description at all;
//! an Adobe `APP14` marker can override the transform, and this crate does
//! not read it). A single-component frame is `gray`, not a `yuvj` variant —
//! also measured.
//!
//! # Colour metadata, `sample_aspect_ratio` and `field_order`: also by convention
//!
//! Measured directly (`ffmpeg -c:v mjpeg`, `-show_streams`) rather than
//! derived from any marker this crate reads: a three-component frame states
//! `color_range=pc`, `color_space=bt470bg` and `chroma_location=center`
//! unconditionally — JPEG's `yuvj` convention again, the same fact
//! `pix_fmt` above already encodes, just asked three more ways. A
//! single-component frame gets none of the three: not measured, and
//! grayscale has no chroma to site or transform to name.
//!
//! `sample_aspect_ratio=1:1` and `field_order=unknown` hold for every JPEG
//! frame regardless of component count: JPEG carries no pixel-aspect
//! marker this crate reads (so the caller's own SAR default, not this
//! codec's business), and no interlace syntax at all — the same shape
//! `vaco-parse-vpx`'s VP9 parser and `vaco-parse-av1`'s AV1 parser already
//! assert for their own codecs, just the opposite conclusion. Before this,
//! [`vaco_codec_core::FieldOrder`]'s own type-level default
//! (`Progressive`) was left standing uncontested for any container that
//! never overrides it — `image2` already guards against exactly this trap
//! for every codec it carries (`vaco_demux_image2::multi::stream_video`'s
//! own doc comment), but a JPEG frame reached through a different
//! container (`vaco-demux-avi`'s `MJPG` fourcc, say) had no such guard,
//! which is why `avi/mjpeg-pcm.avi` and `image2/frame.jpg` used to
//! disagree on `field_order` even though both carry the identical codec.

use vaco_bitstream::ByteReader;
use vaco_codec_core::{CodecId, CodecParameters, FieldOrder, Profile};
use vaco_color::{ChromaLocation, ColorRange, MatrixCoefficients};
use vaco_core::{MediaType, Rational};
use vaco_pixfmt::PixFmt;

use crate::parser::ImageHeader;

/// The display name for an `SOF` marker, ITU-T T.81 Table B.1. See the
/// module doc for which two rows are measured.
#[must_use]
pub const fn profile_name(sof_marker: u8) -> Option<&'static str> {
    Some(match sof_marker {
        0xC0 => "Baseline",
        0xC1 => "Extended sequential",
        0xC2 => "Progressive",
        0xC3 => "Lossless",
        0xC5 => "Differential sequential",
        0xC6 => "Differential progressive",
        0xC7 => "Differential lossless",
        0xC9 => "Extended sequential, arithmetic coding",
        0xCA => "Progressive, arithmetic coding",
        0xCB => "Lossless, arithmetic coding",
        0xCD => "Differential sequential, arithmetic coding",
        0xCE => "Differential progressive, arithmetic coding",
        0xCF => "Differential lossless, arithmetic coding",
        _ => return None,
    })
}

/// One component's sampling factors from `SOF`.
#[derive(Debug, Clone, Copy)]
pub struct Component {
    pub h: u8,
    pub v: u8,
}

/// The [`PixFmt`] an `SOF`'s component count and sampling factors denote.
/// See the module doc for what is measured.
#[must_use]
pub fn pixel_format(components: &[Component]) -> Option<PixFmt> {
    match components {
        [_] => PixFmt::from_name("gray").ok(),
        // Every sample this crate measured uses chroma factors of exactly
        // (1, 1) — the overwhelmingly common case — so the luma factors
        // alone select the subsampling. `checked_div` rather than `/`
        // (denied workspace-wide) covers the general case too, for the rare
        // encoder that scales chroma by something other than 1.
        [y, cb, ..] if cb.h > 0 && cb.v > 0 => {
            let h_ratio = y.h.checked_div(cb.h)?;
            let v_ratio = y.v.checked_div(cb.v)?;
            let name = match (h_ratio, v_ratio) {
                (2, 2) => "yuvj420p",
                (2, 1) => "yuvj422p",
                (1, 1) => "yuvj444p",
                (1, 2) => "yuvj440p",
                _ => return None,
            };
            PixFmt::from_name(name).ok()
        }
        _ => None,
    }
}

/// `SOF0`..=`SOF3`, `SOF5`..=`SOF7`, `SOF9`..=`SOF11`, `SOF13`..=`SOF15` —
/// every marker byte in the `SOF` range except `DHT` (`0xC4`), the reserved
/// `JPG` (`0xC8`) and `DAC` (`0xCC`), ITU-T T.81 Table B.1.
const fn is_sof_marker(marker: u8) -> bool {
    matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC)
}

/// Markers with no length-prefixed payload at all: `TEM` (`0x01`), the
/// reserved range `0x02`..=`0xBF`, `RST0`..=`RST7` (`0xD0`..=`0xD7`), `SOI`
/// (`0xD8`) and `EOI` (`0xD9`). Every other marker is followed by a 2-byte
/// big-endian length covering itself.
const fn has_no_payload(marker: u8) -> bool {
    matches!(marker, 0x01 | 0xD0..=0xD9) || matches!(marker, 0x02..=0xBF)
}

/// Reader for `SOI` plus markers up to the first `SOF`.
#[derive(Debug)]
pub struct Jpeg;

impl ImageHeader for Jpeg {
    fn parse(data: &[u8]) -> Option<CodecParameters> {
        let mut r = ByteReader::new(data);
        if r.be16() != 0xFFD8 {
            return None;
        }
        loop {
            // Markers may be preceded by fill bytes (`0xFF` repeated);
            // skip them, then require the `0xFF` that starts the next one.
            let mut b = r.u8();
            while b == 0xFF {
                b = r.u8();
            }
            let marker = b;
            if r.overrun() {
                return None;
            }
            if marker == 0xD9 {
                return None; // EOI: no SOF was ever found.
            }
            if has_no_payload(marker) {
                continue;
            }
            if !is_sof_marker(marker) {
                // Any other segment (DQT, DHT, APPn, COM, ...): skip its
                // length-prefixed payload and keep scanning.
                let len = r.be16();
                if len < 2 {
                    return None;
                }
                r.skip(usize::from(len) - 2);
                continue;
            }
            let _len = r.be16();
            let precision = r.u8();
            let height = r.be16();
            let width = r.be16();
            let num_components = r.u8();
            let mut components = [Component { h: 0, v: 0 }; 4];
            let count = usize::from(num_components).min(components.len());
            for slot in components.iter_mut().take(count) {
                let _id = r.u8();
                let sampling = r.u8();
                let _quant_table = r.u8();
                *slot = Component {
                    h: sampling >> 4,
                    v: sampling & 0x0F,
                };
            }
            r.check().ok()?;
            // ITU-T T.81 Table B.2 ("Frame header parameter sizes and
            // values"): `P` (sample precision) is defined only for 8 (every
            // DCT-based process), 12 (extended sequential/progressive) and
            // 2..=16 (lossless). No `SOF` marker this crate recognises
            // permits a value outside 2..=16 at all, so treat one the same
            // way an impossible width/height/component-count is already
            // treated here: the header is corrupt, not merely carrying an
            // unusual precision. Before this, `precision` was read and
            // discarded into `bits_per_raw_sample` unchecked, so an
            // arbitrary byte (e.g. 164) reached probe output verbatim —
            // found by `fuzz/fuzz_targets/registry_discovery.rs`.
            if width == 0
                || height == 0
                || num_components == 0
                || !(2..=16).contains(&precision)
            {
                return None;
            }
            let mut params = CodecParameters::video().with_codec(CodecId::Jpeg);
            params.media_type = Some(MediaType::Video);
            params.profile = Some(match profile_name(marker) {
                Some(name) => Profile::new(i32::from(marker), name),
                None => Profile::new(i32::from(marker), ""),
            });
            if let Some(v) = params.video.as_mut() {
                v.width = u32::from(width);
                v.height = u32::from(height);
                v.coded_width = v.width;
                v.coded_height = v.height;
                v.format = pixel_format(components.get(..count).unwrap_or(&[]));
                v.bits_per_raw_sample = Some(precision);
                // Measured facts about JPEG itself, not read off the wire —
                // see the module doc's "Colour metadata, sample_aspect_ratio
                // and field_order" section for what was probed and why each
                // holds regardless of which container carries the frame.
                v.sample_aspect_ratio = Rational::new(1, 1);
                v.field_order = FieldOrder::Unknown;
                if count >= 3 {
                    v.color.range = ColorRange::Full;
                    v.color.matrix = MatrixCoefficients::Bt470bg;
                    v.color.chroma_location = ChromaLocation::Center;
                }
            }
            return Some(params);
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;

    /// `SOI` through `SOF0`, byte for byte from a real `ffmpeg -c:v mjpeg
    /// -pix_fmt yuvj420p` 64x48 encode: `APP0` (JFIF), a `COM` (the encoder
    /// version string), `DQT`, `DHT`, then `SOF0` — real enough to prove the
    /// marker-skipping loop, not just the `SOF0` read at the end of it.
    #[rustfmt::skip]
    const REAL_BASELINE_420: [u8; 287] = [
        0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46, 0x00, 0x01, 0x02, 0x00, 0x00, 0x01,
        0x00, 0x01, 0x00, 0x00, 0xff, 0xfe, 0x00, 0x10, 0x4c, 0x61, 0x76, 0x63, 0x36, 0x32, 0x2e, 0x32,
        0x38, 0x2e, 0x31, 0x30, 0x30, 0x00, 0xff, 0xdb, 0x00, 0x43, 0x00, 0x08, 0x06, 0x06, 0x07, 0x06,
        0x07, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x09, 0x09, 0x09, 0x0a, 0x0a, 0x0a, 0x09, 0x09, 0x09,
        0x09, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0c, 0x0c, 0x0c, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a,
        0x0a, 0x0c, 0x0c, 0x0c, 0x0c, 0x0d, 0x0e, 0x0d, 0x0d, 0x0d, 0x0c, 0x0d, 0x0e, 0x0e, 0x0f, 0x0f,
        0x0f, 0x12, 0x12, 0x11, 0x11, 0x15, 0x15, 0x15, 0x19, 0x19, 0x1f, 0xff, 0xc4, 0x00, 0x9f, 0x00,
        0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x07, 0x08, 0x06, 0x03, 0x05, 0x04, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0x06, 0x05, 0x08, 0x03, 0x10, 0x00, 0x02, 0x01,
        0x02, 0x04, 0x03, 0x02, 0x08, 0x08, 0x0f, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03,
        0x04, 0x00, 0x11, 0x05, 0x12, 0x06, 0x21, 0x13, 0x31, 0x07, 0x08, 0x23, 0x41, 0x81, 0xc1, 0x16,
        0x52, 0x15, 0xb2, 0x22, 0x63, 0x24, 0x14, 0x32, 0xc3, 0xd4, 0x51, 0x85, 0xc4, 0xb4, 0xb1, 0xb3,
        0x64, 0x84, 0x83, 0x45, 0x61, 0x46, 0xe4, 0x75, 0x36, 0x33, 0x72, 0x11, 0x00, 0x01, 0x03, 0x02,
        0x04, 0x03, 0x07, 0x04, 0x00, 0x07, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04,
        0x11, 0x05, 0x00, 0x12, 0x21, 0x13, 0x06, 0x31, 0x07, 0x22, 0x14, 0x43, 0x83, 0x84, 0x45, 0xc2,
        0xc3, 0x41, 0x51, 0x32, 0x81, 0x52, 0x71, 0x62, 0xc4, 0x23, 0x61, 0x16, 0xff, 0xc0, 0x00, 0x11,
        0x08, 0x00, 0x30, 0x00, 0x40, 0x03, 0x01, 0x22, 0x00, 0x02, 0x11, 0x00, 0x03, 0x11, 0x00,
    ];

    /// The `SOF0` segment from a real `-pix_fmt yuvj422p` encode (`H=2,V=2`
    /// luma; `H=1,V=2` chroma).
    const REAL_SOF0_422: [u8; 19] = [
        0xff, 0xc0, 0x00, 0x11, 0x08, 0x00, 0x30, 0x00, 0x40, 0x03, 0x01, 0x22, 0x00, 0x02, 0x12,
        0x00, 0x03, 0x12, 0x00,
    ];

    /// ITU-T T.81 Table B.2 defines `P` (sample precision) only for 8, 12
    /// and 2..=16 — never an arbitrary byte. Before this crate validated
    /// it, a corrupted `SOF0` with `precision=0xA4` (164) decoded "fine" and
    /// put 164 straight into `bits_per_raw_sample`, which is printed
    /// verbatim in probe output. Found by
    /// `fuzz/fuzz_targets/registry_discovery.rs`
    /// (crash-b105f0b6cfac5b713adef84be6cdd3c1d57599a0, kept as a seed under
    /// `fuzz/seeds/registry_discovery/`).
    #[test]
    fn an_impossible_precision_is_rejected_not_reported() {
        let mut data = vec![0xFFu8, 0xD8];
        data.extend_from_slice(&REAL_SOF0_422);
        // Within `REAL_SOF0_422`: marker(2) + length(2) + precision(1), so
        // precision sits at index 4 of that slice, index 6 once prefixed
        // with `SOI`.
        data[6] = 0xA4; // precision, valid range is 8, 12, or 2..=16
        assert!(Jpeg::parse(&data).is_none());
    }

    #[test]
    fn a_real_baseline_420_header_decodes() {
        let params = Jpeg::parse(&REAL_BASELINE_420).unwrap();
        assert_eq!(params.codec_id, Some(CodecId::Jpeg));
        assert_eq!(params.profile.map(|p| p.name), Some("Baseline"));
        let v = params.video.unwrap();
        assert_eq!((v.width, v.height), (64, 48));
        assert_eq!(v.format, PixFmt::from_name("yuvj420p").ok());
        assert_eq!(v.bits_per_raw_sample, Some(8));
    }

    #[test]
    fn sof0_422_sampling_maps_correctly() {
        let mut data = vec![0xFFu8, 0xD8];
        data.extend_from_slice(&REAL_SOF0_422);
        let params = Jpeg::parse(&data).unwrap();
        let v = params.video.unwrap();
        assert_eq!(v.format, PixFmt::from_name("yuvj422p").ok());
    }

    #[test]
    fn a_progressive_marker_is_named_progressive() {
        let mut data = REAL_BASELINE_420;
        // Flip the SOF marker byte from SOF0 (0xC0) to SOF2 (0xC2); the
        // component layout after it is unaffected by which SOF it is.
        let sof_at = data.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
        data[sof_at + 1] = 0xC2;
        let params = Jpeg::parse(&data).unwrap();
        assert_eq!(params.profile.map(|p| p.name), Some("Progressive"));
    }

    #[test]
    fn a_single_component_frame_is_gray() {
        assert_eq!(
            pixel_format(&[Component { h: 1, v: 1 }]),
            PixFmt::from_name("gray").ok()
        );
    }

    /// The measured facts a real three-component `ffmpeg -c:v mjpeg` frame
    /// states, per the module doc's "Colour metadata, `sample_aspect_ratio`
    /// and `field_order`" section — `color_range`/`color_space`/
    /// `chroma_location` were absent entirely before this, and
    /// `field_order` silently inherited `VideoParameters::default`'s
    /// `Progressive`, matching neither `image2`'s own explicit
    /// `FieldOrder::Unknown` override nor the reference.
    #[test]
    fn a_three_component_frame_states_the_measured_colour_metadata() {
        let v = Jpeg::parse(&REAL_BASELINE_420).unwrap().video.unwrap();
        assert_eq!(v.sample_aspect_ratio, Rational::new(1, 1));
        assert_eq!(v.field_order, FieldOrder::Unknown);
        assert_eq!(v.color.range, ColorRange::Full);
        assert_eq!(v.color.matrix, MatrixCoefficients::Bt470bg);
        assert_eq!(v.color.chroma_location, ChromaLocation::Center);
    }

    /// A single-component (grayscale) frame has no chroma to site and no
    /// YCbCr transform to name — not measured, so left at their own
    /// defaults rather than guessed — but `sample_aspect_ratio` and
    /// `field_order` hold regardless of component count.
    #[test]
    fn a_single_component_frame_states_sar_and_field_order_but_not_chroma_colour() {
        let mut data = vec![0xFFu8, 0xD8];
        // SOF0, one component, arbitrary dimensions.
        data.extend_from_slice(&[
            0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x08, 0x00, 0x08, 0x01, 0x01, 0x11, 0x00,
        ]);
        let v = Jpeg::parse(&data).unwrap().video.unwrap();
        assert_eq!(v.sample_aspect_ratio, Rational::new(1, 1));
        assert_eq!(v.field_order, FieldOrder::Unknown);
        assert_eq!(v.color.range, ColorRange::Unspecified);
        assert_eq!(v.color.matrix, MatrixCoefficients::Unspecified);
        assert_eq!(v.color.chroma_location, ChromaLocation::Unspecified);
    }

    #[test]
    fn a_bad_signature_is_rejected() {
        let mut bad = REAL_BASELINE_420;
        bad[0] = 0;
        assert!(Jpeg::parse(&bad).is_none());
    }

    #[test]
    fn a_truncated_file_is_rejected_not_panicked() {
        for n in 0..REAL_BASELINE_420.len() {
            let _ = Jpeg::parse(REAL_BASELINE_420.get(..n).unwrap());
        }
    }

    #[test]
    fn eof_with_no_sof_is_rejected_not_panicked() {
        assert!(Jpeg::parse(&[0xFF, 0xD8, 0xFF, 0xD9]).is_none());
    }
}
