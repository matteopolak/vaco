//! The client `Manifest` — an XML document describing every `StreamIndex`
//! (one per elementary stream) and, inside each, the `QualityLevel`s and the
//! `c` (chunk) list a client sums to find each fragment's nominal position.
//!
//! Every attribute name, ordering and the exact absence of a `t` (start
//! time) attribute on `<c>` elements below is measured against real
//! `ffmpeg -f smoothstreaming` output (`provenance/sources.toml`,
//! `ffmpeg-smoothstreaming-mux-probe`), not the MS-SSTR text alone: a
//! conformant client is specified to derive `{start time}` in the `Url`
//! template by summing preceding `d` values starting from `t=0`, but the
//! reference's own fragment *filenames* are the track's true encoder-timeline
//! absolute time — for a stream whose first sample does not start at pts 0,
//! those two numbers disagree, in the reference itself, by that startup
//! offset. This module reproduces the reference's own convention (`c`
//! carries only `d`) rather than inventing a `t` the reference never writes;
//! see `docs/format/vaco-mux-smoothstreaming.md` for the tracked
//! consequence.

use std::fmt::Write as _;

/// One `StreamIndex`'s worth of `Manifest` data.
#[derive(Debug, Clone)]
pub struct ManifestStream {
    pub kind: StreamKind,
    pub bitrate: u64,
    pub codec_private_data_hex: String,
    /// One entry per fragment already flushed, in order: (`n` is the index).
    pub chunk_durations_hns: Vec<u64>,
}

#[derive(Debug, Clone)]
pub enum StreamKind {
    Video {
        max_width: u32,
        max_height: u32,
    },
    Audio {
        sampling_rate: u32,
        channels: u32,
    },
}

impl StreamKind {
    const fn type_attr(&self) -> &'static str {
        match self {
            Self::Video { .. } => "video",
            Self::Audio { .. } => "audio",
        }
    }

    const fn fourcc(&self) -> &'static str {
        match self {
            // The only two codecs this muxer supports; see `lib.rs`.
            Self::Video { .. } => "H264",
            Self::Audio { .. } => "AACL",
        }
    }

    const fn url_track_kind(&self) -> &'static str {
        match self {
            Self::Video { .. } => "video",
            Self::Audio { .. } => "audio",
        }
    }
}

