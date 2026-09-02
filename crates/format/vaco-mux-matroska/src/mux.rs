//! [`MatroskaMuxer`]: the shared implementation behind the `matroska` and
//! `webm` registrations.
//!
//! # Metadata, chapters and attachments (M30, gap 1)
//!
//! [`Muxer::set_metadata`] stores the [`MuxMetadata`] it is handed; the
//! actual `Tags`/`Chapters`/`Attachments` elements are built in
//! [`MatroskaMuxer::write_header`], right after `Tracks` — matching the
//! element order measured against `ffmpeg 8.1` (`Info`, `Tracks`,
//! `Chapters`, `Attachments`, `Tags`, then the first `Cluster`).
//!
//! Three keys route to a dedicated element instead of a `SimpleTag`, each
//! measured directly (`ebmldump`-style byte inspection of `ffmpeg -metadata
//! title=... -metadata:s:v:0 language=eng -metadata:s:v:0 title=...`):
//!
//! * A file-level `title` tag becomes `Info > Title`, not a `Tags` entry.
//! * A per-stream `title` tag becomes that `TrackEntry`'s `Name` (`0x536E`).
//! * A per-stream `language` tag becomes that `TrackEntry`'s `Language`,
//!   replacing the `"und"` default — [`crate::codec`] and the rest of this
//!   file are otherwise unaware any language was ever stated.
//!
//! Every other tag becomes a `SimpleTag` inside a `Tag`: one `Tag` with an
//! empty `Targets` for file-level tags, one `Tag` per stream that has any
//! left over with `Targets > TagTrackUID` naming it. `TagName` is the
//! caller's key **uppercased** — measured: `-metadata artist=X` writes
//! `TagName=ARTIST`, not `TagName=artist`. This crate does not reproduce the
//! reference's own auto `ENCODER`/`DURATION` `SimpleTag`s (those stamp the
//! reference's own build identity and a duration this trait cannot see
//! ahead of time; `Info > WritingApp` already carries this crate's own
//! identity).
//!
//! Chapters map [`vaco_core::Chapter`] fields directly: `ChapterUID` is the
//! chapter's `id` when positive (matching the reference, which — for a
//! `[CHAPTER]` script with no explicit `id` — numbers chapters `1, 2, ...` in
//! order) or the chapter's 1-based position otherwise; `ChapterTimeStart`/
//! `ChapterTimeEnd` are the timestamps rescaled to nanoseconds (RFC 9559's
//! unit for these two fields, independent of `TimestampScale`); a `title` key
//! in the chapter's own metadata becomes `ChapterDisplay > ChapString`, and a
//! `language` key becomes `ChapLanguage` (default `"und"`).
//!
//! Attachments map [`vaco_format_core::MuxAttachment`] directly onto
//! `AttachedFile`: `filename` → `FileName`, `mime_type` → `FileMimeType`,
//! `description` → `FileDescription` (omitted when empty), `data` →
//! `FileData`. `FileUID` has no caller-supplied source (`MuxAttachment` has
//! no UID field) and is derived deterministically from the attachment's
//! position and filename rather than drawn from a clock or an RNG — neither
//! is reachable from `wasm32` and a random `FileUID` would make output
//! non-reproducible under `-fflags +bitexact`, which is exactly the failure
//! mode this crate's own module docs already record for `DateUTC`. `webm`
//! measured as rejecting attachments outright (the reference silently drops
//! the input stream); this crate does not special-case that — `webm` has no
//! attachment allow-list the way `codec::webm_allows_video` does for tracks,
//! so an attachment handed to a `webm` output is written anyway rather than
//! silently dropped, which is the more honest failure (a reader ignores an
//! element it does not expect; a silent drop looks like the caller's data
//! vanished).
//!
//! `Cues` needs no such channel — every field it carries comes from the
//! packets themselves — so it was already implemented in full.
//!
//! # `CRC-32` and `SeekHead` (CONFORMANCE-FINDINGS 15)
//!
//! Both measured directly against `ffmpeg 8.1`, `-bitexact`, on
//! `ebmldump`-style byte inspection of a real muxed file (see
//! `docs/format/vaco-mux-matroska.md`) — this is what closes the byte gap
//! that made every Matroska output in the byte-identical conformance suite
//! diverge, not a stylistic addition.
//!
//! **`CRC-32`.** Every Level-1 element (`SeekHead`, `Info`, `Tracks`,
//! `Chapters`, `Attachments`, `Tags`, `Cluster`, `Cues`) opens with a
//! `CRC-32` element (RFC 8794 §11.3.2) as its first child: standard CRC-32
//! (IEEE, `vaco_hash::crc32` — D11's single owner of the `crc` crate, no
//! second table here), little-endian, over the element's own payload
//! excluding the `CRC-32` element itself. [`with_crc32`] is the one place
//! that wrapping happens; every `*_bytes` builder in this file routes its
//! body through it before handing it to `write_element`. Written
//! unconditionally in this crate, which matches the reference's own default:
//! `ffmpeg -h muxer=matroska` lists `-write_crc32 <boolean> ... (default
//! true)`, a real `AVOption` this crate has no channel to turn off (`Muxer`
//! carries no per-muxer option surface) — moot in practice, since `true` is
//! also what every measurement in this file was taken against, and
//! `-bitexact` does not touch it either.
//!
//! **`SeekHead`.** The crate's own former objection — that building it
//! needs "either a second seek-patch pass or fixed-width placeholder
//! arithmetic" — turned out to describe a harder problem than the reference
//! actually solves. It reserves a **fixed** budget
//! ([`SEEKHEAD_RESERVED_BYTES`], measured at 161 bytes and stable across a
//! 3-, 4-, 5- and 6-entry `SeekHead`, a 3 KB file and a 300 KB one — i.e.
//! independent of how many entries there are or how wide their
//! `SeekPosition` values encode), writes whatever real `Seek` entries it
//! already knows, and pads the remainder of that fixed budget with a `Void`
//! element whose own size field is always the full eight-octet VINT width
//! (measured on every sample: `Void`'s header is always 9 bytes, `0xEC` plus
//! an 8-octet size), which is what lets the same reserved span be patched
//! later without moving anything after it. [`seekhead_and_void`] builds this
//! reserved region and both write sites — [`MatroskaMuxer::write_header`]'s
//! initial commit and [`MatroskaMuxer::write_trailer`]'s later patch — call
//! it, so the padding math lives in exactly one place. `patch_known_size`
//! (already in `vaco-format-ebml`) is the same seek-and-overwrite primitive
//! `Segment`'s own size field already used; nothing new needed adding there.
//!
//! `Info`, `Tracks`, `Chapters` and `Attachments` (whichever of the last two
//! exist) get a `Seek` entry immediately, at `write_header` time, because
//! their absolute positions are fully determined the moment their bodies are
//! built — they sit back-to-back right after the fixed reservation, in the
//! order this crate already writes them. `Cues` cannot: its content and
//! position are only known after every `Cluster` has been written, at
//! `write_trailer` time. So, measured on a **seekable** sink: the reference
//! seeks back to the start of the reservation and rewrites it — same 161-byte
//! span, recomputed from scratch with the `Cues` entry added — once `Cues`'
//! position is known. On a **non-seekable** sink (a pipe: `ffmpeg -f
//! matroska -` into a plain redirect, which disables seeking at the
//! `pipe:` protocol layer regardless of what the receiving fd could
//! technically do) it cannot go back, so it commits to the final `SeekHead`
//! at `write_header` time with whatever it already knows — and, measured
//! directly, **omits `Cues` entirely** rather than writing an unindexed one:
//! there is no `Cues` element anywhere in the piped output, not merely a
//! missing `Seek` entry for it. This crate reproduces exactly that asymmetry
//! rather than writing `Cues` unconditionally and only varying whether it is
//! indexed.

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Disposition, Error, MediaType, Rational, Result};
use vaco_demux_matroska::ebml::schema as el;
use vaco_format_core::metadata::MuxMetadata;
use vaco_format_core::options::{FFlags, FormatOptions};
use vaco_format_core::{FormatFlags, Muxer, MuxerDesc};
use vaco_format_ebml::{
    id_bytes, patch_known_size, vint_unknown, write_element, write_float, write_int, write_string,
    write_uint,
};
use vaco_io::{IoOptions, IoWriter, MediaSink};
use vaco_packet::{Packet, PacketSideData};

use crate::block;
use crate::codec;

/// How long, in milliseconds, a `Cluster` is allowed to span before this
/// muxer starts a new one regardless of keyframes. `ffmpeg`'s own
/// `cluster_time_limit` defaults to "no limit", but an audio-only file still
/// needs *some* cap or the whole file becomes one `Cluster` — five seconds is
/// a conservative, commonly used value and is not claimed to match the
/// reference's own unbounded default byte-for-byte.
const MAX_CLUSTER_MS: i64 = 5000;

/// Bytes always reserved for `SeekHead` plus the `Void` that pads it out —
/// see the module docs' *`CRC-32` and `SeekHead`* section for how this was
/// measured and why it is a fixed budget rather than something computed per
/// file. `ffmpeg 8.1` keeps this constant across a 3-, 4-, 5- and 6-entry
/// `SeekHead` and across file sizes from ~3 KB to ~300 KB; nothing this
/// crate ever seeks to (`Info`, `Tracks`, `Chapters`, `Attachments`, `Tags`,
/// `Cues`) needs more than a fraction of it.
const SEEKHEAD_RESERVED_BYTES: u64 = 161;

/// The `Void`-header width the reference always uses inside the `SeekHead`
/// reservation: element ID (one octet, `0xEC`) plus an eight-octet size
/// field encoded at full VINT width rather than the shortest one — measured
/// on every sample gathered for this crate (see the module docs), which is
/// what lets the same reserved span be overwritten later without the
/// `Void`'s own header width changing underneath it.
const VOID_HEADER_BYTES: u64 = 1 + 8;

/// One `Seek` entry: `SeekID` (the target's own element ID, stored as binary
/// with its length marker intact — RFC 9559 §11.2) and `SeekPosition`
/// (relative to the `Segment`'s data start, per the reference's own choice
/// of the "fewest octets that hold the value" uinteger encoding, matching
/// [`vaco_format_ebml::uint`]'s own scheme).
fn seek_entry(target_id: u32, position_rel: u64) -> Vec<u8> {
    let mut body = vaco_format_ebml::binary(el::SEEKID, &id_bytes(target_id));
    body.extend_from_slice(&write_uint(el::SEEKPOSITION, position_rel));
    write_element(el::SEEK, &body)
}

/// `SeekHead` plus its padding `Void`, sized to exactly
/// [`SEEKHEAD_RESERVED_BYTES`] regardless of how many `entries` there are or
/// how wide their positions encode — see the module docs. `entries` is
/// `(target element ID, position relative to the Segment's data start)`,
/// in the order the `Seek` entries should appear (this crate always passes
/// them in file order, matching every sample measured).
///
/// Returns `None` if `entries` alone would not fit the reservation — not
/// observed against the reference at up to six entries, but a real
/// possibility this crate cannot rule out for a caller-supplied metadata set
/// large enough to need wide `SeekPosition`s everywhere; the caller falls
/// back to writing no `SeekHead` at all rather than corrupting the fixed
/// span every other element's position depends on.
fn seekhead_and_void(entries: &[(u32, u64)]) -> Option<Vec<u8>> {
    let mut seekhead_body = Vec::new();
    for &(id, pos) in entries {
        seekhead_body.extend_from_slice(&seek_entry(id, pos));
    }
    let seekhead = write_element(el::SEEKHEAD, &with_crc32(&seekhead_body));
    let seekhead_len = seekhead.len() as u64;
    let void_total = SEEKHEAD_RESERVED_BYTES.checked_sub(seekhead_len)?;
    let void_body_len = void_total.checked_sub(VOID_HEADER_BYTES)?;
    let mut out = seekhead;
    out.extend_from_slice(&id_bytes(el::VOID));
    out.extend_from_slice(&vaco_format_ebml::vint(void_body_len, 8));
    out.resize(out.len() + void_body_len as usize, 0);
    Some(out)
}

