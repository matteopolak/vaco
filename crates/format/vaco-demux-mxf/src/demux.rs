//! [`MxfDemuxer`]: the four layers wired together into a [`Demuxer`].
//!
//! # Scope, stated plainly
//!
//! This demuxer supports **one essence track per `BodySID`** — the shape
//! every file in this crate's corpus has (`OP1a` and `OP-Atom` alike, both
//! single-video-track). MXF's index tables can describe several interleaved
//! tracks sharing one `BodySID` via `DeltaEntryArray`'s slice numbers; that
//! interleaving is not implemented, so a multi-essence-track `OP1a` file will
//! have its packets demuxed (the KLV walk is track-agnostic) but only the
//! first recognised track gets a seek index. See this crate's closing
//! report for the exact status of each layer.

use std::collections::HashMap;

use vaco_core::MediaType;
use vaco_core::{Duration, Error, Rational, Result, Timestamp};
use vaco_format_core::seek::{
    IndexEntry as FcIndexEntry, IndexFlags, PacketIndex, SeekFlags, SeekTarget,
};
use vaco_format_core::{Demuxer, ParserProvider, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource, Seekability};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

use crate::descriptor;
use crate::index::IndexTableSegment;
use crate::klv::{self, KlvHeader};
use crate::metadata::{self, MetadataGraph};
use crate::partition::{self, PartitionKind};
use crate::primer;
use crate::properties::Resolver;
use crate::ul::PartitionFamilyKind;

/// One packet's worth of essence, at most: a real-world frame this crate
/// has seen tops out around 100 KB; 256 MiB is generous headroom for an
/// uncompressed or very high-bitrate frame while still refusing a
/// declared-length attack before it is read.
const MAX_PACKET_BYTES: u64 = 256 * 1024 * 1024;

/// One recognised essence track: which stream it feeds and its Generic
/// Container track number.
struct TrackBinding {
    stream_index: u32,
    track_number: u32,
    edit_rate: Rational,
    next_edit_unit: i64,
}

pub struct MxfDemuxer {
    io: IoContext,
    budget: Budget,
    /// Essence-payload allocation is charged here and released immediately
    /// (mirroring `vaco-demux-mp4`'s `packet_budget`): this demuxer retains
    /// no packet after handing it back, so a cumulative cap on `budget`
    /// would otherwise refuse to read a file larger than the cap.
    packet_budget: Budget,
    streams: Vec<Stream>,
    format_metadata: Vec<(String, String)>,
    duration: Option<Duration>,
    bindings: Vec<TrackBinding>,
    /// Per-stream seek index, built from a measured file's Index Table
    /// Segment(s) — see `index` and `essence` module docs for how a
    /// `StreamOffset` becomes an absolute file position.
    indices: HashMap<u32, PacketIndex>,
    eof: bool,
}

impl std::fmt::Debug for MxfDemuxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MxfDemuxer")
            .field("streams", &self.streams.len())
            .field("eof", &self.eof)
            .finish_non_exhaustive()
    }
}

