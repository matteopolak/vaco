//! `hdrl`: `avih` (`AVIMAINHEADER`), `strh` (`AVISTREAMHEADER`), `strf`, `strn`.
//!
//! Microsoft's *AVI RIFF File Reference* (part of the Multimedia Programming
//! Interface and Data Specifications). Every field layout below was measured
//! against `ffmpeg 8.1`'s own AVI muxer output
//! (`ffmpeg -f lavfi -i testsrc=... -f lavfi -i sine=... -c:v mpeg4 -c:a
//! pcm_s16le out.avi`, then `xxd`) rather than transcribed from memory — see
//! `docs/format/vaco-demux-avi.md` for the exact bytes. One thing the
//! measurement corrected: `AVISTREAMHEADER.rcFrame` is four `i16`s (8 bytes),
//! not the four `i32`s a Win32 `RECT` would suggest, which is why the chunk is
//! 56 bytes and not 64.
//!
//! `strf`'s payload is `BITMAPINFOHEADER` (video) or `WAVEFORMATEX` (audio) —
//! both already parsed by [`vaco_format_riff`], which is the whole reason this
//! crate exists on top of it rather than re-deriving RIFF from scratch.

use vaco_bitstream::ByteReader;
use vaco_codec_core::CodecParameters;
use vaco_core::{Error, MediaType, Rational, Result, Rounding, Timestamp};
use vaco_format_riff::bitmapinfo::BitmapInfoHeader;
use vaco_format_riff::chunk::{ChunkIter, ids as riff_ids};
use vaco_format_riff::wave::WaveFormatEx;
use vaco_format_riff::{video_tags, wave_tags};
use vaco_limits::Budget;

/// `AVIF_HASINDEX` — an `idx1` chunk should be present.
pub(crate) const AVIF_HASINDEX: u32 = 0x0000_0010;

/// The `avih` payload, the fields this crate actually uses.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MainHeader {
    pub micro_sec_per_frame: u32,
    /// `dwFlags`. Consulted only for [`AVIF_HASINDEX`] — everything else
    /// (`AVIF_MUSTUSEINDEX`, `AVIF_ISINTERLEAVED`, …) is advisory and this
    /// crate's behaviour does not change on it.
    pub flags: u32,
    pub total_frames: u32,
}

/// Bytes in `AVIMAINHEADER`: ten named `DWORD`s plus `dwReserved[4]`.
pub(crate) const MAIN_HEADER_LEN: usize = 56;

impl MainHeader {
    pub(crate) fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < MAIN_HEADER_LEN {
            return Err(Error::InvalidData("avi: avih shorter than AVIMAINHEADER"));
        }
        let mut r = ByteReader::new(data);
        let micro_sec_per_frame = r.le32();
        let _max_bytes_per_sec = r.le32();
        let _padding_granularity = r.le32();
        let flags = r.le32();
        let total_frames = r.le32();
        let _initial_frames = r.le32();
        // `dwStreams` is not kept: the real stream count is
        // `parse_hdrl`'s own `streams.len()`, taken from the `strl`s that
        // actually parsed, which is the number that matters if the two ever
        // disagree.
        let _streams = r.le32();
        r.check()?;
        Ok(Self {
            micro_sec_per_frame,
            flags,
            total_frames,
        })
    }

    /// Whether `AVIF_HASINDEX` is set — an `idx1` chunk should follow `movi`.
    /// [`crate::demux::AviDemuxer::open_with_limits`] uses this only to skip
    /// the trailing scan entirely when nothing declares an index at all
    /// (neither this flag nor any stream's `OpenDML` `indx`): a writer that
    /// forgets the flag but writes `idx1` anyway is handled by the scan
    /// finding it regardless, since the flag never *prevents* looking, only
    /// skips looking when there is a second reason not to.
    #[must_use]
    pub(crate) const fn has_index(&self) -> bool {
        self.flags & AVIF_HASINDEX != 0
    }
}

/// The `strh` payload, the fields this crate actually uses.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct StreamHeader {
    pub fcc_type: [u8; 4],
    pub scale: u32,
    pub rate: u32,
    pub start: u32,
    pub length: u32,
    /// `0` means one chunk is one frame (video, and VBR audio); non-zero is
    /// the fixed byte count of one sample (CBR audio), and packet timestamps
    /// come from a running byte count divided by it rather than from a
    /// running chunk count. See [`crate::demux`]'s clock.
    pub sample_size: u32,
}

