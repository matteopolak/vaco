//! Microsoft Smooth Streaming (MS-SSTR) muxer.
//!
//! # What it is
//!
//! Smooth Streaming's "container" is a directory tree, not one file: a
//! `Manifest` (XML) plus, per bitrate (`QualityLevel`) and per track, a
//! sequence of `Fragments(TYPE=STARTTIME)` files (an ISO-BMFF `moof`+`mdat`
//! pair) and matching `FragmentInfo(TYPE=STARTTIME)` files (the *same* `moof`
//! bytes, with no `mdat`). This crate supports the two codecs `ffmpeg`'s own
//! `smoothstreaming` muxer supports and that this project has encoders for:
//! H.264 video and AAC audio.
//!
//! # How it works
//!
//! Every fact below is measured against real `ffmpeg -f smoothstreaming`
//! output rather than any published Microsoft spec text (no demuxer exists
//! for this format anywhere, in this project or in `ffmpeg` itself, so a
//! round-trip check is not available — see `provenance/sources.toml`,
//! `ffmpeg-smoothstreaming-mux-probe`, and the two reference trees it was
//! captured from).
//!
//! - **Timescale**: fixed at 10,000,000 ticks/second ("HNS", hundred
//!   nanosecond units) for every track, regardless of the track's own sample
//!   rate or frame rate.
//! - **Fragmentation is per-track, independent**: video flushes at the next
//!   keyframe once the accumulated duration since the last flush reaches
//!   [`SmoothStreamingMuxOptions::min_frag_duration_us`] (default 5 seconds,
//!   matching `ffmpeg -h muxer=smoothstreaming`'s own default); audio, which
//!   has no keyframe concept, flushes as soon as the threshold is reached.
//!   The final fragment of each track is always flushed short, at
//!   [`Muxer::write_trailer`], regardless of the threshold.
//! - **`moof`/`mdat` construction** reuses `vaco-format-isom::writer`'s
//!   existing fragment box writers (`mfhd`/`tfhd`/`trun`/`traf`/`moof`) —
//!   see [`fragment`] for the exact flag combinations measured for each
//!   track kind, and for the MS-specific `tfxd` `uuid` extension box this
//!   crate hand-builds (no ISO base-spec box carries a fragment's absolute
//!   time; `tfxd` is what this format uses instead of `tfdt`+external
//!   knowledge).
//! - **`CodecPrivateData`** is Annex-B SPS/PPS (from the H.264 stream's
//!   `avcC`, unpacked by [`avcc`]) for video, and the raw `AudioSpecificConfig`
//!   bytes for audio — both hex-encoded. See [`avcc`] for why this crate hand
//!   parses `avcC` rather than depending on `vaco-parse-h264` (D14.1).
//! - **`Manifest`** XML shape is in [`manifest`], including a documented,
//!   deliberate divergence from a literal reading of MS-SSTR: this crate
//!   reproduces the reference's own `<c>`-carries-only-`d"` convention rather
//!   than inventing a `t` attribute the reference never writes.
//!
//! # What is scoped out
//!
//! - **`tfrf`** (the look-ahead box naming *future* fragments, used by live
//!   players to avoid a manifest round trip): not written. It is a
//!   live-streaming latency optimisation with no VOD correctness role — a
//!   client with the full `Manifest` chunk list does not need it — and the
//!   reference's own encoding of it requires a seek-back rewrite of
//!   already-written files once later fragments exist, which is
//!   disproportionate complexity for what it buys a VOD asset. Tracked in
//!   `planning/TECH-DEBT.md`.
//! - **Playback through a real Smooth Streaming / Silverlight client**: not
//!   verifiable on this machine (no such client is available here). This
//!   crate's own bar is structural/self-consistency verification against the
//!   two measured reference trees; issue #617's "plays back through a
//!   reference client" acceptance criterion is reported, not silently
//!   assumed, as unreachable in this environment.
//! - **Creating each `QualityLevels(<bitrate>)/` directory**: not done by
//!   this muxer. `vaco_protocol_core::Protocol` has no directory-creation
//!   verb (`planning/INTERFACE-GAPS.md` gap 27) — a caller driving this
//!   muxer against a local `file:` output must pre-create every
//!   `QualityLevels(<bitrate>)/` directory before the first flush for that
//!   bitrate, exactly as `tests/roundtrip.rs` does. `vaco-mux-dash` and
//!   `vaco-mux-hls` never needed this because both name every segment flat,
//!   in the manifest's own directory; Smooth Streaming's naming convention
//!   is measured, not chosen, and requires the subdirectory.
//!
//! # Configuration
//!
//! [`SmoothStreamingMuxOptions`] — currently just `min_frag_duration_us`.
//!
//! # Dependencies
//!
//! `vaco-format-adaptive` (`WriteAccess`, relative-URL `resolve`),
//! `vaco-format-isom` (`writer`/`build`, box construction only — never its
//! demux/parsing surface), `vaco-format-core`, `vaco-io`, `vaco-codec-core`,
//! `vaco-packet`, `vaco-core`, `vaco-limits`, `vaco-protocol-core`. No
//! dependency on any `vaco-parse-*` crate (D14.1): `avcC` unpacking is local.