/// Prefix `body` with an EBML `CRC-32` element (RFC 8794 §11.3.2): standard
/// CRC-32 (IEEE — [`vaco_hash::crc32`], D11's single owner of the `crc`
/// crate), emitted little-endian, over `body` itself — never over the
/// `CRC-32` element this produces. Measured unconditional on every Level-1
/// element the reference writes; see the module docs.
fn with_crc32(body: &[u8]) -> Vec<u8> {
    let crc = vaco_hash::crc32(body);
    let mut out = vaco_format_ebml::binary(el::CRC32, &crc.to_le_bytes());
    out.extend_from_slice(body);
    out
}

/// A container profile: what differs between `matroska` and `webm` beyond
/// the element tree, which both share in full.
#[derive(Debug, Clone, Copy)]
pub struct Variant {
    doc_type: &'static str,
    is_webm: bool,
}

/// The `matroska` `DocType`.
pub const MATROSKA: Variant = Variant {
    doc_type: "matroska",
    is_webm: false,
};

/// The `webm` `DocType`.
pub const WEBM: Variant = Variant {
    doc_type: "webm",
    is_webm: true,
};

/// The registry descriptor for `matroska`.
pub const MUXER_MATROSKA: MuxerDesc = MuxerDesc {
    name: "matroska",
    long_name: "Matroska",
    extensions: &["mkv"],
    default_video: Some(CodecId::H264),
    // Measured: `ffmpeg -h muxer=matroska` prints "Default audio codec:
    // ac3.", not AAC — easy to guess wrong since AAC is `webm`'s sibling
    // muxer's own instinct, but the reference's is AC-3.
    default_audio: Some(CodecId::Ac3),
    open: open_matroska,
};

/// The registry descriptor for `webm`.
pub const MUXER_WEBM: MuxerDesc = MuxerDesc {
    name: "webm",
    long_name: "WebM",
    extensions: &["webm"],
    default_video: Some(CodecId::Vp9),
    default_audio: Some(CodecId::Opus),
    open: open_webm,
};

fn open_matroska(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(MatroskaMuxer::new(
        MATROSKA,
        sink,
        &FormatOptions::default(),
    )?))
}

fn open_webm(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(MatroskaMuxer::new(
        WEBM,
        sink,
        &FormatOptions::default(),
    )?))
}

/// One declared track's write-side state.
#[derive(Debug, Clone)]
struct TrackOut {
    number: u64,
    is_video: bool,
    codec_id: &'static str,
    default_duration_ns: Option<u64>,
    width: u32,
    height: u32,
    sample_rate: f64,
    channels: u64,
    bit_depth: Option<u64>,
    extradata: Option<Vec<u8>>,
    /// Video only. `Video::FlagInterlaced`'s source (CONFORMANCE-FINDINGS 49).
    field_order: vaco_codec_core::FieldOrder,
    /// Video only. `Video::Colour`'s source, when it maps to one this crate
    /// has actually measured a reference value for (CONFORMANCE-FINDINGS 49).
    chroma_location: vaco_color::ChromaLocation,
}

/// `FileMimeType`. Not in `vaco-demux-matroska::ebml::schema` (that crate has
/// no attachment reader yet), so it lives here rather than adding a field to
/// a crate this one only reads from (D19's reuse, not ownership) — this is
/// the reference's own RFC 9559 element ID, the same way every other `el::*`
/// constant this file uses is.
const FILEMIMETYPE: u32 = 0x4660;

/// One buffered `Cluster`, built fully in memory before it is written.
///
/// Measured against `ffmpeg 8.1` (see the crate's module docs): a `Cluster`'s
/// size field is always the shortest VINT that holds it, on both a seekable
/// and a non-seekable sink, which is only possible if the whole cluster is
/// assembled before its header is written. This mirrors that.
#[derive(Debug)]
struct Cluster {
    start_ticks: i64,
    body: Vec<u8>,
    /// Absolute byte offset, in the sink, of this cluster's own element ID —
    /// recorded when the cluster is flushed, for `Cues`.
    byte_pos: u64,
    /// Whether a video keyframe opened this cluster, which is what earns it
    /// a `CuePoint`.
    keyframe_opened: bool,
}

/// One `Cues` entry.
#[derive(Debug)]
struct CueEntry {
    time_ticks: u64,
    track: u64,
    /// Byte offset of the cluster's ID, relative to the first byte of the
    /// `Segment`'s data (RFC 9559 section 11.8's `CueClusterPosition`).
    cluster_pos_rel: u64,
}

/// The Matroska/`WebM` muxer.
#[derive(Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent one-way latch (header/trailer written once, \
              DocType bumped once, SeekHead reservation present once), not related state \
              that wants an enum"
)]
pub struct MatroskaMuxer {
    variant: Variant,
    out: IoWriter,
    tracks: Vec<TrackOut>,
    header_written: bool,
    /// Whether `Segment`/`Info`/`Tracks`/... bytes have actually reached
    /// [`MatroskaMuxer::out`]. Separate from `header_written` (which only
    /// says [`Muxer::write_header`] was *called*, closing `add_stream`) so
    /// that the real byte-write can be delayed past it — see
    /// `pending_packets`' docs for why that delay exists.
    header_flushed: bool,
    /// Packets buffered because [`MatroskaMuxer::header_flushed`] is still
    /// false, in arrival order, replayed in one go the moment it flips.
    ///
    /// # Why this exists — the FFV1-into-Matroska bug
    ///
    /// `write_header` used to write `Tracks` (and so `CodecPrivate`)
    /// immediately, from whatever `extradata` [`Muxer::add_stream`] was
    /// handed — which for a video encoder that cannot know its own
    /// configuration record before it has seen a pixel format (RFC 9043's
    /// FFV1 is the measured case: `Ffv1Encoder` cannot answer before the
    /// first [`vaco_codec_core::Encoder::send_frame`]) is whatever
    /// `CodecParameters::extradata` happened to already hold — the *previous*
    /// codec's configuration record, verbatim, for a transcode. `ffmpeg`
    /// then reads that as an FFV1 Configuration Record and rejects it
    /// (`Invalid version in global header`): every FFV1 file this crate ever
    /// wrote was silently corrupt.
    ///
    /// [`MatroskaMuxer::adopt_new_extradata`] is the same fix
    /// `vaco-mux-mp4`'s `adopt_new_extradata` already applies for `moov`,
    /// which is naturally deferred to `write_trailer`; Matroska's `Tracks`
    /// has no such natural deferral point, since nothing after `Tracks`
    /// revisits it (`write_trailer` only rewrites `Info` and the
    /// `SeekHead`/`Cues` reservation). So the flush itself is deferred
    /// instead, buffering packets here until every declared track has
    /// produced at least one — the point by which every encoder that is
    /// ever going to attach [`vaco_packet::PacketSideData::NewExtradata`]
    /// has done so — and *then* the real `Tracks` bytes are built and
    /// written, with each track's adopted extradata rather than its
    /// `add_stream`-time guess.
    ///
    /// Bounded by "one packet per track that has not yet produced one", not
    /// by file length: a track that never produces a packet at all leaves
    /// this buffering until [`MatroskaMuxer::write_trailer`]'s own
    /// safety-net flush, which is the pathological case, not the common one.
    pending_packets: Vec<Packet>,
    /// `true` at index `i` once track `i` has been handed at least one
    /// packet — the gate [`MatroskaMuxer::header_flushed`] waits on. Sized to
    /// `tracks.len()` the moment [`Muxer::write_header`] closes `add_stream`.
    first_packet_seen: Vec<bool>,
    /// `true` at index `i` once track `i` has had a real block written by
    /// [`MatroskaMuxer::write_block`] — distinct from `first_packet_seen`,
    /// which only tracks arrival during the pre-header-flush buffering
    /// window. Gates the "first pts must be set" check below, which needs
    /// to fire exactly once per track regardless of when the header
    /// happens to flush.
    wrote_first_block: Vec<bool>,
    trailer_written: bool,
    /// Absolute byte offset of `Segment`'s eight-octet size field.
    segment_size_at: u64,
    /// Absolute byte offset of the first octet of `Segment`'s data — what
    /// every `Cues` position is relative to.
    segment_data_start: u64,
    cluster: Option<Cluster>,
    cues: Vec<CueEntry>,
    /// `DateUTC`, nanoseconds since the Matroska epoch (2001-01-01), or
    /// `None` to omit the element — the `+bitexact` default, and the only
    /// path that does not touch a clock (see the crate's module docs).
    date_utc_ns: Option<i64>,
    /// `webm` starts at `DocTypeVersion` 2 and is bumped to 4 the moment a
    /// track needs a version-4 feature; `matroska` is always 4 (both
    /// measured against `ffmpeg 8.1`, see `docs/format/vaco-mux-matroska.md`).
    needs_doctype_v4: bool,
    max_end_ticks: u64,
    /// How long, in ticks, a `Cluster` may span before a new one starts
    /// regardless of keyframes. Configurable so [`crate::webm_chunk`] can
    /// make a `Cluster` boundary and a chunk boundary the same thing.
    max_cluster_ms: i64,
    /// Absolute byte offset of every `Cluster`'s own element ID, in the order
    /// they were opened — [`crate::webm_chunk::WebmChunkMuxer`] reads this to
    /// know where each chunk begins in the single stream this trait can
    /// write to (see that module's docs for why it needs to).
    cluster_starts: Vec<u64>,
    /// Set by [`Muxer::set_metadata`], read by [`MatroskaMuxer::write_header`]
    /// (M30, gap 1). Empty for every caller that never calls
    /// `MuxBuilder::with_metadata`, which is every pre-existing call site.
    metadata: MuxMetadata,
    /// `(target element ID, position relative to the Segment's data start)`
    /// for every `Seek` entry written into the `SeekHead` reservation at
    /// `write_header` time — i.e. everything except `Cues`, whose position
    /// is not yet known then. [`MatroskaMuxer::write_trailer`] reuses this
    /// list, with `Cues` appended, to recompute the same reservation when the
    /// sink is seekable — see the module docs' *`CRC-32` and `SeekHead`*
    /// section.
    seek_targets: Vec<(u32, u64)>,
    /// Whether [`MatroskaMuxer::write_header`] actually wrote the `SeekHead`
    /// reservation — false only in the unreached-in-practice case where
    /// [`seekhead_and_void`] refuses because `seek_targets` alone would not
    /// fit [`SEEKHEAD_RESERVED_BYTES`] (see that function's docs). Guards
    /// [`MatroskaMuxer::write_trailer`]'s later patch: with no reservation to
    /// begin with, there is no fixed span to seek back and overwrite.
    seekhead_reserved: bool,
    /// `-fflags +bitexact` on this output. Already computed once in `new`
    /// for [`MatroskaMuxer::date_utc_ns`]; kept here too because
    /// [`MatroskaMuxer::tags_bytes`] needs the same fact to drop the
    /// auto-populated `encoder` file tag the same way `DateUTC` is dropped
    /// (CONFORMANCE-FINDINGS 49).
    bitexact: bool,
    /// Absolute byte offset of `Info`'s own element ID, once written — `None`
    /// when the sink is not seekable, since `Duration` (and so `Info`'s
    /// whole byte content) is fixed for good the moment it is written (see
    /// [`MatroskaMuxer::info_bytes`]'s docs). [`MatroskaMuxer::write_trailer`]
    /// rewrites the **whole** `Info` element in place once
    /// [`MatroskaMuxer::max_end_ticks`] is known, rather than patching just
    /// `Duration`'s own bytes: `Info`'s body carries a `CRC-32` over itself,
    /// and [`MatroskaMuxer::info_bytes`] is built so that re-running it with
    /// the real duration reproduces the exact same total length (every field
    /// in it is fixed-width) with a correct checksum, which a narrower patch
    /// would have needed a second, separate step to keep valid.
    info_start: Option<u64>,
}

/// Matroska's epoch (2001-01-01T00:00:00 UTC) as Unix nanoseconds.
const MATROSKA_EPOCH_UNIX_NS: i64 = 978_307_200_000_000_000;

