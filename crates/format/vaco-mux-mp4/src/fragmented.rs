//! Fragmented muxing: `moof`/`traf`/`tfhd`/`tfdt`/`trun`, the `movflags`
//! fragmentation policy, `mfra`, and a buffered `sidx` for `dash`/`cmaf`.
//!
//! # Fragment boundaries
//!
//! Checked on every packet, in this order, any one of which starts a new
//! fragment before the packet that triggered it is buffered into the next
//! one:
//!
//! 1. `frag_every_frame` — always.
//! 2. `frag_keyframe` — the packet is a sync sample on the first track.
//! 3. `frag_duration` — the packet's DTS, in the *first* track's time base,
//!    has advanced past the fragment's start by at least the threshold.
//! 4. `frag_size` — the fragment's accumulated payload bytes have reached
//!    the threshold.
//!
//! A file with none of these set still fragments — once, at [`finish`] — so
//! `empty_moov` alone produces one giant final fragment rather than a file
//! with no samples at all.
//!
//! # Byte addressing
//!
//! `default_base_moof` (and `dash`/`cmaf`, which imply it) sets
//! `tfhd.default-base-is-moof` and gives `trun.data_offset` relative to the
//! start of the enclosing `moof`, so nothing here depends on the fragment's
//! *absolute* file position — which is exactly what makes the buffered
//! `sidx` path below safe: inserting `sidx` between `moov` and the first
//! fragment shifts every fragment's absolute offset uniformly, and
//! moof-relative addressing does not notice.
//!
//! Otherwise (no `default_base_moof`) every `tfhd` states an explicit
//! `base_data_offset` — one `traf` at a time, always stated, rather than
//! only on the first `traf` and relying on the "end of the previous track
//! fragment's data" carry-over §8.8.7.1 also allows: stating it every time is
//! one shape to compute instead of two.
//!
//! # What is simplified
//!
//! One `mfra ▸ tfra` entry is written per track **per fragment**, pointing at
//! that fragment's first sample when it is a sync sample — correct whenever a
//! fragment starts on one (always true under `frag_keyframe`, and for a
//! single-fragment file), approximate otherwise. `sidx` covers the whole file
//! as one presentation timeline with one reference per fragment — a
//! single-segment index, not DASH's multi-`Representation` manifest story,
//! which is out of scope for a container muxer. Both are recorded in
//! `docs/format/vaco-mux-mp4.md`.

use vaco_core::{Error, Result};
use vaco_format_isom::frag::{
    TR_DATA_OFFSET, TR_SAMPLE_CTS_OFFSET, TR_SAMPLE_DURATION, TR_SAMPLE_FLAGS, TR_SAMPLE_SIZE,
};
use vaco_format_isom::writer;
use vaco_io::IoWriter;

use crate::options::{MovFlags, MuxOptions};
use crate::track::TrackState;

/// A `sample_flags` word for a non-sync sample: `sample_is_non_sync_sample`
/// set, `sample_depends_on == 1` (not intra).
const FLAGS_NON_SYNC: u32 = 0x0001_0000 | (1 << 24);
/// A sync sample: `sample_depends_on == 2` (does not depend on others).
const FLAGS_SYNC: u32 = 2 << 24;

#[derive(Debug, Clone)]
struct PendingSample {
    payload: Vec<u8>,
    duration: u32,
    cts: i32,
    is_sync: bool,
}

#[derive(Debug, Clone, Default)]
struct PendingTrack {
    samples: Vec<PendingSample>,
    /// DTS of this track's first buffered sample, for `tfdt`.
    start_dts: Option<i64>,
}

/// One `sidx` reference already committed: byte length, duration and
/// whether it starts with a stream access point.
#[derive(Debug, Clone, Copy)]
struct SidxSeg {
    size: u32,
    duration: u32,
    starts_with_sap: bool,
}