/// Render the full `Manifest` XML for `streams`, in the order given —
/// measured: video `StreamIndex` elements precede audio ones when both are
/// present, which falls out naturally from a caller that adds the video
/// stream first (this crate does not reorder).
#[must_use]
pub fn build_manifest(streams: &[ManifestStream]) -> String {
    let total_duration_hns: u64 = streams
        .iter()
        .map(|s| s.chunk_durations_hns.iter().sum::<u64>())
        .max()
        .unwrap_or(0);

    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    let _ = writeln!(
        out,
        "<SmoothStreamingMedia MajorVersion=\"2\" MinorVersion=\"0\" Duration=\"{total_duration_hns}\">"
    );

    for stream in streams {
        let chunks = stream.chunk_durations_hns.len();
        let _ = writeln!(
            out,
            "<StreamIndex Type=\"{}\" QualityLevels=\"1\" Chunks=\"{chunks}\" Url=\"QualityLevels({{bitrate}})/Fragments({}={{start time}})\">",
            stream.kind.type_attr(),
            stream.kind.url_track_kind(),
        );
        match &stream.kind {
            StreamKind::Video {
                max_width,
                max_height,
            } => {
                let _ = writeln!(
                    out,
                    "<QualityLevel Index=\"0\" Bitrate=\"{}\" FourCC=\"{}\" MaxWidth=\"{max_width}\" MaxHeight=\"{max_height}\" CodecPrivateData=\"{}\" />",
                    stream.bitrate,
                    stream.kind.fourcc(),
                    stream.codec_private_data_hex,
                );
            }
            StreamKind::Audio {
                sampling_rate,
                channels,
            } => {
                let _ = writeln!(
                    out,
                    "<QualityLevel Index=\"0\" Bitrate=\"{}\" FourCC=\"{}\" SamplingRate=\"{sampling_rate}\" Channels=\"{channels}\" BitsPerSample=\"16\" PacketSize=\"4\" AudioTag=\"255\" CodecPrivateData=\"{}\" />",
                    stream.bitrate,
                    stream.kind.fourcc(),
                    stream.codec_private_data_hex,
                );
            }
        }
        for (n, d) in stream.chunk_durations_hns.iter().enumerate() {
            let _ = writeln!(out, "<c n=\"{n}\" d=\"{d}\" />");
        }
        out.push_str("</StreamIndex>\n");
    }

    out.push_str("</SmoothStreamingMedia>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Byte-for-byte against `mss-samples/out.ism/Manifest` (single fragment
    /// per track), modulo the trailing-newline convention this module always
    /// uses (the reference fixture was captured without checking that byte).
    #[test]
    fn matches_the_single_fragment_reference_shape() {
        let streams = vec![
            ManifestStream {
                kind: StreamKind::Video {
                    max_width: 320,
                    max_height: 240,
                },
                bitrate: 60317,
                codec_private_data_hex:
                    "0000000167f4000d919b28283f6022000003000200000300641e28532c0000000168ebe3c44844"
                        .to_owned(),
                chunk_durations_hns: vec![30_000_000],
            },
            ManifestStream {
                kind: StreamKind::Audio {
                    sampling_rate: 48_000,
                    channels: 1,
                },
                bitrate: 69000,
                codec_private_data_hex: "118856e500".to_owned(),
                chunk_durations_hns: vec![30_213_333],
            },
        ];
        let xml = build_manifest(&streams);
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n"));
        assert!(xml.contains("<SmoothStreamingMedia MajorVersion=\"2\" MinorVersion=\"0\" Duration=\"30213333\">"));
        assert!(xml.contains(
            "<StreamIndex Type=\"video\" QualityLevels=\"1\" Chunks=\"1\" Url=\"QualityLevels({bitrate})/Fragments(video={start time})\">"
        ));
        assert!(xml.contains(
            "<QualityLevel Index=\"0\" Bitrate=\"60317\" FourCC=\"H264\" MaxWidth=\"320\" MaxHeight=\"240\" CodecPrivateData=\"0000000167f4000d919b28283f6022000003000200000300641e28532c0000000168ebe3c44844\" />"
        ));
        assert!(xml.contains("<c n=\"0\" d=\"30000000\" />"));
        assert!(xml.contains(
            "<QualityLevel Index=\"0\" Bitrate=\"69000\" FourCC=\"AACL\" SamplingRate=\"48000\" Channels=\"1\" BitsPerSample=\"16\" PacketSize=\"4\" AudioTag=\"255\" CodecPrivateData=\"118856e500\" />"
        ));
        assert!(xml.contains("<c n=\"0\" d=\"30213333\" />"));
        assert!(xml.trim_end().ends_with("</SmoothStreamingMedia>"));
    }

    #[test]
    fn three_chunks_get_three_c_elements_in_order() {
        let streams = vec![ManifestStream {
            kind: StreamKind::Video {
                max_width: 320,
                max_height: 240,
            },
            bitrate: 59793,
            codec_private_data_hex: String::new(),
            chunk_durations_hns: vec![50_000_000, 50_000_000, 20_000_000],
        }];
        let xml = build_manifest(&streams);
        assert!(xml.contains("Chunks=\"3\""));
        assert!(xml.contains("<c n=\"0\" d=\"50000000\" />"));
        assert!(xml.contains("<c n=\"1\" d=\"50000000\" />"));
        assert!(xml.contains("<c n=\"2\" d=\"20000000\" />"));
        assert!(xml.contains("Duration=\"120000000\""));
    }
}
