//! `MxfMuxer`: `OP1a`, one video track and at most one audio track,
//! frame-wrapped.
//!
//! # Partition layout and why no backpatch is needed for essence
//!
//! A "closed, complete" header partition (full structural metadata, no
//! `Duration`/Index yet) directly followed by essence — no separate body
//! partition pack, the same single-partition-carries-essence shape
//! `vaco-demux-mxf` had to learn to read for real D-10 files — then a
//! "closed, complete" footer partition restating the same graph (same
//! `InstanceUID`s) with the real `Duration` and a real Index Table Segment,
//! then a Random Index Pack.
//!
//! This needs no backpatch for the essence bytes themselves: they stream
//! straight to the sink as `write_packet` receives them. The **one** field
//! that must be correct for a seekable round trip is the header partition
//! pack's own `FooterPartition` — `vaco-demux-mxf::demux::MxfDemuxer::open`
//! uses it, not the Random Index Pack, to find the footer's restated graph
//! and its Index Table Segment (see that crate's `open`). It is `0`
//! (unknown) when `write_header` writes it, since the footer's position is
//! not known yet, and is corrected with a single small seek+overwrite in
//! `write_trailer` when the sink is seekable. On a non-seekable sink the
//! field stays `0`: the footer is still present and still sequentially
//! readable, but a reader that trusts `FooterPartition == 0` to mean
//! "there is no footer" (as `vaco-demux-mxf` itself does) will not go
//! looking for it — a real, honest degradation, not a silent one.

use vaco_codec_core::CodecParameters;
use vaco_core::{Error, MediaType, Rational, Result};
use vaco_format_core::{FormatOptions, Muxer, MuxerDesc};
use vaco_io::{IoOptions, IoWriter, MediaSink};
use vaco_packet::{Packet, PacketFlags};

use crate::index::{self, Entry};
use crate::klv;
use crate::localset;
use crate::metadata::{self, GraphIds, TrackIds, TrackPlan};
use crate::partition::{self, PartitionPackFields};
use crate::uid::IdGenerator;
use crate::ul;

/// Options this muxer accepts. Empty today (matching `vaco-mux-avi`'s own
/// `FormatOptions`-shaped constructor) — kept as a distinct type so a
/// future option (KAG size, an explicit edit rate for an audio-only file)
/// does not need a signature change.
#[derive(Debug, Clone, Default)]
pub struct MxfOptions;

/// The registry descriptor.
pub const MUXER: MuxerDesc = MuxerDesc {
    name: "mxf",
    long_name: "MXF (Material eXchange Format)",
    extensions: &["mxf"],
    default_video: Some(vaco_codec_core::CodecId::Mpeg2video),
    default_audio: None,
    open: open_muxer,
};

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(MxfMuxer::new(sink, &FormatOptions::default())?))
}

#[derive(Debug)]
pub struct MxfMuxer {
    out: IoWriter,
    pending: Vec<CodecParameters>,
    tracks: Vec<TrackPlan>,
    graph_ids: Option<GraphIds>,
    edit_rate: Rational,
    essence_container: ul::Ul,
    header_written: bool,
    trailer_written: bool,
    /// Absolute file offset of the header partition pack's own key (always
    /// `0`: this crate never writes anything before it).
    header_this_partition: u64,
    /// Absolute file offset of the `FooterPartition` field inside the
    /// header partition pack, for the seekable-sink backpatch.
    footer_field_pos: u64,
    /// Absolute offset of the first essence element's own key —
    /// `IndexEntryArray`'s `StreamOffset`s are relative to this, matching
    /// `vaco-demux-mxf`'s own measured convention.
    essence_origin: Option<u64>,
    video_entries: Vec<Entry>,
    /// Packets written per stream, by stream index — this file's real
    /// per-track duration once `write_trailer` runs.
    packet_counts: Vec<u64>,
}