#![forbid(unsafe_code)]

pub mod avcc;
pub mod fragment;
pub mod manifest;

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, Result};
use vaco_format_adaptive::{WriteAccess, resolve};
use vaco_format_core::{Muxer, MuxerDesc};
use vaco_io::MediaSink;
use vaco_packet::Packet;

use fragment::PendingSample;
use manifest::{ManifestStream, StreamKind, build_manifest};

/// `ffmpeg -h muxer=smoothstreaming`'s own `-min_frag_duration` default: 5
/// seconds, in microseconds. HDS's default (10 seconds) is different —
/// measured separately, see `vaco-mux-hds` if present.
pub const DEFAULT_MIN_FRAG_DURATION_US: u64 = 5_000_000;

/// The fixed Smooth Streaming timescale: ticks per second.
pub const TICKS_PER_SECOND: u64 = 10_000_000;

/// Muxer-level options.
#[derive(Debug, Clone, Copy)]
pub struct SmoothStreamingMuxOptions {
    pub min_frag_duration_us: u64,
}

impl SmoothStreamingMuxOptions {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            min_frag_duration_us: DEFAULT_MIN_FRAG_DURATION_US,
        }
    }

    fn min_frag_duration_hns(self) -> u64 {
        self.min_frag_duration_us.saturating_mul(10)
    }
}

impl Default for SmoothStreamingMuxOptions {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match MuxerDesc::open's fn pointer type"
)]
fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    // `MuxerDesc::open` has no filename and no protocol write access, both
    // of which this multi-file format needs (the same gap `vaco-mux-dash`
    // and `vaco-mux-hls` document). This writes the one sink it is given,
    // once, as the `Manifest`, and refuses every `write_packet` since there
    // is nowhere to create a fragment file.
    Ok(Box::new(SmoothStreamingMuxer {
        manifest_url: String::new(),
        write: None,
        manifest_sink: Some(sink),
        opts: SmoothStreamingMuxOptions::new(),
        tracks: Vec::new(),
        header_written: false,
        trailer_written: false,
    }))
}

