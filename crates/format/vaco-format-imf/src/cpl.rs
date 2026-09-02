//! Composition Playlist parsing (SMPTE ST 2067-3): the edit-decision-list
//! layer — `SegmentList` > `Segment` > `SequenceList` > one sequence per
//! essence kind (`MainImageSequence`, `MainAudioSequence`, ...) >
//! `ResourceList` > `Resource`, each `Resource` naming a `TrackFileId` (a
//! `UUID` the ASSETMAP resolves to a real file) plus the edit-unit range of
//! that file this composition actually uses.
//!
//! # Virtual tracks
//!
//! A CPL's real timeline is not the `Segment` list directly: every
//! `Sequence` across every `Segment` that shares the same `TrackId` is one
//! **virtual track**, and its own timeline is those sequences' `Resource`s
//! concatenated in segment order. [`Cpl::virtual_tracks`] performs exactly
//! that grouping; nothing else in this module needs to know a composition
//! can have more than one `Segment` at all.
//!
//! # Scope
//!
//! `MainImageSequence` and `MainAudioSequence` are read; `MarkerSequence`,
//! `MainSubtitleSequence`, `MainCaptionSequence`, `AncillaryDataSequence`
//! and IAB/ACES extension sequences are not — see "How to change it" in
//! this crate's top-level docs for what each would need. A `Resource` whose
//! own `EditRate` differs from the composition's `EditRate` (legal per the
//! schema, meant for a track file authored at a different rate than the
//! composition plays it at) is rejected with [`Error::Unsupported`] rather
//! than silently played at the wrong rate — this crate has not measured a
//! real file exercising that path, so guessing the correct resampling
//! arithmetic would be exactly the kind of guess D6/D17 rules out.

use vaco_core::{Error, Rational, Result};
use vaco_limits::Budget;

use crate::assetmap::strip_urn_uuid;
use crate::xml::{self, XmlNode};

/// Element names of the sequence kinds this crate reads, in the order a
/// spec-conforming file's own `SequenceList` states them (image before
/// audio) — not load-bearing (lookup is by name, not position), just the
/// order [`Cpl::virtual_tracks`]'s own output happens to fall out in.
pub const SEQUENCE_KINDS: &[&str] = &["MainImageSequence", "MainAudioSequence"];

/// One `<Resource>`: a range of edit units from one track file.
#[derive(Debug, Clone)]
pub struct Resource {
    pub id: String,
    /// `<TrackFileId>` — resolved against the ASSETMAP by
    /// [`crate::package::Package`], not here.
    pub track_file_id: String,
    /// `<EntryPoint>`, in edit units of the composition's own `EditRate`
    /// (checked equal to the resource's own `<EditRate>` when present — see
    /// the module docs). Defaults to `0`.
    pub entry_point: u64,
    /// `<SourceDuration>`, in edit units. Defaults to
    /// `IntrinsicDuration - EntryPoint` when absent, matching the schema's
    /// own stated default.
    pub source_duration: u64,
    /// `<RepeatCount>` — how many times this exact range plays consecutively
    /// before the next `Resource`. Defaults to `1`.
    pub repeat_count: u32,
}

