//! The MPD semantic model, interpreted from the generic [`crate::tree::Node`]
//! tree: `Period` -> `AdaptationSet` -> `Representation`, and the three
//! addressing modes (ISO/IEC 23009-1 §5.3.9).
//!
//! # `$Time$`/`SegmentTimeline`, the fiddly part
//!
//! `<SegmentTimeline>`'s `<S t="…" d="…" r="…"/>` run-length encoding is
//! expanded by [`vaco_format_adaptive::timeline::expand`] — this module's job
//! is only to parse the `<S>` elements into
//! [`vaco_format_adaptive::TimelineEntry`] and to know which of `$Number$` or
//! `$Time$` a `SegmentTemplate`'s `media` pattern asks for, so [`substitute`]
//! can supply it. Getting the *timeline* arithmetic right is
//! `vaco-format-adaptive`'s job and its own proptest; this module's job is
//! getting the *substitution* right.

use vaco_core::{Duration, Error, Result};
use vaco_format_adaptive::walltime::parse_iso8601_duration;
use vaco_format_adaptive::{TimelineEntry, WallClock, walltime::parse_iso8601_datetime};

use crate::tree::Node;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationType {
    Static,
    Dynamic,
}

/// `ContentProtection` — detected and reported, never acted on. The DASH
/// analogue of `vaco_demux_hls::key::KeyInfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentProtectionInfo {
    pub scheme_id_uri: String,
    pub default_kid: Option<String>,
    /// Whether a `<cenc:pssh>` child was present (its contents are not
    /// decoded — CENC key derivation is exactly the boundary this crate does
    /// not cross).
    pub has_pssh: bool,
}

impl ContentProtectionInfo {
    #[must_use]
    pub fn unsupported_error(&self) -> Error {
        Error::Unsupported("DASH ContentProtection segments are not decrypted")
    }
}

#[derive(Debug, Clone, Default)]
pub struct SegmentTemplate {
    pub media: Option<String>,
    pub initialization: Option<String>,
    pub timescale: u64,
    pub duration: Option<u64>,
    pub start_number: u64,
    pub timeline: Option<Vec<TimelineEntry>>,
}

#[derive(Debug, Clone)]
pub struct SegmentUrl {
    pub media: String,
    pub media_range: Option<vaco_format_adaptive::ByteRange>,
}

#[derive(Debug, Clone, Default)]
pub struct SegmentList {
    pub duration: Option<u64>,
    pub timescale: u64,
    pub initialization: Option<(String, Option<vaco_format_adaptive::ByteRange>)>,
    pub urls: Vec<SegmentUrl>,
}

#[derive(Debug, Clone, Default)]
pub struct SegmentBase {
    pub index_range: Option<vaco_format_adaptive::ByteRange>,
    pub initialization: Option<(Option<String>, Option<vaco_format_adaptive::ByteRange>)>,
}

/// Addressing declared at `AdaptationSet` or `Representation` level. Only
/// one is normally present; when more than one appears (which the schema
/// technically forbids but real files sometimes have redundant copies of),
/// `SegmentTimeline`d templates win, then plain templates, then lists, then
/// base — the order most likely to be *complete* enough to enumerate from.
#[derive(Debug, Clone, Default)]
pub struct Addressing {
    pub template: Option<SegmentTemplate>,
    pub list: Option<SegmentList>,
    pub base: Option<SegmentBase>,
}