impl MatroskaMuxer {
    /// A muxer over `sink` for the given container `variant`.
    ///
    /// # Errors
    ///
    /// Propagates buffer allocation failure from [`IoWriter::new`].
    pub fn new(variant: Variant, sink: Box<dyn MediaSink>, opts: &FormatOptions) -> Result<Self> {
        let bitexact = opts.fflags.contains(FFlags::BITEXACT);
        let date_utc_ns = if bitexact || opts.start_time_realtime == i64::MIN {
            None
        } else {
            // `start_time_realtime` is Unix microseconds (see
            // `vaco-format-core`'s own use of it); Matroska wants nanoseconds
            // since 2001-01-01.
            opts.start_time_realtime
                .checked_mul(1000)
                .and_then(|unix_ns| unix_ns.checked_sub(MATROSKA_EPOCH_UNIX_NS))
        };
        Ok(Self {
            variant,
            out: IoWriter::new(sink, &IoOptions::default())?,
            tracks: Vec::new(),
            header_written: false,
            header_flushed: false,
            pending_packets: Vec::new(),
            first_packet_seen: Vec::new(),
            wrote_first_block: Vec::new(),
            trailer_written: false,
            segment_size_at: 0,
            segment_data_start: 0,
            cluster: None,
            cues: Vec::new(),
            date_utc_ns,
            needs_doctype_v4: !variant.is_webm,
            max_end_ticks: 0,
            max_cluster_ms: MAX_CLUSTER_MS,
            cluster_starts: Vec::new(),
            metadata: MuxMetadata::default(),
            seek_targets: Vec::new(),
            seekhead_reserved: false,
            bitexact,
            info_start: None,
        })
    }

    /// Override the cluster time span cap (see [`MatroskaMuxer::max_cluster_ms`]'s
    /// field docs). Must be called before [`MatroskaMuxer::write_header`].
    pub const fn set_max_cluster_ms(&mut self, ms: i64) {
        self.max_cluster_ms = ms;
    }

    /// Absolute byte offset of every `Cluster` opened so far, in order.
    #[must_use]
    pub fn cluster_starts(&self) -> &[u64] {
        &self.cluster_starts
    }

    /// A muxer for the `matroska` `DocType`.
    ///
    /// # Errors
    /// As [`MatroskaMuxer::new`].
    pub fn new_matroska(sink: Box<dyn MediaSink>, opts: &FormatOptions) -> Result<Self> {
        Self::new(MATROSKA, sink, opts)
    }

    /// A muxer for the `webm` `DocType`.
    ///
    /// # Errors
    /// As [`MatroskaMuxer::new`].
    pub fn new_webm(sink: Box<dyn MediaSink>, opts: &FormatOptions) -> Result<Self> {
        Self::new(WEBM, sink, opts)
    }

    /// Bytes written to the sink so far.
    #[must_use]
    pub fn position(&self) -> u64 {
        self.out.pos()
    }

    fn ebml_header_bytes(&self) -> Vec<u8> {
        let doc_version: u64 = if self.needs_doctype_v4 { 4 } else { 2 };
        let mut body = write_uint(el::EBMLVERSION, 1);
        body.extend_from_slice(&write_uint(el::EBMLREADVERSION, 1));
        body.extend_from_slice(&write_uint(el::EBMLMAXIDLENGTH, 4));
        body.extend_from_slice(&write_uint(el::EBMLMAXSIZELENGTH, 8));
        body.extend_from_slice(&write_string(el::DOCTYPE, self.variant.doc_type));
        body.extend_from_slice(&write_uint(el::DOCTYPEVERSION, doc_version));
        body.extend_from_slice(&write_uint(el::DOCTYPEREADVERSION, 2));
        write_element(el::EBML, &body)
    }