/// One `<Sequence>` (of whichever kind), holding one virtual track's
/// contribution from a single `Segment`.
#[derive(Debug, Clone)]
pub struct Sequence {
    pub id: String,
    /// `<TrackId>` — the identity [`Cpl::virtual_tracks`] groups by. Distinct
    /// from `Sequence.Id`: two sequences in different `Segment`s that
    /// belong to the same virtual track share `TrackId`, not `Id`.
    pub track_id: String,
    pub kind: SequenceKind,
    pub resources: Vec<Resource>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceKind {
    MainImage,
    MainAudio,
}

/// One `<Segment>`: an ordered set of sequences, one per virtual track
/// active during that segment (a composition need not use every track in
/// every segment, though every corpus file measured elsewhere in this
/// project's format work does).
#[derive(Debug, Clone)]
pub struct Segment {
    pub id: String,
    pub sequences: Vec<Sequence>,
}

/// The parsed Composition Playlist.
#[derive(Debug, Clone)]
pub struct Cpl {
    pub id: String,
    pub content_title: Option<String>,
    /// `<EditRate>` — the composition's own edit rate; every `Resource`'s
    /// edit-unit counts are in this rate unless a `Resource` states its own
    /// (rejected, see the module docs).
    pub edit_rate: Rational,
    pub segments: Vec<Segment>,
}

/// One virtual track: every `Resource`, across every `Segment`, that shares
/// a `TrackId`, concatenated in segment order.
#[derive(Debug, Clone)]
pub struct VirtualTrack {
    pub track_id: String,
    pub kind: SequenceKind,
    pub resources: Vec<Resource>,
}

impl Cpl {
    /// Group every `Sequence` sharing a `TrackId` into one [`VirtualTrack`],
    /// in the order each track's `TrackId` first appears.
    #[must_use]
    pub fn virtual_tracks(&self) -> Vec<VirtualTrack> {
        let mut order: Vec<String> = Vec::new();
        let mut by_id: std::collections::HashMap<String, VirtualTrack> =
            std::collections::HashMap::new();
        for segment in &self.segments {
            for seq in &segment.sequences {
                let entry = by_id.entry(seq.track_id.clone()).or_insert_with(|| {
                    order.push(seq.track_id.clone());
                    VirtualTrack {
                        track_id: seq.track_id.clone(),
                        kind: seq.kind,
                        resources: Vec::new(),
                    }
                });
                entry.resources.extend(seq.resources.iter().cloned());
            }
        }
        order
            .into_iter()
            .filter_map(|id| by_id.remove(&id))
            .collect()
    }
}

/// Parse `"24 1"` / `"25 1"` / `"30000 1001"` — every rate in the IMF XML
/// family is two whitespace-separated integers, numerator then
/// denominator, the same shape MXF's own `EditRate` states in binary
/// (`vaco-demux-mxf::localset::rational_be`'s doc comment has that side).
///
/// # Errors
/// [`Error::InvalidData`] when `s` is not exactly two integers.
pub fn parse_rate(s: &str) -> Result<Rational> {
    let mut parts = s.split_whitespace();
    let num = parts
        .next()
        .and_then(|p| p.parse::<i32>().ok())
        .ok_or(Error::InvalidData("malformed IMF edit rate"))?;
    let den = parts
        .next()
        .and_then(|p| p.parse::<i32>().ok())
        .ok_or(Error::InvalidData("malformed IMF edit rate"))?;
    if parts.next().is_some() || den == 0 {
        return Err(Error::InvalidData("malformed IMF edit rate"));
    }
    Ok(Rational::new(num, den))
}

/// Parse a Composition Playlist document's bytes.
///
/// # Errors
/// [`Error::InvalidData`] for malformed XML, the wrong root element, or a
/// missing required element; [`Error::Unsupported`] for a `Resource`
/// stating its own `EditRate` different from the composition's (see the
/// module docs).
pub fn parse(xml_bytes: &str, budget: &mut Budget) -> Result<Cpl> {
    let root = xml::parse(xml_bytes, budget)?;
    if root.name != "CompositionPlaylist" {
        return Err(Error::InvalidData(
            "not a Composition Playlist document (root element is not CompositionPlaylist)",
        ));
    }
    let id = strip_urn_uuid(
        root.child_text("Id")
            .ok_or(Error::InvalidData("CompositionPlaylist has no Id"))?,
    );
    let edit_rate = parse_rate(
        root.child_text("EditRate")
            .ok_or(Error::InvalidData("CompositionPlaylist has no EditRate"))?,
    )?;
    let content_title = root.child_text("ContentTitleText").map(str::to_owned);

    let segment_list = root
        .child("SegmentList")
        .ok_or(Error::InvalidData("CompositionPlaylist has no SegmentList"))?;

    let mut segments = Vec::new();
    for seg_node in segment_list.children_named("Segment") {
        segments.push(parse_segment(seg_node, edit_rate)?);
    }
    if segments.is_empty() {
        return Err(Error::InvalidData(
            "CompositionPlaylist's SegmentList has no Segment",
        ));
    }

    Ok(Cpl {
        id,
        content_title,
        edit_rate,
        segments,
    })
}

fn parse_segment(node: &XmlNode, cpl_rate: Rational) -> Result<Segment> {
    let id = strip_urn_uuid(
        node.child_text("Id")
            .ok_or(Error::InvalidData("CPL Segment has no Id"))?,
    );
    let seq_list = node
        .child("SequenceList")
        .ok_or(Error::InvalidData("CPL Segment has no SequenceList"))?;

    let mut sequences = Vec::new();
    for &(name, kind) in &[
        ("MainImageSequence", SequenceKind::MainImage),
        ("MainAudioSequence", SequenceKind::MainAudio),
    ] {
        for seq_node in seq_list.children_named(name) {
            sequences.push(parse_sequence(seq_node, kind, cpl_rate)?);
        }
    }
    Ok(Segment { id, sequences })
}

fn parse_sequence(node: &XmlNode, kind: SequenceKind, cpl_rate: Rational) -> Result<Sequence> {
    let id = strip_urn_uuid(
        node.child_text("Id")
            .ok_or(Error::InvalidData("CPL Sequence has no Id"))?,
    );
    let track_id = strip_urn_uuid(
        node.child_text("TrackId")
            .ok_or(Error::InvalidData("CPL Sequence has no TrackId"))?,
    );
    let resource_list = node
        .child("ResourceList")
        .ok_or(Error::InvalidData("CPL Sequence has no ResourceList"))?;
    let mut resources = Vec::new();
    for res_node in resource_list.children_named("Resource") {
        resources.push(parse_resource(res_node, cpl_rate)?);
    }
    Ok(Sequence {
        id,
        track_id,
        kind,
        resources,
    })
}

fn parse_resource(node: &XmlNode, cpl_rate: Rational) -> Result<Resource> {
    let id = strip_urn_uuid(
        node.child_text("Id")
            .ok_or(Error::InvalidData("CPL Resource has no Id"))?,
    );
    let track_file_id = strip_urn_uuid(
        node.child_text("TrackFileId")
            .ok_or(Error::InvalidData("CPL Resource has no TrackFileId"))?,
    );
    if let Some(rate_text) = node.child_text("EditRate") {
        let resource_rate = parse_rate(rate_text)?;
        if resource_rate != cpl_rate {
            return Err(Error::Unsupported(
                "imf: a Resource whose own EditRate differs from the composition's is not supported",
            ));
        }
    }
    let intrinsic_duration = node
        .child_text("IntrinsicDuration")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .ok_or(Error::InvalidData("CPL Resource has no IntrinsicDuration"))?;
    let entry_point = node
        .child_text("EntryPoint")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let source_duration = node
        .child_text("SourceDuration")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(intrinsic_duration.saturating_sub(entry_point));
    let repeat_count = node
        .child_text("RepeatCount")
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(1);

    Ok(Resource {
        id,
        track_file_id,
        entry_point,
        source_duration,
        repeat_count,
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<CompositionPlaylist xmlns="http://www.smpte-ra.org/schemas/2067-3/2016">
  <Id>urn:uuid:11111111-1111-1111-1111-111111111111</Id>
  <ContentTitleText>Test Composition</ContentTitleText>
  <EditRate>24 1</EditRate>
  <SegmentList>
    <Segment>
      <Id>urn:uuid:22222222-2222-2222-2222-222222222222</Id>
      <SequenceList>
        <MainImageSequence>
          <Id>urn:uuid:33333333-3333-3333-3333-333333333333</Id>
          <TrackId>urn:uuid:44444444-4444-4444-4444-444444444444</TrackId>
          <ResourceList>
            <Resource>
              <Id>urn:uuid:55555555-5555-5555-5555-555555555555</Id>
              <TrackFileId>urn:uuid:cccccccc-2222-2222-2222-222222222222</TrackFileId>
              <EditRate>24 1</EditRate>
              <IntrinsicDuration>240</IntrinsicDuration>
              <EntryPoint>10</EntryPoint>
              <SourceDuration>100</SourceDuration>
            </Resource>
          </ResourceList>
        </MainImageSequence>
      </SequenceList>
    </Segment>
    <Segment>
      <Id>urn:uuid:66666666-6666-6666-6666-666666666666</Id>
      <SequenceList>
        <MainImageSequence>
          <Id>urn:uuid:77777777-7777-7777-7777-777777777777</Id>
          <TrackId>urn:uuid:44444444-4444-4444-4444-444444444444</TrackId>
          <ResourceList>
            <Resource>
              <Id>urn:uuid:88888888-8888-8888-8888-888888888888</Id>
              <TrackFileId>urn:uuid:dddddddd-3333-3333-3333-333333333333</TrackFileId>
              <IntrinsicDuration>50</IntrinsicDuration>
            </Resource>
          </ResourceList>
        </MainImageSequence>
      </SequenceList>
    </Segment>
  </SegmentList>
</CompositionPlaylist>"#;

    #[test]
    fn parses_edit_rate_and_two_segments() {
        let mut b = Budget::new(Limits::permissive());
        let cpl = parse(SAMPLE, &mut b).unwrap();
        assert_eq!(cpl.edit_rate, Rational::new(24, 1));
        assert_eq!(cpl.segments.len(), 2);
        assert_eq!(cpl.content_title.as_deref(), Some("Test Composition"));
    }

    #[test]
    fn resource_defaults_match_the_schema() {
        let mut b = Budget::new(Limits::permissive());
        let cpl = parse(SAMPLE, &mut b).unwrap();
        let r = &cpl.segments[1].sequences[0].resources[0];
        assert_eq!(r.entry_point, 0);
        assert_eq!(r.source_duration, 50); // IntrinsicDuration - EntryPoint(0)
        assert_eq!(r.repeat_count, 1);
    }

    #[test]
    fn virtual_track_concatenates_across_segments_in_order() {
        let mut b = Budget::new(Limits::permissive());
        let cpl = parse(SAMPLE, &mut b).unwrap();
        let tracks = cpl.virtual_tracks();
        assert_eq!(tracks.len(), 1);
        let track = &tracks[0];
        assert_eq!(track.track_id, "44444444-4444-4444-4444-444444444444");
        assert_eq!(track.resources.len(), 2);
        assert_eq!(
            track.resources[0].track_file_id,
            "cccccccc-2222-2222-2222-222222222222"
        );
        assert_eq!(track.resources[0].entry_point, 10);
        assert_eq!(track.resources[0].source_duration, 100);
        assert_eq!(
            track.resources[1].track_file_id,
            "dddddddd-3333-3333-3333-333333333333"
        );
    }

    #[test]
    fn a_resource_edit_rate_mismatch_is_unsupported() {
        let xml = SAMPLE.replace(
            "<EditRate>24 1</EditRate>\n              <IntrinsicDuration>240",
            "<EditRate>25 1</EditRate>\n              <IntrinsicDuration>240",
        );
        let mut b = Budget::new(Limits::permissive());
        let err = parse(&xml, &mut b).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn rejects_the_wrong_root_element() {
        let mut b = Budget::new(Limits::permissive());
        assert!(parse("<NotACompositionPlaylist/>", &mut b).is_err());
    }
}
