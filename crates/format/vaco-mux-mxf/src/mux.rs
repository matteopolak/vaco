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

use crate::ber;
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

/// The KLV Alignment Grid size every real `ffmpeg -f mxf`/`-f mxf_d10` file
/// this session measured uses (`klv::pad_to_kag`'s own doc comment has the
/// evidence: Fill Items padding the header region out to 512-byte
/// boundaries, but never between subsequent essence elements).
const KAG_SIZE: u64 = 512;

/// D-10's own Index Table Segment/partition `IndexSID` — an arbitrary but
/// fixed choice (only needs to be nonzero and match the value the header
/// partition pack itself states), mirroring `write_trailer`'s own
/// `OP1a`/OP-Atom.
///
/// Also the value every variant's `EssenceContainerData` set states for its
/// own `IndexSID` property (`metadata::build_sets`'s new argument) — this
/// crate always uses exactly one Index Table Segment per file, so the two
/// former separately-named constants collapsed into this one shared value.
const ESSENCE_INDEX_SID: u32 = 2;
/// Every variant's essence-carrying partition's own `BodySID` (D-10's
/// header, or `OP1a`'s/OP-Atom's Body Partition Pack) — also what an
/// `EssenceContainerData` set's own `BodySID` property states, since this
/// crate never writes more than one essence-carrying `BodySID` per file.
const ESSENCE_BODY_SID: u32 = 1;

/// Round `n` up to the next multiple of [`KAG_SIZE`] — the same arithmetic
/// `klv::pad_to_kag` performs on a live `IoWriter` position, extracted here
/// as a pure function so `build_d10_index_table` can predict D-10's
/// `EditUnitByteCount` before any byte of the edit unit exists.
#[allow(
    clippy::integer_division,
    clippy::cast_possible_wrap,
    reason = "rounding up to a multiple; the truncation is the point, not a bug, and KAG_SIZE (512) never approaches i64::MAX"
)]
fn round_up_to_kag(n: i64) -> i64 {
    let kag = KAG_SIZE as i64;
    if n <= 0 {
        return kag;
    }
    ((n + kag - 1) / kag) * kag
}

/// The registry descriptor: `OP1a`, `ffmpeg`'s default `-f mxf`.
pub const MUXER: MuxerDesc = MuxerDesc {
    name: "mxf",
    long_name: "MXF (Material eXchange Format)",
    extensions: &["mxf"],
    default_video: Some(vaco_codec_core::CodecId::Mpeg2video),
    default_audio: None,
    open: open_muxer,
};

/// D-10 (SMPTE 386M), matching `ffmpeg`'s own distinct `-f mxf_d10` muxer
/// name (`ffmpeg -muxers` lists `mxf`, `mxf_d10`, `mxf_opatom` as three
/// separate registered muxers, not one muxer with a variant option — see
/// `vaco-mux-asf`'s own `MUXER`/`MUXER_STREAM` pair for the same pattern
/// elsewhere in this workspace). Video-only in this crate today: D-10's
/// fixed 8-slot AES3 audio bundle (`4 + 1920×8×4 = 61444` bytes per edit
/// unit at 25fps, measured on the read side) is not yet implemented on the
/// write side — see `MxfVariant::D10`'s docs and
/// `docs/format/vaco-mux-mxf.md`.
pub const MUXER_D10: MuxerDesc = MuxerDesc {
    name: "mxf_d10",
    long_name: "MXF (Material eXchange Format) D-10 Mapping",
    extensions: &[],
    default_video: Some(vaco_codec_core::CodecId::Mpeg2video),
    default_audio: None,
    open: open_muxer_d10,
};

/// OP-Atom (SMPTE 390), matching `ffmpeg`'s own `-f mxf_opatom`.
pub const MUXER_OPATOM: MuxerDesc = MuxerDesc {
    name: "mxf_opatom",
    long_name: "MXF (Material eXchange Format) Operational Pattern Atom",
    extensions: &[],
    default_video: Some(vaco_codec_core::CodecId::Mpeg2video),
    default_audio: None,
    open: open_muxer_opatom,
};

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(MxfMuxer::new(sink, &FormatOptions::default())?))
}

