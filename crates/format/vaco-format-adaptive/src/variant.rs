//! Variant/representation selection: HLS's `#EXT-X-STREAM-INF` and DASH's
//! `Representation` are the same idea in two syntaxes.
//!
//! Both name one alternative media presentation among several by a bitrate, a
//! resolution, a codec list and (for HLS's `#EXT-X-MEDIA`/DASH's
//! `AdaptationSet` `@lang`) a language and a group. [`Variant`] is the common
//! shape; [`select_variant`] is `ffmpeg`'s own default rule reproduced from
//! observed behaviour (D17): the highest bandwidth at or under a cap, and
//! among ties the one with the largest resolution.

use vaco_codec_core::CodecId;

/// One `#EXT-X-STREAM-INF` variant or one `Representation`.
#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    /// `BANDWIDTH` (HLS, required) or `@bandwidth` (DASH).
    pub bandwidth: u64,
    /// `AVERAGE-BANDWIDTH`, when the playlist states one separately from the
    /// peak `BANDWIDTH` — DASH has no equivalent second figure.
    pub average_bandwidth: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub frame_rate: Option<f64>,
    /// `CODECS`/`@codecs`, RFC 6381 strings, uninterpreted — the caller maps
    /// these onto [`CodecId`] with whatever parser it has, since this crate
    /// must not depend on one.
    pub codecs: Vec<String>,
    /// The playlist/manifest URI this variant's media playlist or segments
    /// are found at, already resolved against the parent manifest
    /// ([`crate::url::resolve`]).
    pub uri: String,
    /// A stable identifier for CLI selection and for pairing a DASH
    /// `Representation` with its `AdaptationSet`.
    pub id: Option<String>,
}

/// An alternate rendition: HLS's `#EXT-X-MEDIA` or a DASH `AdaptationSet`
/// carrying a role rather than a bitrate ladder rung (a separate audio or
/// subtitle track, not a resolution alternative of the same track).
#[derive(Debug, Clone, PartialEq)]
pub struct Rendition {
    pub kind: RenditionKind,
    /// `GROUP-ID` (HLS) or the adaptation set's own id (DASH).
    pub group_id: String,
    pub name: Option<String>,
    /// BCP 47 language tag.
    pub language: Option<String>,
    pub is_default: bool,
    pub autoselect: bool,
    pub forced: bool,
    /// `URI`, when this rendition is not carried muxed into a variant's own
    /// stream (HLS may omit it for a video-muxed audio track).
    pub uri: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RenditionKind {
    Audio,
    Video,
    Subtitles,
    ClosedCaptions,
}

/// Select one variant the way the reference's default stream selection does:
/// the highest `bandwidth` at or under `max_bandwidth`, and among ties the
/// largest `width * height`. `max_bandwidth = None` means unrestricted, in
/// which case this is simply "the highest-bitrate variant" — `ffmpeg`'s own
/// default with no `-hls_start_program_id`/bitrate option supplied is `input
/// selects the first stream it can decode`, but a caller wanting adaptive
/// behaviour (this crate's actual reason to exist) wants the best one it can
/// afford, which is the rule implemented here and is what a real player does.
///
/// Returns `None` for an empty slice, or when every variant exceeds
/// `max_bandwidth`.
#[must_use]
pub fn select_variant(variants: &[Variant], max_bandwidth: Option<u64>) -> Option<&Variant> {
    variants
        .iter()
        .filter(|v| max_bandwidth.is_none_or(|cap| v.bandwidth <= cap))
        .max_by_key(|v| {
            (
                v.bandwidth,
                u64::from(v.width.unwrap_or(0)) * u64::from(v.height.unwrap_or(0)),
            )
        })
}

/// Whether any of `codecs`' RFC 6381 strings names a codec this build does
/// not merely list but can actually decode-check for, given a lookup from
/// RFC 6381 prefix to [`CodecId`].
///
/// A thin helper rather than a hard dependency on a specific mapping table:
/// HLS and DASH each spell RFC 6381 slightly differently in practice
/// (`avc1.640028` vs `hvc1.1.6.L93.B0`), and the byte-for-byte prefix table is
/// each concrete crate's own job (measured against the reference, D17), not
/// this shared crate's.
#[must_use]
pub fn codec_prefix_matches(codec_string: &str, id: CodecId) -> bool {
    let prefix = match id {
        CodecId::H264 => "avc1",
        CodecId::Hevc => "hvc1",
        CodecId::Aac => "mp4a",
        CodecId::Ac3 => "ac-3",
        CodecId::Eac3 => "ec-3",
        _ => return false,
    };
    codec_string
        .split_once('.')
        .map_or(codec_string, |(p, _)| p)
        == prefix
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn v(bandwidth: u64, w: u32, h: u32) -> Variant {
        Variant {
            bandwidth,
            average_bandwidth: None,
            width: Some(w),
            height: Some(h),
            frame_rate: None,
            codecs: Vec::new(),
            uri: format!("v{bandwidth}.m3u8"),
            id: None,
        }
    }

    #[test]
    fn selects_the_highest_bandwidth_under_the_cap() {
        let vs = [
            v(500_000, 640, 360),
            v(1_500_000, 1280, 720),
            v(3_000_000, 1920, 1080),
        ];
        let picked = select_variant(&vs, Some(2_000_000)).unwrap();
        assert_eq!(picked.bandwidth, 1_500_000);
    }

    #[test]
    fn selects_the_highest_overall_with_no_cap() {
        let vs = [v(500_000, 640, 360), v(3_000_000, 1920, 1080)];
        let picked = select_variant(&vs, None).unwrap();
        assert_eq!(picked.bandwidth, 3_000_000);
    }

    #[test]
    fn ties_break_on_resolution() {
        let vs = [v(1_000_000, 640, 360), v(1_000_000, 1280, 720)];
        let picked = select_variant(&vs, None).unwrap();
        assert_eq!((picked.width, picked.height), (Some(1280), Some(720)));
    }

    #[test]
    fn nothing_fits_under_an_impossible_cap() {
        let vs = [v(500_000, 640, 360)];
        assert!(select_variant(&vs, Some(100)).is_none());
    }

    #[test]
    fn empty_has_no_selection() {
        assert!(select_variant(&[], None).is_none());
    }

    #[test]
    fn rfc6381_prefixes_match_the_common_families() {
        assert!(codec_prefix_matches("avc1.640028", CodecId::H264));
        assert!(codec_prefix_matches("mp4a.40.2", CodecId::Aac));
        assert!(!codec_prefix_matches("hvc1.1.6.L93.B0", CodecId::H264));
    }
}
