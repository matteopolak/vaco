//! Non-fragmented `ftyp`/`mdat`/`moov` muxing and `-movflags faststart`.
//! By default (no `faststart`) `mdat` is written **immediately after `ftyp`**,
//! directly to the sink, and every sample's absolute file offset is therefore
//! known the instant it is written — nothing is ever shifted. `moov` is built
//! afterward, at [`finish`], and appended: this is the same "moov at the end"
//! shape `ffmpeg 8.1`'s own default `mov` muxer produces.
//!
//! `mdat` is written with an 8-byte small header (`size`+`"mdat"`) and its
//! size field is a zero placeholder until [`finish`] seeks back and patches
//! it. The `free`/`wide` box written just before it ([`placeholder_box`]) is
//! not decoration: it is the reference's own **reservation** for the case
//! where the payload turns out to need a 64-bit size. A small header can only
//! ever state a size up to `u32::MAX`; if the final payload does not fit,
//! [`finish`] backs up *into* the reservation and overwrites both boxes at
//! once with one 16-byte extended header (`size==1`, `"mdat"`, an 8-byte
//! `largesize`) — the placeholder's 8 bytes plus the small header's 8 bytes
//! being exactly the 16 the extended form needs, so nothing already written
//! after it ever has to move. Measured against `ffmpeg -c copy -f mp4`
//! : a 6242-byte payload gets the small, 32-bit
//! form; this crate wrote the extended form unconditionally before this fix.
//!
//! Putting `moov` *before* `mdat` needs `mdat`'s bytes to already exist when
//! `moov`'s chunk offsets are computed, and this crate's sink
//! ([`vaco_io::MediaSink`]) cannot be read back from — there is no way to
//! "move" already-written bytes without a working read side. So under
//! `faststart` every sample's payload is buffered in memory (`ProgressiveState::mdat_buf`)
//! instead of being written to the sink as it arrives; [`finish`] then knows
//! the whole file's size before it writes a single byte of it. This is a
//! real memory cost for a large file and is documented as one rather than
//! hidden — see `docs/format/vaco-mux-mp4.md`.
//!
//! Once every sample is buffered, the exact byte length of `moov` depends on
//! the chunk offsets it carries, and the chunk offsets depend on how long the
//! prefix (`ftyp`+`moov`+the `mdat` header) is — a fixed point. [`finish`]
//! resolves it the same way any two-pass writer does: build `moov` assuming a
//! trial prefix length, and if the built length does not match the trial,
//! retry with the length just produced. Growing `moov` (by switching a
//! track's `stco` to `co64`) can only ever push offsets *up*, never back
//! below the threshold that required `co64` in the first place, so this
//! converges within a small, bounded number of passes — [`MAX_FASTSTART_PASSES`].

use vaco_core::{Error, Result};
use vaco_format_isom::fourcc::boxes;
use vaco_format_isom::writer;
use vaco_io::IoWriter;

use crate::meta::build_udta;
use crate::options::{Brand, MuxOptions};
use crate::track::TrackState;

/// Bytes of the extended `largesize` `mdat` header: 4-byte `size==1`, 4-byte
/// `"mdat"`, 8-byte `largesize`. Used unconditionally by the `faststart`
/// path, which knows the final payload length before writing a single byte
/// and so never needs the small-header/reservation dance [`finish_streaming`]
/// does.
const MDAT_HEADER_LEN: u64 = 16;

/// Passes [`finish`]'s faststart fixed point is allowed before giving up —
/// generous: the size argument in the module docs converges in at most two.
const MAX_FASTSTART_PASSES: usize = 8;

/// One in-progress chunk: which track it belongs to, where it starts, and how
/// many samples it holds so far.
#[derive(Debug, Clone, Copy)]
struct OpenChunk {
    track_index: usize,
    offset: u64,
    count: u32,
}

/// Progressive-mode session state, carried by [`crate::mux::MovMuxer`].
#[derive(Debug)]
pub struct ProgressiveState {
    /// Absolute position of the small header's 4-byte `size` field, once
    /// written — the *start* of the small `mdat` box, 8 bytes after the
    /// `free`/`wide` reservation's own start.
    mdat_size_field_at: u64,
    open_chunk: Option<OpenChunk>,
    /// `Some` under `faststart`: every sample's payload accumulates here
    /// instead of being written to the sink, and `offset` fields recorded on
    /// tracks are relative to the start of this buffer, not the file.
    mdat_buf: Option<Vec<u8>>,
}

