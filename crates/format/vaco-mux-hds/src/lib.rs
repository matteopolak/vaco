//! Adobe HTTP Dynamic Streaming (HDS) muxer.
//!
//! # What it is
//!
//! Writes an HDS asset: an `index.f4m` client manifest (XML) plus, per
//! bitrate ("quality level"), one `stream<N>.abst` bootstrap box and a
//! sequence of `stream<N>Seg1-Frag<M>` fragment files. Unlike
//! `vaco-mux-smoothstreaming` (one file per elementary stream), **a quality
//! level bundles video and audio together into one interleaved fragment
//! stream** — measured directly: an `ffmpeg -f hds` run with one video plus
//! one audio input produces exactly one `stream0` (not a `stream0`+
//! `stream1` pair), and a two-quality-level run produces `stream0`/`stream1`
//! each carrying both media types. Supports the two codecs `ffmpeg`'s own
//! `hds` muxer supports and this project has encoders for: H.264 video, AAC
//! audio.
//!
//! # How it works
//!
//! No demuxer for this format exists anywhere — in this project or in
//! `ffmpeg` itself — so every fact below is measured against two real
//! `ffmpeg -f hds` reference trees (one quality level/two fragments, and
//! two quality levels/one fragment each; `provenance/sources.toml`'s
//! `ffmpeg-hds-mux-probe` entry), not a round trip.
//!
//! - **Fragments are not ISOBMFF.** A `stream<N>Seg1-Frag<M>` file is one
//!   bare `mdat` box (`vaco_format_isom::build::bx`) wrapping a sequence of
//!   classic FLV tags (see [`flv`]) — audio and video interleaved in
//!   arrival order, exactly the shape a `.flv` file's own body has. None of
//!   `vaco-format-isom::writer`'s ISOBMFF fragment writers
//!   (`mfhd`/`tfhd`/`trun`/`traf`/`moof`) apply here; only its generic,
//!   format-agnostic `build::{bx, fullbx}` box-header helpers do (reused in
//!   [`flv`] for `mdat` and in [`bootstrap`] for `abst`/`asrt`/`afrt`).
//! - **Every fragment restates both tracks' sequence headers** (measured:
//!   the second fragment of a two-fragment reference tree opens with a
//!   fresh copy of the AVC sequence header, then the AAC sequence header,
//!   both timestamped at the fragment's own start time, before any real
//!   sample) — this crate reproduces that so each fragment is independently
//!   decodable.
//! - **Fragmentation is gated on the video track's keyframes when a
//!   quality level has one** (`min_frag_duration_us`, default 10s, measured
//!   against `ffmpeg -h muxer=hds` — different from Smooth Streaming's 5s):
//!   the same "flush at the next keyframe once the threshold is met" policy
//!   `vaco-mux-smoothstreaming` uses, applied to the whole interleaved
//!   quality-level stream rather than per elementary stream. A
//!   video-less (audio-only) quality level flushes purely on accumulated
//!   duration, checked after each sample, mirroring Smooth Streaming's
//!   audio-only case.
//! - **The `abst` bootstrap box** ([`bootstrap`]) is this format's
//!   addressing scheme — a wrong reading here produces a file that parses
//!   and resolves to the wrong fragment, not one that fails to parse — so
//!   every field in it was measured byte-by-byte against the reference
//!   rather than assumed. Every fragment lands in segment 1; a second
//!   segment is a live-streaming (`-window_size`) concern this crate does
//!   not implement.
//! - **`Manifest`** ([`manifest`]) matches the reference's `manifest`/
//!   `bootstrapInfo`/`media` shape, including the `<media>` element's own
//!   base64-encoded `onMetaData` AMF0 blob ([`amf0`]).
//! - **No directory-creation gap here** (contrast `vaco-mux-smoothstreaming`,
//!   `planning/INTERFACE-GAPS.md` gap 27): every file this crate writes —
//!   the manifest, every `.abst`, every fragment — sits flat in the
//!   manifest's own directory, exactly the naming convention
//!   `vaco-mux-dash`/`vaco-mux-hls` already use. Measured directly, with a
//!   two-quality-level reference tree: `stream0.abst`/`stream1.abst` and
//!   their fragments never nest under a per-quality subdirectory the way
//!   Smooth Streaming's `QualityLevels(<bitrate>)/` does.
//!
//! # What is deferred
//!
//! - **Re-framing Annex-B H.264 into length-prefixed NALUs**: not done.
//!   This crate requires `CodecParameters::nal_length_size == Some(4)`
//!   (i.e. already-`avcC`-framed samples, the convention every other
//!   MP4-family muxer in this workspace already relies on) and refuses
//!   anything else with a clear error rather than silently mis-framing.
//! - **ADTS-framed AAC**: not accepted. Samples must already be raw AAC
//!   access units with no ADTS header, matching the same MP4-family
//!   convention.
//! - **Playback through a real Flash/HDS client**: not verifiable on this
//!   machine (no such client is available here) — this issue's own Acc
//!   criterion is only "the manifest and fragment set match the
//!   reference's structure", which this crate's tests do check, end to
//!   end, against real `file:` output.
//!
//! # Configuration
//!
//! [`HdsMuxOptions`] — currently `min_frag_duration_us` (default
//! `10_000_000`, matching `ffmpeg -h muxer=hds`).
//!
//! # Dependencies
//!
//! `vaco-format-adaptive` (`WriteAccess`, relative-URL `resolve`),
//! `vaco-format-isom` (`build::{bx, fullbx}` only), `vaco-format-core`,
//! `vaco-io`, `vaco-codec-core`, `vaco-packet`, `vaco-core`, `vaco-limits`,
//! `vaco-protocol-core`. No dependency on any `vaco-parse-*` crate, and no
//! new external dependency for base64 (D10) — [`base64`] hand-rolls it,
//! following the same convention `vaco-protocol-http`, `vaco-protocol-local`
//! and others in this workspace already use.

