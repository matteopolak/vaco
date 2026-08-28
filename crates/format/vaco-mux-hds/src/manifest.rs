//! The `index.f4m` client manifest.
//!
//! Shape measured against `hds-samples/out12.f4m/index.f4m` and its
//! two-quality-level sibling (`out_multi.f4m/index.f4m`):
//! `manifest`/`id`/`streamType`/`deliveryType`/`duration`, then, per
//! quality level, one `bootstrapInfo` naming its `.abst` file and one
//! `media` element (carrying the base64 `onMetaData` blob) naming the
//! quality's own `stream<N>` bitrate URL — `<bootstrapInfo>` always
//! immediately precedes its matching `<media>`, in quality-level order.

use crate::amf0;
use crate::base64;

/// One quality level's worth of `Manifest` data.
#[derive(Debug, Clone)]
pub struct ManifestLevel {
    pub index: u32,
    /// Combined video+audio bitrate, in decimal kbit/s, rounded — the
    /// `bitrate` attribute's own unit, measured to be `/1000` rather than
    /// the `onMetaData` blob's `/1024` (see `amf0.rs`).
    pub bitrate_kbps: u64,
    pub metadata: amf0::OnMetaData,
}

/// Render the full `index.f4m` XML for `levels`, plus the total duration in
/// seconds (the longest quality level's own accumulated media time).
#[must_use]
pub fn build_manifest(id: &str, total_duration_secs: f64, levels: &[ManifestLevel]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    out.push_str("<manifest xmlns=\"http://ns.adobe.com/f4m/1.0\">\n");
    let _ = writeln!(out, "\t<id>{id}</id>");
    out.push_str("\t<streamType>recorded</streamType>\n");
    out.push_str("\t<deliveryType>streaming</deliveryType>\n");
    let _ = writeln!(out, "\t<duration>{total_duration_secs:.6}</duration>");
    for level in levels {
        let _ = writeln!(
            out,
            "\t<bootstrapInfo profile=\"named\" url=\"stream{}.abst\" id=\"bootstrap{}\" />",
            level.index, level.index
        );
        let meta_b64 = base64::encode(&amf0::encode_on_metadata(&level.metadata));
        let _ = writeln!(
            out,
            "\t<media bitrate=\"{}\" url=\"stream{}\" bootstrapInfoId=\"bootstrap{}\">",
            level.bitrate_kbps, level.index, level.index
        );
        let _ = writeln!(out, "\t\t<metadata>{meta_b64}</metadata>");
        out.push_str("\t</media>\n");
    }
    out.push_str("</manifest>\n");
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    fn level(index: u32, bitrate_kbps: u64) -> ManifestLevel {
        ManifestLevel {
            index,
            bitrate_kbps,
            metadata: amf0::OnMetaData {
                width: 320.0,
                height: 240.0,
                video_datarate_kibit: 390.625,
                video_codec_id: 7.0,
                audio_datarate_kibit: 67.382_812_5,
                audio_sample_rate: 48_000.0,
                audio_sample_size: 16.0,
                audio_codec_id: 10.0,
            },
        }
    }

    #[test]
    fn matches_the_single_level_reference_shape() {
        let xml = build_manifest("out12.f4m", 12.069, &[level(0, 469)]);
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n"));
        assert!(xml.contains("<manifest xmlns=\"http://ns.adobe.com/f4m/1.0\">"));
        assert!(xml.contains("<id>out12.f4m</id>"));
        assert!(xml.contains("<streamType>recorded</streamType>"));
        assert!(xml.contains("<deliveryType>streaming</deliveryType>"));
        assert!(xml.contains("<duration>12.069000</duration>"));
        assert!(xml.contains("<bootstrapInfo profile=\"named\" url=\"stream0.abst\" id=\"bootstrap0\" />"));
        assert!(xml.contains("<media bitrate=\"469\" url=\"stream0\" bootstrapInfoId=\"bootstrap0\">"));
        assert!(xml.trim_end().ends_with("</manifest>"));
    }

    #[test]
    fn two_levels_get_two_bootstrap_media_pairs_in_order() {
        let xml = build_manifest("out_multi.f4m", 6.075, &[level(0, 469), level(1, 240)]);
        let first_bootstrap = xml.find("bootstrap0").unwrap();
        let second_bootstrap = xml.find("bootstrap1").unwrap();
        assert!(first_bootstrap < second_bootstrap);
        assert!(xml.contains("url=\"stream1\""));
        assert!(xml.contains("bitrate=\"240\""));
    }
}