impl ProgressiveState {
    /// A fresh session. `mdat_buf` starts `None` regardless of `faststart`;
    /// [`write_header`] sets it to `Some(Vec::new())` once it knows buffering
    /// is needed, so this constructor takes no argument at all.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            mdat_size_field_at: 0,
            open_chunk: None,
            mdat_buf: None,
        }
    }
}

impl Default for ProgressiveState {
    fn default() -> Self {
        Self::new()
    }
}

/// `ftyp`, then either a `free`/`wide` placeholder box followed by the real
/// `mdat` header (streaming), or nothing yet (`faststart`, where `mdat`'s
/// bytes are buffered until [`finish`] and no placeholder is written at all —
/// measured: `-movflags faststart` puts `moov` directly after `ftyp`, with no
/// intervening box).
///
/// # Errors
/// Propagates I/O failure.
pub fn write_header(
    out: &mut IoWriter,
    opts: &MuxOptions,
    state: &mut ProgressiveState,
    tracks: &[TrackState],
) -> Result<()> {
    out.write(&crate::brand::file_type_box(opts.brand, tracks))?;
    if opts.movflags.contains(crate::options::MovFlags::FASTSTART) {
        state.mdat_buf = Some(Vec::new());
    } else {
        out.write(&placeholder_box(opts.brand))?;
        state.mdat_size_field_at = out.pos();
        out.write(&0u32.to_be_bytes())?; // patched in `finish`: a 32-bit size,
        // or backed into the reservation above to form a 64-bit one — see the
        // module docs.
        out.write(b"mdat")?;
    }
    Ok(())
}

/// The 8-byte empty box the reference writes between `ftyp` and `mdat` in
/// streaming (non-`faststart`) mode: `wide` for `-f mov`, `free` for every
/// other brand this crate writes progressively (measured across `mp4`,
/// `ipod`, `f4v`, `psp`, `3gp`, `3g2` — all `free`; `mov` alone is `wide`).
/// Real players ignore an unknown box here regardless of name, but the byte
/// layout is part of what `remux-bitexact` compares.
fn placeholder_box(brand: Brand) -> [u8; 8] {
    let kind: &[u8; 4] = if matches!(brand, Brand::Mov) {
        b"wide"
    } else {
        b"free"
    };
    let mut b = [0u8; 8];
    b[..4].copy_from_slice(&8u32.to_be_bytes());
    b[4..].copy_from_slice(kind);
    b
}

/// Write one sample's payload, recording its offset and updating chunk
/// grouping: consecutive samples from the same track extend the current
/// chunk, a track change starts a new one.
///
/// # Errors
/// Propagates I/O failure.
pub fn write_sample(
    out: &mut IoWriter,
    state: &mut ProgressiveState,
    tracks: &mut [TrackState],
    track_index: usize,
    payload: &[u8],
    dts: i64,
    cts_offset: i32,
    is_sync: bool,
) -> Result<()> {
    let offset = if let Some(buf) = &mut state.mdat_buf {
        let at = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        buf.extend_from_slice(payload);
        at
    } else {
        let at = out.pos();
        out.write(payload)?;
        at
    };

    let same_track = state
        .open_chunk
        .is_some_and(|c| c.track_index == track_index);
    if same_track {
        if let Some(c) = &mut state.open_chunk {
            c.count = c.count.saturating_add(1);
        }
    } else {
        close_chunk(state, tracks);
        state.open_chunk = Some(OpenChunk {
            track_index,
            offset,
            count: 1,
        });
    }

    let size =
        u32::try_from(payload.len()).map_err(|_| Error::Unsupported("mp4: sample too large"))?;
    let Some(track) = tracks.get_mut(track_index) else {
        return Err(Error::InvalidData("mp4: packet names an unknown track"));
    };
    track.samples.push(crate::track::SampleRecord {
        offset,
        size,
        dts,
        cts_offset,
        is_sync,
    });
    Ok(())
}

fn close_chunk(state: &mut ProgressiveState, tracks: &mut [TrackState]) {
    if let Some(c) = state.open_chunk.take()
        && let Some(track) = tracks.get_mut(c.track_index)
    {
        track.chunks.push(crate::track::ChunkRecord {
            offset: c.offset,
            sample_count: c.count,
        });
    }
}