#![forbid(unsafe_code)]

pub mod amf0;
pub mod base64;
pub mod bootstrap;
pub mod flv;
pub mod manifest;

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Duration, Error, Rational, Result, Timestamp};
use vaco_format_adaptive::{WriteAccess, resolve};
use vaco_format_core::{Muxer, MuxerDesc};
use vaco_io::MediaSink;
use vaco_packet::Packet;

use bootstrap::FragmentRun;

/// `ffmpeg -h muxer=hds`'s own `-min_frag_duration` default: 10 seconds, in
/// microseconds.
pub const DEFAULT_MIN_FRAG_DURATION_US: u64 = 10_000_000;

#[derive(Debug, Clone, Copy)]
pub struct HdsMuxOptions {
    pub min_frag_duration_us: u64,
}

impl HdsMuxOptions {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            min_frag_duration_us: DEFAULT_MIN_FRAG_DURATION_US,
        }
    }

    fn min_frag_duration_ms(self) -> u64 {
        micros_to_ms(i64::try_from(self.min_frag_duration_us).unwrap_or(i64::MAX))
    }
}

impl Default for HdsMuxOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// Rescale a microsecond count to milliseconds via `vaco_core::Duration`'s
/// own checked-rounding rescale rather than a bare `/` (workspace lint:
/// `clippy::integer_division`).
fn micros_to_ms(us: i64) -> u64 {
    Duration::from_micros(us)
        .to_ticks(Rational::new(1, 1000))
        .unwrap_or(0)
        .max(0) as u64
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match MuxerDesc::open's fn pointer type"
)]
fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    // Same gap `vaco-mux-smoothstreaming`/`vaco-mux-dash`/`vaco-mux-hls`
    // document: `MuxerDesc::open` has no filename and no protocol write
    // access. This writes the one sink it is given, once, as the manifest,
    // and refuses every `write_packet`.
    Ok(Box::new(HdsMuxer {
        manifest_url: String::new(),
        write: None,
        manifest_sink: Some(sink),
        opts: HdsMuxOptions::new(),
        levels: Vec::new(),
        next_stream_index: 0,
        header_written: false,
        trailer_written: false,
    }))
}