    /// File-level `title`, matched case-insensitively against
    /// [`MuxMetadata::tags`] — measured: the reference routes a `-metadata
    /// title=...` value into `Info > Title`, never into `Tags` (see the
    /// module docs).
    fn title(&self) -> Option<&str> {
        self.metadata
            .tags
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("title"))
            .map(|(_, v)| v.as_str())
    }

    /// `Info`. `duration_ticks` is `None` on a non-seekable sink — measured
    /// directly on a pipe (CONFORMANCE-FINDINGS 49), the reference omits
    /// `Duration` entirely there, the same asymmetry this crate already
    /// reproduces for `Cues` (see the module docs) — and, on a seekable one,
    /// `Some(0)` the first time this runs (`write_header`, before any packet
    /// exists, so the real total is not known yet) and
    /// `Some(self.max_end_ticks)` the second (`write_trailer`, once it is).
    /// [`float`](vaco_format_ebml::float)'s element is a fixed 11 bytes
    /// regardless of the value inside it (a constant 2-byte ID, a 1-byte
    /// size — always `0x88`, since the body is always exactly 8 bytes — and
    /// the 8-byte body itself), so the two calls produce byte-for-byte the
    /// same length, which is what lets [`MatroskaMuxer::write_trailer`]
    /// overwrite the whole element in place — see
    /// [`MatroskaMuxer::info_start`]'s field docs for why a narrower patch
    /// (just `Duration`'s own bytes) does not work here the way it does for
    /// `Segment`'s size field: `Info`'s body carries a `CRC-32` over itself,
    /// so patching only `Duration` would leave that checksum invalid.
    fn info_bytes(&self, duration_ticks: Option<u64>) -> Vec<u8> {
        let mut body = write_uint(el::TIMESTAMPSCALE, 1_000_000);
        if let Some(title) = self.title().filter(|t| !t.is_empty()) {
            body.extend_from_slice(&write_string(el::TITLE, title));
        }
        body.extend_from_slice(&write_string(el::MUXINGAPP, "vaco-mux-matroska"));
        body.extend_from_slice(&write_string(el::WRITINGAPP, "vaco-mux-matroska"));
        if let Some(ns) = self.date_utc_ns {
            body.extend_from_slice(&write_int(el::DATEUTC, ns));
        }
        if let Some(ticks) = duration_ticks {
            body.extend_from_slice(&write_float(el::DURATION, ticks as f64));
        }
        write_element(el::INFO, &with_crc32(&body))
    }

    /// `name`/`language` are resolved by the caller ([`MatroskaMuxer::tracks_bytes`])
    /// from [`MatroskaMuxer::metadata`] at write time rather than stored on
    /// [`TrackOut`] — deliberately, so this box's content does not depend on
    /// whether [`Muxer::set_metadata`] was called before or after
    /// [`Muxer::add_stream`] (a caller driving the muxer directly through
    /// `dyn Muxer`, as `vaco-cli`'s scheduler does, has no way to guarantee
    /// that order — see `docs/format/vaco-mux-matroska.md`).
    /// Child order matches the reference exactly (measured on both a
    /// reordered-video and a video+audio file, CONFORMANCE-FINDINGS 49):
    /// `TrackNumber TrackUID FlagLacing Language [disposition flags] CodecID
    /// TrackType DefaultDuration Video MaxBlockAdditionID Void CodecPrivate`
    /// for a video track, the same minus the four video-only fields for
    /// audio. The disposition flags' own position (right after `Language`,
    /// before `CodecID`) is separately measured — see their own comment.
    /// `Name`'s position is **not** measured — neither sample file used here
    /// sets a per-track title — so it stays where it always has, right
    /// after `FlagLacing`.
    fn track_entry_bytes(t: &TrackOut, name: Option<&str>, language: &str, disposition: Disposition) -> Vec<u8> {
        let mut body = write_uint(el::TRACKNUMBER, t.number);
        // Full 8-byte width, not the fewest octets `write_uint` would pick —
        // measured, the reference always writes `TrackUID` this way even
        // when the value (here, the 1-based track number) fits in one byte.
        body.extend_from_slice(&write_element(el::TRACKUID, &t.number.to_be_bytes()));
        // Measured against `ffmpeg 8.1`: `FlagLacing` is written explicitly,
        // and is always 0 — this crate never emits a laced block by default
        // (see `crate::block`'s module docs).
        body.extend_from_slice(&write_uint(el::FLAGLACING, 0));
        if let Some(name) = name.filter(|n| !n.is_empty()) {
            body.extend_from_slice(&write_string(el::NAME, name));
        }
        body.extend_from_slice(&write_string(el::LANGUAGE, language));
        // `FlagDefault`/`FlagForced` position and omission rule are measured
        // against `ffmpeg 8.1` (`-disposition:v default`, `forced`, and
        // `default+forced`, compared byte-for-byte): RFC 9559 §5.1.4.1.9
        // states `FlagDefault` defaults to 1, which is exactly why the
        // reference omits the element when the bit *is* set and writes an
        // explicit `0` only to override that implied default — the same
        // reading this crate's own demuxer already takes (see
        // `vaco-demux-matroska::demux`'s `flag_default` comment). Every
        // other boolean flag here (`FlagForced` included) has an implied
        // default of `0`, so the rule for those is the ordinary "omit unless
        // set". The five beyond `FlagDefault`/`FlagForced` are placed in the
        // same slot by symmetry with what this crate's own demuxer already
        // reads (RFC 9559, Tier A), not independently re-measured one by one
        // against the reference.
        if !disposition.contains(Disposition::DEFAULT) {
            body.extend_from_slice(&write_uint(el::FLAGDEFAULT, 0));
        }
        if disposition.contains(Disposition::FORCED) {
            body.extend_from_slice(&write_uint(el::FLAGFORCED, 1));
        }
        if disposition.contains(Disposition::HEARING_IMPAIRED) {
            body.extend_from_slice(&write_uint(el::FLAGHEARINGIMPAIRED, 1));
        }
        if disposition.contains(Disposition::VISUAL_IMPAIRED) {
            body.extend_from_slice(&write_uint(el::FLAGVISUALIMPAIRED, 1));
        }
        if disposition.contains(Disposition::DESCRIPTIONS) {
            body.extend_from_slice(&write_uint(el::FLAGTEXTDESCRIPTIONS, 1));
        }
        if disposition.contains(Disposition::ORIGINAL) {
            body.extend_from_slice(&write_uint(el::FLAGORIGINAL, 1));
        }
        if disposition.contains(Disposition::COMMENT) {
            body.extend_from_slice(&write_uint(el::FLAGCOMMENTARY, 1));
        }
        body.extend_from_slice(&write_string(el::CODECID, t.codec_id));
        body.extend_from_slice(&write_uint(el::TRACKTYPE, if t.is_video { 1 } else { 2 }));
        if let Some(dur) = t.default_duration_ns {
            body.extend_from_slice(&write_uint(el::DEFAULTDURATION, dur));
        }
        if t.is_video {
            let mut video = write_uint(el::PIXELWIDTH, u64::from(t.width));
            video.extend_from_slice(&write_uint(el::PIXELHEIGHT, u64::from(t.height)));
            // `FlagInterlaced` (§RFC 9559 field order): 0 undetermined, 1
            // interlaced, 2 not interlaced. Measured: a progressive H.264
            // source gets `2`; the interlaced field orders below are mapped
            // by the field's own name rather than measured against the
            // reference directly (no interlaced sample was available), so
            // treat that half of this mapping as unverified.
            let flag_interlaced: u64 = match t.field_order {
                vaco_codec_core::FieldOrder::Progressive => 2,
                vaco_codec_core::FieldOrder::TopFirst
                | vaco_codec_core::FieldOrder::BottomFirst
                | vaco_codec_core::FieldOrder::TopCodedFirst
                | vaco_codec_core::FieldOrder::BottomCodedFirst => 1,
                vaco_codec_core::FieldOrder::Unknown => 0,
            };
            video.extend_from_slice(&write_uint(el::FLAGINTERLACED, flag_interlaced));
            // `Colour > ChromaSitingHorz/ChromaSitingVert`. Only
            // `ChromaLocation::Left` is measured (a `chroma_location=left`
            // H.264 source produces `(1, 2)`); every other siting is omitted
            // rather than guessed, per the same "say so rather than invent a
            // rationale" rule this crate's `BlockGroup` finding was named
            // for.
            if t.chroma_location == vaco_color::ChromaLocation::Left {
                let mut colour = write_uint(el::CHROMASITINGHORZ, 1);
                colour.extend_from_slice(&write_uint(el::CHROMASITINGVERT, 2));
                video.extend_from_slice(&write_element(el::COLOUR, &colour));
            }
            body.extend_from_slice(&write_element(el::VIDEO, &video));
            // `MaxBlockAdditionID` then a 2-byte `Void` — measured
            // unconditional on every video track sampled, always `0` and
            // always exactly 2 bytes of padding, and absent from every audio
            // track sampled alongside one (CONFORMANCE-FINDINGS 49). The
            // `Void`'s size field is the full 8-octet VINT width, not the
            // shortest one — measured, and the same convention this crate's
            // own `SeekHead` padding already uses (see [`VOID_HEADER_BYTES`]).
            body.extend_from_slice(&write_uint(el::MAXBLOCKADDITIONID, 0));
            body.extend_from_slice(&id_bytes(el::VOID));
            body.extend_from_slice(&vaco_format_ebml::vint(2, 8));
            body.resize(body.len() + 2, 0);
        } else {
            let mut audio = write_float(el::SAMPLINGFREQUENCY, t.sample_rate);
            audio.extend_from_slice(&write_uint(el::CHANNELS, t.channels.max(1)));
            if let Some(bits) = t.bit_depth {
                audio.extend_from_slice(&write_uint(el::BITDEPTH, bits));
            }
            body.extend_from_slice(&write_element(el::AUDIO, &audio));
        }
        if !codec::never_carries_extradata_str(t.codec_id)
            && let Some(bytes) = t.extradata.as_ref().filter(|d| !d.is_empty())
        {
            body.extend_from_slice(&vaco_format_ebml::binary(el::CODECPRIVATE, bytes));
        }
        // `TrackEntry`'s own size field is the full 8-octet VINT width, not
        // the shortest one `write_element` would pick — measured; `Tracks`,
        // `Tag` and `SimpleTag` all use the shortest width right alongside
        // it, so this is specific to `TrackEntry`, not a general rule this
        // crate's other master elements should copy.
        let mut out = id_bytes(el::TRACKENTRY);
        out.extend_from_slice(&vaco_format_ebml::vint(body.len() as u64, 8));
        out.extend_from_slice(&body);
        out
    }

    fn tracks_bytes(&self) -> Vec<u8> {
        let mut body = Vec::new();
        for (i, t) in self.tracks.iter().enumerate() {
            let stream_index = u32::try_from(i).unwrap_or(0);
            let stream_tags = self.metadata.tags_for_stream(stream_index);
            let name = stream_tags
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("title"))
                .map(|(_, v)| v.as_str());
            let language = stream_tags
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("language"))
                .map_or("und", |(_, v)| v.as_str());
            let disposition = self.metadata.disposition_for_stream(stream_index);
            body.extend_from_slice(&Self::track_entry_bytes(t, name, language, disposition));
        }
        write_element(el::TRACKS, &with_crc32(&body))
    }

    /// Flush the in-progress `Cluster`, if any, writing it as one complete
    /// element and recording its byte position for any keyframe it opened.
    fn flush_cluster(&mut self) -> Result<()> {
        let Some(cluster) = self.cluster.take() else {
            return Ok(());
        };
        let mut body = write_uint(
            el::TIMESTAMP,
            u64::try_from(cluster.start_ticks).unwrap_or(0),
        );
        body.extend_from_slice(&cluster.body);
        let bytes = write_element(el::CLUSTER, &with_crc32(&body));
        // `byte_pos` was recorded before any of this cluster's bytes were
        // written, so it is exactly where `out.pos()` is now, before this
        // write — nothing to recompute.
        self.out.write(&bytes)?;
        if cluster.keyframe_opened {
            // The keyframe that opened the cluster is always its first
            // block, at the cluster's own start timestamp.
            for cue_track in self.tracks.iter().filter(|t| t.is_video) {
                self.cues.push(CueEntry {
                    time_ticks: u64::try_from(cluster.start_ticks).unwrap_or(0),
                    track: cue_track.number,
                    cluster_pos_rel: cluster.byte_pos.saturating_sub(self.segment_data_start),
                });
            }
        }
        Ok(())
    }

    /// Build one `SimpleTag` per `(key, value)` pair, uppercasing `key` for
    /// `TagName` — measured, see the module docs.
    fn simple_tags(pairs: &[(String, String)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (k, v) in pairs {
            let mut tag = write_string(el::TAGNAME, &k.to_ascii_uppercase());
            tag.extend_from_slice(&write_string(el::TAGSTRING, v));
            body.extend_from_slice(&write_element(el::SIMPLETAG, &tag));
        }
        body
    }

    /// `Tags`, or `None` if there is nothing left to write once `title` (file
    /// level) and `title`/`language` (per-stream) have been routed to their
    /// own dedicated elements.
    fn tags_bytes(&self) -> Option<Vec<u8>> {
        let mut body = Vec::new();

        // `encoder` is dropped under `bitexact`, file-level tags only — the
        // same suppression `vaco-mux-mp4` makes for its own `©too`, and
        // measured the same way: an MP4-sourced `encoder=Lavf62.12.100`
        // format tag (the tool that made the *input*, carried through on a
        // stream copy, not this crate's own identity — `MuxingApp`/
        // `WritingApp` already state that honestly) reaches a plain remux's
        // file-level `Tag` but is absent from the reference's own bitexact
        // output. A *per-track* `encoder` tag (e.g. `Lavc62.28.100
        // libx264`, the codec that made that stream's data) is a different
        // fact and is not suppressed — measured present in the reference's
        // own bitexact output right alongside it (CONFORMANCE-FINDINGS 49).
        let file_tags: Vec<(String, String)> = self
            .metadata
            .tags
            .iter()
            .filter(|(k, _)| {
                !(k.eq_ignore_ascii_case("title")
                    || (self.bitexact && k.eq_ignore_ascii_case("encoder")))
            })
            .cloned()
            .collect();
        if !file_tags.is_empty() {
            let targets = write_element(el::TARGETS, &[]);
            let mut tag = targets;
            tag.extend_from_slice(&Self::simple_tags(&file_tags));
            body.extend_from_slice(&write_element(el::TAG, &tag));
        }

        for track in &self.tracks {
            let stream_tags: Vec<(String, String)> = self
                .metadata
                .tags_for_stream(u32::try_from(track.number - 1).unwrap_or(0))
                .iter()
                .filter(|(k, _)| {
                    !k.eq_ignore_ascii_case("title") && !k.eq_ignore_ascii_case("language")
                })
                .cloned()
                .collect();
            if stream_tags.is_empty() {
                continue;
            }
            let targets_body = write_uint(el::TAGTRACKUID, track.number);
            let mut tag = write_element(el::TARGETS, &targets_body);
            tag.extend_from_slice(&Self::simple_tags(&stream_tags));
            body.extend_from_slice(&write_element(el::TAG, &tag));
        }

        (!body.is_empty()).then(|| write_element(el::TAGS, &with_crc32(&body)))
    }

    /// Rescale a chapter bound to RFC 9559's fixed nanosecond unit for
    /// `ChapterTimeStart`/`ChapterTimeEnd`, independent of `TimestampScale`.
    /// An absent timestamp (a chapter with no stated start, say) becomes `0`
    /// rather than being omitted, since both fields are mandatory.
    fn chapter_time_ns(ts: vaco_core::Timestamp, base: Rational) -> u64 {
        ts.to_duration(base)
            .map(|d| d.as_micros().saturating_mul(1000))
            .and_then(|ns| u64::try_from(ns).ok())
            .unwrap_or(0)
    }

    /// `Chapters`, or `None` when [`MuxMetadata::chapters`] is empty.
    fn chapters_bytes(&self) -> Option<Vec<u8>> {
        if self.metadata.chapters.is_empty() {
            return None;
        }
        let mut editions = Vec::new();
        for (i, chapter) in self.metadata.chapters.iter().enumerate() {
            let uid = u64::try_from(chapter.id)
                .ok()
                .filter(|&id| id != 0)
                .unwrap_or_else(|| i as u64 + 1);
            let mut atom = write_uint(el::CHAPTERUID, uid);
            atom.extend_from_slice(&write_uint(
                el::CHAPTERTIMESTART,
                Self::chapter_time_ns(chapter.start, chapter.time_base),
            ));
            atom.extend_from_slice(&write_uint(
                el::CHAPTERTIMEEND,
                Self::chapter_time_ns(chapter.end, chapter.time_base),
            ));
            let title = chapter
                .metadata
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("title"))
                .map_or("", |(_, v)| v.as_str());
            let language = chapter
                .metadata
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("language"))
                .map_or("und", |(_, v)| v.as_str());
            let mut display = write_string(el::CHAPSTRING, title);
            display.extend_from_slice(&write_string(el::CHAPLANGUAGE, language));
            atom.extend_from_slice(&write_element(el::CHAPTERDISPLAY, &display));
            editions.extend_from_slice(&write_element(el::CHAPTERATOM, &atom));
        }
        let edition_entry = write_element(el::EDITIONENTRY, &editions);
        Some(write_element(el::CHAPTERS, &with_crc32(&edition_entry)))
    }

    /// `Attachments`, or `None` when [`MuxMetadata::attachments`] is empty.
    /// See the module docs for `FileUID`'s derivation and the `webm` note.
    fn attachments_bytes(&self) -> Option<Vec<u8>> {
        if self.metadata.attachments.is_empty() {
            return None;
        }
        let mut body = Vec::new();
        for (i, att) in self.metadata.attachments.iter().enumerate() {
            let mut file = Vec::new();
            if !att.description.is_empty() {
                file.extend_from_slice(&write_string(el::FILEDESCRIPTION, &att.description));
            }
            file.extend_from_slice(&write_string(el::FILENAME, &att.filename));
            file.extend_from_slice(&write_string(FILEMIMETYPE, &att.mime_type));
            file.extend_from_slice(&vaco_format_ebml::binary(el::FILEDATA, &att.data));
            // Deterministic rather than random (see module docs): a
            // simple hash of position and filename, never the clock or an
            // RNG, so `wasm32` and `-fflags +bitexact` both stay reachable.
            let uid = Self::deterministic_uid(i, &att.filename);
            file.extend_from_slice(&write_uint(el::FILEUID, uid));
            body.extend_from_slice(&write_element(el::ATTACHEDFILE, &file));
        }
        Some(write_element(el::ATTACHMENTS, &with_crc32(&body)))
    }

    /// A small, deterministic 64-bit value from `salt` and `text` — used
    /// where RFC 9559 wants a UID but this crate has no caller-supplied one
    /// and, per the module docs, will not draw one from a clock or an RNG.
    /// Not cryptographic; only needs to differ across attachments in the
    /// same file, which a per-position salt already guarantees on its own.
    fn deterministic_uid(salt: usize, text: &str) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ (salt as u64);
        for b in text.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        }
        h | 1 // never zero, which some readers treat as "absent"
    }

    fn cues_bytes(&self) -> Vec<u8> {
        let mut body = Vec::new();
        for c in &self.cues {
            let mut positions = write_uint(el::CUETRACK, c.track);
            positions.extend_from_slice(&write_uint(el::CUECLUSTERPOSITION, c.cluster_pos_rel));
            let mut point = write_uint(el::CUETIME, c.time_ticks);
            point.extend_from_slice(&write_element(el::CUETRACKPOSITIONS, &positions));
            body.extend_from_slice(&write_element(el::CUEPOINT, &point));
        }
        write_element(el::CUES, &with_crc32(&body))
    }

    /// The real, one-time `write_header` body: everything up to and
    /// including the first `Cluster`'s worth of framing is fixed once this
    /// returns. Called lazily — see `pending_packets`' field docs for why —
    /// either by `write_packet` once every track has produced its first
    /// packet, or by `write_trailer` as the pathological-case fallback.
    ///
    /// # Errors
    /// [`Error::Unsupported`] naming a track whose codec needs an
    /// out-of-band Configuration Record (`codec::requires_extradata_str`)
    /// and still has none at this point — the point after which Matroska has
    /// no mechanism left to supply one (see that function's own docs).
    /// Refusing here is what turns the FFV1 bug this module's docs describe
    /// into a loud error instead of a silently corrupt file for any codec
    /// that still cannot answer, rather than fixing FFV1 alone and leaving
    /// the same silent failure for the next one.
    fn flush_header_bytes(&mut self) -> Result<()> {
        for t in &self.tracks {
            if codec::requires_extradata_str(t.codec_id)
                && t.extradata.as_ref().is_none_or(Vec::is_empty)
            {
                return Err(Error::Unsupported(
                    "matroska: this codec needs an out-of-band configuration record and none \
                     was produced (the encoder never attached one, and the container was not \
                     told one directly)",
                ));
            }
        }
        self.header_flushed = true;

        self.out.write(&self.ebml_header_bytes())?;

        self.out.write(&id_bytes(el::SEGMENT))?;
        self.segment_size_at = self.out.pos();
        self.out.write(&vint_unknown(8))?;
        self.segment_data_start = self.out.pos();

        let seekable = self.out.is_seekable();
        let info = self.info_bytes(seekable.then_some(0));
        // `Info` starts right after the fixed `SeekHead` reservation — see
        // `info_start`'s field docs for why `write_trailer` needs this to
        // rewrite the whole element rather than patching one field.
        self.info_start = seekable.then_some(self.segment_data_start + SEEKHEAD_RESERVED_BYTES);
        let tracks = self.tracks_bytes();
        let chapters = self.chapters_bytes();
        let attachments = self.attachments_bytes();
        let tags = self.tags_bytes();

        // `Info`, `Tracks`, `Chapters` and `Attachments` (whichever exist)
        // sit back-to-back right after the fixed `SeekHead` reservation, in
        // this same order — so their positions are fully known before any of
        // them is written. `Tags` is deliberately not indexed yet: see below.
        let mut pos = SEEKHEAD_RESERVED_BYTES;
        let mut targets = vec![(el::INFO, pos)];
        pos += info.len() as u64;
        targets.push((el::TRACKS, pos));
        pos += tracks.len() as u64;
        if let Some(c) = &chapters {
            targets.push((el::CHAPTERS, pos));
            pos += c.len() as u64;
        }
        if let Some(a) = &attachments {
            targets.push((el::ATTACHMENTS, pos));
            pos += a.len() as u64;
        }
        if tags.is_some() {
            targets.push((el::TAGS, pos));
        }
        self.seek_targets = targets;

        // `Cues`' position is not known until `write_trailer` (after every
        // `Cluster`), so it has no entry yet. A seekable sink rewrites this
        // same reservation there once it does; a non-seekable one commits to
        // this `SeekHead` as final — see the module docs.
        if let Some(region) = seekhead_and_void(&self.seek_targets) {
            self.out.write(&region)?;
            self.seekhead_reserved = true;
        }

        self.out.write(&info)?;
        self.out.write(&tracks)?;
        if let Some(chapters) = chapters {
            self.out.write(&chapters)?;
        }
        if let Some(attachments) = attachments {
            self.out.write(&attachments)?;
        }
        if let Some(tags) = tags {
            self.out.write(&tags)?;
        }
        Ok(())
    }

    /// Copy a packet's [`PacketSideData::NewExtradata`], if it carries one,
    /// into its track's own `extradata` — the same fix `vaco-mux-mp4`'s
    /// `adopt_new_extradata` applies for `moov`. Harmless once
    /// `header_flushed` is already true: Matroska has nothing left to patch
    /// at that point, so this just updates state nothing reads again.
    fn adopt_new_extradata(&mut self, idx: usize, packet: &Packet) {
        let Some(new_extradata) = packet.side_data.iter().find_map(|sd| match sd {
            PacketSideData::NewExtradata(buf) => Some(buf.as_slice().to_vec()),
            _ => None,
        }) else {
            return;
        };
        let Some(track) = self.tracks.get_mut(idx) else {
            return;
        };
        if track.extradata.as_deref() != Some(new_extradata.as_slice()) {
            track.extradata = Some(new_extradata);
        }
    }

    /// The actual `Cluster`/`Block` write, once the header is committed
    /// (`header_flushed`). Split out of [`Muxer::write_packet`] so that
    /// method can buffer instead when it is not.
    fn write_block(&mut self, idx: usize, packet: &Packet) -> Result<()> {
        // CONFORMANCE-FINDINGS 19: measured directly (`ffmpeg -i
        // <asf-with-no-video-pts> -c copy -f matroska`) — the reference
        // refuses with "Can't write packet with unknown timestamp" rather
        // than silently reusing the previous clock or writing `pts=0`. A
        // source whose demuxer genuinely leaves a video packet's pts unset
        // (AVI, ASF — neither carries a native per-packet presentation
        // time distinct from decode order) produces exactly this on its
        // first packet per track. Mirrors the identical, already-fixed
        // check in `vaco-mux-mpegts` (first packet per stream only,
        // matching the reference's own behaviour on this muxer) rather
        // than `vaco-mux-flv`'s (every packet — that muxer's own message
        // carries no "first" qualifier).
        let is_first_for_track = matches!(self.wrote_first_block.get(idx), Some(false));
        if is_first_for_track && packet.pts.ticks().is_none() {
            return Err(Error::InvalidData(
                "matroska: first pts value must be set",
            ));
        }
        if let Some(seen) = self.wrote_first_block.get_mut(idx) {
            *seen = true;
        }

        let ts = packet.pts.ticks().unwrap_or(0);
        // Matroska has no decode timestamp of its own (CONFORMANCE-FINDINGS
        // 37) — `dts` is read only to fall back to it when `pts` is absent.
        let _dts = packet.dts.ticks().unwrap_or(ts);
        let is_key = packet.is_key();

        // Decide whether the current cluster can still hold this block:
        // reset when there is none yet, when a video keyframe should start a
        // fresh one, when the elapsed time is past the cap, or when the
        // relative timestamp would not fit the signed 16-bit field.
        let track_is_video = self.tracks.get(idx).is_some_and(|t| t.is_video);
        let needs_new_cluster = match &self.cluster {
            None => true,
            Some(c) => {
                (track_is_video && is_key)
                    || ts.saturating_sub(c.start_ticks) > self.max_cluster_ms
                    || i16::try_from(ts.saturating_sub(c.start_ticks)).is_err()
            }
        };
        if needs_new_cluster {
            self.flush_cluster()?;
            self.cluster_starts.push(self.out.pos());
            self.cluster = Some(Cluster {
                start_ticks: ts,
                body: Vec::new(),
                byte_pos: self.out.pos(),
                keyframe_opened: track_is_video && is_key,
            });
        }

        let Some(cluster) = self.cluster.as_mut() else {
            return Err(Error::InvalidData("matroska: no open cluster"));
        };
        let rel_ts = ts.saturating_sub(cluster.start_ticks);

        // `Packet::duration` is always microseconds (see `vaco_core::Duration`),
        // independent of the stream's time base, so it is converted to
        // `TimestampScale` ticks (1 tick == 1 ms, fixed in `info_bytes`)
        // directly rather than through the packet-timestamp rescale chain.
        // `ZERO` is also the field's default for "not stated", so it is
        // treated as absent rather than as a real zero-length block.
        let duration_ticks: Option<i64> = if packet.duration == vaco_core::Duration::ZERO {
            None
        } else {
            packet.duration.to_ticks(Rational::new(1, 1000))
        };
        let track = self.tracks.get_mut(idx).ok_or(Error::InvalidData(
            "matroska: packet names an unknown stream",
        ))?;
        #[allow(
            clippy::integer_division,
            reason = "converting a nanosecond count to whole TimestampScale ticks is an exact \
                      unit change, not a ratio computation"
        )]
        let default_duration_ticks = track
            .default_duration_ns
            .map(|ns| i64::try_from(ns / 1_000_000).unwrap_or(i64::MAX));
        let needs_duration = duration_ticks.is_some() && duration_ticks != default_duration_ticks;

        // Reordering alone does **not** call for a `BlockGroup`. It reads like
        // it should — `SimpleBlock` cannot carry a `ReferenceBlock`, and a
        // B-frame plainly references other frames — but Matroska has no notion
        // of a decode timestamp at all: a block's timestamp is its presentation
        // time and decode order is file order, so there is nothing for a
        // `ReferenceBlock` to state that the format does not already imply.
        //
        // Measured on `ffmpeg -c copy -f matroska`, remuxing reordered H.264
        // (and again with AAC alongside it): **every** block is a
        // `SimpleBlock` — 125 of them in the first cluster, zero
        // `BlockGroup`s. We wrote 94 `BlockGroup`s and 31 `SimpleBlock`s for
        // the same input, which cost 1697 bytes across two clusters and, worse,
        // dropped the keyframe flag on every frame it wrapped, since a
        // `BlockGroup` states keyframe-ness only by the *absence* of a
        // `ReferenceBlock` (CONFORMANCE-FINDINGS 37).
        let block_bytes = if needs_duration {
            block::block_group(
                track.number,
                rel_ts,
                packet.payload(),
                duration_ticks.map(|d| u64::try_from(d).unwrap_or(0)),
                None,
            )?
        } else {
            block::simple_block(track.number, rel_ts, is_key, packet.payload())?
        };
        cluster.body.extend_from_slice(&block_bytes);

        let end_ticks = ts.saturating_add(duration_ticks.unwrap_or(0)).max(0);
        self.max_end_ticks = self
            .max_end_ticks
            .max(u64::try_from(end_ticks).unwrap_or(0));
        Ok(())
    }
}