fn open_muxer_d10(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(MxfMuxer::new_variant(sink, &FormatOptions::default(), ul::MxfVariant::D10)?))
}

fn open_muxer_opatom(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(MxfMuxer::new_variant(sink, &FormatOptions::default(), ul::MxfVariant::OpAtom)?))
}

#[derive(Debug)]
pub struct MxfMuxer {
    out: IoWriter,
    pending: Vec<CodecParameters>,
    tracks: Vec<TrackPlan>,
    graph_ids: Option<GraphIds>,
    edit_rate: Rational,
    header_written: bool,
    trailer_written: bool,
    /// Absolute file offset of the header partition pack's own key (always
    /// `0`: this crate never writes anything before it).
    header_this_partition: u64,
    /// Absolute file offset of the `FooterPartition` field inside every
    /// partition pack written before the footer (the header, and a body
    /// partition pack when more than one essence track is written) —
    /// each gets the same seekable-sink backpatch. A real `ffmpeg -f mxf`
    /// file backpatches every partition's own `FooterPartition`, not just
    /// the header's (measured this session); `vaco-demux-mxf`'s own reader
    /// only ever checks the header's, so this is for other readers.
    footer_field_positions: Vec<u64>,
    /// `(BodySID, this_partition offset)` for every partition pack written
    /// so far except the footer (header, and the body partition pack when
    /// one is written) — the Random Index Pack's own entries, in file
    /// order. Measured against three real fixtures this session (an `OP1a`
    /// file, a D-10 file, an OP-Atom file): a real RIP has one entry per
    /// partition pack in the file, each stating *that partition's own*
    /// `BodySID` field, not a value assumed from which partition kind it
    /// is — the header's own entry is `BodySID = 0` for OP1a/OP-Atom
    /// (their header carries no essence) but `BodySID = 1` for D-10
    /// (whose header carries essence directly, no body partition at all).
    /// An earlier version of this crate's RIP hardcoded two entries
    /// (header stated as `BodySID = 1`, footer as `0`) and never entered
    /// one for the Body Partition Pack at all — wrong on both counts for
    /// OP1a/OP-Atom, coincidentally close to right only for D-10's
    /// no-body-partition shape.
    rip_entries: Vec<(u32, u64)>,
    /// Absolute offset of the first essence element's own key —
    /// `IndexEntryArray`'s `StreamOffset`s are relative to this, matching
    /// `vaco-demux-mxf`'s own measured convention.
    essence_origin: Option<u64>,
    video_entries: Vec<Entry>,
    /// Packets written per stream, by stream index — this file's real
    /// per-track duration once `write_trailer` runs.
    packet_counts: Vec<u64>,
    /// The edit-unit tick (`Packet::pts`) the last-written Generic Container
    /// System Item covers, so a second track's packet for the *same* edit
    /// unit does not get a redundant one — matching a real file's own
    /// convention (measured this session against a real two-track `ffmpeg
    /// -f mxf` file: exactly one System Item per edit unit, immediately
    /// before that unit's first essence element, shared across every
    /// track). `None` until the first packet is written.
    last_system_item_pts: Option<i64>,
    /// Which real `ffmpeg` MXF muxer this instance imitates — see
    /// [`ul::MxfVariant`].
    variant: ul::MxfVariant,
    /// `Some` only for [`ul::MxfVariant::OpAtom`]: OP-Atom's essence is
    /// clip-wrapped (one Generic Container element for the whole file, not
    /// one per frame — measured this session against a real `ffmpeg -f
    /// mxf_opatom` file, see `write_trailer`'s own docs), so every packet's
    /// payload is appended here instead of going straight to `self.out`,
    /// and the single element is written for real once `write_trailer`
    /// knows its final length. `None` for `OP1a`/D-10, whose essence streams
    /// straight to the sink per packet as it always has.
    clip_buffer: Option<Vec<u8>>,
}

