//! Media playlist parsing: the segment list itself (RFC 8216 §4.3.3, §4.4.4).
//!
//! One pass over the lines, carrying forward whatever "applies to every
//! following segment until changed" state RFC 8216 defines: the current
//! `#EXT-X-MAP` (§4.4.4.5), the current `#EXT-X-KEY` (§4.4.4.4), whether a
//! `#EXT-X-DISCONTINUITY` (§4.4.4.3) preceded the next segment, and the byte
//! offset an omitted `#EXT-X-BYTERANGE` offset continues from (§4.4.4.2).

use vaco_core::{Duration, Error, Result};
use vaco_format_adaptive::{ByteRange, WallClock, walltime::parse_iso8601_datetime};

use crate::attrs::{get, parse_attribute_list};
use crate::key::KeyInfo;

/// `#EXT-X-PLAYLIST-TYPE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaylistType {
    Vod,
    Event,
}

/// One `#EXT-X-MAP`: an initialization segment a following run of fMP4 media
/// segments needs read first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapInfo {
    pub uri: String,
    pub byte_range: Option<ByteRange>,
}

/// One media segment: an `#EXTINF` duration, a URI, and whatever tags applied
/// to it.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaSegment {
    pub uri: String,
    pub duration: Duration,
    pub title: String,
    pub byte_range: Option<ByteRange>,
    /// A `#EXT-X-DISCONTINUITY` immediately preceded this segment: the
    /// timeline resets here (RFC 8216 §4.4.4.3) — a new encoding profile, a
    /// different timestamp origin, or a genuine ad break.
    pub discontinuity: bool,
    pub program_date_time: Option<WallClock>,
    /// Index into [`MediaPlaylist::maps`], when an `#EXT-X-MAP` applies.
    pub map: Option<usize>,
    /// Index into [`MediaPlaylist::keys`], when an `#EXT-X-KEY` other than
    /// `METHOD=NONE` applies.
    pub key: Option<usize>,
    /// `#EXT-X-MEDIA-SEQUENCE` plus this segment's position — the number a
    /// live playlist reload correlates segments by.
    pub media_sequence: u64,
}

/// A parsed media playlist.
#[derive(Debug, Clone, Default)]
pub struct MediaPlaylist {
    pub target_duration: Duration,
    pub media_sequence: u64,
    pub discontinuity_sequence: u64,
    pub playlist_type: Option<PlaylistType>,
    /// `#EXT-X-ENDLIST`: VOD-shaped even without `#EXT-X-PLAYLIST-TYPE:VOD` —
    /// RFC 8216 §4.3.3.4 allows either to state completeness, and a caller
    /// asking "is this stream finished" wants this flag, not the type.
    pub end_list: bool,
    pub independent_segments: bool,
    pub segments: Vec<MediaSegment>,
    pub maps: Vec<MapInfo>,
    pub keys: Vec<KeyInfo>,
}

impl MediaPlaylist {
    /// Whether this is a live (still-growing) playlist — the distinction the
    /// brief calls out by name.
    #[must_use]
    pub const fn is_live(&self) -> bool {
        !self.end_list
    }
}