/// Finalise: close the last chunk, write `moov`, and either patch `mdat`'s
/// size in place (streaming) or write the whole buffered file at once
/// (`faststart` — this needs no seek at all, since every sample is already
/// held in memory and the file is written once, in final order).
///
/// # Errors
/// I/O failure. The streaming path patches `mdat`'s size by seeking; on a
/// sink that cannot seek the placeholder `largesize` is left as `0` rather
/// than failing the whole mux, the same tradeoff `vaco-mux-avi` makes for
/// its own un-patchable fields on a non-seekable sink.
pub fn finish(
    out: &mut IoWriter,
    state: &mut ProgressiveState,
    tracks: &mut [TrackState],
    opts: &MuxOptions,
    movie_timescale: u32,
) -> Result<()> {
    close_chunk(state, tracks);

    match state.mdat_buf.take() {
        Some(buf) => finish_faststart(out, tracks, opts, movie_timescale, &buf),
        None => finish_streaming(out, state, tracks, opts, movie_timescale),
    }
}

fn finish_streaming(
    out: &mut IoWriter,
    state: &ProgressiveState,
    tracks: &[TrackState],
    opts: &MuxOptions,
    movie_timescale: u32,
) -> Result<()> {
    let end = out.pos();
    if out.is_seekable() {
        // `total` is the small header (8 bytes) plus the payload — exactly
        // what the box's own `size` field states when it fits in 32 bits.
        let total = end.saturating_sub(state.mdat_size_field_at);
        if let Ok(total32) = u32::try_from(total) {
            out.seek(state.mdat_size_field_at)?;
            out.write(&total32.to_be_bytes())?;
        } else {
            // Does not fit: back up into the `free`/`wide` reservation
            // written just before this box (module docs) and overwrite both
            // as one 16-byte extended header. `ext_total` adds the
            // reservation's own 8 bytes back in, since they are now part of
            // the header rather than a separate box.
            let ext_start = state.mdat_size_field_at.saturating_sub(8);
            let ext_total = total.saturating_add(8);
            out.seek(ext_start)?;
            out.write(&1u32.to_be_bytes())?; // size == 1: largesize follows
            out.write(b"mdat")?;
            out.write(&ext_total.to_be_bytes())?;
        }
        out.seek(end)?;
    }
    let moov = build_moov(tracks, opts, movie_timescale, 0, end);
    out.write(&moov)?;
    out.flush()
}

fn finish_faststart(
    out: &mut IoWriter,
    tracks: &mut [TrackState],
    opts: &MuxOptions,
    movie_timescale: u32,
    mdat_buf: &[u8],
) -> Result<()> {
    // No seek ever happens here — every sample is already buffered in
    // `mdat_buf`, so the whole file is written once, in final order. `ftyp`
    // is already on `out` (written in `write_header`), and its length is the
    // base every chunk offset's shift starts from.
    let ftyp_len = out.pos();
    let prefix_before_mdat = |moov_len: u64| {
        ftyp_len
            .saturating_add(moov_len)
            .saturating_add(MDAT_HEADER_LEN)
    };

    let mut trial_moov_len: u64 = 0;
    let mut moov = Vec::new();
    for _ in 0..MAX_FASTSTART_PASSES {
        let shift = prefix_before_mdat(trial_moov_len);
        moov = build_moov(tracks, opts, movie_timescale, shift, ftyp_len);
        let got = u64::try_from(moov.len()).unwrap_or(u64::MAX);
        if got == trial_moov_len {
            break;
        }
        trial_moov_len = got;
    }

    out.write(&moov)?;
    let total = MDAT_HEADER_LEN.saturating_add(u64::try_from(mdat_buf.len()).unwrap_or(u64::MAX));
    out.write(&1u32.to_be_bytes())?;
    out.write(b"mdat")?;
    out.write(&total.to_be_bytes())?;
    out.write(mdat_buf)?;
    out.flush()
}