impl MxfMuxer {
    /// # Errors
    /// Propagates buffer allocation failure from [`IoWriter`].
    pub fn new(sink: Box<dyn MediaSink>, opts: &FormatOptions) -> Result<Self> {
        Self::new_variant(sink, opts, ul::MxfVariant::Op1a)
    }

    /// As [`Self::new`], but for a non-`OP1a` variant (`open_muxer_d10`/
    /// `open_muxer_opatom`, the crate's two other registered muxer names).
    ///
    /// # Errors
    /// Propagates buffer allocation failure from [`IoWriter`].
    pub(crate) fn new_variant(
        sink: Box<dyn MediaSink>,
        _opts: &FormatOptions,
        variant: ul::MxfVariant,
    ) -> Result<Self> {
        Ok(Self {
            out: IoWriter::new(sink, &IoOptions::default())?,
            pending: Vec::new(),
            tracks: Vec::new(),
            graph_ids: None,
            edit_rate: Rational { num: 25, den: 1 },
            header_written: false,
            trailer_written: false,
            header_this_partition: 0,
            footer_field_positions: Vec::new(),
            rip_entries: Vec::new(),
            essence_origin: None,
            video_entries: Vec::new(),
            packet_counts: Vec::new(),
            last_system_item_pts: None,
            variant,
            clip_buffer: None,
        })
    }