impl Muxer for MatroskaMuxer {
    fn flags(&self) -> FormatFlags {
        FormatFlags::empty()
    }

    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.header_written {
            return Err(Error::InvalidData(
                "matroska: streams must be added before the header is written",
            ));
        }
        let media = params
            .effective_media_type()
            .ok_or(Error::Unsupported("matroska: stream has no media type"))?;
        let is_video = match media {
            MediaType::Video => true,
            MediaType::Audio => false,
            _ => return Err(Error::Unsupported("matroska: only video and audio streams")),
        };
        let codec_id = params
            .codec_id
            .ok_or(Error::Unsupported("matroska: stream has no codec id"))?;

        if self.variant.is_webm {
            let allowed = if is_video {
                codec::webm_allows_video(codec_id)
            } else {
                codec::webm_allows_audio(codec_id)
            };
            if !allowed {
                return Err(Error::Unsupported(codec::WEBM_REJECTION));
            }
        }
        let codec_str = codec::codec_id_str(codec_id)
            .ok_or(Error::Unsupported("matroska: codec has no CodecID mapping"))?;

        // Measured against `ffmpeg 8.1`: a `webm` output needs `DocTypeVersion`
        // 4 once Opus is present (`CodecDelay`/`SeekPreRoll`); `matroska` is
        // always 4 regardless of codec.
        if codec_id == CodecId::Opus {
            self.needs_doctype_v4 = true;
        }

        let mut t = TrackOut {
            number: self.tracks.len() as u64 + 1,
            is_video,
            codec_id: codec_str,
            default_duration_ns: None,
            width: 0,
            height: 0,
            sample_rate: 0.0,
            channels: 1,
            bit_depth: None,
            extradata: params.extradata.clone(),
            field_order: vaco_codec_core::FieldOrder::default(),
            chroma_location: vaco_color::ChromaLocation::default(),
        };
        if is_video {
            let v = params.video.as_ref().ok_or(Error::Unsupported(
                "matroska: video stream has no VideoParameters",
            ))?;
            t.width = v.width;
            t.height = v.height;
            t.field_order = v.field_order;
            t.chroma_location = v.color.chroma_location;
            if v.frame_rate.is_defined() && !v.frame_rate.is_zero() && !v.frame_rate.is_infinite() {
                let per_frame = v.frame_rate.inverse(); // seconds per frame, as num/den
                let secs = f64::from(per_frame.num) / f64::from(per_frame.den);
                if secs.is_finite() && secs > 0.0 {
                    t.default_duration_ns = Some((secs * 1_000_000_000.0).round() as u64);
                }
            }
        } else {
            let a = params.audio.as_ref().ok_or(Error::Unsupported(
                "matroska: audio stream has no AudioParameters",
            ))?;
            if a.sample_rate == 0 {
                return Err(Error::Unsupported(
                    "matroska: audio stream has no sample rate",
                ));
            }
            t.sample_rate = f64::from(a.sample_rate);
            t.channels = a.layout.as_ref().map_or(1, |l| u64::from(l.channels));
            if codec_str.starts_with("A_PCM") {
                t.bit_depth = a.bits_per_coded_sample.map(u64::from).or(Some(16));
            }
        }