/// The descriptor a registry would hold.
pub const MUXER: MuxerDesc = MuxerDesc {
    name: "smoothstreaming",
    long_name: "Smooth Streaming Muxer",
    extensions: &["ism", "ismv", "isma"],
    default_video: Some(CodecId::H264),
    default_audio: Some(CodecId::Aac),
    open: open_muxer,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackKind {
    Video,
    Audio,
}

struct Track {
    stream_index: u32,
    id: u32,
    kind: TrackKind,
    bitrate: u64,
    codec_private_data_hex: String,
    width: u32,
    height: u32,
    sample_rate: u32,
    channels: u32,
    pending: Vec<PendingSample>,
    accumulated_hns: u64,
    frag_start_hns: u64,
    sequence_number: u32,
    manifest_chunks: Vec<u64>,
    /// Fallback duration (in HNS) for a packet that states none —
    /// `Packet::duration == 0` is the ordinary `-c copy` case out of a
    /// demuxer that reports none, and neither `trun.sample_duration` nor
    /// `tfxd.fragment_duration_hns` has an "unknown" encoding: a zero here
    /// makes the sample, the fragment, and `Manifest`'s `<c d="…">` all read
    /// zero, and a zero-duration fragment for a live-style muxer means
    /// `accumulated_hns` never reaches [`SmoothStreamingMuxOptions`]'s
    /// threshold either.
    ///
    /// Seeded from the stream's own declared frame rate at [`Muxer::add_stream`]
    /// (video only — [`vaco_codec_core::AudioParameters`] carries no
    /// samples-per-frame field to derive an audio one from), then kept
    /// current from the last packet that *did* state a duration — the same
    /// "repeat the previous delta" fallback `vaco-mux-mp4`'s `stts_runs` uses
    /// for its own trailing sample.
    duration_hint_hns: u64,
}

/// The Smooth Streaming muxer.
pub struct SmoothStreamingMuxer {
    manifest_url: String,
    write: Option<WriteAccess>,
    manifest_sink: Option<Box<dyn MediaSink>>,
    opts: SmoothStreamingMuxOptions,
    tracks: Vec<Track>,
    header_written: bool,
    trailer_written: bool,
}

impl core::fmt::Debug for SmoothStreamingMuxer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SmoothStreamingMuxer")
            .field("tracks", &self.tracks.len())
            .finish_non_exhaustive()
    }
}

impl SmoothStreamingMuxer {
    /// The real entry point: `manifest_url` is where `Manifest` will be
    /// written, and every `QualityLevels(N)/Fragments(...)`/`FragmentInfo(...)`
    /// file is resolved relative to it.
    #[must_use]
    pub fn new(
        manifest_url: String,
        write: Option<WriteAccess>,
        opts: SmoothStreamingMuxOptions,
    ) -> Self {
        Self {
            manifest_url,
            write,
            manifest_sink: None,
            opts,
            tracks: Vec::new(),
            header_written: false,
            trailer_written: false,
        }
    }

    fn write_text_file(&mut self, url: &str, content: &str) -> Result<()> {
        if let Some(mut sink) = self.manifest_sink.take() {
            sink.write(content.as_bytes())?;
            return sink.flush();
        }
        let Some(write) = &self.write else {
            return Err(Error::Unsupported(
                "smoothstreaming output needs protocol write access, and none was supplied",
            ));
        };
        let mut sink = write.create(url)?;
        sink.write(content.as_bytes())?;
        sink.flush()
    }

    fn track_index_for(&self, stream_index: u32) -> Result<usize> {
        self.tracks
            .iter()
            .position(|t| t.stream_index == stream_index)
            .ok_or(Error::InvalidData("packet names an unknown stream"))
    }

    /// Flush `idx`'s pending samples into one `Fragments`/`FragmentInfo`
    /// pair, if any are pending. A no-op when nothing is pending, so callers
    /// (including the forced end-of-stream flush in
    /// [`Muxer::write_trailer`]) never need to check first.
    fn flush_track(&mut self, idx: usize) -> Result<()> {
        let Some(track) = self.tracks.get(idx) else {
            return Ok(());
        };
        if track.pending.is_empty() {
            return Ok(());
        }
        let is_video = track.kind == TrackKind::Video;
        let (moof, mdat) = fragment::build_fragment(
            track.id,
            track.sequence_number,
            is_video,
            track.frag_start_hns,
            track.accumulated_hns,
            &track.pending,
        );

        let kind_word = if is_video { "video" } else { "audio" };
        let frag_name = format!(
            "QualityLevels({})/Fragments({kind_word}={})",
            track.bitrate, track.frag_start_hns
        );
        let info_name = format!(
            "QualityLevels({})/FragmentInfo({kind_word}={})",
            track.bitrate, track.frag_start_hns
        );

        let Some(write) = &self.write else {
            return Err(Error::Unsupported(
                "smoothstreaming output needs protocol write access, and none was supplied",
            ));
        };
        let frag_url = resolve(&self.manifest_url, &frag_name);
        let info_url = resolve(&self.manifest_url, &info_name);

        let mut frag_sink = write.create(&frag_url)?;
        frag_sink.write(&moof)?;
        frag_sink.write(&mdat)?;
        frag_sink.flush()?;

        let mut info_sink = write.create(&info_url)?;
        info_sink.write(&moof)?;
        info_sink.flush()?;

        let Some(track) = self.tracks.get_mut(idx) else {
            return Ok(());
        };
        track.manifest_chunks.push(track.accumulated_hns);
        track.frag_start_hns = track.frag_start_hns.saturating_add(track.accumulated_hns);
        track.accumulated_hns = 0;
        track.sequence_number = track.sequence_number.saturating_add(1);
        track.pending.clear();
        Ok(())
    }