impl MxfDemuxer {
    /// Open an MXF source.
    ///
    /// `parsers` is accepted for interface symmetry with every other
    /// demuxer (D14.1: a format crate reaches bitstream parsers only through
    /// the injected [`ParserProvider`]) but is not yet called — this crate
    /// derives everything it reports about a stream from container-stated
    /// descriptor properties, never from parsing the elementary stream, so
    /// there is currently nothing to ask a parser for.
    ///
    /// # Errors
    /// [`Error::InvalidData`] if the source does not begin with a Header
    /// Partition Pack, or if required header metadata cannot be resolved.
    pub fn open(src: Box<dyn MediaSource>, parsers: &dyn ParserProvider) -> Result<Self> {
        let _ = parsers;
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let mut budget = Budget::new(Limits::permissive());

        let header_klv = klv::read_header(&mut io)?;
        let header_pp = partition::parse(&mut io, &mut budget, &header_klv)?;
        if header_pp.kind != PartitionKind::Header {
            return Err(Error::InvalidData(
                "mxf: file does not begin with a header partition pack",
            ));
        }

        let primer_klv = find_primer_pack(&mut io, &mut budget)?;
        let primer = primer::parse(&mut io, &mut budget, &primer_klv)?;
        let resolver = Resolver::new();

        let mut graph = MetadataGraph::default();
        let mut index_segments: Vec<IndexTableSegment> = Vec::new();
        metadata::scan_region(
            &mut io,
            &mut budget,
            &primer,
            &resolver,
            &mut graph,
            &mut index_segments,
        )?;
        let body_start = io.pos();

        let seekable = io.seekability() != Seekability::None;
        if seekable
            && header_pp.footer_partition != 0
            && header_pp.footer_partition != header_pp.this_partition
        {
            if let Ok(footer_klv) = seek_and_read_header(&mut io, header_pp.footer_partition)
                && let Ok(footer_pp) = partition::parse(&mut io, &mut budget, &footer_klv)
                && footer_pp.kind == PartitionKind::Footer
            {
                // Best-effort: a truncated or unusual footer must not stop
                // the file from opening with what the header already gave
                // us.
                let _ = metadata::scan_region(
                    &mut io,
                    &mut budget,
                    &primer,
                    &resolver,
                    &mut graph,
                    &mut index_segments,
                );
            }
            io.seek(body_start)?;
        }

        // Find the first essence element without consuming it, so its
        // offset can anchor every Index Table Segment's `StreamOffset` (see
        // `essence` module docs) before any packet is read. Only attempted
        // on a seekable source: the lookahead consumes bytes a forward-only
        // transport could never give back.
        let essence_origin = if seekable {
            let origin = find_first_essence_offset(&mut io, &mut budget)?;
            io.seek(body_start)?;
            origin
        } else {
            None
        };

        let (streams, bindings, format_metadata) = build_streams(&graph, &mut budget)?;

        let indices = essence_origin
            .map(|origin| build_indices(&bindings, &index_segments, origin))
            .unwrap_or_default();

        let duration = bindings
            .iter()
            .zip(streams.iter())
            .filter_map(|(b, s)| {
                let seg = index_segments
                    .iter()
                    .find(|seg| seg.index_edit_rate == Some(b.edit_rate))?;
                Timestamp::new(seg.index_duration).to_duration(s.time_base)
            })
            .max_by_key(|d: &Duration| d.as_micros());

        Ok(Self {
            io,
            budget,
            packet_budget: Budget::new(Limits::permissive()),
            streams,
            format_metadata,
            duration,
            bindings,
            indices,
            eof: false,
        })
    }
}

/// Read forward past any KLV Fill Item(s) to find the Primer Pack.
///
/// Measured: a real header partition pack is followed by a Filler (KAG
/// alignment padding) before the Primer Pack proper, not by the Primer Pack
/// directly — see `out.mxf`'s byte layout in the `ul` module docs. Bounded
/// by a small fixed iteration cap; a real file needs at most one or two
/// fillers here.
fn find_primer_pack(io: &mut IoContext, budget: &mut Budget) -> Result<KlvHeader> {
    for _ in 0..64u32 {
        budget.consume_fuel(1)?;
        let header = klv::read_header(io)?;
        if header.key.partition_family_kind() == Some(PartitionFamilyKind::Primer) {
            return Ok(header);
        }
        if header.key == crate::ul::KLV_FILL_ITEM {
            klv::skip_value(io, &header)?;
            continue;
        }
        return Err(Error::InvalidData(
            "mxf: expected a primer pack (or filler) after the header partition pack",
        ));
    }
    Err(Error::InvalidData(
        "mxf: too many fillers before a primer pack was found",
    ))
}

fn seek_and_read_header(io: &mut IoContext, offset: u64) -> Result<KlvHeader> {
    io.seek(offset)?;
    klv::read_header(io)
}

/// Walk forward from the current position, skipping partition packs, filler
/// and unrecognised KLVs, until an essence element is found. Does not
/// consume it — the caller seeks back.
fn find_first_essence_offset(io: &mut IoContext, budget: &mut Budget) -> Result<Option<u64>> {
    for _ in 0..1_000_000u32 {
        budget.consume_fuel(1)?;
        let start = io.pos();
        let header = match klv::read_header(io) {
            Ok(h) => h,
            Err(Error::UnexpectedEof) => return Ok(None),
            Err(e) => return Err(e),
        };
        if header.key.is_essence_element() {
            return Ok(Some(start));
        }
        if header.key.partition_family_kind() == Some(PartitionFamilyKind::RandomIndexPack) {
            return Ok(None);
        }
        klv::skip_value(io, &header)?;
    }
    Ok(None)
}