/// Build the whole `moov`, with every track's chunk offsets shifted by
/// `offset_shift` — `0` in streaming mode (offsets are already absolute),
/// the trial prefix length under `faststart`.
fn build_moov(
    tracks: &[TrackState],
    opts: &MuxOptions,
    movie_timescale: u32,
    offset_shift: u64,
    moov_start: u64,
) -> Vec<u8> {
    let creation_time = if opts.bitexact {
        0
    } else {
        opts.creation_time_unix
            .map_or(0, vaco_format_isom::movie::from_unix_time)
    };

    // `presented_duration`, not `media_duration`: the reference's `mvhd`
    // duration excludes the initial reorder-delay lead-in an edit list skips
    // over, the same adjustment `build_trak`'s `tkhd` duration makes
    // from source measurements.
    let movie_duration = tracks
        .iter()
        .map(|t| rescale(t.presented_duration(), t.timescale, movie_timescale))
        .max()
        .unwrap_or(0);
    let next_track_id = tracks
        .iter()
        .map(|t| t.track_id)
        .max()
        .unwrap_or(0)
        .saturating_add(1);

    let mut moov_body = writer::mvhd(&writer::MvhdFields {
        creation_time,
        modification_time: creation_time,
        timescale: movie_timescale,
        duration: movie_duration,
        rate: 0x0001_0000,
        volume: 0x0100,
        matrix: vaco_format_isom::fixed::IDENTITY_MATRIX,
        next_track_id,
    });

    // `moov`'s own 8-byte box header precedes `moov_body` in the file, so a
    // `trak`'s absolute start is `moov_start + 8 + (bytes of moov_body
    // already appended)` — computable up front, unlike a chunk offset into
    // `mdat`, because `moov`'s own start position never depends on its own
    // length the way `mdat`'s does under `faststart` (see the module docs'
    // fixed-point argument, which this sidesteps entirely).
    let encryption = opts.encryption();
    for t in tracks {
        let trak_abs_start = moov_start
            .saturating_add(8)
            .saturating_add(moov_body.len() as u64);
        moov_body.extend_from_slice(&build_trak(
            t,
            movie_timescale,
            creation_time,
            offset_shift,
            encryption.as_ref(),
            trak_abs_start,
        ));
    }

    if let Some(udta) = build_udta(opts) {
        moov_body.extend_from_slice(&udta);
    }

    vaco_format_isom::build::bx(b"moov", &moov_body)
}

fn build_trak(
    track: &TrackState,
    movie_timescale: u32,
    creation_time: u64,
    offset_shift: u64,
    encryption: Option<&crate::options::EncryptionOptions>,
    trak_abs_start: u64,
) -> Vec<u8> {
    // Post-edit duration (see `build_moov`'s comment on the same call) —
    // `mdhd`, below, uses the raw, un-adjusted `media_duration` instead.
    let track_duration = rescale(track.presented_duration(), track.timescale, movie_timescale);

    // Built in this order — rather than the file's own `tkhd, edts, mdia`
    // order — because `stbl`'s absolute file position (needed only when
    // `encryption` asks for a `saio` inside it) depends on the byte lengths
    // of everything that precedes it: `tkhd`+`edts` inside `trak`, then
    // `mdhd`+`hdlr` inside `mdia`, then `vmhd`/`smhd`+`dinf` inside `minf`.
    // Each of those is independent of `stbl` itself, so computing them first
    // resolves the position without a second pass.
    let tkhd = writer::tkhd(&writer::TkhdFields {
        flags: writer::tkhd_flags::ENABLED | writer::tkhd_flags::IN_MOVIE,
        creation_time,
        modification_time: creation_time,
        track_id: track.track_id,
        duration: track_duration,
        layer: 0,
        alternate_group: 0,
        volume: track.volume,
        matrix: track.matrix.map(i32::cast_unsigned),
        width: track.width,
        height: track.height,
    });
    let edts = build_edts(track, movie_timescale);
    let mdia_abs_start = trak_abs_start
        .saturating_add(8)
        .saturating_add(tkhd.len() as u64)
        .saturating_add(edts.len() as u64);

    let mdhd = writer::mdhd(&writer::MdhdFields {
        creation_time,
        modification_time: creation_time,
        timescale: track.timescale,
        duration: track.media_duration(),
        language: track.language,
    });
    let hdlr = writer::hdlr(track.handler, handler_name(track.handler));
    let minf_abs_start = mdia_abs_start
        .saturating_add(8)
        .saturating_add(mdhd.len() as u64)
        .saturating_add(hdlr.len() as u64);

    let mut minf = Vec::new();
    minf.extend_from_slice(&match track.media {
        vaco_core::MediaType::Audio => writer::smhd(),
        _ => writer::vmhd(),
    });
    minf.extend_from_slice(&writer::dinf_self_contained());
    let stbl_abs_start = minf_abs_start
        .saturating_add(8)
        .saturating_add(minf.len() as u64);
    minf.extend_from_slice(&build_stbl(track, offset_shift, encryption, stbl_abs_start));

    let mut mdia = mdhd;
    mdia.extend_from_slice(&hdlr);
    mdia.extend_from_slice(&vaco_format_isom::build::bx(b"minf", &minf));

    let mut trak = tkhd;
    trak.extend_from_slice(&edts);
    trak.extend_from_slice(&vaco_format_isom::build::bx(b"mdia", &mdia));
    vaco_format_isom::build::bx(b"trak", &trak)
}