/// Parse `text`, a media playlist already read into memory.
///
/// # Errors
/// [`Error::InvalidData`] with no `#EXTM3U` header, or an `#EXTINF` with no
/// following URI line.
pub fn parse(text: &str, base_url: &str) -> Result<MediaPlaylist> {
    let mut lines = text.lines().map(str::trim);
    match lines.next() {
        Some(first) if first.starts_with("#EXTM3U") => {}
        _ => return Err(Error::InvalidData("HLS playlist has no #EXTM3U header")),
    }

    let mut out = MediaPlaylist::default();
    // RFC 8216 §4.3.3.2/§4.4.4.3: both default to 0 when the tag is absent.
    let mut media_sequence = 0u64;
    let mut discontinuity_sequence = 0u64;

    let mut pending_duration: Option<(Duration, String)> = None;
    let mut pending_byterange: Option<ByteRange> = None;
    let mut pending_discontinuity = false;
    let mut pending_pdt: Option<WallClock> = None;
    let mut current_map: Option<usize> = None;
    let mut current_key: Option<usize> = None;
    let mut last_byterange_end = 0u64;

    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            let secs: u64 = rest.trim().parse().unwrap_or(0);
            out.target_duration = Duration::from_micros(
                i64::try_from(secs.saturating_mul(1_000_000)).unwrap_or(i64::MAX),
            );
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            media_sequence = rest.trim().parse().unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("#EXT-X-DISCONTINUITY-SEQUENCE:") {
            discontinuity_sequence = rest.trim().parse().unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("#EXT-X-PLAYLIST-TYPE:") {
            out.playlist_type = match rest.trim() {
                "VOD" => Some(PlaylistType::Vod),
                "EVENT" => Some(PlaylistType::Event),
                _ => None,
            };
        } else if line.starts_with("#EXT-X-ENDLIST") {
            out.end_list = true;
        } else if line.starts_with("#EXT-X-INDEPENDENT-SEGMENTS") {
            out.independent_segments = true;
        } else if line.starts_with("#EXT-X-DISCONTINUITY") {
            pending_discontinuity = true;
        } else if let Some(rest) = line.strip_prefix("#EXT-X-PROGRAM-DATE-TIME:") {
            pending_pdt = parse_iso8601_datetime(rest.trim());
        } else if let Some(rest) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            if let Some(r) = ByteRange::parse_hls(rest.trim(), last_byterange_end) {
                pending_byterange = Some(r);
            }
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            let attrs = parse_attribute_list(rest);
            let Some(uri) = get(&attrs, "URI") else {
                continue;
            };
            let byte_range = get(&attrs, "BYTERANGE").and_then(|v| ByteRange::parse_hls(v, 0));
            out.maps.push(MapInfo {
                uri: vaco_format_adaptive::resolve(base_url, uri),
                byte_range,
            });
            current_map = Some(out.maps.len() - 1);
        } else if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
            let attrs = parse_attribute_list(rest);
            let method = get(&attrs, "METHOD").unwrap_or("NONE").to_owned();
            if method == "NONE" {
                current_key = None;
            } else {
                out.keys.push(KeyInfo {
                    method,
                    uri: get(&attrs, "URI").map(|u| vaco_format_adaptive::resolve(base_url, u)),
                    iv: get(&attrs, "IV").map(str::to_owned),
                    key_format: get(&attrs, "KEYFORMAT").map(str::to_owned),
                    key_format_versions: get(&attrs, "KEYFORMATVERSIONS").map(str::to_owned),
                });
                current_key = Some(out.keys.len() - 1);
            }
        } else if let Some(rest) = line.strip_prefix("#EXTINF:") {
            let (dur_str, title) = rest.split_once(',').unwrap_or((rest, ""));
            let duration = Duration::from_decimal_seconds(dur_str.trim()).unwrap_or(Duration::ZERO);
            pending_duration = Some((duration, title.to_owned()));
        } else if line.starts_with('#') {
            // Unrecognised tag or comment: RFC 8216 §4.1 tolerance.
        } else {
            // A bare line: the URI for the pending #EXTINF.
            let Some((duration, title)) = pending_duration.take() else {
                continue;
            };
            if let Some(r) = pending_byterange {
                last_byterange_end = r.end();
            }
            let seq = media_sequence.saturating_add(out.segments.len() as u64);
            out.segments.push(MediaSegment {
                uri: vaco_format_adaptive::resolve(base_url, line),
                duration,
                title,
                byte_range: pending_byterange.take(),
                discontinuity: std::mem::take(&mut pending_discontinuity),
                program_date_time: pending_pdt.take(),
                map: current_map,
                key: current_key,
                media_sequence: seq,
            });
        }
    }

    if pending_duration.is_some() {
        return Err(Error::InvalidData("HLS #EXTINF has no following URI"));
    }
    out.media_sequence = media_sequence;
    out.discontinuity_sequence = discontinuity_sequence;
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    const VOD: &str = "#EXTM3U\n\
#EXT-X-VERSION:3\n\
#EXT-X-TARGETDURATION:6\n\
#EXT-X-MEDIA-SEQUENCE:5\n\
#EXT-X-PLAYLIST-TYPE:VOD\n\
#EXTINF:6.000,\n\
seg5.ts\n\
#EXT-X-DISCONTINUITY\n\
#EXT-X-PROGRAM-DATE-TIME:2020-01-01T00:00:00Z\n\
#EXTINF:6.000,\n\
seg6.ts\n\
#EXT-X-ENDLIST\n";

    #[test]
    fn parses_a_vod_playlist_end_to_end() {
        let p = parse(VOD, "http://a/media.m3u8").unwrap();
        assert!(!p.is_live());
        assert_eq!(p.playlist_type, Some(PlaylistType::Vod));
        assert_eq!(p.media_sequence, 5);
        assert_eq!(p.segments.len(), 2);
        assert_eq!(p.segments[0].uri, "http://a/seg5.ts");
        assert_eq!(p.segments[0].media_sequence, 5);
        assert!(!p.segments[0].discontinuity);
        assert!(p.segments[1].discontinuity);
        assert_eq!(p.segments[1].media_sequence, 6);
        assert!(p.segments[1].program_date_time.is_some());
    }

    #[test]
    fn a_live_playlist_has_no_endlist() {
        let text = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6.0,\nseg0.ts\n";
        let p = parse(text, "x").unwrap();
        assert!(p.is_live());
    }

    #[test]
    fn extinf_retains_submicrosecond_decimal_digits() {
        let playlist = parse("#EXTM3U\n#EXTINF:0.0000001,\nseg.ts\n", "x").unwrap();
        assert_eq!(playlist.segments[0].duration.as_ratio(), (1, 10_000_000));
    }

    #[test]
    fn byterange_without_offset_continues_the_previous_one() {
        let text = "#EXTM3U\n\
#EXT-X-TARGETDURATION:2\n\
#EXTINF:2.0,\n\
#EXT-X-BYTERANGE:1000@0\n\
seg.ts\n\
#EXTINF:2.0,\n\
#EXT-X-BYTERANGE:500\n\
seg.ts\n";
        let p = parse(text, "x").unwrap();
        assert_eq!(
            p.segments[0].byte_range,
            Some(ByteRange {
                offset: 0,
                length: 1000
            })
        );
        assert_eq!(
            p.segments[1].byte_range,
            Some(ByteRange {
                offset: 1000,
                length: 500
            })
        );
    }

    #[test]
    fn map_and_key_carry_forward_across_segments() {
        let text = "#EXTM3U\n\
#EXT-X-TARGETDURATION:2\n\
#EXT-X-MAP:URI=\"init.mp4\"\n\
#EXT-X-KEY:METHOD=AES-128,URI=\"https://x/key\",IV=0x00000000000000000000000000000001\n\
#EXTINF:2.0,\n\
seg0.mp4\n\
#EXTINF:2.0,\n\
seg1.mp4\n\
#EXT-X-KEY:METHOD=NONE\n\
#EXTINF:2.0,\n\
seg2.mp4\n";
        let p = parse(text, "x").unwrap();
        assert_eq!(p.segments[0].map, Some(0));
        assert_eq!(p.segments[1].map, Some(0));
        assert_eq!(p.segments[0].key, Some(0));
        assert_eq!(p.segments[1].key, Some(0));
        assert_eq!(p.segments[2].key, None);
        assert_eq!(p.keys[0].method, "AES-128");
    }

    #[test]
    fn extinf_with_no_uri_is_an_error() {
        let text = "#EXTM3U\n#EXTINF:2.0,\n";
        assert!(parse(text, "x").is_err());
    }
}