/// Bytes in the classic `AVISTREAMHEADER` prefix, up to and including
/// `dwSampleSize` — the part every writer this crate has seen agrees on.
const STREAM_HEADER_FIXED_LEN: usize = 48;

impl StreamHeader {
    pub(crate) fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < STREAM_HEADER_FIXED_LEN {
            return Err(Error::InvalidData("avi: strh shorter than AVISTREAMHEADER"));
        }
        let mut r = ByteReader::new(data);
        let fcc_type = [r.u8(), r.u8(), r.u8(), r.u8()];
        let _fcc_handler = r.le32();
        let _flags = r.le32();
        let _priority = r.le16();
        let _language = r.le16();
        let _initial_frames = r.le32();
        let scale = r.le32();
        let rate = r.le32();
        let start = r.le32();
        let length = r.le32();
        let _suggested_buffer_size = r.le32();
        let _quality = r.le32();
        let sample_size = r.le32();
        r.check()?;
        Ok(Self {
            fcc_type,
            scale,
            rate,
            start,
            length,
            sample_size,
        })
    }

    /// The stream's time base: `dwScale / dwRate`. Neither field is trusted —
    /// a zero `dwRate` (or `dwScale`) makes the base undefined, which the
    /// caller must reject rather than divide by.
    #[must_use]
    pub(crate) fn time_base(&self) -> Rational {
        Rational::new(self.scale.cast_signed(), self.rate.cast_signed())
    }
}

/// Everything [`crate::demux::AviDemuxer`] needs about one `strl`, beyond the
/// generic [`vaco_format_core::Stream`] it builds.
#[derive(Debug, Clone)]
pub(crate) struct StreamBuild {
    pub stream: vaco_format_core::Stream,
    pub sample_size: u32,
    pub start: u32,
    /// The raw `indx` (`AVISUPERINDEX`) payload, if this stream carries an
    /// `OpenDML` super-index. Parsed and resolved lazily by [`crate::index`],
    /// only when the source can seek — resolving it means seeking to every
    /// `ix##` chunk it names.
    pub super_index: Option<Vec<u8>>,
    /// One `sample_size == 0` chunk's duration, in `stream.time_base` ticks —
    /// see [`crate::demux`]'s clock for why this is not always `1`.
    pub native_ticks_per_chunk: i64,
}

/// The `avih` fields [`crate::demux::AviDemuxer::open_with_limits`] needs,
/// plus the parsed streams — the whole `LIST/hdrl` payload, resolved in one
/// pass.
pub(crate) struct Hdrl {
    pub main: MainHeader,
    pub streams: Vec<StreamBuild>,
}

/// Parse a `LIST/hdrl` chunk's children: one `avih`, then one `LIST/strl` per
/// stream.
///
/// `payload` is the `hdrl` list's contents *after* its own `hdrl` list-type
/// marker.
pub(crate) fn parse_hdrl(payload: &[u8], budget: &mut Budget) -> Result<Hdrl> {
    let mut main: Option<MainHeader> = None;
    let mut streams = Vec::new();
    for chunk in ChunkIter::new(payload, 0).flatten() {
        if chunk.id == ids::AVIH {
            main = Some(MainHeader::parse(chunk.payload)?);
            continue;
        }
        // A `strl` is a `LIST` chunk (ckID `"LIST"`) whose list-type — the
        // first four bytes of *its* payload, not its ckID — is `"strl"`. Every
        // other `LIST` this crate sees at this level (there should be none,
        // but a stray `JUNK`-adjacent one is not our business) is skipped.
        if chunk.id != riff_ids::LIST {
            continue;
        }
        let Some(list_type) = chunk.payload.first_chunk::<4>() else {
            continue;
        };
        if *list_type != ids::STRL.as_bytes() {
            continue;
        }
        let strl_payload = chunk.payload.get(4..).unwrap_or(&[]);
        let index = u32::try_from(streams.len()).unwrap_or(u32::MAX);
        streams.push(parse_strl(strl_payload, index, budget)?);
    }
    let main = main.ok_or(Error::InvalidData("avi: hdrl has no avih"))?;
    Ok(Hdrl { main, streams })
}