impl Addressing {
    fn merge_override(base: &Self, over: &Self) -> Self {
        Self {
            template: over.template.clone().or_else(|| base.template.clone()),
            list: over.list.clone().or_else(|| base.list.clone()),
            base: over.base.clone().or_else(|| base.base.clone()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Representation {
    pub id: String,
    pub bandwidth: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub frame_rate: Option<f64>,
    pub codecs: Vec<String>,
    pub mime_type: Option<String>,
    pub base_url: Option<String>,
    pub addressing: Addressing,
}

#[derive(Debug, Clone)]
pub struct AdaptationSet {
    pub id: Option<String>,
    pub mime_type: Option<String>,
    pub lang: Option<String>,
    pub content_protection: Vec<ContentProtectionInfo>,
    pub base_url: Option<String>,
    pub representations: Vec<Representation>,
}

#[derive(Debug, Clone)]
pub struct Period {
    pub id: Option<String>,
    pub start: Option<Duration>,
    pub duration: Option<Duration>,
    pub base_url: Option<String>,
    pub adaptation_sets: Vec<AdaptationSet>,
}

#[derive(Debug, Clone)]
pub struct Mpd {
    pub presentation_type: PresentationType,
    pub availability_start_time: Option<WallClock>,
    pub publish_time: Option<WallClock>,
    pub media_presentation_duration: Option<Duration>,
    pub min_buffer_time: Option<Duration>,
    pub base_url: Option<String>,
    pub periods: Vec<Period>,
}

fn parse_u64(s: Option<&str>) -> Option<u64> {
    s.and_then(|v| v.trim().parse().ok())
}

fn parse_duration_attr(s: Option<&str>) -> Option<Duration> {
    s.and_then(parse_iso8601_duration)
}

fn parse_byte_range(s: &str) -> Option<vaco_format_adaptive::ByteRange> {
    vaco_format_adaptive::ByteRange::parse_dash_range(s)
}

fn parse_segment_template(n: &Node) -> SegmentTemplate {
    // Deliberately does *not* fill in an omitted `@t` here (unlike a first
    // version of this function, which precomputed it from a running cursor
    // and broke `vaco_format_adaptive::timeline::expand`'s own "omitted `@t`
    // continues from the previous entry" rule two ways at once — this
    // function's test caught the disagreement). Store exactly what the XML
    // stated; `expand` is the sole authority on filling the gap.
    let timeline = n.child("SegmentTimeline").map(|tl| {
        tl.children_named("S")
            .map(|s| TimelineEntry {
                t: parse_u64(s.attr("t")),
                d: parse_u64(s.attr("d")).unwrap_or(0),
                r: s.attr("r").and_then(|v| v.trim().parse().ok()),
            })
            .collect()
    });
    SegmentTemplate {
        media: n.attr("media").map(str::to_owned),
        initialization: n.attr("initialization").map(str::to_owned),
        timescale: parse_u64(n.attr("timescale")).unwrap_or(1),
        duration: parse_u64(n.attr("duration")),
        start_number: parse_u64(n.attr("startNumber")).unwrap_or(1),
        timeline,
    }
}

fn parse_segment_list(n: &Node) -> SegmentList {
    let initialization = n.child("Initialization").map(|i| {
        (
            i.attr("sourceURL").unwrap_or_default().to_owned(),
            i.attr("range").and_then(parse_byte_range),
        )
    });
    let urls = n
        .children_named("SegmentURL")
        .filter_map(|u| {
            u.attr("media").map(|media| SegmentUrl {
                media: media.to_owned(),
                media_range: u.attr("mediaRange").and_then(parse_byte_range),
            })
        })
        .collect();
    SegmentList {
        duration: parse_u64(n.attr("duration")),
        timescale: parse_u64(n.attr("timescale")).unwrap_or(1),
        initialization,
        urls,
    }
}

fn parse_segment_base(n: &Node) -> SegmentBase {
    let initialization = n.child("Initialization").map(|i| {
        (
            i.attr("sourceURL").map(str::to_owned),
            i.attr("range").and_then(parse_byte_range),
        )
    });
    SegmentBase {
        index_range: n.attr("indexRange").and_then(parse_byte_range),
        initialization,
    }
}

fn parse_addressing(n: &Node) -> Addressing {
    Addressing {
        template: n.child("SegmentTemplate").map(parse_segment_template),
        list: n.child("SegmentList").map(parse_segment_list),
        base: n.child("SegmentBase").map(parse_segment_base),
    }
}

fn parse_content_protection(n: &Node) -> Vec<ContentProtectionInfo> {
    n.children_named("ContentProtection")
        .map(|cp| ContentProtectionInfo {
            scheme_id_uri: cp.attr("schemeIdUri").unwrap_or_default().to_owned(),
            default_kid: cp.attr("default_KID").map(str::to_owned),
            has_pssh: cp.child("pssh").is_some(),
        })
        .collect()
}

fn parse_representation(n: &Node, inherited: &Addressing) -> Representation {
    let own = parse_addressing(n);
    Representation {
        id: n.attr("id").unwrap_or_default().to_owned(),
        bandwidth: parse_u64(n.attr("bandwidth")).unwrap_or(0),
        width: n.attr("width").and_then(|v| v.parse().ok()),
        height: n.attr("height").and_then(|v| v.parse().ok()),
        frame_rate: n.attr("frameRate").and_then(parse_frame_rate),
        codecs: n
            .attr("codecs")
            .map(|c| c.split(',').map(|s| s.trim().to_owned()).collect())
            .unwrap_or_default(),
        mime_type: n.attr("mimeType").map(str::to_owned),
        base_url: n.child("BaseURL").map(|b| b.text.trim().to_owned()),
        addressing: Addressing::merge_override(inherited, &own),
    }
}

/// `frameRate` may be `"30"` or `"30000/1001"`.
fn parse_frame_rate(s: &str) -> Option<f64> {
    if let Some((num, den)) = s.split_once('/') {
        let num: f64 = num.trim().parse().ok()?;
        let den: f64 = den.trim().parse().ok()?;
        if den == 0.0 { None } else { Some(num / den) }
    } else {
        s.trim().parse().ok()
    }
}

fn parse_adaptation_set(n: &Node) -> AdaptationSet {
    let own = parse_addressing(n);
    let mime_type = n.attr("mimeType").map(str::to_owned);
    AdaptationSet {
        id: n.attr("id").map(str::to_owned),
        lang: n.attr("lang").map(str::to_owned),
        content_protection: parse_content_protection(n),
        base_url: n.child("BaseURL").map(|b| b.text.trim().to_owned()),
        representations: n
            .children_named("Representation")
            .map(|r| {
                let mut rep = parse_representation(r, &own);
                if rep.mime_type.is_none() {
                    rep.mime_type.clone_from(&mime_type);
                }
                rep
            })
            .collect(),
        mime_type,
    }
}

fn parse_period(n: &Node) -> Period {
    Period {
        id: n.attr("id").map(str::to_owned),
        start: parse_duration_attr(n.attr("start")),
        duration: parse_duration_attr(n.attr("duration")),
        base_url: n.child("BaseURL").map(|b| b.text.trim().to_owned()),
        adaptation_sets: n
            .children_named("AdaptationSet")
            .map(parse_adaptation_set)
            .collect(),
    }
}

/// Interpret a parsed [`Node`] tree as an [`Mpd`].
///
/// # Errors
/// [`Error::InvalidData`] when the root element is not `MPD`.
pub fn interpret(root: &Node) -> Result<Mpd> {
    if root.name != "MPD" {
        return Err(Error::InvalidData("document root is not <MPD>"));
    }
    let presentation_type = match root.attr("type") {
        Some("dynamic") => PresentationType::Dynamic,
        _ => PresentationType::Static,
    };
    Ok(Mpd {
        presentation_type,
        availability_start_time: root
            .attr("availabilityStartTime")
            .and_then(parse_iso8601_datetime),
        publish_time: root.attr("publishTime").and_then(parse_iso8601_datetime),
        media_presentation_duration: parse_duration_attr(root.attr("mediaPresentationDuration")),
        min_buffer_time: parse_duration_attr(root.attr("minBufferTime")),
        base_url: root.child("BaseURL").map(|b| b.text.trim().to_owned()),
        periods: root.children_named("Period").map(parse_period).collect(),
    })
}

/// Substitute `$Identifier[%0Nd]$` tokens in a `SegmentTemplate` pattern
/// (ISO/IEC 23009-1 §5.3.9.4.4): `$$` is a literal `$`; `$RepresentationID$`,
/// `$Bandwidth$`, `$Number$`, `$Time$`, each optionally followed by a
/// `%0Nd` width specifier.
#[must_use]
pub fn substitute(
    pattern: &str,
    representation_id: &str,
    bandwidth: u64,
    number: Option<u64>,
    time: Option<u64>,
) -> String {
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(start) = rest.find('$') {
        let Some(before) = rest.get(..start) else {
            break;
        };
        out.push_str(before);
        let Some(after) = rest.get(start + 1..) else {
            break;
        };
        if let Some(tail) = after.strip_prefix('$') {
            out.push('$');
            rest = tail;
            continue;
        }
        let Some(end) = after.find('$') else {
            // Unterminated `$`: emit literally and stop substituting.
            out.push('$');
            out.push_str(after);
            return out;
        };
        let Some(token) = after.get(..end) else {
            break;
        };
        let Some(tail) = after.get(end + 1..) else {
            break;
        };
        let (ident, width) = token.split_once('%').map_or((token, None), |(i, w)| {
            (
                i,
                w.strip_suffix('d')
                    .and_then(|w| w.trim_start_matches('0').parse::<usize>().ok()),
            )
        });
        let value = match ident {
            "RepresentationID" => Some(representation_id.to_owned()),
            "Bandwidth" => Some(bandwidth.to_string()),
            "Number" => number.map(|n| n.to_string()),
            "Time" => time.map(|t| t.to_string()),
            _ => None,
        };
        if let Some(v) = value {
            if let Some(w) = width {
                let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{v:0>w$}"));
            } else {
                out.push_str(&v);
            }
        } else {
            out.push('$');
            out.push_str(token);
            out.push('$');
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use crate::tree;
    use vaco_limits::{Budget, Limits};

    fn tree_of(xml: &str) -> Node {
        let mut b = Budget::new(Limits::permissive());
        tree::parse(xml, &mut b).unwrap()
    }

    #[test]
    fn substitutes_number_and_representation_id() {
        assert_eq!(
            substitute(
                "chunk-$RepresentationID$-$Number%05d$.m4s",
                "v0",
                500_000,
                Some(7),
                None
            ),
            "chunk-v0-00007.m4s"
        );
    }

    #[test]
    fn substitutes_time_without_width() {
        assert_eq!(
            substitute("seg-$Time$.m4s", "v0", 0, None, Some(4_500_000)),
            "seg-4500000.m4s"
        );
    }

    #[test]
    fn double_dollar_is_literal() {
        assert_eq!(
            substitute("price_$$5.m4s", "v0", 0, None, None),
            "price_$5.m4s"
        );
    }

    #[test]
    fn a_missing_value_is_left_as_the_token() {
        // $Number$ requested but none supplied (a $Time$-addressed template):
        // left visible rather than silently blanked, which would produce a
        // URL that looks plausible and is wrong.
        assert_eq!(
            substitute("seg-$Number$.m4s", "v0", 0, None, Some(1)),
            "seg-$Number$.m4s"
        );
    }

    #[test]
    fn interprets_a_minimal_static_mpd_with_segment_template() {
        let xml = r#"
<MPD type="static" mediaPresentationDuration="PT10S">
  <Period id="0">
    <AdaptationSet mimeType="video/mp4">
      <SegmentTemplate media="chunk-$RepresentationID$-$Number%05d$.m4s"
                       initialization="init-$RepresentationID$.m4s"
                       timescale="90000" duration="180000" startNumber="1"/>
      <Representation id="v0" bandwidth="500000" width="640" height="360" codecs="avc1.640028"/>
    </AdaptationSet>
  </Period>
</MPD>"#;
        let mpd = interpret(&tree_of(xml)).unwrap();
        assert_eq!(mpd.presentation_type, PresentationType::Static);
        assert_eq!(
            mpd.media_presentation_duration.unwrap().as_micros(),
            10_000_000
        );
        let period = &mpd.periods[0];
        let aset = &period.adaptation_sets[0];
        let rep = &aset.representations[0];
        assert_eq!(rep.bandwidth, 500_000);
        assert_eq!(rep.codecs, vec!["avc1.640028".to_owned()]);
        let tmpl = rep.addressing.template.as_ref().unwrap();
        assert_eq!(tmpl.timescale, 90_000);
        assert_eq!(tmpl.duration, Some(180_000));
    }

    #[test]
    fn segment_timeline_s_elements_parse_into_timeline_entries() {
        let xml = r#"
<MPD type="static">
  <Period>
    <AdaptationSet>
      <SegmentTemplate media="s-$Time$.m4s" timescale="1000">
        <SegmentTimeline>
          <S t="0" d="2000" r="2"/>
          <S d="1000"/>
        </SegmentTimeline>
      </SegmentTemplate>
      <Representation id="v0" bandwidth="1"/>
    </AdaptationSet>
  </Period>
</MPD>"#;
        let mpd = interpret(&tree_of(xml)).unwrap();
        let tmpl = mpd.periods[0].adaptation_sets[0].representations[0]
            .addressing
            .template
            .clone()
            .unwrap();
        let entries = tmpl.timeline.unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0],
            TimelineEntry {
                t: Some(0),
                d: 2000,
                r: Some(2)
            }
        );
        assert_eq!(
            entries[1],
            TimelineEntry {
                t: None,
                d: 1000,
                r: None
            }
        );
    }

    #[test]
    fn content_protection_is_detected_and_reported() {
        let xml = r#"
<MPD type="static">
  <Period>
    <AdaptationSet>
      <ContentProtection schemeIdUri="urn:mpeg:dash:mp4protection:2011" default_KID="11111111-2222-3333-4444-555555555555"/>
      <Representation id="v0" bandwidth="1"/>
    </AdaptationSet>
  </Period>
</MPD>"#;
        let mpd = interpret(&tree_of(xml)).unwrap();
        let cp = &mpd.periods[0].adaptation_sets[0].content_protection;
        assert_eq!(cp.len(), 1);
        assert_eq!(cp[0].scheme_id_uri, "urn:mpeg:dash:mp4protection:2011");
        assert!(matches!(cp[0].unsupported_error(), Error::Unsupported(_)));
    }

    #[test]
    fn representation_inherits_the_adaptation_sets_template() {
        let xml = r#"
<MPD type="static">
  <Period>
    <AdaptationSet>
      <SegmentTemplate media="a-$Number$.m4s" timescale="1" duration="2"/>
      <Representation id="v0" bandwidth="1"/>
      <Representation id="v1" bandwidth="2">
        <SegmentTemplate media="b-$Number$.m4s" timescale="1" duration="2"/>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>"#;
        let mpd = interpret(&tree_of(xml)).unwrap();
        let reps = &mpd.periods[0].adaptation_sets[0].representations;
        assert_eq!(
            reps[0]
                .addressing
                .template
                .as_ref()
                .unwrap()
                .media
                .as_deref(),
            Some("a-$Number$.m4s")
        );
        assert_eq!(
            reps[1]
                .addressing
                .template
                .as_ref()
                .unwrap()
                .media
                .as_deref(),
            Some("b-$Number$.m4s"),
            "a Representation's own SegmentTemplate must override the AdaptationSet's"
        );
    }

    #[test]
    fn rejects_a_non_mpd_root() {
        let tree = tree_of("<NotAnMpd/>");
        assert!(interpret(&tree).is_err());
    }
}