fn build_streams(
    graph: &MetadataGraph,
    budget: &mut Budget,
) -> Result<(Vec<Stream>, Vec<TrackBinding>, Vec<(String, String)>)> {
    let mut streams = Vec::new();
    let mut bindings = Vec::new();
    let mut format_metadata = Vec::new();

    let Some(material) = graph
        .of_class(crate::ul::StructuralClass::MaterialPackage)
        .next()
        .and_then(|s| s.instance_uid)
    else {
        // No Material Package resolved: an empty but valid demux (some
        // partial/growing files legitimately have no complete header yet).
        return Ok((streams, bindings, format_metadata));
    };

    if let Some(pref) = graph.preface()
        && let Some(op) = pref.get_ul(crate::properties::PropertyId::PrefaceOperationalPattern)
    {
        format_metadata.push(("operational_pattern_ul".to_owned(), op.to_string()));
    }

    let (tracks, timecodes) = metadata::resolve_essence(graph, material, budget)?;
    if let Some(tc) = timecodes.first() {
        format_metadata.push(("timecode".to_owned(), format_timecode(*tc)));
    }

    for track in tracks {
        if track.is_timecode {
            continue;
        }
        let Some(descriptor_id) = track.descriptor else {
            continue;
        };
        let Some(desc) = graph.get(descriptor_id) else {
            continue;
        };
        // MultipleDescriptor (several essence tracks behind one package): a
        // real, documented MXF shape this crate does not expand into
        // per-track descriptors yet (see this crate's closing report). The
        // stream is skipped rather than built with the wrong parameters.
        if matches!(desc.class, crate::ul::StructuralClass::Descriptor(0x44)) {
            continue;
        }
        // Only a picture descriptor is mapped to `CodecParameters` today
        // (see this crate's closing report): a sound or data descriptor is
        // recognised by class but has none of a picture descriptor's
        // properties, and is skipped rather than reported with guessed
        // parameters.
        if desc
            .get_u32(crate::properties::PropertyId::StoredWidth)
            .is_none()
            && desc
                .get_ul(crate::properties::PropertyId::PictureEssenceCoding)
                .is_none()
        {
            continue;
        }
        let params = descriptor::picture_parameters(desc);
        let media_type = MediaType::Video;
        let edit_rate = track.edit_rate.unwrap_or(Rational { num: 25, den: 1 });
        let time_base = Rational {
            num: edit_rate.den,
            den: edit_rate.num,
        };
        let index = streams.len() as u32;
        let mut stream = Stream::new(index, media_type, time_base);
        stream.params = params;
        stream.r_frame_rate = edit_rate;
        stream.avg_frame_rate = edit_rate;
        if let Some(id) = track.track_id {
            stream.id = Some(i64::from(id));
        }
        streams.push(stream);
        bindings.push(TrackBinding {
            stream_index: index,
            track_number: track.track_number.unwrap_or(0),
            edit_rate,
            next_edit_unit: 0,
        });
    }
    Ok((streams, bindings, format_metadata))
}

#[allow(
    clippy::integer_division,
    reason = "base is clamped to at least 1 above; this is timecode arithmetic, not a rounding bug"
)]
fn format_timecode(tc: metadata::Timecode) -> String {
    let base = i64::from(tc.base.max(1));
    let total_frames = tc.start.max(0);
    let hours = total_frames / (base * 3600);
    let minutes = (total_frames / (base * 60)) % 60;
    let seconds = (total_frames / base) % 60;
    let frames = total_frames % base;
    let sep = if tc.drop_frame { ';' } else { ':' };
    format!("{hours:02}:{minutes:02}:{seconds:02}{sep}{frames:02}")
}