/// Fragmented-mode session state.
#[derive(Debug, Default)]
pub struct FragmentedState {
    sequence_number: u32,
    pending: Vec<PendingTrack>,
    /// Accumulated payload bytes in the fragment being built, for `frag_size`.
    frag_bytes: u64,
    /// DTS (track 0's time base) the current fragment started at, for `frag_duration`.
    frag_start_dts0: Option<i64>,
    /// Per-track `tfra` rows, `moof_offset` recorded relative to the start of
    /// the fragment stream (i.e. as if `sidx` did not exist); corrected in
    /// [`finish`] once `sidx`'s final length is known.
    tfra: Vec<Vec<writer::TfraEntry>>,
    /// Bytes of `ftyp ++ moov` already written (or, when buffered, that will
    /// precede the fragment stream) — the base every `tfra.moof_offset` is
    /// measured from.
    header_len: u64,
    /// `Some` when the whole fragment stream must be buffered before
    /// anything is written — `dash`/`cmaf`'s `sidx`, which must be known
    /// before the first fragment on disk.
    buffer: Option<Vec<u8>>,
    sidx_segments: Vec<SidxSeg>,
}

impl FragmentedState {
    #[must_use]
    pub fn new(track_count: usize) -> Self {
        Self {
            pending: vec![PendingTrack::default(); track_count],
            tfra: vec![Vec::new(); track_count],
            ..Self::default()
        }
    }
}

/// `ftyp` + the initial (empty-tables) `moov`, and — for `dash`/`cmaf` — a
/// switch to buffering the fragment stream instead of writing straight to
/// the sink.
///
/// # Errors
/// Propagates I/O failure.
pub fn write_header(
    out: &mut IoWriter,
    opts: &MuxOptions,
    state: &mut FragmentedState,
    tracks: &[TrackState],
    movie_timescale: u32,
) -> Result<()> {
    // Defensive: `Muxer::init` is where `MuxBuilder` normally sizes this to
    // the final track count, but a caller driving the trait directly could
    // skip it — and a size mismatch here would silently drop every sample
    // (`buffer_sample` finds no slot and no-ops) rather than error, which is
    // a much worse failure than resizing on the way in.
    if state.pending.len() != tracks.len() {
        *state = FragmentedState::new(tracks.len());
    }
    let flags = opts.effective_flags();
    let mut header = crate::brand::file_type_box(opts.brand);
    header.extend_from_slice(&build_initial_moov(tracks, opts, movie_timescale));
    state.header_len = u64::try_from(header.len()).unwrap_or(u64::MAX);
    // `header` is written immediately either way — only the *fragment
    // stream* is ever buffered, so `sidx` can still be spliced in between
    // the two without moving anything already on the sink.
    if flags.intersects(MovFlags::DASH | MovFlags::CMAF) {
        state.buffer = Some(Vec::new());
    }
    out.write(&header)
}

fn build_initial_moov(tracks: &[TrackState], opts: &MuxOptions, movie_timescale: u32) -> Vec<u8> {
    let creation_time = if opts.bitexact {
        0
    } else {
        opts.creation_time_unix
            .map_or(0, vaco_format_isom::movie::from_unix_time)
    };
    let next_track_id = tracks
        .iter()
        .map(|t| t.track_id)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let mut body = writer::mvhd(&writer::MvhdFields {
        creation_time,
        modification_time: creation_time,
        timescale: movie_timescale,
        duration: 0,
        rate: 0x0001_0000,
        volume: 0x0100,
        matrix: vaco_format_isom::fixed::IDENTITY_MATRIX,
        next_track_id,
    });
    for t in tracks {
        body.extend_from_slice(&build_empty_trak(t, creation_time));
    }
    let mut mvex = Vec::new();
    for t in tracks {
        mvex.extend_from_slice(&writer::trex(t.track_id, 1, 0, 0, FLAGS_NON_SYNC));
    }
    body.extend_from_slice(&writer::mvex(&mvex));
    if let Some(udta) = crate::meta::build_udta(opts) {
        body.extend_from_slice(&udta);
    }
    vaco_format_isom::build::bx(b"moov", &body)
}