pub const MUXER: MuxerDesc = MuxerDesc {
    name: "hds",
    long_name: "HDS Muxer",
    extensions: &["f4m"],
    default_video: Some(CodecId::H264),
    default_audio: Some(CodecId::Aac),
    open: open_muxer,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackKind {
    Video,
    Audio,
}

struct TrackState {
    stream_index: u32,
    bit_rate: u64,
    /// `avcC` (video) or raw `AudioSpecificConfig` (audio), verbatim — the
    /// FLV sequence-header tag body for this track.
    sequence_header: Vec<u8>,
    running_ms: u64,
    width: u32,
    height: u32,
    sample_rate: u32,
    /// Fallback duration (ms) for a packet that states none —
    /// `Packet::duration == 0` is the ordinary `-c copy` case out of a
    /// demuxer that reports none, and the FLV tag timestamp this track
    /// writes is `running_ms` *before* adding the current packet's own
    /// duration, so a zero here does not just leave one tag's timestamp
    /// wrong: it freezes every later tag on this track at whatever
    /// timestamp `running_ms` last reached, since it never advances again.
    /// Seeded from the declared frame rate for video (audio has no
    /// samples-per-frame field to derive one from —
    /// [`vaco_codec_core::AudioParameters`] carries none), then kept
    /// current from the last packet that *did* state a duration.
    duration_hint_ms: u64,
}

struct Level {
    index: u32,
    video: Option<TrackState>,
    audio: Option<TrackState>,
    pending_tags: Vec<u8>,
    fragment_start_ms: u64,
    fragment_index: u32,
    fragment_runs: Vec<FragmentRun>,
}

impl Level {
    fn new(index: u32) -> Self {
        Self {
            index,
            video: None,
            audio: None,
            pending_tags: Vec::new(),
            fragment_start_ms: 0,
            fragment_index: 1,
            fragment_runs: Vec::new(),
        }
    }

    fn gating_running_ms(&self) -> u64 {
        self.video.as_ref().map_or_else(
            || {
                self.audio
                    .as_ref()
                    .map_or(self.fragment_start_ms, |a| a.running_ms)
            },
            |v| v.running_ms,
        )
    }
}

/// The HDS muxer.
pub struct HdsMuxer {
    manifest_url: String,
    write: Option<WriteAccess>,
    manifest_sink: Option<Box<dyn MediaSink>>,
    opts: HdsMuxOptions,
    levels: Vec<Level>,
    next_stream_index: u32,
    header_written: bool,
    trailer_written: bool,
}

impl core::fmt::Debug for HdsMuxer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HdsMuxer")
            .field("levels", &self.levels.len())
            .finish_non_exhaustive()
    }
}

