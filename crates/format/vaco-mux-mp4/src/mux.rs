//! [`MovMuxer`]: the `vaco_format_core::Muxer` implementation, dispatching to
//! [`crate::progressive`] or [`crate::fragmented`] depending on `-movflags`.

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, MediaType, Rational, Result};
use vaco_format_core::mux::{BitstreamAction, CodecSupport, global_header_action};
use vaco_format_core::{FormatFlags, Muxer};
use vaco_io::{IoOptions, IoWriter, MediaSink};
use vaco_packet::Packet;

use crate::options::MuxOptions;
use crate::track::TrackState;
use crate::{entry, fragmented, progressive};

/// The default movie timescale this crate writes: high enough that no common
/// frame rate needs `mvhd`'s duration field to round, and the same order of
/// magnitude `ffmpeg 8.1` itself picks absent an explicit `-video_track_timescale`.
const DEFAULT_MOVIE_TIMESCALE: u32 = 1000;

/// Codecs this crate has a sample-entry mapping for at all — [`add_stream`]
/// refuses anything else before a single byte is written, per M15.
const SUPPORTED_VIDEO: &[CodecId] = &[
    CodecId::H264,
    CodecId::Hevc,
    CodecId::Av1,
    CodecId::Vp8,
    CodecId::Vp9,
    CodecId::Jpeg,
    CodecId::Png,
];
const SUPPORTED_AUDIO: &[CodecId] = &[CodecId::Aac, CodecId::Opus, CodecId::Flac, CodecId::Mp3];

enum Mode {
    Progressive(progressive::ProgressiveState),
    Fragmented(fragmented::FragmentedState),
}

/// The MP4/MOV muxer.
pub struct MovMuxer {
    out: IoWriter,
    opts: MuxOptions,
    tracks: Vec<TrackState>,
    movie_timescale: u32,
    header_written: bool,
    trailer_written: bool,
    mode: Mode,
}

impl core::fmt::Debug for MovMuxer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MovMuxer")
            .field("tracks", &self.tracks.len())
            .field("header_written", &self.header_written)
            .finish_non_exhaustive()
    }
}

impl MovMuxer {
    /// A muxer with the default options for its registry brand.
    ///
    /// # Errors
    /// Propagates [`vaco_io::IoWriter::new`]'s allocation failure.
    pub fn new(sink: Box<dyn MediaSink>) -> Result<Self> {
        Self::with_options(sink, MuxOptions::default())
    }

    /// A muxer configured beyond what the registry's bare constructor can
    /// express — `movflags`, fragmentation thresholds, metadata. This is the
    /// entry point a caller reaches for anything `-movflags`-shaped.
    ///
    /// # Errors
    /// [`Error::Unsupported`] when `opts` is internally inconsistent (see
    /// [`MuxOptions::validate`]); otherwise as [`MovMuxer::new`].
    pub fn with_options(sink: Box<dyn MediaSink>, opts: MuxOptions) -> Result<Self> {
        opts.validate()?;
        let fragmented = opts.effective_flags().is_fragmented();
        Ok(Self {
            out: IoWriter::new(sink, &IoOptions::default())?,
            mode: if fragmented {
                Mode::Fragmented(fragmented::FragmentedState::new(0))
            } else {
                Mode::Progressive(progressive::ProgressiveState::new())
            },
            opts,
            tracks: Vec::new(),
            movie_timescale: DEFAULT_MOVIE_TIMESCALE,
            header_written: false,
            trailer_written: false,
        })
    }

    /// Bytes written so far.
    #[must_use]
    pub const fn position(&self) -> u64 {
        self.out.pos()
    }

    fn track_time_base(params: &CodecParameters) -> u32 {
        if let Some(v) = &params.video
            && v.frame_rate.is_defined()
            && !v.frame_rate.is_zero()
            && !v.frame_rate.is_infinite()
            && v.frame_rate.num > 0
        {
            return u32::try_from(v.frame_rate.num).unwrap_or(90_000);
        }
        if let Some(a) = &params.audio
            && a.sample_rate > 0
        {
            return a.sample_rate;
        }
        90_000
    }
}

impl Muxer for MovMuxer {
    fn flags(&self) -> FormatFlags {
        // MP4 carries every codec's configuration out of band (`avcC`/`hvcC`/
        // `esds`/...), so `GLOBALHEADER` is unconditional; `SHOW_IDS` because
        // `track_ID` is a real, user-meaningful identifier the reference
        // prints. `TS_NONSTRICT` for fragmented output: a fragment boundary
        // can legitimately repeat a DTS across `traf`s in some encoders'
        // output, and this crate does not need strict monotonicity to place
        // samples correctly the way `stss`-based seeking would.
        let mut f = FormatFlags::GLOBALHEADER | FormatFlags::SHOW_IDS;
        if matches!(self.mode, Mode::Fragmented(_)) {
            f |= FormatFlags::TS_NONSTRICT;
        }
        f
    }