fn build_indices(
    bindings: &[TrackBinding],
    segments: &[IndexTableSegment],
    essence_origin: u64,
) -> HashMap<u32, PacketIndex> {
    let mut out = HashMap::new();
    // Single-track-per-BodySID (see module docs): pair each binding with the
    // first segment whose edit rate matches, in order. Good enough for this
    // crate's corpus (one video track, one segment) and stated as a scope
    // limit rather than pretended to be a general BodySID/track match.
    for binding in bindings {
        let Some(seg) = segments
            .iter()
            .find(|s| s.index_edit_rate == Some(binding.edit_rate))
        else {
            continue;
        };
        let mut idx = PacketIndex::new();
        if seg.is_cbe() {
            let unit = u64::from(seg.edit_unit_byte_count);
            if unit == 0 {
                continue;
            }
            let count = u64::try_from(seg.index_duration).unwrap_or(0);
            for n in 0..count {
                let Some(rel) = seg.cbe_offset(n) else { break };
                let Ok(ticks) = i64::try_from(n) else { break };
                idx.add(FcIndexEntry {
                    pos: essence_origin.saturating_add(rel),
                    timestamp: Timestamp::new(ticks),
                    flags: IndexFlags::KEYFRAME,
                    size: unit.try_into().unwrap_or(u32::MAX),
                    min_distance: 0,
                });
            }
        } else {
            for (i, entry) in seg.entries.iter().enumerate() {
                let Ok(ticks) = i64::try_from(i) else { break };
                let size = seg.entries.get(i + 1).map_or(0, |next| {
                    next.stream_offset.saturating_sub(entry.stream_offset)
                });
                idx.add(FcIndexEntry {
                    pos: essence_origin.saturating_add(entry.stream_offset),
                    timestamp: Timestamp::new(ticks),
                    flags: if entry.is_key_frame() {
                        IndexFlags::KEYFRAME
                    } else {
                        IndexFlags::empty()
                    },
                    size: size.try_into().unwrap_or(u32::MAX),
                    min_distance: 0,
                });
            }
        }
        out.insert(binding.stream_index, idx);
    }
    out
}