impl HdsMuxer {
    /// The real entry point: `manifest_url` is where `index.f4m` will be
    /// written; every `stream<N>.abst`/`stream<N>Seg1-Frag<M>` file is
    /// resolved relative to it.
    #[must_use]
    pub fn new(manifest_url: String, write: Option<WriteAccess>, opts: HdsMuxOptions) -> Self {
        Self {
            manifest_url,
            write,
            manifest_sink: None,
            opts,
            levels: Vec::new(),
            next_stream_index: 0,
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
                "hds output needs protocol write access, and none was supplied",
            ));
        };
        let mut sink = write.create(url)?;
        sink.write(content.as_bytes())?;
        sink.flush()
    }

    fn write_binary_file(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let Some(write) = &self.write else {
            return Err(Error::Unsupported(
                "hds output needs protocol write access, and none was supplied",
            ));
        };
        let url = resolve(&self.manifest_url, name);
        let mut sink = write.create(&url)?;
        sink.write(bytes)?;
        sink.flush()
    }

    /// `(level index, is_video)` for the level that owns `stream_index`.
    fn locate(&self, stream_index: u32) -> Result<(usize, bool)> {
        for (i, level) in self.levels.iter().enumerate() {
            if level
                .video
                .as_ref()
                .is_some_and(|t| t.stream_index == stream_index)
            {
                return Ok((i, true));
            }
            if level
                .audio
                .as_ref()
                .is_some_and(|t| t.stream_index == stream_index)
            {
                return Ok((i, false));
            }
        }
        Err(Error::InvalidData("packet names an unknown stream"))
    }

    /// Write both available tracks' sequence-header tags at the start of a
    /// fresh fragment, if this is in fact a fresh fragment (a no-op once
    /// any real sample has already been buffered for it).
    fn ensure_fragment_started(&mut self, level_idx: usize) -> Result<()> {
        let Some(level) = self.levels.get_mut(level_idx) else {
            return Err(Error::InvalidData("packet names an unknown stream"));
        };
        if !level.pending_tags.is_empty() {
            return Ok(());
        }
        if let Some(video) = &level.video {
            let payload = flv::video_payload(
                true,
                flv::AVC_PACKET_TYPE_SEQUENCE_HEADER,
                0,
                &video.sequence_header,
            );
            let ts = u32::try_from(video.running_ms).unwrap_or(u32::MAX);
            flv::write_tag(&mut level.pending_tags, flv::TAG_TYPE_VIDEO, ts, &payload);
        }
        if let Some(audio) = &level.audio {
            let payload =
                flv::audio_payload(flv::AAC_PACKET_TYPE_SEQUENCE_HEADER, &audio.sequence_header);
            let ts = u32::try_from(audio.running_ms).unwrap_or(u32::MAX);
            flv::write_tag(&mut level.pending_tags, flv::TAG_TYPE_AUDIO, ts, &payload);
        }
        Ok(())
    }

    fn flush_level(&mut self, level_idx: usize) -> Result<()> {
        let Some(level) = self.levels.get(level_idx) else {
            return Ok(());
        };
        if level.pending_tags.is_empty() {
            return Ok(());
        }
        let mdat = vaco_format_isom::build::bx(b"mdat", &level.pending_tags);
        let frag_name = format!("stream{}Seg1-Frag{}", level.index, level.fragment_index);
        self.write_binary_file(&frag_name, &mdat)?;

        let Some(level) = self.levels.get_mut(level_idx) else {
            return Ok(());
        };
        let gating_ms = level.gating_running_ms();
        let duration_ms = gating_ms.saturating_sub(level.fragment_start_ms);
        level.fragment_runs.push(FragmentRun {
            first_fragment: level.fragment_index,
            first_fragment_timestamp_ms: level.fragment_start_ms,
            duration_ms: u32::try_from(duration_ms).unwrap_or(u32::MAX),
        });
        level.fragment_index = level.fragment_index.saturating_add(1);
        level.fragment_start_ms = gating_ms;
        level.pending_tags.clear();
        Ok(())
    }

    fn composition_time_ms(pts: Timestamp, dts: Timestamp) -> i32 {
        match (pts.ticks(), dts.ticks()) {
            (Some(p), Some(d)) => {
                let diff_us = p.saturating_sub(d);
                i32::try_from(micros_to_ms(diff_us).min(u64::from(u32::MAX))).unwrap_or(0)
                    * if diff_us < 0 { -1 } else { 1 }
            }
            _ => 0,
        }
    }
}

