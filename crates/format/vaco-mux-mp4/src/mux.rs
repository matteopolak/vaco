//! [`MovMuxer`]: the `vaco_format_core::Muxer` implementation, dispatching to
//! [`crate::progressive`] or [`crate::fragmented`] depending on `-movflags`.

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, MediaType, Rational, Result};
use vaco_format_core::metadata::MuxMetadata;
use vaco_format_core::mux::{BitstreamAction, CodecSupport, global_header_action};
use vaco_format_core::{FormatFlags, Muxer};
use vaco_io::{IoOptions, IoWriter, MediaSink};
use vaco_packet::{Packet, PacketSideData};

use crate::options::{ChapterMark, CoverArt, MuxOptions};
use crate::track::TrackState;
use crate::{entry, fragmented, meta, progressive};

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
const SUPPORTED_AUDIO: &[CodecId] = &[CodecId::Aac, CodecId::Opus, CodecId::Flac, CodecId::Mp3, CodecId::Alac];

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
    /// Set by [`Muxer::set_metadata`], resolved into `opts`/`tracks` at the
    /// top of [`MovMuxer::write_header`] rather than at `set_metadata` time —
    /// see that method's docs for why the order it runs in relative to
    /// [`Muxer::add_stream`] cannot be assumed.
    metadata: MuxMetadata,
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
            metadata: MuxMetadata::default(),
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
        let mut built = entry::build(params)?;
        if let Some(enc) = self.opts.encryption() {
            built = entry::wrap_encrypted(built, enc.key_id);
        }
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
        // Re-validated here, not only in `with_options`: `set_option` can
        // reach every field `validate` inspects on an already-constructed
        // muxer (M29's private-options path calls it before `init`, not
        // before construction), so a bad combination assembled one option at
        // a time must be caught at the same point a bad combination handed
        // to `with_options` in one shot already is.
        self.opts.validate()?;
        // `DEFAULT_MOVIE_TIMESCALE` (1000) whenever any track is video —
        // measured on `ffmpeg -c copy -f mp4` across a video-only reordered
        // stream, a video-only non-reordered one, a raw H.264 elementary
        // stream, and a video+audio file: `mvhd.timescale` is `1000` in every
        // one, never the video track's own (CONFORMANCE-FINDINGS 49; a
        // 12800Hz video track timescale stays `1000` at the movie level).
        // Audio-only is the one case that gets the track's own timescale
        // instead (measured: an audio-only AAC/48000 file's `mvhd.timescale`
        // is `48000`), which is what this falls through to below.
        let has_video = self.tracks.iter().any(|t| t.media == MediaType::Video);
        if !has_video
            && let Some(max_ts) = self.tracks.iter().map(|t| t.timescale).max()
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
        self.resolve_metadata();
        match &mut self.mode {
            Mode::Progressive(state) => {
                progressive::write_header(&mut self.out, &self.opts, state, &self.tracks)?;
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
        self.adopt_new_extradata(idx, packet)?;
        let dts = packet.dts.ticks().or(packet.pts.ticks()).unwrap_or(0);
        let pts = packet.pts.ticks().unwrap_or(dts);
        let cts_offset = i32::try_from(pts.saturating_sub(dts)).unwrap_or(0);
        let is_sync = packet.is_key();
        // `Packet::duration` is always microseconds (see `vaco_packet::Packet::rescale_ts`'s
        // doc comment: only `pts`/`dts` are rescaled to the muxer's own time base, `duration`
        // deliberately is not), so it must be converted into this track's own timescale here —
        // storing the raw microsecond count as if it already were a tick count in `mdhd`'s
        // timescale is what produced a ~1600x wrong `mvhd`/`mdhd` duration (measured: a 1-second,
        // 25fps clip's last sample duration of 40000us was written verbatim as `40000` ticks at
        // timescale 25, instead of the `1` tick that timescale actually calls for).
        let duration_ticks = self
            .tracks
            .get(idx)
            .and_then(|t| packet.duration.to_ticks(t.time_base()))
            .unwrap_or(0)
            .max(0);
        let duration = u32::try_from(duration_ticks).unwrap_or(0);
        // Common Encryption never reaches the fragmented arm below —
        // `MuxOptions::validate` (checked in `init`) refuses that combination
        // outright — so encrypting unconditionally before the match is safe:
        // the fragmented path's own `.to_vec()` just copies whichever slice
        // this resolves to.
        let mut encrypted;
        let payload = match self.opts.encryption() {
            Some(enc) => {
                let sample_index = self.tracks.get(idx).map_or(0, |t| t.samples.len());
                encrypted = packet.payload().to_vec();
                encrypt_cenc_sample(&enc.key, sample_index, &mut encrypted);
                encrypted.as_slice()
            }
            None => packet.payload(),
        };

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

    /// Reaches [`MuxOptions::bitexact`], which existed already (it already
    /// suppresses `creation_time_unix`) but had no caller: nothing in this
    /// crate overrode the trait's no-op default, so `-fflags +bitexact` on
    /// the output never actually reached it — the same "an API with no
    /// caller is invisible to every test you will write" shape
    /// `planning/AGENT-CONSTRAINTS.md` warns about, found the same way it
    /// says to find it: running the command a user would run and comparing
    /// against the reference (CONFORMANCE-FINDINGS 49).
    fn set_bitexact(&mut self, bitexact: bool) {
        self.opts.bitexact = bitexact;
    }

    fn check_bitstream(&mut self, params: &CodecParameters, pkt: &Packet) -> Result<BitstreamAction> {
        // Without this, a `GLOBALHEADER` track with empty extradata asks for
        // `extract_extradata` on every one of `decide_bitstream`'s re-asks:
        // nothing about `params` changes between them, so the *filter
        // request* never changes either, and `MuxWriter` refuses a muxer
        // that answers `Insert` with the same name twice — this was
        // unreachable before a `BsfProvider` actually supplied
        // `extract_extradata`, which is what makes it worth guarding now.
        let idx = usize::try_from(pkt.stream_index).ok();
        if idx.and_then(|i| self.tracks.get(i)).is_some_and(|t| t.bsf_decided) {
            return Ok(BitstreamAction::Keep);
        }
        if let Some(t) = idx.and_then(|i| self.tracks.get_mut(i)) {
            t.bsf_decided = true;
        }
        Ok(global_header_action(self.flags(), params))
    }

    fn set_metadata(&mut self, metadata: &MuxMetadata) -> Result<()> {
        // Just storage — see `MovMuxer::metadata`'s field docs for why
        // resolution is deferred to `write_header` rather than done here.
        self.metadata = metadata.clone();
        Ok(())
    }

    /// Parses this crate's own `-movflags` spelling and the `-encryption_*`/
    /// `-frag_*` options, so a caller reaches them through
    /// `MuxBuilder::with_private_options` rather than only through
    /// [`MovMuxer::with_options`]. Every name matches `ffmpeg -h muxer=mov`'s
    /// own vocabulary; an unrecognised name or `movflags` value is refused
    /// (M8's "reported, not silently dropped" rule), not ignored.
    fn set_option(&mut self, name: &str, value: &str) -> Result<()> {
        match name {
            "movflags" => {
                self.opts.movflags |= parse_movflags(value)?;
                Ok(())
            }
            "encryption_scheme" => {
                self.opts.encryption_scheme = match value {
                    "none" => None,
                    "cenc-aes-ctr" => Some(crate::options::EncryptionScheme::CencAesCtr),
                    other => {
                        return Err(Error::Option {
                            name: name.to_owned(),
                            detail: format!(
                                "unknown encryption_scheme {other:?}; this muxer writes cenc-aes-ctr only"
                            ),
                        });
                    }
                };
                Ok(())
            }
            "encryption_key" => {
                self.opts.encryption_key = Some(parse_hex16(name, value)?);
                Ok(())
            }
            "encryption_kid" => {
                self.opts.encryption_key_id = Some(parse_hex16(name, value)?);
                Ok(())
            }
            "frag_duration" => {
                let micros: i64 = value.parse().map_err(|_| Error::Option {
                    name: name.to_owned(),
                    detail: "expected an integer microsecond count".to_owned(),
                })?;
                self.opts.frag_duration = Some(vaco_core::Duration::from_micros(micros));
                Ok(())
            }
            "frag_size" => {
                let bytes: u64 = value.parse().map_err(|_| Error::Option {
                    name: name.to_owned(),
                    detail: "expected an integer byte count".to_owned(),
                })?;
                self.opts.frag_size = Some(bytes);
                Ok(())
            }
            _ => Err(Error::Option {
                name: name.to_owned(),
                detail: "this muxer has no such option".to_owned(),
            }),
        }
    }
}

/// Parse `-movflags`' `+flag+flag` (equivalently `flag+flag`, `-flag` to
/// clear) spelling into the subset of `ffmpeg -h muxer=mov`'s flag list this
/// crate implements. An unimplemented or unknown flag name is refused rather
/// than silently dropped, so `+faststart+rtphint` fails loudly instead of
/// quietly writing a file without hint tracks nobody asked to omit.
fn parse_movflags(value: &str) -> Result<crate::options::MovFlags> {
    use crate::options::MovFlags;
    let mut out = MovFlags::empty();
    for tok in value.split('+') {
        if tok.is_empty() {
            continue;
        }
        let (negate, name) = tok.strip_prefix('-').map_or((false, tok), |n| (true, n));
        let flag = match name {
            "faststart" => MovFlags::FASTSTART,
            "empty_moov" => MovFlags::EMPTY_MOOV,
            "frag_keyframe" => MovFlags::FRAG_KEYFRAME,
            "frag_every_frame" => MovFlags::FRAG_EVERY_FRAME,
            "default_base_moof" => MovFlags::DEFAULT_BASE_MOOF,
            "omit_tfhd_offset" => MovFlags::OMIT_TFHD_OFFSET,
            "separate_moof" => MovFlags::SEPARATE_MOOF,
            "dash" => MovFlags::DASH,
            "cmaf" => MovFlags::CMAF,
            other => {
                return Err(Error::Option {
                    name: "movflags".to_owned(),
                    detail: format!("unknown or unimplemented movflag {other:?}"),
                });
            }
        };
        if negate {
            out.remove(flag);
        } else {
            out.insert(flag);
        }
    }
    Ok(out)
}

/// Parse a 32-hex-character `-encryption_key`/`-encryption_kid` value.
fn parse_hex16(name: &str, value: &str) -> Result<[u8; 16]> {
    let bad = || Error::Option {
        name: name.to_owned(),
        detail: "expected 32 hex characters (16 bytes)".to_owned(),
    };
    if value.len() != 32 {
        return Err(bad());
    }
    let mut out = [0u8; 16];
    for (i, slot) in out.iter_mut().enumerate() {
        let byte_str = value.get(i.saturating_mul(2)..i.saturating_mul(2).saturating_add(2));
        *slot = byte_str
            .and_then(|s| u8::from_str_radix(s, 16).ok())
            .ok_or_else(bad)?;
    }
    Ok(out)
}

/// Apply the CENC 'cenc' scheme's full-sample AES-128-CTR in place:
/// `counter_block = IV(8 bytes) ++ 0u64`, `IV = sample_index + 1` big-endian
/// — the same numbering [`crate::progressive`]'s `senc` writer uses, so the
/// two agree without either side storing the IV separately.
fn encrypt_cenc_sample(key: &[u8; 16], sample_index: usize, payload: &mut [u8]) {
    let iv = u64::try_from(sample_index)
        .unwrap_or(u64::MAX)
        .saturating_add(1)
        .to_be_bytes();
    let mut counter = [0u8; 16];
    if let Some(slot) = counter.get_mut(..8) {
        slot.copy_from_slice(&iv);
    }
    vaco_crypto::ctr_apply_aes128(key, &counter, payload);
}

impl MovMuxer {
    /// Rebuild track `idx`'s sample entry when `packet` carries a
    /// [`PacketSideData::NewExtradata`] — the bytes
    /// [`MovMuxer::check_bitstream`]'s `extract_extradata` request produces,
    /// once a `BsfProvider` actually supplies that filter.
    ///
    /// This is what closes the loop `entry.rs`'s own module docs describe:
    /// `add_stream` built `track.entry` from whatever extradata the caller
    /// had *before* the first packet was ever inspected, which for a stream
    /// sourced from a container with no configuration record (AVI, raw
    /// Annex B) is none at all. [`crate::progressive::finish`] does not write
    /// `stsd` until [`Muxer::write_trailer`], reading `track.entry` at that
    /// point rather than at `add_stream` time, so replacing it here — on the
    /// first packet that actually has parameter sets to offer — reaches the
    /// file. Fragmented mode's init segment is written earlier and is not
    /// helped by this; a fragmented `GLOBALHEADER` track still needs
    /// extradata declared up front, same as before this method existed.
    ///
    /// # Errors
    ///
    /// Whatever [`entry::build`] returns for the updated parameters —
    /// propagated rather than discarded, since a `let _ =` here would be
    /// exactly the "the failure was discarded rather than reported" mistake
    /// `planning/AGENT-CONSTRAINTS.md` warns about for this exact seam.
    fn adopt_new_extradata(&mut self, idx: usize, packet: &Packet) -> Result<()> {
        let Some(new_extradata) = packet.side_data.iter().find_map(|sd| match sd {
            PacketSideData::NewExtradata(buf) => Some(buf.as_slice().to_vec()),
            _ => None,
        }) else {
            return Ok(());
        };
        let Some(track) = self.tracks.get_mut(idx) else {
            return Ok(());
        };
        if track.params.extradata.as_deref() == Some(new_extradata.as_slice()) {
            return Ok(());
        }
        track.params.extradata = Some(new_extradata);
        track.entry = entry::build(&track.params)?;
        Ok(())
    }

    /// Fold `self.metadata` into `self.opts`/`self.tracks`, once, at the top
    /// of [`write_header`](Muxer::write_header) — by which point every
    /// `add_stream` call has already happened regardless of when
    /// [`Muxer::set_metadata`] itself ran (M30, gap 1; see that method's
    /// docs and `crate::meta`'s module docs for the exact key mapping).
    fn resolve_metadata(&mut self) {
        // File-level tags: only the keys `meta::itunes_fourcc` maps reach
        // `ilst` at all (see that function's docs for why the rest are
        // dropped rather than guessed at). A later `-metadata` for the same
        // key replaces rather than duplicates the atom.
        //
        // `encoder` is dropped outright under `bitexact`: measured, an
        // MP4-sourced `encoder=Lavf62.12.100` tag (carried in from the
        // *input's* own metadata on a stream copy, not fabricated by this
        // crate) reaches `©too` under a plain remux but is absent from the
        // reference's own bitexact output — and an *explicit*
        // `-metadata title=...` still comes through under bitexact, so this
        // is specifically the auto-populated tool tag, not metadata in
        // general (CONFORMANCE-FINDINGS 49).
        for (key, value) in &self.metadata.tags {
            if self.opts.bitexact && key.eq_ignore_ascii_case("encoder") {
                continue;
            }
            if let Some(fourcc) = meta::itunes_fourcc(key) {
                self.opts.tags.retain(|(k, _)| *k != fourcc);
                self.opts.tags.push((fourcc, value.clone()));
            }
        }

        // Per-stream `language`: the only per-stream concept this container
        // format has a field for outside `ilst` (there is no per-track
        // title box this crate writes). Anything else in a per-stream tag
        // list has nowhere to go in MP4 and is silently dropped, matching
        // `itunes_fourcc`'s own policy for an unmapped file-level key.
        for (i, track) in self.tracks.iter_mut().enumerate() {
            let Ok(stream_index) = u32::try_from(i) else {
                continue;
            };
            for (k, v) in self.metadata.tags_for_stream(stream_index) {
                if k.eq_ignore_ascii_case("language")
                    && let Some(lang) = meta::parse_iso639(v)
                {
                    track.language = lang.pack();
                }
            }
        }

        // The first attachment whose `mime_type` measures as an image
        // becomes `covr` — `MuxOptions::cover_art` holds at most one, so a
        // caller-supplied `-vf`-driven `MovMuxer::with_options` cover art
        // wins over anything `set_metadata` would add.
        if self.opts.cover_art.is_none() {
            self.opts.cover_art = self.metadata.attachments.iter().find_map(|att| {
                let mime = att.mime_type.to_ascii_lowercase();
                if mime.contains("png") {
                    Some(CoverArt {
                        is_png: true,
                        data: att.data.clone(),
                    })
                } else if mime.contains("jpeg") || mime.contains("jpg") {
                    Some(CoverArt {
                        is_png: false,
                        data: att.data.clone(),
                    })
                } else {
                    None
                }
            });
        }

        if self.opts.chapters.is_empty() {
            self.opts.chapters = self
                .metadata
                .chapters
                .iter()
                .map(|c| {
                    let title = c
                        .metadata
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("title"))
                        .map_or_else(String::new, |(_, v)| v.clone());
                    ChapterMark {
                        start: c.start,
                        time_base: c.time_base,
                        title,
                    }
                })
                .collect();
        }
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