/// Parse one `LIST/strl` chunk's children into a [`StreamBuild`].
///
/// `payload` is the `strl` list's contents *after* its own `strl` list-type
/// marker (i.e. what [`vaco_format_riff::chunk::Chunk::children`] returns).
pub(crate) fn parse_strl(payload: &[u8], index: u32, budget: &mut Budget) -> Result<StreamBuild> {
    let children = ChunkIter::new(payload, 0);
    let mut strh: Option<StreamHeader> = None;
    let mut params: Option<CodecParameters> = None;
    let mut time_base_hint: Option<Rational> = None;
    let mut name: Option<String> = None;
    let mut super_index: Option<Vec<u8>> = None;

    for chunk in children.flatten() {
        if chunk.id == ids::STRH {
            strh = StreamHeader::parse(chunk.payload).ok();
        } else if chunk.id == ids::STRF {
            if let Some(h) = &strh {
                let (p, tb) = parse_strf(h.fcc_type, chunk.payload, budget)?;
                params = p;
                time_base_hint = tb;
            }
        } else if chunk.id == ids::STRN {
            name = Some(read_cstr(chunk.payload));
        } else if chunk.id == ids::INDX {
            // Bounded by `budget`, the same allocation ceiling everything
            // else in this crate goes through — a hostile `indx` claiming a
            // huge chunk cannot force an unbounded copy.
            let mut buf = budget.alloc::<u8>(chunk.payload.len())?;
            buf.copy_from_slice(chunk.payload);
            super_index = Some(buf);
        }
    }

    let strh = strh.ok_or(Error::InvalidData("avi: strl has no strh"))?;
    let media_type = match &strh.fcc_type {
        b"vids" => MediaType::Video,
        b"auds" => MediaType::Audio,
        b"txts" => MediaType::Subtitle,
        _ => MediaType::Data,
    };
    let time_base = time_base_hint
        .filter(|r| r.is_defined())
        .unwrap_or_else(|| strh.time_base());
    if !time_base.is_defined() || time_base.is_zero() {
        return Err(Error::InvalidData("avi: stream has an unusable time base"));
    }

    // `strh`'s own `dwScale`/`dwRate` is AVI's *chunk* clock: `strh.time_base()`
    // is how long one `sample_size == 0` chunk lasts, in seconds, whatever
    // `time_base` above ends up being. The two agree for video — nothing
    // above overrides `strh.time_base()` there, so this rescale is `1` by
    // construction — but audio's `time_base` was just overridden to the
    // format's own true sample rate, finer than `strh.time_base()` in every
    // real encode measured for this crate (`dwScale=256, dwRate=11025` —
    // `256/11025` seconds, i.e. exactly 1024 ticks of a `1/44100` `time_base`
    // — for one AAC frame in `av-src.avi`). Rounded rather than propagated
    // as a `Rational`, since [`crate::demux::AviDemuxer`]'s clock counts
    // whole ticks and every real encoder's own `dwScale`/`dwRate` divides
    // its format's sample rate evenly.
    let native_ticks_per_chunk = Timestamp::new(1)
        .rescale(strh.time_base(), time_base, Rounding::NearestAwayFromZero)
        .ticks()
        .unwrap_or(1)
        .max(1);

    // Measured: `ffprobe 8.1 -show_streams` prints `id=N/A` for every stream
    // in an AVI file. Unlike an MP4 track id or an MPEG-TS PID, there is no
    // container-level identifier independent of stream order here, so
    // `Stream::id` stays `None` (`Stream::new`'s default) rather than
    // aliasing it to the index.
    let mut stream = vaco_format_core::Stream::new(index, media_type, time_base);
    if strh.length > 0 {
        stream.set_duration_ts(i64::from(strh.length));
    }
    if let Some(p) = params {
        stream.params = p;
    } else {
        stream.params = CodecParameters::new(media_type);
    }
    if let Some(n) = name {
        stream.metadata_set("title", n);
    }

    Ok(StreamBuild {
        stream,
        sample_size: strh.sample_size,
        start: strh.start,
        super_index,
        native_ticks_per_chunk,
    })
}