impl MxfMuxer {
    /// # Errors
    /// Propagates buffer allocation failure from [`IoWriter`].
    pub fn new(sink: Box<dyn MediaSink>, _opts: &FormatOptions) -> Result<Self> {
        Ok(Self {
            out: IoWriter::new(sink, &IoOptions::default())?,
            pending: Vec::new(),
            tracks: Vec::new(),
            graph_ids: None,
            edit_rate: Rational { num: 25, den: 1 },
            essence_container: ul::ESSENCE_CONTAINER_MPEG_FRAME_WRAPPED,
            header_written: false,
            trailer_written: false,
            header_this_partition: 0,
            footer_field_pos: 0,
            essence_origin: None,
            video_entries: Vec::new(),
            packet_counts: Vec::new(),
        })
    }
}

impl Muxer for MxfMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.header_written {
            return Err(Error::InvalidData(
                "mxf: streams must be added before the header is written",
            ));
        }
        let media = params
            .media_type
            .ok_or(Error::Unsupported("mxf: stream has no media type"))?;
        if !matches!(media, MediaType::Video | MediaType::Audio) {
            return Err(Error::Unsupported("mxf: only video and audio streams"));
        }
        if media == MediaType::Video && self.pending.iter().any(|p| p.media_type == Some(MediaType::Video)) {
            return Err(Error::Unsupported(
                "mxf: this muxer writes at most one video track",
            ));
        }
        if media == MediaType::Audio && self.pending.iter().any(|p| p.media_type == Some(MediaType::Audio)) {
            return Err(Error::Unsupported(
                "mxf: this muxer writes at most one audio track",
            ));
        }
        let index = self.pending.len() as u32;
        self.pending.push(params.clone());
        self.packet_counts.push(0);
        Ok(index)
    }

    fn init(&mut self) -> Result<()> {
        if let Some(v) = self
            .pending
            .iter()
            .find(|p| p.media_type == Some(MediaType::Video))
            .and_then(|p| p.video.as_ref())
            && v.frame_rate.num > 0
            && v.frame_rate.den > 0
        {
            self.edit_rate = v.frame_rate;
        }

        let mut idgen = IdGenerator::new();
        let mut video_n = 0u32;
        let mut audio_n = 0u32;
        for (i, params) in self.pending.iter().enumerate() {
            let media = params.media_type.unwrap_or(MediaType::Data);
            let n = if media == MediaType::Audio {
                let v = audio_n;
                audio_n += 1;
                v
            } else {
                let v = video_n;
                video_n += 1;
                v
            };
            self.tracks.push(TrackPlan {
                media_type: media,
                params: params.clone(),
                // Track IDs start at 2, not 1: every real `ffmpeg -f mxf`
                // file measured this session reserves `TrackID = 1` for a
                // timecode track (`metadata::build_sets`'s own timecode
                // track uses it) — this is not just a spec-neutral
                // convention this crate is free to ignore, `ffmpeg`'s own
                // demuxer relies on it structurally: an earlier version of
                // this muxer that omitted the timecode track and started
                // essence tracks at `TrackID = 1` had its video stream
                // reported as `Data: mpeg2video` ("Codec type or id
                // mismatches") by a real `ffmpeg -i`, even though this
                // crate's own demuxer (which does not share that
                // assumption) read the same file correctly.
                track_id: (i as u32) + 2,
                gc_track_number: crate::essence::track_number(media, n),
                ids: TrackIds::new(&mut idgen),
            });
        }
        self.graph_ids = Some(GraphIds::new(&mut idgen, self.tracks.len()));
        Ok(())
    }

    fn stream_time_base(&self, stream_index: u32) -> Option<Rational> {
        let _ = stream_index;
        // Every track shares one edit-unit grid (this crate's documented
        // scope: one shared edit rate, see `metadata.rs`'s module docs) —
        // a packet's `pts`/`dts` in this time base *is* its edit unit
        // index, matching `vaco-demux-mxf`'s own read-side convention
        // ("pts == dts == the edit unit's position").
        Some(self.edit_rate)
    }

    fn write_header(&mut self) -> Result<()> {
        let ids = self
            .graph_ids
            .clone()
            .ok_or(Error::InvalidData("mxf: init() was not called"))?;

        let primer_bytes = localset::build_primer_pack(&metadata::primer_entries());
        let sets = metadata::build_sets(&ids, &self.tracks, self.edit_rate, self.essence_container, None);

        self.header_this_partition = self.out.pos();
        let header_byte_count = klv_len(&ul::primer_pack_key(), &primer_bytes)
            + sets
                .iter()
                .map(|(k, v)| klv_len(k, v))
                .sum::<u64>();

        let fields = PartitionPackFields {
            this_partition: self.header_this_partition,
            previous_partition: 0,
            footer_partition: 0,
            header_byte_count,
            index_byte_count: 0,
            index_sid: 0,
            body_offset: 0,
            body_sid: 1,
            operational_pattern: ul::OPERATIONAL_PATTERN_OP1A,
            essence_containers: vec![self.essence_container],
        };
        let key = ul::header_partition_key();
        klv::write(&mut self.out, &key, &{
            // Build once to know the exact value bytes, so
            // `footer_field_pos` below is computed from the same buffer
            // that is actually written (see `partition::write`'s own
            // layout, mirrored here only for the offset arithmetic).
            let mut v = Vec::new();
            v.extend_from_slice(&1u16.to_be_bytes());
            v.extend_from_slice(&2u16.to_be_bytes());
            v.extend_from_slice(&1u32.to_be_bytes());
            v.extend_from_slice(&fields.this_partition.to_be_bytes());
            v.extend_from_slice(&fields.previous_partition.to_be_bytes());
            v.extend_from_slice(&fields.footer_partition.to_be_bytes());
            v.extend_from_slice(&fields.header_byte_count.to_be_bytes());
            v.extend_from_slice(&fields.index_byte_count.to_be_bytes());
            v.extend_from_slice(&fields.index_sid.to_be_bytes());
            v.extend_from_slice(&fields.body_offset.to_be_bytes());
            v.extend_from_slice(&fields.body_sid.to_be_bytes());
            v.extend_from_slice(&fields.operational_pattern.as_bytes());
            v.extend_from_slice(&1u32.to_be_bytes());
            v.extend_from_slice(&16u32.to_be_bytes());
            v.extend_from_slice(&self.essence_container.as_bytes());
            v
        })?;
        // key(16) + BER length prefix(4, this crate's fixed form) precedes
        // the value; `FOOTER_PARTITION_FIELD_OFFSET` is the field's offset
        // within that value.
        self.footer_field_pos =
            self.header_this_partition + 20 + partition::FOOTER_PARTITION_FIELD_OFFSET;

        klv::write(&mut self.out, &ul::primer_pack_key(), &primer_bytes)?;
        for (key, value) in &sets {
            klv::write(&mut self.out, key, value)?;
        }

        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("mxf: packet written before the header"));
        }
        let idx = usize::try_from(packet.stream_index).unwrap_or(usize::MAX);
        let track = self
            .tracks
            .get(idx)
            .ok_or(Error::InvalidData("mxf: packet names an unknown stream"))?
            .clone();

        // One Generic Container System Item per essence element (see this
        // crate's `docs/format/vaco-mux-mxf.md` for why this is a
        // documented simplification rather than the real one-per-edit-unit
        // convention for a multi-track file): this crate's own reader
        // never interprets the System Item's content, only its key, so an
        // empty value is a real, valid KLV and costs nothing to parse
        // around.
        klv::write(&mut self.out, &ul::GC_SYSTEM_ITEM, &[])?;

        let pos = self.out.pos();
        if self.essence_origin.is_none() {
            self.essence_origin = Some(pos);
        }
        let origin = self.essence_origin.unwrap_or(pos);

        if track.media_type == MediaType::Video {
            self.video_entries.push(Entry {
                stream_offset: pos - origin,
                is_key_frame: packet.flags.contains(PacketFlags::KEY),
            });
        }
        if let Some(c) = self.packet_counts.get_mut(idx) {
            *c += 1;
        }

        let key = crate::essence::essence_key(track.gc_track_number);
        klv::write(&mut self.out, &key, packet.payload())
    }

    fn write_trailer(&mut self) -> Result<()> {
        const FOOTER_INDEX_SID: u32 = 2;

        if self.trailer_written {
            return Ok(());
        }
        if self.graph_ids.is_none() {
            return Err(Error::InvalidData("mxf: init() was not called"));
        }

        let duration = self.packet_counts.iter().copied().max().unwrap_or(0).cast_signed();

        // The footer restates nothing from the header: reused across both
        // scans by `vaco-demux-mxf::demux::MxfDemuxer::open` (one `primer`/
        // `resolver` pair, built once from the header, passed to the
        // footer's own `scan_region` call unchanged), a second primer pack
        // is redundant for this crate's own reader — and a real duplicate
        // in a real `ffmpeg 8.1` cross-check: `ffmpeg -i` on an early
        // version of this crate's output that *did* restate the full graph
        // logged "Multiple primer packs" and "Multiple packages_refs" and
        // then misreported the video stream's `codec_type` as `Data`
        // instead of `Video`. Writing the graph exactly once (in the
        // header) and letting the footer carry only the Index Table
        // Segment fixed both warnings and the misreport. The cost is that
        // `Sequence`/`SourceClip.Duration` stays `-1` ("not known when
        // written", a value ST 377-1 explicitly permits) for the life of
        // the file — acceptable because neither reader in this crate's
        // scope uses that property for the real duration: both derive it
        // from the Index Table Segment's own `IndexDuration`/entry count,
        // which this footer does state correctly.
        let footer_this_partition = self.out.pos();
        let (index_key, index_value) = index::build(
            {
                // A fresh instance UID for the Index Table Segment itself —
                // this crate does not need a stable generator handle here
                // since nothing else references this id.
                let mut g = IdGenerator::new();
                g.instance_uid()
            },
            (self.edit_rate.num, self.edit_rate.den),
            duration,
            FOOTER_INDEX_SID,
            1,
            &self.video_entries,
        );

        let index_byte_count = klv_len(&index_key, &index_value);

        let fields = PartitionPackFields {
            this_partition: footer_this_partition,
            previous_partition: self.header_this_partition,
            footer_partition: footer_this_partition,
            header_byte_count: 0,
            index_byte_count,
            index_sid: FOOTER_INDEX_SID,
            body_offset: 0,
            body_sid: 0,
            operational_pattern: ul::OPERATIONAL_PATTERN_OP1A,
            essence_containers: vec![self.essence_container],
        };
        partition::write(&mut self.out, &ul::footer_partition_key(), &fields)?;
        klv::write(&mut self.out, &index_key, &index_value)?;

        // Random Index Pack: `vaco-demux-mxf::partition::find_rip`'s
        // convention — `Count * (BodySID u32, ByteOffset u64)` entries then
        // the RIP's own total KLV length restated as the file's last 4
        // bytes.
        let mut rip = Vec::new();
        rip.extend_from_slice(&1u32.to_be_bytes()); // header partition's BodySID.
        rip.extend_from_slice(&self.header_this_partition.to_be_bytes());
        rip.extend_from_slice(&0u32.to_be_bytes()); // footer partition carries no essence.
        rip.extend_from_slice(&footer_this_partition.to_be_bytes());
        let rip_key = ul::random_index_pack_key();
        let rip_total_len = 16u32 + 4 + (rip.len() as u32) + 4; // key + 4-byte length prefix + value + trailer.
        rip.extend_from_slice(&rip_total_len.to_be_bytes());
        klv::write(&mut self.out, &rip_key, &rip)?;

        let real_end = self.out.pos();
        if self.out.is_seekable() {
            // The one backpatch this crate performs (see this module's
            // docs): `FooterPartition` was `0` when the header was written,
            // since the footer's position was not known yet. Seek back,
            // overwrite just that 8-byte field, then return to the real
            // end of the file — leaving the cursor mid-file after this
            // would silently truncate anything written later even though
            // nothing does today.
            self.out.seek(self.footer_field_pos)?;
            self.out.write(&footer_this_partition.to_be_bytes())?;
            self.out.seek(real_end)?;
        }
        self.out.flush()?;
        self.trailer_written = true;
        Ok(())
    }
}

/// The KLV byte length of one triplet: 16-byte key, this crate's own
/// 4-byte fixed BER length prefix (`crate::ber::encode`'s doc comment),
/// plus the value.
fn klv_len(_key: &[u8; 16], value: &[u8]) -> u64 {
    16 + 4 + value.len() as u64
}