fn build_empty_trak(track: &TrackState, creation_time: u64) -> Vec<u8> {
    let mut minf = Vec::new();
    minf.extend_from_slice(&match track.media {
        vaco_core::MediaType::Audio => writer::smhd(),
        _ => writer::vmhd(),
    });
    minf.extend_from_slice(&writer::dinf_self_contained());
    let mut stbl = writer::stsd(std::slice::from_ref(&track.entry.bytes));
    stbl.extend_from_slice(&writer::stts(&[]));
    stbl.extend_from_slice(&writer::stsc(&[]));
    stbl.extend_from_slice(&writer::stsz(&[]));
    stbl.extend_from_slice(&writer::chunk_offsets(&[]));
    minf.extend_from_slice(&vaco_format_isom::build::bx(b"stbl", &stbl));

    let mut mdia = Vec::new();
    mdia.extend_from_slice(&writer::mdhd(&writer::MdhdFields {
        creation_time,
        modification_time: creation_time,
        timescale: track.timescale,
        duration: 0,
        language: track.language,
    }));
    mdia.extend_from_slice(&writer::hdlr(
        track.handler,
        if track.media == vaco_core::MediaType::Audio {
            "SoundHandler"
        } else {
            "VideoHandler"
        },
    ));
    mdia.extend_from_slice(&vaco_format_isom::build::bx(b"minf", &minf));

    let mut trak = writer::tkhd(&writer::TkhdFields {
        flags: writer::tkhd_flags::ENABLED | writer::tkhd_flags::IN_MOVIE,
        creation_time,
        modification_time: creation_time,
        track_id: track.track_id,
        duration: 0,
        layer: 0,
        alternate_group: 0,
        volume: track.volume,
        matrix: vaco_format_isom::fixed::IDENTITY_MATRIX,
        width: track.width,
        height: track.height,
    });
    trak.extend_from_slice(&vaco_format_isom::build::bx(b"mdia", &mdia));
    vaco_format_isom::build::bx(b"trak", &trak)
}

/// Whether the next packet on `track_index` must start a new fragment before
/// it is buffered.
#[must_use]
pub fn should_flush(
    state: &FragmentedState,
    opts: &MuxOptions,
    track_index: usize,
    dts: i64,
    is_sync: bool,
) -> bool {
    if !has_pending(state) {
        return false;
    }
    let flags = opts.effective_flags();
    if flags.contains(MovFlags::FRAG_EVERY_FRAME) {
        return true;
    }
    if flags.contains(MovFlags::FRAG_KEYFRAME) && track_index == 0 && is_sync {
        return true;
    }
    if let Some(threshold) = opts.frag_duration
        && let Some(start) = state.frag_start_dts0
        && track_index == 0
        && dts.saturating_sub(start) >= threshold.0
    {
        return true;
    }
    if let Some(threshold) = opts.frag_size
        && state.frag_bytes >= threshold
    {
        return true;
    }
    false
}

/// Buffer one sample into the fragment being built.
pub fn buffer_sample(
    state: &mut FragmentedState,
    track_index: usize,
    payload: Vec<u8>,
    dts: i64,
    cts: i32,
    is_sync: bool,
    duration: u32,
) {
    if track_index == 0 && state.frag_start_dts0.is_none() {
        state.frag_start_dts0 = Some(dts);
    }
    state.frag_bytes = state.frag_bytes.saturating_add(payload.len() as u64);
    let Some(pending) = state.pending.get_mut(track_index) else {
        return;
    };
    if pending.start_dts.is_none() {
        pending.start_dts = Some(dts);
    }
    pending.samples.push(PendingSample {
        payload,
        duration,
        cts,
        is_sync,
    });
}

/// Whether anything is currently buffered, across every track.
#[must_use]
pub fn has_pending(state: &FragmentedState) -> bool {
    state.pending.iter().any(|t| !t.samples.is_empty())
}

/// The position the *next* fragment will start at, in the eventual final
/// file — as if no `sidx` will ever be inserted before it. [`finish`] adds
/// `sidx`'s length to every recorded `tfra` entry once that length is known.
fn fragment_stream_pos(out: &IoWriter, state: &FragmentedState) -> u64 {
    match &state.buffer {
        Some(buf) => state
            .header_len
            .saturating_add(u64::try_from(buf.len()).unwrap_or(u64::MAX)),
        None => out.pos(),
    }
}