/// `strf`'s payload, interpreted by the stream type its `strh` already named.
///
/// Returns an optional time-base override: audio's true time base is its
/// sample rate, not whatever `dwScale/dwRate` says (measured: ffmpeg's own
/// writer sets both to the sample rate too, but a reader should not depend on
/// that agreement holding for every writer in the wild).
fn parse_strf(
    fcc_type: [u8; 4],
    payload: &[u8],
    budget: &mut Budget,
) -> Result<(Option<CodecParameters>, Option<Rational>)> {
    match &fcc_type {
        b"vids" => {
            let bih = BitmapInfoHeader::parse(payload)?;
            let mut params = CodecParameters::video();
            if let Some(v) = &mut params.video {
                v.width = bih.width.unsigned_abs();
                v.height = bih.abs_height();
                v.coded_width = v.width;
                v.coded_height = v.height;
            }
            let compression = bih.compression();
            params.codec_id = video_tags::codec_id(compression);
            if let vaco_format_riff::bitmapinfo::Compression::FourCc(id) = compression {
                params.codec_tag = Some(id.as_bytes());
            }
            // Mirrors the audio branch below and `vaco-demux-mp4`'s own
            // `avcC`/`hvcC` handling: an ISOBMFF-style `avc1`/`hvc1`-tagged
            // `strf` carries a configuration record after
            // `BITMAPINFOHEADER`, and the MPEG-4 Part 2/MS-MPEG4 family
            // carries its own VOL-style header the same way (measured: a
            // real `FMP4`-tagged AVI file's `strf` has 46 trailing bytes
            // there, matching real ffmpeg's own reported `extradata_size`
            // for the identical file exactly — this crate's own
            // `carries_config_record`-only gate silently dropped them,
            // since MPEG-4 does not use the ISOBMFF configuration-record
            // convention that check was written for). Handing it to
            // `stream.params.extradata` is what lets the codec parser
            // (reached through `ParserProvider`, once a packet arrives) fill
            // in profile/level/pix_fmt/nal_length_size — this crate never
            // parses the record itself.
            if video_tags::carries_strf_extradata(compression) {
                let extra = payload.get(BitmapInfoHeader::LEN..).unwrap_or(&[]);
                if !extra.is_empty() {
                    let mut buf = budget.alloc::<u8>(extra.len())?;
                    buf.copy_from_slice(extra);
                    params.extradata = Some(buf);
                }
            }
            Ok((Some(params), None))
        }
        b"auds" => {
            let mut wfx = WaveFormatEx::parse(payload, budget)?;
            let mut params = CodecParameters::audio();
            if let Some(a) = &mut params.audio {
                a.sample_rate = wfx.samples_per_sec;
                a.bits_per_coded_sample = u8::try_from(wfx.bits_per_sample).ok();
            }
            params.codec_id = wave_tags::codec_id(&wfx);
            params.codec_tag = Some(wfx.format_tag.to_le_bytes_as_tag());
            let rate = i32::try_from(wfx.samples_per_sec).unwrap_or(0);
            let tb = (rate > 0).then(|| Rational::new(1, rate));
            // A compressed codec's `strf` extension bytes (MS-ADPCM
            // coefficients, AAC's `AudioSpecificConfig` when carried this
            // way) are the extradata, mirroring `vaco-demux-mp4`'s
            // `esds`/`DecoderSpecificInfo` convention.
            if !wfx.extra.is_empty() {
                params.extradata = Some(core::mem::take(&mut wfx.extra));
            }
            Ok((Some(params), tb))
        }
        _ => Ok((None, None)),
    }
}

/// A `wFormatTag` reinterpreted as a four-byte "tag" the way ffprobe's
/// `codec_tag` column prints it for other RIFF-derived tags: the tag padded
/// to four bytes little-endian, matching D9's minimum-width-four rule for
/// `codec_tag` (a two-byte tag is not printed as a two-byte one).
trait FormatTagBytes {
    fn to_le_bytes_as_tag(self) -> [u8; 4];
}
impl FormatTagBytes for u16 {
    fn to_le_bytes_as_tag(self) -> [u8; 4] {
        let b = self.to_le_bytes();
        [b[0], b[1], 0, 0]
    }
}

/// A NUL-terminated (or NUL-padded) `strn`/`ISFT`-style RIFF string.
fn read_cstr(data: &[u8]) -> String {
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    String::from_utf8_lossy(data.get(..end).unwrap_or(&[])).into_owned()
}