        let idx = u32::try_from(self.tracks.len())
            .map_err(|_| Error::Unsupported("matroska: too many tracks"))?;
        self.tracks.push(t);
        Ok(idx)
    }

    fn write_header(&mut self) -> Result<()> {
        if self.header_written {
            return Err(Error::InvalidData("matroska: header written twice"));
        }
        if self.tracks.is_empty() {
            return Err(Error::Unsupported("matroska: no streams to mux"));
        }
        self.header_written = true;
        // The actual `Segment`/`Tracks` bytes are deliberately not written
        // here — see `pending_packets`' field docs. `first_packet_seen` is
        // what `write_packet` polls to know when it is safe to commit.
        self.first_packet_seen = vec![false; self.tracks.len()];
        self.wrote_first_block = vec![false; self.tracks.len()];
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData(
                "matroska: packet written before the header",
            ));
        }
        let idx = usize::try_from(packet.stream_index)
            .ok()
            .filter(|&i| i < self.tracks.len())
            .ok_or(Error::InvalidData(
                "matroska: packet names an unknown stream",
            ))?;

        if !self.header_flushed {
            self.adopt_new_extradata(idx, packet);
            if let Some(seen) = self.first_packet_seen.get_mut(idx) {
                *seen = true;
            }
            self.pending_packets.push(packet.clone());
            if self.first_packet_seen.iter().all(|&seen| seen) {
                self.flush_header_bytes()?;
                let pending = core::mem::take(&mut self.pending_packets);
                for p in pending {
                    let pidx = usize::try_from(p.stream_index)
                        .ok()
                        .filter(|&i| i < self.tracks.len())
                        .ok_or(Error::InvalidData(
                            "matroska: packet names an unknown stream",
                        ))?;
                    self.write_block(pidx, &p)?;
                }
            }
            return Ok(());
        }

        self.write_block(idx, packet)
    }

    fn stream_time_base(&self, _stream_index: u32) -> Option<Rational> {
        // `TimestampScale` is fixed at 1_000_000 ns/tick (see `info_bytes`),
        // which is one millisecond per tick — shared by every track, per
        // RFC 9559 (unlike MP4's per-track time base).
        Some(Rational::new(1, 1000))
    }

    fn write_trailer(&mut self) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData(
                "matroska: trailer written before the header",
            ));
        }
        if self.trailer_written {
            return Err(Error::InvalidData("matroska: trailer written twice"));
        }
        self.trailer_written = true;

        // Safety net for `pending_packets`' deferred flush: reached whenever
        // some declared track never produced a single packet (an empty
        // stream, or a file with zero packets at all) — `write_packet` alone
        // would then wait forever for a first packet that is never coming.
        // Whatever extradata every track has by now (its `add_stream`-time
        // value, or an adopted `NewExtradata` from a track that *did*
        // produce at least one packet) is final.
        if !self.header_flushed {
            self.flush_header_bytes()?;
            let pending = core::mem::take(&mut self.pending_packets);
            for p in pending {
                let pidx = usize::try_from(p.stream_index)
                    .ok()
                    .filter(|&i| i < self.tracks.len())
                    .ok_or(Error::InvalidData(
                        "matroska: packet names an unknown stream",
                    ))?;
                self.write_block(pidx, &p)?;
            }
        }

        self.flush_cluster()?;

        let seekable = self.out.is_seekable();
        // `Cues` is written only when the sink can later seek back and add
        // its `Seek` entry to the `SeekHead` reservation — measured directly
        // on a pipe: the reference omits `Cues` entirely there, not merely
        // its index entry (see the module docs' *`CRC-32` and `SeekHead`*
        // section). A seekable sink with nothing to index (no keyframe ever
        // opened a cluster) keeps the pre-existing "nothing to write"
        // behaviour.
        if seekable && !self.cues.is_empty() {
            let cues_pos_rel = self.out.pos().saturating_sub(self.segment_data_start);
            let cues = self.cues_bytes();
            self.out.write(&cues)?;

            if self.seekhead_reserved {
                let mut targets = self.seek_targets.clone();
                targets.push((el::CUES, cues_pos_rel));
                if let Some(region) = seekhead_and_void(&targets) {
                    let end = self.out.pos();
                    self.out.seek(self.segment_data_start)?;
                    self.out.write(&region)?;
                    self.out.seek(end)?;
                }
            }
        }

        if let Some(at) = self.info_start {
            // Rewrite the whole `Info` element with the now-known duration —
            // not just `Duration`'s own bytes — so the `CRC-32` covering
            // `Info`'s body stays valid. Guaranteed the same length as the
            // placeholder written at `write_header` (see `info_bytes`'s
            // docs), so nothing after it moves.
            let end = self.out.pos();
            let info = self.info_bytes(Some(self.max_end_ticks));
            self.out.seek(at)?;
            self.out.write(&info)?;
            self.out.seek(end)?;
        }

        if seekable {
            let end = self.out.pos();
            let size = end.saturating_sub(self.segment_data_start);
            self.out.seek(self.segment_size_at)?;
            patch_known_size(&mut self.out, size)?;
            self.out.seek(end)?;
        }
        // Non-seekable: the Segment keeps the unknown-size marker written at
        // `write_header`, matching the reference measured on a pipe.

        self.out.flush()
    }

    fn set_metadata(&mut self, metadata: &MuxMetadata) -> Result<()> {
        // Just storage: every field this crate derives from `metadata` is
        // resolved lazily at `write_header` time (`tracks_bytes`,
        // `chapters_bytes`, `attachments_bytes`, `tags_bytes`, `title`) so
        // that `set_metadata` may run before or after `add_stream` — see
        // `tracks_bytes`'s docs for why that matters to a caller that drives
        // this muxer directly through `dyn Muxer`.
        self.metadata = metadata.clone();
        Ok(())
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_codec_core::{AudioParameters, VideoParameters};
    use vaco_core::Timestamp;
    use vaco_format_core::Demuxer;
    use vaco_format_core::discovery::NoParsers;
    use vaco_format_core::vacoraw::{ForwardOnlySink, MemorySink, SharedBytes};
    use vaco_io::MemorySource;
    use vaco_packet::PacketFlags;

    fn h264_params() -> CodecParameters {
        let mut p = CodecParameters::video().with_codec(CodecId::H264);
        p.video = Some(VideoParameters {
            width: 64,
            height: 48,
            frame_rate: Rational::new(25, 1),
            ..VideoParameters::default()
        });
        p.extradata = Some(vec![1, 2, 3, 4]);
        p
    }

    fn opus_params() -> CodecParameters {
        let mut p = CodecParameters::audio().with_codec(CodecId::Opus);
        p.audio = Some(AudioParameters {
            sample_rate: 48000,
            ..AudioParameters::default()
        });
        // `A_OPUS` is one of `codec::requires_extradata_str`'s entries: a
        // real `OpusHead` here, not fixture noise — see
        // `matroska_refuses_to_finalize_a_track_that_needs_extradata_and_has_none`
        // for the case this constructor deliberately does not cover.
        // `vaco_format_fixtures::opus::HEAD_MONO` is the shared copy every
        // container test suite in this tree uses now (planning/E2E-GAPS.md
        // #35 -- this crate's own local copy is what other crates' fixtures
        // drifted from before this consolidation).
        p.extradata = Some(vaco_format_fixtures::opus::HEAD_MONO.to_vec());
        p
    }

    fn pkt(stream: u32, pts: i64, key: bool) -> Packet {
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::strict());
        let mut p = Packet::from_slice(&mut budget, b"payload").unwrap();
        p.stream_index = stream;
        p.pts = Timestamp::new(pts);
        p.dts = Timestamp::new(pts);
        if key {
            p.flags = PacketFlags::KEY;
        }
        p
    }

    #[test]
    fn a_seekable_sink_gets_a_patched_known_segment_size() {
        let s = MemorySink::new();
        let buf = s.shared();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(s), &FormatOptions::default()).unwrap();
        let idx = mux.add_stream(&h264_params()).unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&pkt(idx, 0, true)).unwrap();
        mux.write_trailer().unwrap();

        let bytes = buf.snapshot();
        // Locate the Segment element and confirm its size is not the
        // all-ones unknown marker.
        let seg_id = vaco_format_ebml::id_bytes(el::SEGMENT);
        let at = bytes
            .windows(seg_id.len())
            .position(|w| w == seg_id.as_slice())
            .unwrap();
        let (size, _) = vaco_format_ebml::read_size(&bytes[at + seg_id.len()..], 8).unwrap();
        assert_ne!(size, vaco_format_ebml::Size::Unknown);
        assert_eq!(size.known(), Some(bytes.len() as u64 - (at as u64 + 12)));
    }

    #[test]
    fn a_non_seekable_sink_keeps_the_unknown_size_marker() {
        let s = ForwardOnlySink::new();
        let buf = s.shared();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(s), &FormatOptions::default()).unwrap();
        let idx = mux.add_stream(&h264_params()).unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&pkt(idx, 0, true)).unwrap();
        // A muxer that tried to seek this sink would already have failed by
        // now; `write_trailer` succeeding at all is half the property.
        mux.write_trailer().unwrap();

        let bytes = buf.snapshot();
        let seg_id = vaco_format_ebml::id_bytes(el::SEGMENT);
        let at = bytes
            .windows(seg_id.len())
            .position(|w| w == seg_id.as_slice())
            .unwrap();
        let (size, _) = vaco_format_ebml::read_size(&bytes[at + seg_id.len()..], 8).unwrap();
        assert_eq!(size, vaco_format_ebml::Size::Unknown);
    }

    #[test]
    fn webm_rejects_a_codec_outside_the_allow_list() {
        let mut mux =
            MatroskaMuxer::new_webm(Box::new(MemorySink::new()), &FormatOptions::default())
                .unwrap();
        assert!(mux.add_stream(&h264_params()).is_err());
    }

    #[test]
    fn webm_accepts_opus_and_bumps_doctype_version_to_four() {
        let s = MemorySink::new();
        let buf = s.shared();
        let mut mux = MatroskaMuxer::new_webm(Box::new(s), &FormatOptions::default()).unwrap();
        let idx = mux.add_stream(&opus_params()).unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&pkt(idx, 0, true)).unwrap();
        mux.write_trailer().unwrap();
        let bytes = buf.snapshot();
        // DocTypeVersion is the fifth uint element in the EBML header, value 4.
        assert!(bytes.windows(3).any(|w| w == [0x42, 0x87, 0x81]));
    }

    #[test]
    fn a_track_entry_carries_codec_private_verbatim() {
        let s = MemorySink::new();
        let buf = s.shared();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(s), &FormatOptions::default()).unwrap();
        let idx = mux.add_stream(&h264_params()).unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&pkt(idx, 0, true)).unwrap();
        mux.write_trailer().unwrap();
        let bytes = buf.snapshot();
        assert!(bytes.windows(4).any(|w| w == [1, 2, 3, 4]));
    }

    /// The mirror of the previous test, and the mirror bug: an MP4 source
    /// hands a VP9 stream a real, non-empty `vpcC` as `extradata` (measured,
    /// `vaco-demux-mp4` reads one back off a real ISOBMFF file), but
    /// Matroska/WebM's `V_VP9` never carries `CodecPrivate` at all — real
    /// `ffmpeg 9.0.1`'s own MP4→Matroska remux of the identical stream
    /// writes no `CodecPrivate` child whatsoever. Those `vpcC` bytes must
    /// never reach the file's `CodecPrivate`, even though they are real,
    /// non-empty, and would have passed the old unconditional check.
    #[test]
    fn vp9_never_gets_codec_private_even_with_real_extradata_present() {
        let s = MemorySink::new();
        let buf = s.shared();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(s), &FormatOptions::default()).unwrap();
        let mut p = CodecParameters::video().with_codec(CodecId::Vp9);
        p.video = Some(VideoParameters {
            width: 64,
            height: 48,
            frame_rate: Rational::new(25, 1),
            ..VideoParameters::default()
        });
        // A real 12-byte `vpcC` payload shape (version/flags/profile/level/
        // packed byte/three colour bytes/zero init-data size) — distinctive
        // marker bytes so a false negative here could not slip through by
        // accident.
        p.extradata = Some(vec![0x01, 0x00, 0x00, 0x00, 0xAA, 0xBB, 0x82, 0x02, 0x02, 0x02, 0x00, 0x00]);
        let idx = mux.add_stream(&p).unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&pkt(idx, 0, true)).unwrap();
        mux.write_trailer().unwrap();
        let bytes = buf.snapshot();
        assert!(
            !bytes.windows(4).any(|w| w == [0xAA, 0xBB, 0x82, 0x02]),
            "the vpcC bytes must never reach CodecPrivate for V_VP9"
        );
    }

    /// The FFV1 bug, reproduced with no codec crate at all: `add_stream` is
    /// handed a stale (wrong-codec) configuration record — exactly what
    /// `CodecParameters::with_codec` used to leave behind before its own fix
    /// — and the first packet on that stream attaches the *real* one as
    /// [`PacketSideData::NewExtradata`], the way `Ffv1Encoder` (and every
    /// other encoder whose configuration record depends on data only the
    /// first frame reveals) does.
    ///
    /// `CodecPrivate` in the finished file must be the adopted bytes, not
    /// the stale `add_stream`-time ones — the header commit has to have
    /// waited for this packet before writing `Tracks` at all.
    #[test]
    fn a_stream_first_packet_can_still_correct_codec_private() {
        let s = MemorySink::new();
        let buf = s.shared();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(s), &FormatOptions::default()).unwrap();
        let mut p = h264_params();
        p.extradata = Some(vec![0xDE, 0xAD, 0xBE, 0xEF]); // the "stale" record
        let idx = mux.add_stream(&p).unwrap();
        mux.write_header().unwrap();

        let mut first = pkt(idx, 0, true);
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::strict());
        let real = vaco_pool::Buffer::from_slice(&mut budget, &[1, 2, 3, 4]).unwrap();
        first.set_side_data(PacketSideData::NewExtradata(real));
        mux.write_packet(&first).unwrap();
        mux.write_packet(&pkt(idx, 1, false)).unwrap();
        mux.write_trailer().unwrap();

        let bytes = buf.snapshot();
        assert!(
            bytes.windows(4).any(|w| w == [1, 2, 3, 4]),
            "the adopted NewExtradata must reach CodecPrivate"
        );
        assert!(
            !bytes.windows(4).any(|w| w == [0xDE, 0xAD, 0xBE, 0xEF]),
            "the stale add_stream-time extradata must not survive into the file"
        );
    }

    /// The other half of the fix: a codec `codec::requires_extradata_str`
    /// says needs an out-of-band record must not silently finish a file
    /// with none — that is exactly how the FFV1 bug this module's docs
    /// describe produced a file `ffprobe`/`ffmpeg` could not open while
    /// `vaco` itself exited 0.
    #[test]
    fn matroska_refuses_to_finalize_a_track_that_needs_extradata_and_has_none() {
        let s = MemorySink::new();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(s), &FormatOptions::default()).unwrap();
        let mut p = h264_params();
        p.extradata = None; // never supplied, and no packet will offer one either
        let idx = mux.add_stream(&p).unwrap();
        mux.write_header().unwrap();

        // The lone stream's first packet is what would normally trigger the
        // deferred header flush; here it must surface the missing-record
        // error instead of writing a `Tracks` element with an empty
        // `CodecPrivate`.
        assert!(mux.write_packet(&pkt(idx, 0, true)).is_err());
    }

    /// The pathological path through the same rule: a track that never
    /// produces a single packet is only caught at `write_trailer`'s own
    /// safety-net flush, not left to silently finish the file.
    #[test]
    fn a_trailer_with_no_packets_still_refuses_a_missing_required_record() {
        let s = MemorySink::new();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(s), &FormatOptions::default()).unwrap();
        let mut p = h264_params();
        p.extradata = None;
        mux.add_stream(&p).unwrap();
        mux.write_header().unwrap();
        assert!(mux.write_trailer().is_err());
    }

    #[test]
    fn a_video_keyframe_produces_a_cue_point() {
        let s = MemorySink::new();
        let buf = s.shared();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(s), &FormatOptions::default()).unwrap();
        let idx = mux.add_stream(&h264_params()).unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&pkt(idx, 0, true)).unwrap();
        mux.write_trailer().unwrap();
        let bytes = buf.snapshot();
        let cues_id = vaco_format_ebml::id_bytes(el::CUES);
        assert!(
            bytes
                .windows(cues_id.len())
                .any(|w| w == cues_id.as_slice())
        );
    }

    /// CONFORMANCE-FINDINGS 19: a track's first packet with no pts at all is
    /// refused, not silently written as `pts=0` — measured against `ffmpeg
    /// 9.0.1`, which refuses an ASF/AVI-sourced video stream's first packet
    /// (neither format's demuxer states a video pts) with "Can't write
    /// packet with unknown timestamp" on `-c copy -f matroska`.
    #[test]
    fn a_first_packet_with_no_pts_is_refused_not_written_as_zero() {
        let s = MemorySink::new();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(s), &FormatOptions::default()).unwrap();
        let idx = mux.add_stream(&h264_params()).unwrap();
        mux.write_header().unwrap();

        let mut p = pkt(idx, 0, true);
        p.pts = Timestamp::NONE;
        assert!(mux.write_packet(&p).is_err());
    }

    /// The second and later packets of a track are not held to the same
    /// rule — only the reference's own "first" wording is matched. A
    /// missing `pts` past the first packet falls back to `dts` (or `0`)
    /// exactly as before this change.
    #[test]
    fn a_later_packet_with_no_pts_falls_back_rather_than_being_refused() {
        let s = MemorySink::new();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(s), &FormatOptions::default()).unwrap();
        let idx = mux.add_stream(&h264_params()).unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&pkt(idx, 0, true)).unwrap();

        let mut p = pkt(idx, 40, false);
        p.pts = Timestamp::NONE;
        assert!(mux.write_packet(&p).is_ok());
    }

    /// `Segment`'s direct children, as `(id, offset relative to Segment's
    /// data start)` — the shape every `SeekHead`/reservation test below
    /// needs, built once so each test only asserts.
    fn segment_children(bytes: &[u8]) -> Vec<(u32, u64)> {
        let caps = vaco_format_ebml::Caps::default();
        let top: Vec<_> = vaco_format_ebml::Slice::new(bytes, caps)
            .children()
            .collect();
        let segment = top.iter().find(|c| c.id == el::SEGMENT).unwrap();
        vaco_format_ebml::Slice::new(segment.data, caps)
            .children()
            .map(|c| (c.id, c.offset as u64))
            .collect()
    }

    #[test]
    fn every_level1_element_this_muxer_writes_carries_a_validating_crc32() {
        let s = MemorySink::new();
        let buf = s.shared();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(s), &FormatOptions::default()).unwrap();
        let idx = mux.add_stream(&h264_params()).unwrap();
        mux.write_header().unwrap();
        for i in 0..3i64 {
            mux.write_packet(&pkt(idx, i * 40, i == 0)).unwrap();
        }
        mux.write_trailer().unwrap();
        let bytes = buf.snapshot();

        let caps = vaco_format_ebml::Caps::default();
        let mut checked = Vec::new();
        for (id, _) in segment_children(&bytes) {
            let top: Vec<_> = vaco_format_ebml::Slice::new(&bytes, caps)
                .children()
                .collect();
            let segment = top.iter().find(|c| c.id == el::SEGMENT).unwrap();
            let child = vaco_format_ebml::Slice::new(segment.data, caps)
                .children()
                .find(|c| c.id == id && c.data.len() >= 6)
                .unwrap();
            let Ok((first_id, idl)) =
                vaco_format_ebml::read_id(child.data, vaco_format_ebml::MAX_ID_LEN)
            else {
                continue;
            };
            if first_id != el::CRC32 {
                continue; // `Void`: no CRC-32 child by design.
            }
            let (size, szl) =
                vaco_format_ebml::read_size(&child.data[idl..], vaco_format_ebml::MAX_SIZE_LEN)
                    .unwrap();
            let crc_len = size.known().unwrap() as usize;
            let crc_start = idl + szl;
            let declared_bytes = &child.data[crc_start..crc_start + crc_len];
            let mut declared_le = [0u8; 4];
            declared_le[..declared_bytes.len().min(4)]
                .copy_from_slice(&declared_bytes[..declared_bytes.len().min(4)]);
            let declared = u32::from_le_bytes(declared_le);
            let computed = vaco_hash::crc32(&child.data[crc_start + crc_len..]);
            assert_eq!(declared, computed, "id=0x{id:X}");
            checked.push(id);
        }
        // SeekHead, Info, Tracks, Cluster, Cues — every Level-1 element this
        // fixture produces (no Tags/Chapters/Attachments: no metadata set).
        assert_eq!(checked.len(), 5, "{checked:?}");
    }

    #[test]
    fn seekhead_reservation_is_the_measured_fixed_budget() {
        let s = MemorySink::new();
        let buf = s.shared();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(s), &FormatOptions::default()).unwrap();
        let idx = mux.add_stream(&h264_params()).unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&pkt(idx, 0, true)).unwrap();
        mux.write_trailer().unwrap();
        let bytes = buf.snapshot();

        let children = segment_children(&bytes);
        assert_eq!(children[0].0, el::SEEKHEAD);
        assert_eq!(children[1].0, el::VOID);
        // `Info` starts exactly `SEEKHEAD_RESERVED_BYTES` into the Segment's
        // data regardless of how large the real SeekHead body turned out —
        // that fixed budget, not a per-file computation, is the whole point
        // measured in the module docs.
        assert_eq!(children[2].0, el::INFO);
        // 161, not `SEEKHEAD_RESERVED_BYTES`: this pins the measured literal
        // (see the module docs) so a future edit to the constant is caught
        // here rather than silently redefining what "correct" means.
        assert_eq!(children[2].1, 161);
    }

    #[test]
    fn a_non_seekable_sink_writes_seekhead_without_cues_and_omits_cues_entirely() {
        let s = ForwardOnlySink::new();
        let buf = s.shared();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(s), &FormatOptions::default()).unwrap();
        let idx = mux.add_stream(&h264_params()).unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&pkt(idx, 0, true)).unwrap();
        mux.write_trailer().unwrap();
        let bytes = buf.snapshot();

        // Measured against `ffmpeg 8.1`: a non-seekable sink omits `Cues`
        // entirely, not merely its `Seek` entry (see the module docs).
        let cues_id = vaco_format_ebml::id_bytes(el::CUES);
        assert!(
            bytes
                .windows(cues_id.len())
                .all(|w| w != cues_id.as_slice()),
            "no Cues element at all on a non-seekable sink"
        );

        let caps = vaco_format_ebml::Caps::default();
        let children = segment_children(&bytes);
        let seekhead = children
            .iter()
            .find(|&&(id, _)| id == el::SEEKHEAD)
            .unwrap();
        let (_, seekhead_offset) = *seekhead;
        let top: Vec<_> = vaco_format_ebml::Slice::new(&bytes, caps)
            .children()
            .collect();
        let segment = top.iter().find(|c| c.id == el::SEGMENT).unwrap();
        let seekhead_child = vaco_format_ebml::Slice::new(segment.data, caps)
            .children()
            .find(|c| c.offset as u64 == seekhead_offset)
            .unwrap();
        let seek_count = vaco_format_ebml::Slice::new(seekhead_child.data, caps)
            .children()
            .filter(|c| c.id == el::SEEK)
            .count();
        assert_eq!(
            seek_count, 2,
            "Info and Tracks only: no Tags (no metadata set) and no Cues (non-seekable)"
        );
    }

    #[test]
    fn a_seekable_sinks_seekhead_indexes_cues_and_every_entry_points_at_its_real_target() {
        let s = MemorySink::new();
        let buf = s.shared();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(s), &FormatOptions::default()).unwrap();
        let idx = mux.add_stream(&h264_params()).unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&pkt(idx, 0, true)).unwrap();
        mux.write_trailer().unwrap();
        let bytes = buf.snapshot();

        let caps = vaco_format_ebml::Caps::default();
        let top: Vec<_> = vaco_format_ebml::Slice::new(&bytes, caps)
            .children()
            .collect();
        let segment = top.iter().find(|c| c.id == el::SEGMENT).unwrap();
        let children: Vec<_> = vaco_format_ebml::Slice::new(segment.data, caps)
            .children()
            .collect();
        let seekhead = children.iter().find(|c| c.id == el::SEEKHEAD).unwrap();

        let mut saw_cues = false;
        let mut entries = 0;
        for seek in vaco_format_ebml::Slice::new(seekhead.data, caps)
            .children()
            .filter(|c| c.id == el::SEEK)
        {
            let mut target_id = None;
            let mut position = None;
            for kid in vaco_format_ebml::Slice::new(seek.data, caps).children() {
                match kid.id {
                    el::SEEKID => {
                        target_id =
                            vaco_format_ebml::read_id(kid.data, vaco_format_ebml::MAX_ID_LEN)
                                .ok()
                                .map(|(v, _)| v);
                    }
                    el::SEEKPOSITION => position = vaco_format_ebml::as_uint(kid.data),
                    _ => {}
                }
            }
            let target_id = target_id.unwrap();
            let position = position.unwrap();
            if target_id == el::CUES {
                saw_cues = true;
            }
            let real = children
                .iter()
                .find(|c| c.offset as u64 == position)
                .unwrap();
            assert_eq!(
                real.id, target_id,
                "Seek entry for 0x{target_id:X} points at the wrong element"
            );
            entries += 1;
        }
        assert!(
            saw_cues,
            "a seekable sink with a keyframe should index Cues too"
        );
        // Info, Tracks, Cues (no Tags: no metadata was set on this muxer).
        assert_eq!(entries, 3);
    }

    #[test]
    fn a_second_header_or_trailer_is_refused() {
        let mut mux =
            MatroskaMuxer::new_matroska(Box::new(MemorySink::new()), &FormatOptions::default())
                .unwrap();
        let idx = mux.add_stream(&h264_params()).unwrap();
        mux.write_header().unwrap();
        assert!(mux.write_header().is_err());
        mux.write_packet(&pkt(idx, 0, true)).unwrap();
        mux.write_trailer().unwrap();
        assert!(mux.write_trailer().is_err());
    }

    #[test]
    fn the_whole_file_reads_back_through_the_demuxer() {
        let s = MemorySink::new();
        let buf: SharedBytes = s.shared();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(s), &FormatOptions::default()).unwrap();
        let idx = mux.add_stream(&h264_params()).unwrap();
        mux.write_header().unwrap();
        for i in 0..5i64 {
            mux.write_packet(&pkt(idx, i * 40, i == 0)).unwrap();
        }
        mux.write_trailer().unwrap();
        let bytes = buf.snapshot();

        let src: Box<dyn vaco_io::MediaSource> = Box::new(MemorySource::new(bytes));
        let mut demux =
            vaco_demux_matroska::MatroskaDemuxer::open(src, &NoParsers, &FormatOptions::default())
                .unwrap();
        assert_eq!(demux.streams().len(), 1);
        let mut count = 0;
        while let Ok(p) = demux.read_packet() {
            assert_eq!(p.payload(), b"payload");
            count += 1;
        }
        assert_eq!(count, 5);
    }

    // ----------------------------------------- gap 1: set_metadata round trip

    /// Sets file tags, a per-stream language/title and a custom per-stream
    /// tag, and a chapter, muxes, then reads every bit of it back through
    /// [`vaco_demux_matroska::MatroskaDemuxer`] — the "best test" the CL-16
    /// brief asks for, exercised inside this crate rather than only at the
    /// CLI layer.
    #[test]
    fn set_metadata_round_trips_through_the_demuxer() {
        use vaco_format_core::Chapter;
        use vaco_format_core::metadata::MuxMetadata;

        let s = MemorySink::new();
        let buf: SharedBytes = s.shared();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(s), &FormatOptions::default()).unwrap();
        let idx = mux.add_stream(&h264_params()).unwrap();

        let mut meta = MuxMetadata {
            tags: vec![
                ("title".to_owned(), "Global Title".to_owned()),
                ("comment".to_owned(), "a global comment".to_owned()),
            ],
            chapters: vec![Chapter {
                id: 0,
                time_base: Rational::new(1, 1000),
                start: Timestamp::new(0),
                end: Timestamp::new(1000),
                metadata: vec![("title".to_owned(), "Chapter One".to_owned())],
            }],
            ..MuxMetadata::default()
        };
        meta.stream_tags = vec![vec![
            ("language".to_owned(), "eng".to_owned()),
            ("title".to_owned(), "Video Track".to_owned()),
            ("custom".to_owned(), "custom-value".to_owned()),
        ]];
        mux.set_metadata(&meta).unwrap();

        mux.write_header().unwrap();
        mux.write_packet(&pkt(idx, 0, true)).unwrap();
        mux.write_trailer().unwrap();

        let bytes = buf.snapshot();
        let src: Box<dyn vaco_io::MediaSource> = Box::new(MemorySource::new(bytes));
        let demux =
            vaco_demux_matroska::MatroskaDemuxer::open(src, &NoParsers, &FormatOptions::default())
                .unwrap();

        assert!(
            demux
                .metadata()
                .contains(&("title".to_owned(), "Global Title".to_owned()))
        );
        assert!(
            demux
                .metadata()
                .contains(&("COMMENT".to_owned(), "a global comment".to_owned())),
            "non-dedicated global tags go through Tags, uppercased: {:?}",
            demux.metadata()
        );

        let stream = &demux.streams()[0];
        assert!(
            stream
                .metadata
                .contains(&("language".to_owned(), "eng".to_owned()))
        );
        assert!(
            stream
                .metadata
                .contains(&("title".to_owned(), "Video Track".to_owned()))
        );
        assert!(
            stream
                .metadata
                .contains(&("CUSTOM".to_owned(), "custom-value".to_owned())),
            "non-dedicated per-stream tags go through Tags, uppercased: {:?}",
            stream.metadata
        );

        assert_eq!(demux.chapters().len(), 1);
        assert!(
            demux.chapters()[0]
                .metadata
                .contains(&("title".to_owned(), "Chapter One".to_owned()))
        );
        assert_eq!(demux.chapters()[0].start.ticks(), Some(0));
    }

    /// `-disposition`'s two measured flags, round-tripped through this
    /// crate's own demuxer. Measured against real `ffmpeg 8.1` output
    /// (`-disposition:v default`, `forced`, `default+forced`, compared
    /// byte-for-byte): `FlagDefault` is omitted when the bit is set (RFC
    /// 9559 says 1 is the implied default) and written as an explicit `0`
    /// otherwise; `FlagForced` is written only when set. This pins both
    /// halves of that rule, plus one flag (`original`) not independently
    /// byte-measured, only checked by symmetry with this crate's own
    /// demuxer.
    #[test]
    fn disposition_round_trips_through_the_demuxer_per_the_measured_flagdefault_rule() {
        use vaco_format_core::metadata::MuxMetadata;

        let s = MemorySink::new();
        let buf: SharedBytes = s.shared();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(s), &FormatOptions::default()).unwrap();
        let idx = mux.add_stream(&h264_params()).unwrap();

        let meta = MuxMetadata {
            stream_disposition: vec![Disposition::FORCED.union(Disposition::ORIGINAL)],
            ..MuxMetadata::default()
        };
        mux.set_metadata(&meta).unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&pkt(idx, 0, true)).unwrap();
        mux.write_trailer().unwrap();

        let bytes = buf.snapshot();
        let src: Box<dyn vaco_io::MediaSource> = Box::new(MemorySource::new(bytes));
        let demux =
            vaco_demux_matroska::MatroskaDemuxer::open(src, &NoParsers, &FormatOptions::default())
                .unwrap();
        let stream = &demux.streams()[0];
        assert!(!stream.disposition.contains(Disposition::DEFAULT));
        assert!(stream.disposition.contains(Disposition::FORCED));
        assert!(stream.disposition.contains(Disposition::ORIGINAL));
    }

    #[test]
    fn an_explicit_default_flag_is_written_as_the_implied_ebml_default_and_so_omitted() {
        // The measured half that is easy to get backwards: asking for
        // `default` explicitly must not force an explicit `FlagDefault=1`
        // onto the wire, because RFC 9559 already implies 1 when the
        // element is absent -- writing it anyway would still round-trip
        // through this crate's own demuxer but would diverge from the
        // reference byte-for-byte, exactly the class of difference `705779d`
        // asks to be measured and reported rather than left undetected.
        let disposition = Disposition::DEFAULT;
        let name = None;
        let track = TrackOut {
            number: 1,
            codec_id: "V_MPEG4/ISO/AVC",
            is_video: true,
            width: 32,
            height: 32,
            sample_rate: 0.0,
            channels: 0,
            bit_depth: None,
            default_duration_ns: None,
            field_order: vaco_codec_core::FieldOrder::Progressive,
            chroma_location: vaco_color::ChromaLocation::Unspecified,
            extradata: None,
        };
        let bytes = MatroskaMuxer::track_entry_bytes(&track, name, "und", disposition);
        let needle = write_uint(el::FLAGDEFAULT, 0);
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle),
            "FlagDefault=0 must not appear when the disposition's default bit is set"
        );
    }

    /// `vaco-cli`'s scheduler drives a raw `dyn Muxer` and has no way to
    /// guarantee `set_metadata` runs after `add_stream` — so
    /// this crate must not depend on that order. Same assertions as
    /// [`set_metadata_round_trips_through_the_demuxer`], with `set_metadata`
    /// moved before `add_stream`.
    #[test]
    fn set_metadata_before_add_stream_still_resolves_per_stream_fields() {
        use vaco_format_core::metadata::MuxMetadata;

        let s = MemorySink::new();
        let buf: SharedBytes = s.shared();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(s), &FormatOptions::default()).unwrap();

        let meta = MuxMetadata {
            stream_tags: vec![vec![
                ("language".to_owned(), "fra".to_owned()),
                ("title".to_owned(), "Piste Video".to_owned()),
            ]],
            ..MuxMetadata::default()
        };
        mux.set_metadata(&meta).unwrap();

        let idx = mux.add_stream(&h264_params()).unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&pkt(idx, 0, true)).unwrap();
        mux.write_trailer().unwrap();

        let bytes = buf.snapshot();
        let src: Box<dyn vaco_io::MediaSource> = Box::new(MemorySource::new(bytes));
        let demux =
            vaco_demux_matroska::MatroskaDemuxer::open(src, &NoParsers, &FormatOptions::default())
                .unwrap();
        let stream = &demux.streams()[0];
        assert!(
            stream
                .metadata
                .contains(&("language".to_owned(), "fra".to_owned()))
        );
        assert!(
            stream
                .metadata
                .contains(&("title".to_owned(), "Piste Video".to_owned()))
        );
    }

    #[test]
    fn set_metadata_default_writes_nothing_extra() {
        // Every pre-existing call site never calls `set_metadata` at all, so
        // this exercises the same "nothing changes" property directly rather
        // than through the default trait method.
        let s = MemorySink::new();
        let buf = s.shared();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(s), &FormatOptions::default()).unwrap();
        let idx = mux.add_stream(&h264_params()).unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&pkt(idx, 0, true)).unwrap();
        mux.write_trailer().unwrap();
        let bytes = buf.snapshot();

        assert!(
            bytes
                .windows(vaco_format_ebml::id_bytes(el::TAGS).len())
                .all(|w| w != vaco_format_ebml::id_bytes(el::TAGS).as_slice()),
            "no Tags element without set_metadata"
        );
        assert!(
            bytes
                .windows(vaco_format_ebml::id_bytes(el::CHAPTERS).len())
                .all(|w| w != vaco_format_ebml::id_bytes(el::CHAPTERS).as_slice()),
            "no Chapters element without set_metadata"
        );
    }
}