/// Emit the fragment currently buffered: one `moof` (or, under
/// `separate_moof`, one `moof` per track) plus its `mdat`.
///
/// # Errors
/// Propagates I/O failure.
pub fn flush_fragment(
    out: &mut IoWriter,
    state: &mut FragmentedState,
    tracks: &[TrackState],
    opts: &MuxOptions,
) -> Result<()> {
    if !has_pending(state) {
        return Ok(());
    }
    state.sequence_number = state.sequence_number.saturating_add(1);
    let flags = opts.effective_flags();
    let default_base_moof =
        flags.intersects(MovFlags::DEFAULT_BASE_MOOF | MovFlags::DASH | MovFlags::CMAF);
    let omit_offset = flags.contains(MovFlags::OMIT_TFHD_OFFSET) || default_base_moof;
    let separate = flags.contains(MovFlags::SEPARATE_MOOF);

    let fragment_start = fragment_stream_pos(out, state);
    let mut fragment_bytes: Vec<u8> = Vec::new();
    let mut any_sap = false;
    let mut first_sap = true;

    if separate {
        for (idx, track) in tracks.iter().enumerate() {
            let has_samples = state
                .pending
                .get(idx)
                .is_some_and(|p| !p.samples.is_empty());
            if !has_samples {
                continue;
            }
            let piece_start = fragment_start
                .saturating_add(u64::try_from(fragment_bytes.len()).unwrap_or(u64::MAX));
            let (moof, mdat) = build_one_track_fragment(
                state,
                track,
                idx,
                piece_start,
                default_base_moof,
                omit_offset,
            )?;
            let is_sync = state
                .pending
                .get(idx)
                .and_then(|p| p.samples.first())
                .is_some_and(|s| s.is_sync);
            if is_sync {
                record_tfra(state, idx, piece_start, 1, 1);
                any_sap = true;
            }
            first_sap = first_sap && is_sync;
            fragment_bytes.extend_from_slice(&moof);
            fragment_bytes.extend_from_slice(&mdat);
        }
    } else {
        let (moof, mdat, traf_starts) = build_combined_fragment(
            state,
            tracks,
            fragment_start,
            default_base_moof,
            omit_offset,
        );
        for (idx, entry) in traf_starts.iter().enumerate() {
            let Some((traf_no, is_sync)) = entry else {
                continue;
            };
            if *is_sync {
                record_tfra(state, idx, fragment_start, *traf_no, 1);
                any_sap = true;
            }
            first_sap = first_sap && *is_sync;
        }
        fragment_bytes.extend_from_slice(&moof);
        fragment_bytes.extend_from_slice(&mdat);
    }
    let _ = any_sap;

    let duration0 = state.pending.first().map_or(0, |t| {
        t.samples
            .iter()
            .map(|s| s.duration)
            .fold(0u32, u32::saturating_add)
    });
    state.sidx_segments.push(SidxSeg {
        size: u32::try_from(fragment_bytes.len()).unwrap_or(u32::MAX),
        duration: duration0,
        starts_with_sap: first_sap,
    });

    match &mut state.buffer {
        Some(buf) => buf.extend_from_slice(&fragment_bytes),
        None => out.write(&fragment_bytes)?,
    }

    for pending in &mut state.pending {
        pending.samples.clear();
        pending.start_dts = None;
    }
    state.frag_bytes = 0;
    state.frag_start_dts0 = None;
    Ok(())
}

fn record_tfra(
    state: &mut FragmentedState,
    track_index: usize,
    moof_offset: u64,
    traf_number: u32,
    trun_number: u32,
) {
    let start_dts = state
        .pending
        .get(track_index)
        .and_then(|p| p.start_dts)
        .unwrap_or(0);
    if let Some(list) = state.tfra.get_mut(track_index) {
        list.push(writer::TfraEntry {
            time: u64::try_from(start_dts).unwrap_or(0),
            moof_offset,
            traf_number,
            trun_number,
            sample_number: 1,
        });
    }
}