    /// D-10's `EditUnitByteCount`, computed analytically from the video
    /// track's declared `bit_rate` and this file's shared edit rate — no
    /// packet has arrived yet when `write_header` needs this (D-10's Index
    /// Table Segment is embedded in the header, not deferred to the
    /// footer), so it cannot be measured from an actual frame the way
    /// `OP1a`'s/OP-Atom's VBE index is. Measured against a real 30 Mbit/s
    /// `ffmpeg -f mxf_d10` fixture: `EditUnitByteCount = 151040 =
    /// round_up_to_kag(20) [empty-value System Item KLV] +
    /// round_up_to_kag(20 + 150000) [essence KLV]` — i.e. the System Item
    /// and the essence element are each independently padded out to the
    /// next 512-byte boundary, not the edit unit as a whole; `write_packet`
    /// reproduces the same two-pad shape per edit unit.
    fn build_d10_index_table(&self) -> Result<([u8; 16], Vec<u8>)> {
        let track = self
            .tracks
            .first()
            .ok_or(Error::InvalidData("mxf_d10: no essence track"))?;
        let bit_rate = track
            .params
            .bit_rate
            .ok_or(Error::Unsupported("mxf_d10: the video stream needs an explicit bit_rate"))?;
        let fps_num = i64::from(self.edit_rate.num).max(1);
        let fps_den = i64::from(self.edit_rate.den).max(1);
        #[allow(
            clippy::integer_division,
            reason = "exact CBR frame-byte-count arithmetic (bit_rate * fps_den / (8 * fps_num)); a real D-10 bit_rate is always an exact multiple of 8 * fps_num for a standard frame rate, so this never truncates a real value"
        )]
        let frame_bytes =
            i64::try_from(bit_rate).unwrap_or(i64::MAX).saturating_mul(fps_den) / (8 * fps_num);
        let system_item_block = round_up_to_kag(20);
        let essence_block = round_up_to_kag(20 + frame_bytes);
        let edit_unit_byte_count =
            u32::try_from(system_item_block + essence_block).unwrap_or(u32::MAX);

        let mut g = IdGenerator::new();
        Ok(index::build_cbe(
            g.instance_uid(),
            (self.edit_rate.num, self.edit_rate.den),
            edit_unit_byte_count,
            ESSENCE_INDEX_SID,
            ESSENCE_BODY_SID,
        ))
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
        if self.variant == ul::MxfVariant::OpAtom && !self.pending.is_empty() {
            return Err(Error::Unsupported(
                "mxf_opatom: exactly one essence track per file (OP-Atom's own defining constraint)",
            ));
        }
        if self.variant == ul::MxfVariant::D10 && media == MediaType::Audio {
            return Err(Error::Unsupported(
                "mxf_d10: audio is not yet implemented by this muxer (see docs/format/vaco-mux-mxf.md)",
            ));
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
                gc_track_number: if self.variant == ul::MxfVariant::D10 {
                    crate::essence::track_number_d10(n)
                } else {
                    crate::essence::track_number(media, n)
                },
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
        let sets = metadata::build_sets(
            &ids,
            &self.tracks,
            self.edit_rate,
            None,
            self.variant,
            ESSENCE_BODY_SID,
            ESSENCE_INDEX_SID,
        );
        let essence_containers: Vec<ul::Ul> =
            metadata::essence_containers_used(&self.tracks, self.variant)
                .into_iter()
                .map(ul::Ul::new)
                .collect();

        self.header_this_partition = self.out.pos();

        // D-10 carries essence directly in the header (`body_sid = 1`, no
        // separate Body Partition Pack — measured: a real `ffmpeg -f
        // mxf_d10` file has none, unlike `OP1a`/OP-Atom) and additionally
        // embeds a complete CBE Index Table Segment right there too
        // (measured: `IndexDuration = 0`, `EditUnitByteCount` nonzero,
        // computed entirely upfront since D-10 is CBR — no footer deferral
        // needed the way `OP1a`/OP-Atom's VBE index requires). Everything
        // else keeps the header essence-free (`body_sid = 0`).
        let d10_index = if self.variant == ul::MxfVariant::D10 {
            Some(self.build_d10_index_table()?)
        } else {
            None
        };
        let (body_sid, index_sid) = match &d10_index {
            Some(_) => (ESSENCE_BODY_SID, ESSENCE_INDEX_SID),
            None => (0u32, 0u32),
        };

        let header_byte_count = klv_len_minimal(&ul::primer_pack_key(), &primer_bytes)
            + sets.iter().map(|(k, v)| klv_len_structural(k, v)).sum::<u64>()
            + d10_index.as_ref().map_or(0, |(k, v)| klv_len(k, v));
        let index_byte_count = d10_index.as_ref().map_or(0, |(k, v)| klv_len(k, v));

        let fields = PartitionPackFields {
            kag_size: KAG_SIZE as u32,
            this_partition: self.header_this_partition,
            previous_partition: 0,
            footer_partition: 0,
            header_byte_count,
            index_byte_count,
            index_sid,
            body_offset: 0,
            body_sid,
            operational_pattern: ul::operational_pattern_for(self.variant),
            essence_containers,
        };
        partition::write(&mut self.out, &ul::header_partition_key(), &fields)?;
        // key(16) + BER length prefix(4, this crate's fixed form) precedes
        // the value; `FOOTER_PARTITION_FIELD_OFFSET` is the field's offset
        // within that value, and does not depend on how many essence
        // containers the batch above lists (that batch comes after the
        // field in the fixed layout) — safe to compute independently of
        // the buffer `partition::write` actually built.
        self.footer_field_positions
            .push(self.header_this_partition + 20 + partition::FOOTER_PARTITION_FIELD_OFFSET);
        self.rip_entries.push((body_sid, self.header_this_partition));
        klv::pad_to_kag(&mut self.out, KAG_SIZE)?;

        klv::write_minimal(&mut self.out, &ul::primer_pack_key(), &primer_bytes)?;
        klv::pad_to_kag(&mut self.out, KAG_SIZE)?;
        for (key, value) in &sets {
            klv::write_structural_set(&mut self.out, key, value)?;
        }
        klv::pad_to_kag(&mut self.out, KAG_SIZE)?;
        if let Some((key, value)) = &d10_index {
            klv::write(&mut self.out, key, value)?;
            klv::pad_to_kag(&mut self.out, KAG_SIZE)?;
        }

        // A genuine Body Partition Pack, distinct from the header, right
        // before essence begins — `OP1a` and OP-Atom alike (measured this
        // session against a real `ffmpeg -f mxf_opatom` file too: same
        // relative position as `OP1a`'s). D-10 never gets one (see above).
        if self.variant != ul::MxfVariant::D10 {
            // Corrected this session: an earlier version wrote one only for
            // more than one essence track, on the strength of this crate's
            // own D-10 corpus (single-partition, essence directly in the
            // header) — but a literal `cmp` against a real single-track
            // `ffmpeg -f mxf -fflags +bitexact` file showed a body
            // partition there too (`op1a_mpeg2_sample.mxf`, this crate's
            // own single-track fixture, has one at the same relative
            // position once checked properly). D-10's single-partition
            // shape is real for `-f mxf_d10` specifically, not for `OP1a`'s
            // `-f mxf` — the muxers are not the same shape.
            let body_this_partition = self.out.pos();
            let body_fields = PartitionPackFields {
                kag_size: KAG_SIZE as u32,
                this_partition: body_this_partition,
                previous_partition: self.header_this_partition,
                footer_partition: 0,
                header_byte_count: 0,
                index_byte_count: 0,
                index_sid: 0,
                body_offset: 0,
                body_sid: ESSENCE_BODY_SID,
                operational_pattern: ul::operational_pattern_for(self.variant),
                essence_containers: metadata::essence_containers_used(&self.tracks, self.variant)
                    .into_iter()
                    .map(ul::Ul::new)
                    .collect(),
            };
            partition::write(&mut self.out, &ul::body_partition_key(), &body_fields)?;
            self.footer_field_positions
                .push(body_this_partition + 20 + partition::FOOTER_PARTITION_FIELD_OFFSET);
            self.rip_entries.push((ESSENCE_BODY_SID, body_this_partition));
            klv::pad_to_kag(&mut self.out, KAG_SIZE)?;
        }

        if self.variant == ul::MxfVariant::OpAtom {
            // OP-Atom's essence is clip-wrapped (see `write_trailer`'s own
            // docs) — buffer it here instead of streaming it, so its final
            // length is known before the one Generic Container element is
            // actually written.
            self.clip_buffer = Some(Vec::new());
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

        // OP-Atom's essence is clip-wrapped: buffer the payload, record
        // where it landed within the eventual single element, and stop —
        // none of the frame-wrapped machinery below (System Items, a KAG
        // pad per essence element, an essence key per packet) applies (see
        // `write_trailer`'s own docs for why no System Item was found in a
        // real `ffmpeg -f mxf_opatom` file either).
        if let Some(buf) = self.clip_buffer.as_mut() {
            let offset = buf.len() as u64;
            buf.extend_from_slice(packet.payload());
            if track.media_type == MediaType::Video {
                self.video_entries.push(Entry {
                    stream_offset: offset,
                    is_key_frame: packet.flags.contains(PacketFlags::KEY),
                });
            }
            if let Some(c) = self.packet_counts.get_mut(idx) {
                *c += 1;
            }
            return Ok(());
        }

        // One Generic Container System Item per edit unit, shared across
        // every track (measured against a real two-track file — see
        // `last_system_item_pts`'s doc comment): this crate's own reader
        // never interprets the System Item's content, only its key, so an
        // empty value is a real, valid KLV and costs nothing to parse
        // around. `Packet::pts` is the edit-unit tick in this crate's own
        // time base (`stream_time_base` returns the shared edit rate), so
        // comparing raw ticks across tracks is correct only because every
        // track shares one edit rate (this crate's own documented scope).
        let edit_unit = packet.pts.ticks();
        let first_ever_packet = self.essence_origin.is_none();
        let is_d10 = self.variant == ul::MxfVariant::D10;
        if edit_unit.is_none() || edit_unit != self.last_system_item_pts {
            klv::write(&mut self.out, &ul::GC_SYSTEM_ITEM, &[])?;
            self.last_system_item_pts = edit_unit;
            // D-10 pads the System Item's own KLV out to the KAG grid
            // separately from the essence element that follows it (measured
            // against a real `ffmpeg -f mxf_d10` file: a Fill Item sits
            // between the two, not just after the essence element) — see
            // `build_d10_index_table`'s own doc comment for the exact
            // arithmetic this reproduces.
            if is_d10 {
                klv::pad_to_kag(&mut self.out, KAG_SIZE)?;
            }
        }
        // `OP1a`/D-10 both KAG-align before the essence element: `OP1a` only
        // once, right before the very first essence element in the whole
        // file (measured: the header region is padded to 512-byte
        // boundaries, subsequent essence elements are not); D-10 does it
        // before every single one (measured: every edit unit's essence
        // element starts on its own 512-byte boundary, not just the
        // file's first).
        if first_ever_packet || is_d10 {
            klv::pad_to_kag(&mut self.out, KAG_SIZE)?;
        }

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
        klv::write(&mut self.out, &key, packet.payload())?;
        // D-10's essence element is itself padded out to the KAG grid too
        // (see the comment above `first_ever_packet || is_d10`): the next
        // edit unit's System Item always starts on a fresh 512-byte
        // boundary, not just packed immediately after this element ends.
        if is_d10 {
            klv::pad_to_kag(&mut self.out, KAG_SIZE)?;
        }
        Ok(())
    }

    fn write_trailer(&mut self) -> Result<()> {

        if self.trailer_written {
            return Ok(());
        }
        if self.graph_ids.is_none() {
            return Err(Error::InvalidData("mxf: init() was not called"));
        }

        let duration = self.packet_counts.iter().copied().max().unwrap_or(0).cast_signed();

        // OP-Atom's essence is clip-wrapped: exactly one Generic Container
        // element for the whole file (measured this session against a real
        // `ffmpeg -f mxf_opatom` file: a single essence key appears once,
        // its own BER length stating the entire buffered payload, and no
        // System Item key appears anywhere — OP-Atom needs no per-edit-unit
        // sync marker since there is only ever one essence stream and no
        // interleaving to resynchronise). `write_packet` buffered every
        // packet's payload into `self.clip_buffer` instead of streaming it;
        // now that every packet has arrived and the final length is known,
        // write the one real element. Every `Entry::stream_offset`
        // `write_packet` already recorded is relative to `clip_buffer`'s
        // own start, which is exactly this element's value-start position
        // — the same "relative to the essence container's start" convention
        // `vaco-demux-mxf::index::IndexTableEntry::stream_offset` documents.
        if let Some(buf) = self.clip_buffer.take() {
            let track = self
                .tracks
                .first()
                .ok_or(Error::InvalidData("mxf_opatom: no essence track"))?;
            let key = crate::essence::essence_key(track.gc_track_number);
            klv::write(&mut self.out, &key, &buf)?;
            klv::pad_to_kag(&mut self.out, KAG_SIZE)?;
        }

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
        //
        // D-10 already embedded its (CBE) Index Table Segment in the
        // header (`build_d10_index_table`) — this footer carries no index
        // at all for D-10, matching a real `ffmpeg -f mxf_d10` file's own
        // minimal footer (just the partition pack and the Random Index
        // Pack, measured this session).
        let footer_this_partition = self.out.pos();
        let is_d10 = self.variant == ul::MxfVariant::D10;
        let footer_index = (!is_d10).then(|| {
            index::build(
                {
                    // A fresh instance UID for the Index Table Segment
                    // itself — this crate does not need a stable generator
                    // handle here since nothing else references this id.
                    let mut g = IdGenerator::new();
                    g.instance_uid()
                },
                (self.edit_rate.num, self.edit_rate.den),
                duration,
                ESSENCE_INDEX_SID,
                ESSENCE_BODY_SID,
                &self.video_entries,
            )
        });

        let index_byte_count = footer_index.as_ref().map_or(0, |(k, v)| klv_len(k, v));

        let fields = PartitionPackFields {
            kag_size: KAG_SIZE as u32,
            this_partition: footer_this_partition,
            previous_partition: self.header_this_partition,
            footer_partition: footer_this_partition,
            header_byte_count: 0,
            index_byte_count,
            index_sid: if is_d10 { 0 } else { ESSENCE_INDEX_SID },
            body_offset: 0,
            body_sid: 0,
            operational_pattern: ul::operational_pattern_for(self.variant),
            essence_containers: metadata::essence_containers_used(&self.tracks, self.variant)
                .into_iter()
                .map(ul::Ul::new)
                .collect(),
        };
        partition::write(&mut self.out, &ul::footer_partition_key(), &fields)?;
        klv::pad_to_kag(&mut self.out, KAG_SIZE)?;
        if let Some((index_key, index_value)) = &footer_index {
            klv::write(&mut self.out, index_key, index_value)?;
            klv::pad_to_kag(&mut self.out, KAG_SIZE)?;
        }

        // Random Index Pack: `vaco-demux-mxf::partition::find_rip`'s
        // convention — `Count * (BodySID u32, ByteOffset u64)` entries then
        // the RIP's own total KLV length restated as the file's last 4
        // bytes. One entry per partition pack actually written, each
        // stating *that partition's own* `BodySID`, in file order —
        // measured against three real fixtures this session (an earlier
        // version hardcoded two entries, header stated as `BodySID = 1`
        // unconditionally and no entry for the Body Partition Pack at
        // all; `rip_entries`'s own doc comment has the full account).
        let mut rip = Vec::new();
        for &(body_sid, offset) in &self.rip_entries {
            rip.extend_from_slice(&body_sid.to_be_bytes());
            rip.extend_from_slice(&offset.to_be_bytes());
        }
        rip.extend_from_slice(&0u32.to_be_bytes()); // the footer itself carries no essence.
        rip.extend_from_slice(&footer_this_partition.to_be_bytes());
        let rip_key = ul::random_index_pack_key();
        // key(16) + this KLV's own minimal-width length prefix + value +
        // the trailing 4-byte restated total itself.
        let rip_prefix_width = ber::encode_minimal((rip.len() + 4) as u64).as_slice().len() as u32;
        let rip_total_len = 16 + rip_prefix_width + (rip.len() as u32) + 4;
        rip.extend_from_slice(&rip_total_len.to_be_bytes());
        klv::write_minimal(&mut self.out, &rip_key, &rip)?;

        let real_end = self.out.pos();
        if self.out.is_seekable() {
            // The backpatch this crate performs (see this module's docs):
            // every partition pack's own `FooterPartition` was `0` when it
            // was written, since the footer's position was not known yet.
            // Seek back, overwrite just that 8-byte field in each one, then
            // return to the real end of the file — leaving the cursor
            // mid-file after this would silently truncate anything written
            // later even though nothing does today.
            for &pos in &self.footer_field_positions {
                self.out.seek(pos)?;
                self.out.write(&footer_this_partition.to_be_bytes())?;
            }
            self.out.seek(real_end)?;
        }
        self.out.flush()?;
        self.trailer_written = true;
        Ok(())
    }
}