/// `edts`/`elst`: one entry, always. Measured on `ffmpeg -c copy -f mp4`
/// across three inputs — a reordered stream, a non-reordered one, and a raw
/// H.264 elementary stream with no container edit-list concept of its own —
/// and every track in every one of them gets exactly one `elst` entry, `0`
/// `media_time` included, so this is not conditioned on reordering being
/// present at all. `rate` is always `1.0`
/// (`0x0001_0000`, 16.16 fixed) — no measurement has produced anything else.
fn build_edts(track: &TrackState, movie_timescale: u32) -> Vec<u8> {
    let media_time = track.media_time();
    let segment_duration = rescale(track.presented_duration(), track.timescale, movie_timescale);
    let mut body = Vec::new();
    body.extend_from_slice(&1u32.to_be_bytes()); // entry_count
    body.extend_from_slice(
        &u32::try_from(segment_duration)
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    body.extend_from_slice(&media_time.to_be_bytes());
    body.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate 1.0
    let elst = vaco_format_isom::build::fullbx(b"elst", 0, 0, &body);
    vaco_format_isom::build::bx(b"edts", &elst)
}

fn build_stbl(
    track: &TrackState,
    offset_shift: u64,
    encryption: Option<&crate::options::EncryptionOptions>,
    stbl_abs_start: u64,
) -> Vec<u8> {
    let mut body = Vec::new();
    let entry = with_btrt(track);
    body.extend_from_slice(&writer::stsd(std::slice::from_ref(&entry)));
    body.extend_from_slice(&writer::stts(&track.stts_runs()));
    // `stss` before `ctts`. The order of `stbl`'s children is unconstrained by
    // the specification and load-bearing for byte-identity; measured on
    // `ffmpeg -c copy -f mp4`, the reference writes
    // `stsd stts stss ctts stsc stsz stco` and we had these two the other way
    // round.
    if let Some(syncs) = track.stss_list() {
        body.extend_from_slice(&writer::stss(&syncs));
    }
    let ctts_runs = track.ctts_runs();
    if !ctts_runs.is_empty() {
        body.extend_from_slice(&writer::ctts(&ctts_runs));
    }
    body.extend_from_slice(&writer::stsc(&track.stsc_runs()));
    body.extend_from_slice(&writer::stsz(&track.stsz_list()));
    let offsets: Vec<u64> = track
        .chunk_offset_list()
        .into_iter()
        .map(|o| o.saturating_add(offset_shift))
        .collect();
    body.extend_from_slice(&writer::chunk_offsets(&offsets));
    if encryption.is_some() {
        // `senc`'s IV table starts right after its own 8-byte box header and
        // 8-byte version/flags+sample_count fields — see
        // `vaco_format_isom::cenc::SampleEncryption::records_offset`, which
        // this mirrors on the write side so `saio`'s one offset points at
        // exactly what a demuxer reading `senc` back would compute.
        let sample_count = u32::try_from(track.samples.len()).unwrap_or(u32::MAX);
        let ivs: Vec<[u8; 8]> = (1..=u64::from(sample_count))
            .map(u64::to_be_bytes)
            .collect();
        let senc_abs_start = stbl_abs_start
            .saturating_add(8)
            .saturating_add(body.len() as u64);
        let iv_table_abs = senc_abs_start.saturating_add(16);
        body.extend_from_slice(&writer::senc(&ivs));
        body.extend_from_slice(&writer::saiz(sample_count));
        body.extend_from_slice(&writer::saio(iv_table_abs));
    }
    vaco_format_isom::build::bx(b"stbl", &body)
}

/// [`TrackState::entry`]'s bytes plus a trailing `btrt`. Appended here
/// instead of at [`crate::entry::build`] time because the fallback bitrate
/// (used when the source declared none at all) needs the total payload size
/// and the track's own duration, neither of which exists until every sample
/// has been written. Measured, written unconditionally for both video and
/// audio: `bufferSizeDB` is always `0`,
/// `maxBitrate == avgBitrate`.
fn with_btrt(track: &TrackState) -> Vec<u8> {
    let (max_bitrate, avg_bitrate) = track_bitrate(track);
    let btrt = writer::btrt(0, max_bitrate, avg_bitrate);
    append_child(&track.entry.bytes, &btrt)
}

/// The bitrate `btrt` carries: the container's own declared `bit_rate`,
/// copied straight through, when there is one — measured, an MP4-sourced
/// H.264 track's `bit_rate=8312` becomes `0x2078` verbatim on a stream copy —
/// or a derived average (total payload bits over the track's own presented
/// duration) when the source declares none at all, which a raw H.264
/// elementary stream never does. Measured: the reference still writes a
/// `btrt` in that case rather than omitting it, so this crate must produce
/// *some* number rather than leaving the box out.
fn track_bitrate(track: &TrackState) -> (u32, u32) {
    if let Some(bit_rate) = track.params.bit_rate.filter(|&b| b > 0) {
        let v = u32::try_from(bit_rate).unwrap_or(u32::MAX);
        return (v, v);
    }
    let total_bytes: u64 = track.samples.iter().map(|s| u64::from(s.size)).sum();
    let duration_ticks = track.media_duration();
    if duration_ticks == 0 || track.timescale == 0 {
        return (0, 0);
    }
    let bits = total_bytes.saturating_mul(8);
    let scaled = u128::from(bits).saturating_mul(u128::from(track.timescale));
    #[allow(
        clippy::integer_division,
        reason = "an average bitrate is a genuine floor division of total bits by duration, not a stand-in for an exact one"
    )]
    let rate = scaled / u128::from(duration_ticks);
    let v = u32::try_from(rate).unwrap_or(u32::MAX);
    (v, v)
}