/// Build one `moof` covering every track with pending samples, plus the
/// `mdat` holding their payloads concatenated in track order. `moof_offset`
/// is this fragment's position in the eventual final file (see
/// [`fragment_stream_pos`]), used only when `!default_base_moof`.
///
/// Returns, per track index, `Some((traf_number, first_sample_is_sync))` when
/// that track contributed a `traf` this fragment, `None` otherwise.
fn build_combined_fragment(
    state: &FragmentedState,
    tracks: &[TrackState],
    moof_offset: u64,
    default_base_moof: bool,
    omit_offset: bool,
) -> (Vec<u8>, Vec<u8>, Vec<Option<(u32, bool)>>) {
    let mut trafs = Vec::new();
    let mut mdat_body = Vec::new();
    let mut starts: Vec<Option<(u32, bool)>> = vec![None; tracks.len()];

    // Two passes: `moof`'s own length decides `trun.data_offset` (relative to
    // `moof`'s start) and `tfhd.base_data_offset` (when stated); neither
    // choice changes the length of a `tfhd`/`trun` with a fixed flag set, so
    // one retry always converges.
    let mut moof_len_guess = 0u64;
    for _ in 0..2 {
        trafs.clear();
        mdat_body.clear();
        let mdat_header_len = 8u64;
        let mut data_offset_in_mdat =
            i64::try_from(moof_len_guess.saturating_add(mdat_header_len)).unwrap_or(i64::MAX);
        let base = moof_offset.saturating_add(moof_len_guess);
        let mut traf_no = 0u32;
        for (idx, track) in tracks.iter().enumerate() {
            let Some(pending) = state.pending.get(idx) else {
                continue;
            };
            if pending.samples.is_empty() {
                continue;
            }
            traf_no += 1;
            let traf_bytes = build_traf(
                track,
                pending,
                base,
                data_offset_in_mdat,
                default_base_moof,
                omit_offset,
            );
            let is_sync = pending.samples.first().is_some_and(|s| s.is_sync);
            if let Some(slot) = starts.get_mut(idx) {
                *slot = Some((traf_no, is_sync));
            }
            for s in &pending.samples {
                mdat_body.extend_from_slice(&s.payload);
            }
            let bytes_here: usize = pending.samples.iter().map(|s| s.payload.len()).sum();
            data_offset_in_mdat =
                data_offset_in_mdat.saturating_add(i64::try_from(bytes_here).unwrap_or(i64::MAX));
            trafs.push(traf_bytes);
        }
        let mfhd = writer::mfhd(state.sequence_number);
        let moof = writer::moof(&mfhd, &trafs);
        let got = u64::try_from(moof.len()).unwrap_or(u64::MAX);
        if got == moof_len_guess {
            return (moof, frame_mdat(&mdat_body), starts);
        }
        moof_len_guess = got;
    }
    let mfhd = writer::mfhd(state.sequence_number);
    let moof = writer::moof(&mfhd, &trafs);
    (moof, frame_mdat(&mdat_body), starts)
}

fn build_one_track_fragment(
    state: &FragmentedState,
    track: &TrackState,
    track_index: usize,
    moof_offset: u64,
    default_base_moof: bool,
    omit_offset: bool,
) -> Result<(Vec<u8>, Vec<u8>)> {
    let pending = state
        .pending
        .get(track_index)
        .ok_or(Error::InvalidData("mp4: track index out of range"))?;
    let mut mdat_body = Vec::new();
    for s in &pending.samples {
        mdat_body.extend_from_slice(&s.payload);
    }
    let mut moof_len_guess = 0u64;
    let mut traf_bytes = Vec::new();
    for _ in 0..2 {
        let mdat_header_len = 8u64;
        let base = moof_offset.saturating_add(moof_len_guess);
        let data_offset_in_mdat =
            i64::try_from(moof_len_guess.saturating_add(mdat_header_len)).unwrap_or(i64::MAX);
        traf_bytes = build_traf(
            track,
            pending,
            base,
            data_offset_in_mdat,
            default_base_moof,
            omit_offset,
        );
        let mfhd = writer::mfhd(state.sequence_number);
        let moof = writer::moof(&mfhd, &[traf_bytes.clone()]);
        let got = u64::try_from(moof.len()).unwrap_or(u64::MAX);
        if got == moof_len_guess {
            return Ok((moof, frame_mdat(&mdat_body)));
        }
        moof_len_guess = got;
    }
    let mfhd = writer::mfhd(state.sequence_number);
    let moof = writer::moof(&mfhd, &[traf_bytes]);
    Ok((moof, frame_mdat(&mdat_body)))
}