impl Demuxer for MxfDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn metadata(&self) -> &[(String, String)] {
        &self.format_metadata
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }
        loop {
            let start = self.io.pos();
            let header = match klv::read_header(&mut self.io) {
                Ok(h) => h,
                Err(Error::UnexpectedEof) => {
                    self.eof = true;
                    return Err(Error::Eof);
                }
                Err(e) => return Err(e),
            };
            if header.key.partition_family_kind() == Some(PartitionFamilyKind::RandomIndexPack)
                || header.key.is_any_partition_pack()
            {
                if header.key.partition_family_kind() == Some(PartitionFamilyKind::RandomIndexPack)
                {
                    self.eof = true;
                    return Err(Error::Eof);
                }
                // A new partition: this crate's single-essence-track scope
                // (module docs) means there is nothing partition-specific to
                // do here beyond skipping past its own fixed-layout value;
                // essence elements are recognised by key regardless of which
                // partition carries them.
                self.budget.consume_fuel(1)?;
                klv::skip_value(&mut self.io, &header)?;
                continue;
            }
            if header.key == crate::ul::KLV_FILL_ITEM {
                klv::skip_value(&mut self.io, &header)?;
                continue;
            }
            if header.key.is_index_table_segment() {
                klv::skip_value(&mut self.io, &header)?;
                continue;
            }
            if !header.key.is_essence_element() {
                // Filler, the Generic Container System Item, or a vendor
                // extension this crate does not interpret — skipped, not
                // fatal (D6: demuxing is lenient).
                klv::skip_value(&mut self.io, &header)?;
                continue;
            }
            let track_number = header.key.track_number();
            let Some(binding_idx) = self
                .bindings
                .iter()
                .position(|b| b.track_number == track_number)
            else {
                klv::skip_value(&mut self.io, &header)?;
                continue;
            };
            if header.length > MAX_PACKET_BYTES {
                return Err(Error::LimitExceeded {
                    limit: "mxf_packet_bytes",
                    requested: header.length,
                    cap: MAX_PACKET_BYTES,
                });
            }
            let len = usize::try_from(header.length).map_err(|_| Error::LimitExceeded {
                limit: "mxf_packet_bytes",
                requested: header.length,
                cap: MAX_PACKET_BYTES,
            })?;
            let mut pkt = Packet::alloc(&mut self.packet_budget, len)?;
            self.packet_budget.release(len as u64);
            self.io.read_exact(pkt.payload_mut())?;
            let Some(binding) = self.bindings.get_mut(binding_idx) else {
                // Unreachable in practice (`binding_idx` came from `.position()`
                // on this same, unmutated vector a few lines up), but a direct
                // index would violate the workspace's `indexing_slicing` deny
                // regardless of that invariant, so this is the honest spelling.
                continue;
            };
            pkt.stream_index = binding.stream_index;
            let ticks = binding.next_edit_unit;
            binding.next_edit_unit = binding.next_edit_unit.saturating_add(1);
            pkt.pts = Timestamp::new(ticks);
            pkt.dts = Timestamp::new(ticks);
            pkt.pos = Some(start);
            pkt.flags = PacketFlags::empty();
            if let Some(idx) = self.indices.get(&binding.stream_index) {
                if let Some(e) = idx.entries().iter().find(|e| e.pos == start)
                    && e.is_key()
                {
                    pkt.flags |= PacketFlags::KEY;
                }
            } else if ticks == 0 {
                // No index available: the conservative default is to trust
                // only the very first packet as a keyframe.
                pkt.flags |= PacketFlags::KEY;
            }
            return Ok(pkt);
        }
    }

    fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()> {
        let SeekTarget::Timestamp { stream_index, ts } = target else {
            return Err(Error::Unsupported(
                "mxf: only timestamp seeks are implemented",
            ));
        };
        let Some(index) = self.indices.get(&stream_index) else {
            return Err(Error::NotSeekable);
        };
        let Some(entry) = index.search(ts, flags) else {
            return Err(Error::NotSeekable);
        };
        self.io.seek(entry.pos)?;
        self.eof = false;
        if let Some(binding) = self
            .bindings
            .iter_mut()
            .find(|b| b.stream_index == stream_index)
        {
            binding.next_edit_unit = entry.timestamp.ticks().unwrap_or(0);
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_format_core::discovery::NoParsers;
    use vaco_io::MemorySource;

    fn open_fixture(bytes: &'static [u8]) -> MxfDemuxer {
        let src = Box::new(MemorySource::new(bytes.to_vec()));
        MxfDemuxer::open(src, &NoParsers).unwrap()
    }

    #[test]
    fn op1a_fixture_reports_the_measured_stream_shape() {
        let demux = open_fixture(include_bytes!("../tests/fixtures/op1a_mpeg2_sample.mxf"));
        assert_eq!(demux.streams().len(), 1);
        let s = &demux.streams()[0];
        assert_eq!(s.media_type(), Some(MediaType::Video));
        let v = s.params.video.as_ref().unwrap();
        assert_eq!((v.width, v.height), (720, 576));
        assert_eq!(
            s.params.codec_id,
            Some(vaco_codec_core::CodecId::Mpeg2video)
        );
    }

    #[test]
    fn op1a_fixture_demuxes_three_packets_matching_measured_positions_and_sizes() {
        let mut demux = open_fixture(include_bytes!("../tests/fixtures/op1a_mpeg2_sample.mxf"));
        // Measured with `ffprobe -show_packets`: pos 6144/32768/47616,
        // sizes 26049/13853/1907.
        let expected = [(6144u64, 26049usize), (32768, 13853), (47616, 1907)];
        for (pos, size) in expected {
            let pkt = demux.read_packet().unwrap();
            assert_eq!(pkt.pos, Some(pos));
            assert_eq!(pkt.len, size);
        }
        assert!(matches!(demux.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn opatom_fixture_opens_and_demuxes() {
        let mut demux = open_fixture(include_bytes!("../tests/fixtures/opatom_mpeg2_sample.mxf"));
        assert_eq!(demux.streams().len(), 1);
        let pkt = demux.read_packet().unwrap();
        assert!(pkt.len > 0);
    }

    #[test]
    fn seeking_to_the_second_edit_unit_lands_on_its_measured_position() {
        let mut demux = open_fixture(include_bytes!("../tests/fixtures/op1a_mpeg2_sample.mxf"));
        // Measured: this 3-frame clip's Index Table Segment marks only edit
        // unit 0 as a keyframe (long-GOP MPEG-2 with one I-frame), so a seek
        // to edit unit 1 needs `ANY` — without it, `search` correctly reports
        // no usable keyframe at or after that point, which is not a seek
        // failure, it is an accurate answer about this GOP.
        demux
            .seek(
                SeekTarget::Timestamp {
                    stream_index: 0,
                    ts: Timestamp::new(1),
                },
                SeekFlags::ANY,
            )
            .unwrap();
        let pkt = demux.read_packet().unwrap();
        assert_eq!(pkt.pos, Some(32768));
    }

    #[test]
    fn seeking_backward_to_the_only_keyframe_works_without_any() {
        let mut demux = open_fixture(include_bytes!("../tests/fixtures/op1a_mpeg2_sample.mxf"));
        demux
            .seek(
                SeekTarget::Timestamp {
                    stream_index: 0,
                    ts: Timestamp::new(2),
                },
                SeekFlags::BACKWARD,
            )
            .unwrap();
        let pkt = demux.read_packet().unwrap();
        assert_eq!(pkt.pos, Some(6144));
    }

    #[test]
    fn a_non_mxf_source_is_rejected_not_panicked_on() {
        let src = Box::new(MemorySource::new(
            b"not an mxf file at all, just prose".to_vec(),
        ));
        assert!(MxfDemuxer::open(src, &NoParsers).is_err());
    }
}