/// Extra `ChunkId`s this crate names beyond what `vaco-format-riff::chunk::ids`
/// already does — `strh`/`strf`/`strl`/`hdrl`/`movi` are AVI-only concepts the
/// shared RIFF crate has no reason to know about.
pub(crate) mod ids {
    use vaco_format_riff::chunk::ChunkId;

    macro_rules! ids {
        ($($name:ident = $lit:literal),* $(,)?) => {
            $(pub(crate) const $name: ChunkId = ChunkId::new($lit);)*
        };
    }
    ids! {
        HDRL = b"hdrl", MOVI = b"movi", AVIH = b"avih", STRL = b"strl",
        STRH = b"strh", STRF = b"strf", STRN = b"strn",
        INDX = b"indx", IDX1 = b"idx1", REC_ = b"rec ",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn avih_bytes(flags: u32, streams: u32, total_frames: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&100_000u32.to_le_bytes()); // micro_sec_per_frame
        out.extend_from_slice(&0u32.to_le_bytes()); // max_bytes_per_sec
        out.extend_from_slice(&0u32.to_le_bytes()); // padding
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&total_frames.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // initial frames
        out.extend_from_slice(&streams.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // suggested buffer
        out.extend_from_slice(&64u32.to_le_bytes()); // width
        out.extend_from_slice(&48u32.to_le_bytes()); // height
        out.extend_from_slice(&[0; 16]); // reserved[4]
        out
    }

    #[test]
    fn avih_round_trips_the_measured_layout() {
        let data = avih_bytes(AVIF_HASINDEX, 2, 10);
        let h = MainHeader::parse(&data).unwrap();
        assert_eq!(h.total_frames, 10);
        assert_eq!(h.flags & AVIF_HASINDEX, AVIF_HASINDEX);
    }

    #[test]
    fn avih_shorter_than_fifty_six_bytes_is_rejected() {
        assert!(MainHeader::parse(&[0; 55]).is_err());
    }

    fn strh_bytes(
        fcc_type: [u8; 4],
        scale: u32,
        rate: u32,
        length: u32,
        sample_size: u32,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&fcc_type);
        out.extend_from_slice(b"FMP4"); // fcc_handler
        out.extend_from_slice(&0u32.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // priority
        out.extend_from_slice(&0u16.to_le_bytes()); // language
        out.extend_from_slice(&0u32.to_le_bytes()); // initial frames
        out.extend_from_slice(&scale.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // start
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // suggested buffer
        out.extend_from_slice(&0u32.to_le_bytes()); // quality
        out.extend_from_slice(&sample_size.to_le_bytes());
        out.extend_from_slice(&[0; 8]); // rcFrame
        out
    }

    #[test]
    fn strh_round_trips_the_measured_layout() {
        let data = strh_bytes(*b"vids", 1, 10, 10, 0);
        let h = StreamHeader::parse(&data).unwrap();
        assert_eq!(&h.fcc_type, b"vids");
        assert_eq!(h.time_base(), Rational::new(1, 10));
        assert_eq!(h.length, 10);
        assert_eq!(h.sample_size, 0);
    }

    #[test]
    fn strh_accepts_the_forty_eight_byte_prefix_with_no_rcframe() {
        let mut data = strh_bytes(*b"auds", 1, 8000, 8000, 2);
        data.truncate(48);
        let h = StreamHeader::parse(&data).unwrap();
        assert_eq!(h.sample_size, 2);
    }

    #[test]
    fn strh_shorter_than_the_fixed_prefix_is_rejected() {
        assert!(StreamHeader::parse(&[0; 40]).is_err());
    }

    #[test]
    fn cstr_reads_up_to_the_first_nul() {
        assert_eq!(read_cstr(b"hi\x00\x00"), "hi");
        assert_eq!(read_cstr(b"nonul"), "nonul");
    }

    fn bih_bytes(fourcc: [u8; 4], trailing: &[u8]) -> Vec<u8> {
        let mut bih = Vec::new();
        bih.extend_from_slice(&(40 + trailing.len() as u32).to_le_bytes());
        bih.extend_from_slice(&64i32.to_le_bytes());
        bih.extend_from_slice(&48i32.to_le_bytes());
        bih.extend_from_slice(&1u16.to_le_bytes());
        bih.extend_from_slice(&24u16.to_le_bytes());
        bih.extend_from_slice(&fourcc);
        bih.extend_from_slice(&[0; 20]);
        bih.extend_from_slice(trailing);
        bih
    }

    #[test]
    fn avc1_strf_captures_the_trailing_avcc_as_extradata() {
        let avcc = [0x01, 0x64, 0x00, 0x0A, 0xFF];
        let bih = bih_bytes(*b"avc1", &avcc);
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let (params, tb) = parse_strf(*b"vids", &bih, &mut budget).unwrap();
        assert!(tb.is_none());
        let params = params.unwrap();
        assert_eq!(params.extradata.as_deref(), Some(&avcc[..]));
    }

    #[test]
    fn fmp4_strf_captures_the_trailing_vol_header_as_extradata() {
        // The bug this test exists to catch: a real `FMP4`-tagged AVI
        // file's `strf` carries its VOL header here (measured: 46 bytes,
        // matching real ffmpeg's own `extradata_size` on the identical
        // file exactly), and `carries_config_record` alone -- written for
        // `avc1`/`hvc1`'s ISOBMFF convention -- does not cover it, so this
        // was silently dropped before `carries_strf_extradata` existed.
        let vol = [0x00, 0x00, 0x01, 0xB0, 0x01, 0x00, 0x00, 0x01, 0xB5];
        let bih = bih_bytes(*b"FMP4", &vol);
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let (params, tb) = parse_strf(*b"vids", &bih, &mut budget).unwrap();
        assert!(tb.is_none());
        let params = params.unwrap();
        assert_eq!(params.extradata.as_deref(), Some(&vol[..]));
    }

    #[test]
    fn xvid_strf_also_captures_extradata_same_family_as_fmp4() {
        let vol = [0xAA, 0xBB, 0xCC];
        let bih = bih_bytes(*b"XVID", &vol);
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let (params, _tb) = parse_strf(*b"vids", &bih, &mut budget).unwrap();
        assert_eq!(params.unwrap().extradata.as_deref(), Some(&vol[..]));
    }

    #[test]
    fn h264_strf_has_no_config_record_to_capture() {
        // `H264`-tagged `strf` carries Annex B in-band; any trailing bytes
        // are not a configuration record and must not be read as one.
        let bih = bih_bytes(*b"H264", &[0xAA, 0xBB]);
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let (params, _tb) = parse_strf(*b"vids", &bih, &mut budget).unwrap();
        assert_eq!(params.unwrap().extradata, None);
    }

    #[test]
    fn avc1_strf_with_no_trailing_bytes_leaves_extradata_unset() {
        let bih = bih_bytes(*b"avc1", &[]);
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let (params, _tb) = parse_strf(*b"vids", &bih, &mut budget).unwrap();
        assert_eq!(params.unwrap().extradata, None);
    }

    #[test]
    fn parse_strl_builds_a_video_stream() {
        let strh = strh_bytes(*b"vids", 1, 10, 10, 0);
        let mut strh_chunk = b"strh".to_vec();
        strh_chunk.extend_from_slice(&(strh.len() as u32).to_le_bytes());
        strh_chunk.extend_from_slice(&strh);

        let mut bih = Vec::new();
        bih.extend_from_slice(&40u32.to_le_bytes());
        bih.extend_from_slice(&64i32.to_le_bytes());
        bih.extend_from_slice(&48i32.to_le_bytes());
        bih.extend_from_slice(&1u16.to_le_bytes());
        bih.extend_from_slice(&24u16.to_le_bytes());
        bih.extend_from_slice(b"FMP4");
        bih.extend_from_slice(&[0; 20]);
        let mut strf_chunk = b"strf".to_vec();
        strf_chunk.extend_from_slice(&(bih.len() as u32).to_le_bytes());
        strf_chunk.extend_from_slice(&bih);

        let mut payload = strh_chunk;
        payload.extend_from_slice(&strf_chunk);

        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let build = parse_strl(&payload, 0, &mut budget).unwrap();
        assert_eq!(build.stream.media_type(), Some(MediaType::Video));
        assert_eq!(build.stream.params.video.as_ref().unwrap().width, 64);
        assert_eq!(build.stream.params.video.as_ref().unwrap().height, 48);
    }
}