fn build_traf(
    track: &TrackState,
    pending: &PendingTrack,
    base_data_offset: u64,
    data_offset: i64,
    default_base_moof: bool,
    omit_offset: bool,
) -> Vec<u8> {
    let tfhd_bytes = writer::tfhd(&writer::TfhdFields {
        track_id: track.track_id,
        base_data_offset: if omit_offset {
            None
        } else {
            Some(base_data_offset)
        },
        default_base_is_moof: default_base_moof,
        ..writer::TfhdFields::default()
    });
    let tfdt_bytes = writer::tfdt(u64::try_from(pending.start_dts.unwrap_or(0)).unwrap_or(0));
    let samples: Vec<writer::TrunSample> = pending
        .samples
        .iter()
        .map(|s| writer::TrunSample {
            duration: s.duration,
            size: u32::try_from(s.payload.len()).unwrap_or(u32::MAX),
            flags: if s.is_sync {
                FLAGS_SYNC
            } else {
                FLAGS_NON_SYNC
            },
            cts: s.cts,
        })
        .collect();
    let tr_flags = TR_SAMPLE_DURATION
        | TR_SAMPLE_SIZE
        | TR_SAMPLE_FLAGS
        | TR_SAMPLE_CTS_OFFSET
        | TR_DATA_OFFSET;
    let data_offset_i32 = i32::try_from(data_offset).unwrap_or(i32::MAX);
    let trun_bytes = writer::trun(tr_flags, &samples, data_offset_i32, 0);
    let mut body = Vec::new();
    body.extend_from_slice(&tfhd_bytes);
    body.extend_from_slice(&tfdt_bytes);
    body.extend_from_slice(&trun_bytes);
    writer::traf(&body)
}

fn frame_mdat(body: &[u8]) -> Vec<u8> {
    vaco_format_isom::build::bx(b"mdat", body)
}

/// Finalise: flush anything still pending, then either nothing further
/// (streaming — every fragment is already on the sink) or write the
/// buffered `sidx` + fragment stream (`dash`/`cmaf`), followed by `mfra`.
///
/// # Errors
/// Propagates I/O failure.
pub fn finish(
    out: &mut IoWriter,
    state: &mut FragmentedState,
    tracks: &[TrackState],
    opts: &MuxOptions,
) -> Result<()> {
    flush_fragment(out, state, tracks, opts)?;

    let flags = opts.effective_flags();
    let sidx = if flags.intersects(MovFlags::DASH | MovFlags::CMAF) {
        tracks.first().map(|first| {
            let refs: Vec<writer::SidxReference> = state
                .sidx_segments
                .iter()
                .map(|s| writer::SidxReference {
                    is_index: false,
                    referenced_size: s.size,
                    subsegment_duration: s.duration,
                    starts_with_sap: s.starts_with_sap,
                    sap_type: 1,
                    sap_delta_time: 0,
                })
                .collect();
            writer::sidx(first.track_id, first.timescale, 0, 0, &refs)
        })
    } else {
        None
    };
    let sidx_len = sidx
        .as_ref()
        .map_or(0u64, |s| u64::try_from(s.len()).unwrap_or(0));

    let mut mfra_children = Vec::new();
    for (track, list) in tracks.iter().zip(&state.tfra) {
        if list.is_empty() {
            continue;
        }
        let corrected: Vec<writer::TfraEntry> = list
            .iter()
            .map(|e| writer::TfraEntry {
                moof_offset: e.moof_offset.saturating_add(sidx_len),
                ..*e
            })
            .collect();
        mfra_children.push(writer::tfra(track.track_id, &corrected));
    }
    let mfra = if mfra_children.is_empty() {
        Vec::new()
    } else {
        writer::mfra(&mfra_children)
    };

    if let Some(sidx_bytes) = sidx {
        out.write(&sidx_bytes)?;
    }
    if let Some(buf) = state.buffer.take() {
        out.write(&buf)?;
    }
    if !mfra.is_empty() {
        out.write(&mfra)?;
    }
    out.flush()
}