    fn query_codec(&self, codec: CodecId, _strict: i32) -> CodecSupport {
        if SUPPORTED_VIDEO.contains(&codec) || SUPPORTED_AUDIO.contains(&codec) {
            CodecSupport::Supported
        } else {
            CodecSupport::Unsupported
        }
    }

    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.header_written {
            return Err(Error::InvalidData(
                "mp4: streams must be added before the header is written",
            ));
        }
        let built = entry::build(params)?;
        let track_id = u32::try_from(self.tracks.len())
            .unwrap_or(u32::MAX)
            .saturating_add(1);
        let timescale = Self::track_time_base(params);
        let mut track = TrackState::new(track_id, timescale, built, params.clone());
        track.language = vaco_format_isom::lang::PACKED_UND;
        let index = u32::try_from(self.tracks.len())
            .map_err(|_| Error::Unsupported("mp4: too many tracks"))?;
        self.tracks.push(track);
        Ok(index)
    }

    fn init(&mut self) -> Result<()> {
        // A caller may still want a specific movie timescale; absent one,
        // the largest track timescale keeps every track's duration an exact
        // (or close) multiple, which is what `ffmpeg 8.1` itself gravitates
        // toward for a single-track file (measured: an audio-only AAC/48000
        // file's `mvhd.timescale` is `48000`, not `1000`).
        if let Some(max_ts) = self.tracks.iter().map(|t| t.timescale).max()
            && max_ts > 0
        {
            self.movie_timescale = max_ts;
        }
        if let Mode::Fragmented(state) = &mut self.mode {
            *state = fragmented::FragmentedState::new(self.tracks.len());
        }
        Ok(())
    }

    fn write_header(&mut self) -> Result<()> {
        if self.header_written {
            return Err(Error::InvalidData("mp4: header written twice"));
        }
        if self.tracks.is_empty() {
            return Err(Error::Unsupported("mp4: no streams to mux"));
        }
        match &mut self.mode {
            Mode::Progressive(state) => {
                progressive::write_header(&mut self.out, &self.opts, state)?;
            }
            Mode::Fragmented(state) => {
                fragmented::write_header(
                    &mut self.out,
                    &self.opts,
                    state,
                    &self.tracks,
                    self.movie_timescale,
                )?;
            }
        }
        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("mp4: packet written before the header"));
        }
        let idx = usize::try_from(packet.stream_index)
            .ok()
            .filter(|&i| i < self.tracks.len())
            .ok_or(Error::InvalidData("mp4: packet names an unknown track"))?;
        let dts = packet.dts.ticks().or(packet.pts.ticks()).unwrap_or(0);
        let pts = packet.pts.ticks().unwrap_or(dts);
        let cts_offset = i32::try_from(pts.saturating_sub(dts)).unwrap_or(0);
        let is_sync = packet.is_key();
        let duration = u32::try_from(packet.duration.0.max(0)).unwrap_or(0);
        let payload = packet.payload();

        match &mut self.mode {
            Mode::Progressive(state) => {
                progressive::write_sample(
                    &mut self.out,
                    state,
                    &mut self.tracks,
                    idx,
                    payload,
                    dts,
                    cts_offset,
                    is_sync,
                )?;
                if let Some(track) = self.tracks.get_mut(idx)
                    && duration > 0
                {
                    track.last_duration_hint = duration;
                }
            }
            Mode::Fragmented(state) => {
                if fragmented::should_flush(state, &self.opts, idx, dts, is_sync) {
                    fragmented::flush_fragment(&mut self.out, state, &self.tracks, &self.opts)?;
                }
                fragmented::buffer_sample(
                    state,
                    idx,
                    payload.to_vec(),
                    dts,
                    cts_offset,
                    is_sync,
                    duration,
                );
            }
        }
        Ok(())
    }

    fn write_trailer(&mut self) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("mp4: trailer written before the header"));
        }
        if self.trailer_written {
            return Err(Error::InvalidData("mp4: trailer written twice"));
        }
        self.trailer_written = true;
        match &mut self.mode {
            Mode::Progressive(state) => progressive::finish(
                &mut self.out,
                state,
                &mut self.tracks,
                &self.opts,
                self.movie_timescale,
            ),
            Mode::Fragmented(state) => {
                fragmented::finish(&mut self.out, state, &self.tracks, &self.opts)
            }
        }
    }

    fn stream_time_base(&self, stream_index: u32) -> Option<Rational> {
        usize::try_from(stream_index)
            .ok()
            .and_then(|i| self.tracks.get(i))
            .map(TrackState::time_base)
    }

    fn check_bitstream(
        &mut self,
        params: &CodecParameters,
        _pkt: &Packet,
    ) -> Result<BitstreamAction> {
        Ok(global_header_action(self.flags(), params))
    }
}

/// Whether `media`/`codec` is one this crate can mux at all — used by
/// [`crate::brand`]'s `MuxerDesc::default_video`/`default_audio` consumers
/// and available for a caller that wants to check before calling
/// [`vaco_format_core::mux::MuxBuilder::add_stream`].
#[must_use]
pub fn is_supported(media: MediaType, codec: CodecId) -> bool {
    match media {
        MediaType::Video => SUPPORTED_VIDEO.contains(&codec),
        MediaType::Audio => SUPPORTED_AUDIO.contains(&codec),
        _ => false,
    }
}
