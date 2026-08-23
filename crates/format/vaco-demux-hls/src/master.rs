//! Master playlist parsing: `#EXT-X-STREAM-INF` variants and `#EXT-X-MEDIA`
//! alternate renditions (RFC 8216 §4.3.4.1, §4.3.4.2).

use vaco_core::{Error, Result};
use vaco_format_adaptive::{Rendition, RenditionKind, Variant};

use crate::attrs::{get, parse_attribute_list};

/// A parsed master playlist: every variant stream and every alternate
/// rendition it names, with every `URI` already resolved against the
/// playlist's own address.
#[derive(Debug, Clone, Default)]
pub struct MasterPlaylist {
    pub variants: Vec<Variant>,
    pub renditions: Vec<Rendition>,
    /// `#EXT-X-INDEPENDENT-SEGMENTS`: every segment in every variant/rendition
    /// this master names is independently decodable (starts with a
    /// keyframe/has no reference outside itself), which lets a player switch
    /// variants at any segment boundary rather than only at ones already
    /// known to be aligned.
    pub independent_segments: bool,
}

/// Parse `text`, a master playlist already read into memory, resolving every
/// `URI` against `base_url` ([`vaco_format_adaptive::url::resolve`]).
///
/// # Errors
/// [`Error::InvalidData`] when the playlist has no `#EXTM3U` header, or an
/// `#EXT-X-STREAM-INF` tag has no following URI line — RFC 8216 §4.3.4.1
/// requires one and a variant that names no playlist cannot be used at all,
/// so failing here is more useful than silently dropping the variant.
pub fn parse(text: &str, base_url: &str) -> Result<MasterPlaylist> {
    let mut lines = text.lines().map(str::trim);
    match lines.next() {
        Some(first) if first.starts_with("#EXTM3U") => {}
        _ => return Err(Error::InvalidData("HLS playlist has no #EXTM3U header")),
    }

    let mut out = MasterPlaylist::default();
    let mut pending_stream_inf: Option<Vec<crate::attrs::Attr<'_>>> = None;

    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            pending_stream_inf = Some(parse_attribute_list(rest));
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA:") {
            if let Some(r) = parse_media_tag(rest, base_url) {
                out.renditions.push(r);
            }
            continue;
        }
        if line.starts_with("#EXT-X-INDEPENDENT-SEGMENTS") {
            out.independent_segments = true;
            continue;
        }
        if line.starts_with('#') {
            continue; // Unrecognised tag or comment: RFC 8216 §4.1 tolerance.
        }
        // A bare line following #EXT-X-STREAM-INF is that variant's URI.
        if let Some(attrs) = pending_stream_inf.take() {
            out.variants.push(build_variant(&attrs, line, base_url)?);
        }
        // A bare line with no pending EXT-X-STREAM-INF is not valid master
        // playlist syntax; RFC 8216 tolerance says ignore it rather than fail
        // the whole document.
    }

    if pending_stream_inf.is_some() {
        return Err(Error::InvalidData(
            "HLS #EXT-X-STREAM-INF has no following URI",
        ));
    }
    Ok(out)
}

fn build_variant(
    attrs: &[crate::attrs::Attr<'_>],
    uri_line: &str,
    base_url: &str,
) -> Result<Variant> {
    let bandwidth: u64 =
        get(attrs, "BANDWIDTH")
            .and_then(|v| v.parse().ok())
            .ok_or(Error::InvalidData(
                "HLS #EXT-X-STREAM-INF has no BANDWIDTH attribute",
            ))?;
    let average_bandwidth = get(attrs, "AVERAGE-BANDWIDTH").and_then(|v| v.parse().ok());
    let (width, height) = get(attrs, "RESOLUTION")
        .and_then(|v| v.split_once('x'))
        .map_or((None, None), |(w, h)| (w.parse().ok(), h.parse().ok()));
    let frame_rate = get(attrs, "FRAME-RATE").and_then(|v| v.parse().ok());
    let codecs = get(attrs, "CODECS")
        .map(|v| v.split(',').map(|c| c.trim().to_owned()).collect())
        .unwrap_or_default();
    let id = get(attrs, "VIDEO")
        .or_else(|| get(attrs, "AUDIO"))
        .map(str::to_owned);
    Ok(Variant {
        bandwidth,
        average_bandwidth,
        width,
        height,
        frame_rate,
        codecs,
        uri: vaco_format_adaptive::resolve(base_url, uri_line.trim()),
        id,
    })
}

fn parse_media_tag(rest: &str, base_url: &str) -> Option<Rendition> {
    let attrs = parse_attribute_list(rest);
    let kind = match get(&attrs, "TYPE")? {
        "AUDIO" => RenditionKind::Audio,
        "VIDEO" => RenditionKind::Video,
        "SUBTITLES" => RenditionKind::Subtitles,
        "CLOSED-CAPTIONS" => RenditionKind::ClosedCaptions,
        _ => return None,
    };
    let group_id = get(&attrs, "GROUP-ID")?.to_owned();
    Some(Rendition {
        kind,
        group_id,
        name: get(&attrs, "NAME").map(str::to_owned),
        language: get(&attrs, "LANGUAGE").map(str::to_owned),
        is_default: get(&attrs, "DEFAULT") == Some("YES"),
        autoselect: get(&attrs, "AUTOSELECT") == Some("YES"),
        forced: get(&attrs, "FORCED") == Some("YES"),
        uri: get(&attrs, "URI").map(|u| vaco_format_adaptive::resolve(base_url, u)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    const MASTER: &str = "#EXTM3U\n\
#EXT-X-INDEPENDENT-SEGMENTS\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,AUTOSELECT=YES,URI=\"audio/en.m3u8\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=1280000,AVERAGE-BANDWIDTH=1000000,CODECS=\"avc1.4d401f,mp4a.40.2\",RESOLUTION=640x360,AUDIO=\"aud\"\n\
low/index.m3u8\n\
#EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080,CODECS=\"avc1.640028,mp4a.40.2\",AUDIO=\"aud\"\n\
high/index.m3u8\n";

    #[test]
    fn parses_variants_and_renditions() {
        let m = parse(MASTER, "http://a/master.m3u8").unwrap();
        assert!(m.independent_segments);
        assert_eq!(m.variants.len(), 2);
        assert_eq!(m.variants[0].bandwidth, 1_280_000);
        assert_eq!(m.variants[0].average_bandwidth, Some(1_000_000));
        assert_eq!(m.variants[0].width, Some(640));
        assert_eq!(m.variants[0].height, Some(360));
        assert_eq!(m.variants[0].uri, "http://a/low/index.m3u8");
        assert_eq!(
            m.variants[0].codecs,
            vec!["avc1.4d401f".to_owned(), "mp4a.40.2".to_owned()]
        );
        assert_eq!(m.variants[1].uri, "http://a/high/index.m3u8");

        assert_eq!(m.renditions.len(), 1);
        assert_eq!(m.renditions[0].group_id, "aud");
        assert!(m.renditions[0].is_default);
        assert_eq!(
            m.renditions[0].uri.as_deref(),
            Some("http://a/audio/en.m3u8")
        );
    }

    #[test]
    fn rejects_a_playlist_with_no_extm3u_header() {
        assert!(parse("not a playlist", "x").is_err());
    }

    #[test]
    fn a_stream_inf_with_no_uri_is_an_error() {
        let text = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100\n";
        assert!(parse(text, "x").is_err());
    }
}