    fn render_manifest(&self) -> String {
        let streams: Vec<ManifestStream> = self
            .tracks
            .iter()
            .map(|t| ManifestStream {
                kind: match t.kind {
                    TrackKind::Video => StreamKind::Video {
                        max_width: t.width,
                        max_height: t.height,
                    },
                    TrackKind::Audio => StreamKind::Audio {
                        sampling_rate: t.sample_rate,
                        channels: t.channels,
                    },
                },
                bitrate: t.bitrate,
                codec_private_data_hex: t.codec_private_data_hex.clone(),
                chunk_durations_hns: t.manifest_chunks.clone(),
            })
            .collect();
        build_manifest(&streams)
    }
}

impl Muxer for SmoothStreamingMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.header_written {
            return Err(Error::InvalidData(
                "streams must be added before the header is written",
            ));
        }
        let bitrate = params.bit_rate.ok_or(Error::InvalidData(
            "smoothstreaming needs a declared bit_rate: it names the QualityLevels() folder",
        ))?;
        let stream_index =
            u32::try_from(self.tracks.len()).map_err(|_| Error::InvalidData("too many streams"))?;
        let track_id = stream_index + 1;

        let track = match params.codec_id {
            Some(CodecId::H264) => {
                let extradata = params.extradata.as_deref().ok_or(Error::InvalidData(
                    "smoothstreaming needs avcC extradata for an H.264 stream",
                ))?;
                let annexb = avcc::avcc_to_annexb(extradata).ok_or(Error::InvalidData(
                    "smoothstreaming could not unpack this stream's avcC extradata",
                ))?;
                let video = params.video.as_ref().ok_or(Error::InvalidData(
                    "smoothstreaming needs VideoParameters for an H.264 stream",
                ))?;
                // Seed the fallback from the declared frame rate, not an
                // invented constant — `0` only when the container states no
                // frame rate either, in which case there is nothing to
                // derive from until a real packet duration arrives.
                let duration_hint_hns = match (
                    u64::try_from(video.frame_rate.num),
                    u64::try_from(video.frame_rate.den),
                ) {
                    (Ok(num), Ok(den)) if num > 0 => {
                        TICKS_PER_SECOND.saturating_mul(den).checked_div(num).unwrap_or(0)
                    }
                    _ => 0,
                };
                Track {
                    stream_index,
                    id: track_id,
                    kind: TrackKind::Video,
                    bitrate,
                    codec_private_data_hex: avcc::to_hex(&annexb),
                    width: video.width,
                    height: video.height,
                    sample_rate: 0,
                    channels: 0,
                    pending: Vec::new(),
                    accumulated_hns: 0,
                    frag_start_hns: 0,
                    sequence_number: 1,
                    manifest_chunks: Vec::new(),
                    duration_hint_hns,
                }
            }
            Some(CodecId::Aac) => {
                let extradata = params.extradata.as_deref().ok_or(Error::InvalidData(
                    "smoothstreaming needs AudioSpecificConfig extradata for an AAC stream",
                ))?;
                let audio = params.audio.as_ref().ok_or(Error::InvalidData(
                    "smoothstreaming needs AudioParameters for an AAC stream",
                ))?;
                let channels = audio.layout.as_ref().map_or(1, |l| l.channels);
                Track {
                    stream_index,
                    id: track_id,
                    kind: TrackKind::Audio,
                    bitrate,
                    codec_private_data_hex: avcc::to_hex(extradata),
                    width: 0,
                    height: 0,
                    sample_rate: audio.sample_rate,
                    channels,
                    pending: Vec::new(),
                    accumulated_hns: 0,
                    frag_start_hns: 0,
                    sequence_number: 1,
                    manifest_chunks: Vec::new(),
                    // No samples-per-frame field to derive one from; the
                    // fallback starts at 0 and picks up the first packet
                    // that states a real duration.
                    duration_hint_hns: 0,
                }
            }
            _ => {
                return Err(Error::Unsupported(
                    "smoothstreaming only carries H.264 video and AAC audio (measured: \
                     ffmpeg -f smoothstreaming supports no other codec either)",
                ));
            }
        };
        self.tracks.push(track);
        Ok(stream_index)
    }

    fn write_header(&mut self) -> Result<()> {
        if self.header_written {
            return Err(Error::InvalidData("header written twice"));
        }
        if self.tracks.is_empty() {
            return Err(Error::InvalidData(
                "smoothstreaming output needs at least one stream",
            ));
        }
        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("packet written before the header"));
        }
        let idx = self.track_index_for(packet.stream_index)?;
        let Some(track) = self.tracks.get(idx) else {
            return Err(Error::InvalidData("packet names an unknown stream"));
        };
        let is_video = track.kind == TrackKind::Video;
        let threshold = self.opts.min_frag_duration_hns();

        let duration_us = packet.duration.as_micros().max(0);
        let stated_hns = (duration_us as u64).saturating_mul(10);
        // `Packet::duration == 0` is the ordinary case for a `-c copy` remux
        // out of a demuxer that reports none; neither `trun.sample_duration`
        // nor `tfxd.fragment_duration_hns` has an "unknown" encoding, so a
        // literal zero here is not "no information", it is "zero-length" —
        // see `Track::duration_hint_hns`'s own doc.
        let duration_hns = if stated_hns == 0 {
            track.duration_hint_hns
        } else {
            stated_hns
        };
        let duration_hns_u32 = u32::try_from(duration_hns).unwrap_or(u32::MAX);

        // Video: flush *before* appending this sample, once it is a
        // keyframe and the previous fragment has already met the target
        // duration — the new fragment always starts on a keyframe.
        if is_video
            && packet.is_key()
            && !track.pending.is_empty()
            && track.accumulated_hns >= threshold
        {
            self.flush_track(idx)?;
        }

        let flags = if is_video {
            if packet.is_key() { 0 } else { 0x0001_0000 }
        } else {
            0
        };
        let sample = PendingSample {
            duration_hns: duration_hns_u32,
            size: u32::try_from(packet.len).unwrap_or(u32::MAX),
            flags,
            cts_offset: 0,
            payload: packet.payload().to_vec(),
        };
        let Some(track) = self.tracks.get_mut(idx) else {
            return Err(Error::InvalidData("packet names an unknown stream"));
        };
        track.pending.push(sample);
        track.accumulated_hns = track.accumulated_hns.saturating_add(duration_hns);
        if stated_hns > 0 {
            track.duration_hint_hns = stated_hns;
        }
        let accumulated_hns = track.accumulated_hns;

        // Audio: no keyframe concept, so flush as soon as the threshold is
        // met, *including* the sample that crossed it.
        if !is_video && accumulated_hns >= threshold {
            self.flush_track(idx)?;
        }
        Ok(())
    }

    fn write_trailer(&mut self) -> Result<()> {
        if self.trailer_written {
            return Err(Error::InvalidData("trailer written twice"));
        }
        for idx in 0..self.tracks.len() {
            self.flush_track(idx)?;
        }
        self.trailer_written = true;
        let text = self.render_manifest();
        self.write_text_file(&self.manifest_url.clone(), &text)
    }
}