impl Muxer for HdsMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.header_written {
            return Err(Error::InvalidData(
                "streams must be added before the header is written",
            ));
        }
        let bit_rate = params.bit_rate.ok_or(Error::InvalidData(
            "hds needs a declared bit_rate: it feeds the manifest's bitrate and onMetaData datarate fields",
        ))?;
        let stream_index = self.next_stream_index;
        self.next_stream_index = self.next_stream_index.saturating_add(1);

        let (kind, track) = match params.codec_id {
            Some(CodecId::H264) => {
                let video = params.video.as_ref().ok_or(Error::InvalidData(
                    "hds needs VideoParameters for an H.264 stream",
                ))?;
                if video.nal_length_size != Some(4) {
                    return Err(Error::Unsupported(
                        "hds needs already length-prefixed (avcC-style) H.264 samples \
                         (VideoParameters::nal_length_size == Some(4)); re-framing Annex-B is not done here",
                    ));
                }
                let sequence_header = params.extradata.clone().ok_or(Error::InvalidData(
                    "hds needs avcC extradata for an H.264 stream",
                ))?;
                // Not an invented constant: derived from this stream's own
                // declared frame rate, `0` only when the container states no
                // frame rate either.
                let duration_hint_ms = if video.frame_rate.num > 0 && video.frame_rate.den > 0 {
                    match (
                        u64::try_from(video.frame_rate.num),
                        u64::try_from(video.frame_rate.den),
                    ) {
                        (Ok(num), Ok(den)) if num > 0 => {
                            1000u64.saturating_mul(den).checked_div(num).unwrap_or(0)
                        }
                        _ => 0,
                    }
                } else {
                    0
                };
                (
                    TrackKind::Video,
                    TrackState {
                        stream_index,
                        bit_rate,
                        sequence_header,
                        running_ms: 0,
                        width: video.width,
                        height: video.height,
                        sample_rate: 0,
                        duration_hint_ms,
                    },
                )
            }
            Some(CodecId::Aac) => {
                let sequence_header = params.extradata.clone().ok_or(Error::InvalidData(
                    "hds needs AudioSpecificConfig extradata for an AAC stream",
                ))?;
                let audio = params.audio.as_ref().ok_or(Error::InvalidData(
                    "hds needs AudioParameters for an AAC stream",
                ))?;
                (
                    TrackKind::Audio,
                    TrackState {
                        stream_index,
                        bit_rate,
                        sequence_header,
                        running_ms: 0,
                        width: 0,
                        height: 0,
                        sample_rate: audio.sample_rate,
                        // No samples-per-frame field to derive one from; the
                        // fallback starts at 0 and picks up the first packet
                        // that states a real duration.
                        duration_hint_ms: 0,
                    },
                )
            }
            _ => {
                return Err(Error::Unsupported(
                    "hds only carries H.264 video and AAC audio (measured: ffmpeg -f hds supports no other codec either)",
                ));
            }
        };

        let needs_new_level = match self.levels.last() {
            Some(l) => match kind {
                TrackKind::Video => l.video.is_some(),
                TrackKind::Audio => l.audio.is_some(),
            },
            None => true,
        };
        if needs_new_level {
            let index = u32::try_from(self.levels.len())
                .map_err(|_| Error::InvalidData("too many quality levels"))?;
            self.levels.push(Level::new(index));
        }
        let Some(level) = self.levels.last_mut() else {
            return Err(Error::InvalidData("level vanished"));
        };
        match kind {
            TrackKind::Video => level.video = Some(track),
            TrackKind::Audio => level.audio = Some(track),
        }
        Ok(stream_index)
    }

    fn write_header(&mut self) -> Result<()> {
        if self.header_written {
            return Err(Error::InvalidData("header written twice"));
        }
        if self.levels.is_empty() {
            return Err(Error::InvalidData("hds output needs at least one stream"));
        }
        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("packet written before the header"));
        }
        let (level_idx, is_video) = self.locate(packet.stream_index)?;
        let threshold = self.opts.min_frag_duration_ms();
        let stated_ms = micros_to_ms(packet.duration.as_micros());

        if is_video {
            let should_flush = {
                let Some(level) = self.levels.get(level_idx) else {
                    return Err(Error::InvalidData("packet names an unknown stream"));
                };
                let Some(video) = level.video.as_ref() else {
                    return Err(Error::InvalidData("packet names an unknown stream"));
                };
                packet.is_key()
                    && !level.pending_tags.is_empty()
                    && video.running_ms.saturating_sub(level.fragment_start_ms) >= threshold
            };
            if should_flush {
                self.flush_level(level_idx)?;
            }
        }

        self.ensure_fragment_started(level_idx)?;

        let Some(level) = self.levels.get_mut(level_idx) else {
            return Err(Error::InvalidData("packet names an unknown stream"));
        };
        if is_video {
            let Some(video) = level.video.as_mut() else {
                return Err(Error::InvalidData("packet names an unknown stream"));
            };
            let ts = u32::try_from(video.running_ms).unwrap_or(u32::MAX);
            let cts = Self::composition_time_ms(packet.pts, packet.dts);
            let payload = flv::video_payload(
                packet.is_key(),
                flv::AVC_PACKET_TYPE_NALU,
                cts,
                packet.payload(),
            );
            flv::write_tag(&mut level.pending_tags, flv::TAG_TYPE_VIDEO, ts, &payload);
            // See `TrackState::duration_hint_ms`'s own doc: a zero here does
            // not merely mislabel this tag, it freezes every later one too,
            // since `running_ms` never advances again.
            let dur_ms = if stated_ms == 0 {
                video.duration_hint_ms
            } else {
                video.duration_hint_ms = stated_ms;
                stated_ms
            };
            video.running_ms = video.running_ms.saturating_add(dur_ms);
        } else {
            let Some(audio) = level.audio.as_mut() else {
                return Err(Error::InvalidData("packet names an unknown stream"));
            };
            let ts = u32::try_from(audio.running_ms).unwrap_or(u32::MAX);
            let payload = flv::audio_payload(flv::AAC_PACKET_TYPE_RAW, packet.payload());
            flv::write_tag(&mut level.pending_tags, flv::TAG_TYPE_AUDIO, ts, &payload);
            let dur_ms = if stated_ms == 0 {
                audio.duration_hint_ms
            } else {
                audio.duration_hint_ms = stated_ms;
                stated_ms
            };
            audio.running_ms = audio.running_ms.saturating_add(dur_ms);

            let audio_only_should_flush = level.video.is_none()
                && level.audio.as_ref().is_some_and(|a| {
                    a.running_ms.saturating_sub(level.fragment_start_ms) >= threshold
                });
            if audio_only_should_flush {
                self.flush_level(level_idx)?;
            }
        }
        Ok(())
    }

    fn write_trailer(&mut self) -> Result<()> {
        if self.trailer_written {
            return Err(Error::InvalidData("trailer written twice"));
        }
        for idx in 0..self.levels.len() {
            self.flush_level(idx)?;
        }
        self.trailer_written = true;

        let mut manifest_levels = Vec::new();
        let mut total_ms: u64 = 0;
        for level in &self.levels {
            let current_media_time_ms = level.gating_running_ms();
            total_ms = total_ms.max(current_media_time_ms);
            let abst = bootstrap::build_abst(current_media_time_ms, &level.fragment_runs);
            self.write_binary_file(&format!("stream{}.abst", level.index), &abst)?;

            let mut total_bps: u64 = 0;
            let (mut width, mut height) = (0.0, 0.0);
            let (mut video_kibit, mut video_codec_id) = (0.0, 0.0);
            let (mut audio_kibit, mut audio_sample_rate, mut audio_sample_size, mut audio_codec_id) =
                (0.0, 0.0, 0.0, 0.0);
            if let Some(v) = &level.video {
                total_bps = total_bps.saturating_add(v.bit_rate);
                width = f64::from(v.width);
                height = f64::from(v.height);
                video_kibit = (v.bit_rate as f64) / 1024.0;
                video_codec_id = 7.0;
            }
            if let Some(a) = &level.audio {
                total_bps = total_bps.saturating_add(a.bit_rate);
                audio_kibit = (a.bit_rate as f64) / 1024.0;
                audio_sample_rate = f64::from(a.sample_rate);
                audio_sample_size = 16.0;
                audio_codec_id = 10.0;
            }
            let bitrate_kbps = ((total_bps as f64) / 1000.0).round() as u64;

            manifest_levels.push(manifest::ManifestLevel {
                index: level.index,
                bitrate_kbps,
                metadata: amf0::OnMetaData {
                    width,
                    height,
                    video_datarate_kibit: video_kibit,
                    video_codec_id,
                    audio_datarate_kibit: audio_kibit,
                    audio_sample_rate,
                    audio_sample_size,
                    audio_codec_id,
                },
            });
        }
        let total_secs = (total_ms as f64) / 1000.0;
        let text = manifest::build_manifest("index.f4m", total_secs, &manifest_levels);
        self.write_text_file(&self.manifest_url.clone(), &text)
    }
}