/// Append `child` to an already-framed box's bytes, patching the leading
/// 4-byte size field in place — how [`with_btrt`] adds a `btrt` after
/// [`crate::entry::build`] already framed the sample entry, without that
/// function needing to know a fallback bitrate it cannot yet compute.
fn append_child(entry_bytes: &[u8], child: &[u8]) -> Vec<u8> {
    let mut out = entry_bytes.to_vec();
    out.extend_from_slice(child);
    let new_len = u32::try_from(out.len()).unwrap_or(u32::MAX);
    if let Some(size_field) = out.get_mut(0..4) {
        size_field.copy_from_slice(&new_len.to_be_bytes());
    }
    out
}

fn handler_name(handler: vaco_format_isom::fourcc::FourCc) -> &'static str {
    if handler == boxes::SOUN {
        "SoundHandler"
    } else {
        "VideoHandler"
    }
}

/// `value * to / from`, saturating and division-safe for a zero `from`.
#[allow(
    clippy::integer_division,
    reason = "a timescale rescale is a genuine floor division, not a stand-in for an exact one"
)]
fn rescale(value: u64, from: u32, to: u32) -> u64 {
    if from == 0 {
        return 0;
    }
    let scaled = u128::from(value).saturating_mul(u128::from(to));
    let out = scaled / u128::from(from);
    u64::try_from(out).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Finding 14: the reference writes an 8-byte `free` box between `ftyp`
    /// and `mdat` for every brand this crate mux-writes progressively except
    /// `mov`, which gets `wide` instead (measured with `-c copy`, no
    /// `-movflags faststart`, across `mp4`/`ipod`/`f4v`/`psp`/`3gp`/`3g2`/`mov`).
    #[test]
    fn mov_gets_wide_every_other_brand_gets_free() {
        assert_eq!(&placeholder_box(Brand::Mov), b"\0\0\0\x08wide");
        for brand in [
            Brand::Mp4,
            Brand::Ipod,
            Brand::F4v,
            Brand::Psp,
            Brand::ThreeGp,
            Brand::ThreeG2,
        ] {
            assert_eq!(
                &placeholder_box(brand),
                b"\0\0\0\x08free",
                "brand {brand:?} should get a free placeholder"
            );
        }
    }
}