/// The KLV byte length of one triplet: 16-byte key, this crate's own
/// fixed-width BER length prefix (`crate::ber::encode`'s doc comment),
/// plus the value. For a KLV `klv::write` (not `write_minimal`/
/// `write_structural_set`) actually writes.
fn klv_len(_key: &[u8; 16], value: &[u8]) -> u64 {
    16 + ber::encode(value.len() as u64).as_slice().len() as u64 + value.len() as u64
}

/// As [`klv_len`], but for a KLV `klv::write_minimal` writes.
fn klv_len_minimal(_key: &[u8; 16], value: &[u8]) -> u64 {
    16 + ber::encode_minimal(value.len() as u64).as_slice().len() as u64 + value.len() as u64
}

/// As [`klv_len`], but for a structural-metadata set `klv::
/// write_structural_set` writes — mirrors that function's own
/// fixed-vs-minimal class-byte switch exactly, so `header_byte_count`
/// states the same total the header region's own bytes actually add up to.
fn klv_len_structural(key: &[u8; 16], value: &[u8]) -> u64 {
    let class = key[14];
    if matches!(
        class,
        ul::class::MPEG_VIDEO_DESCRIPTOR | ul::class::AES3_PCM_DESCRIPTOR | ul::class::CDCI_ESSENCE_DESCRIPTOR
    ) {
        klv_len(key, value)
    } else {
        klv_len_minimal(key, value)
    }
}
