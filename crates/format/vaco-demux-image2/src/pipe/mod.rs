//! The 37 `*_pipe` splitters: one codec's worth of image framing each, over
//! a single already-open [`vaco_io::MediaSource`].
//!
//! # Why these are separate from [`crate::multi::Image2Demuxer`]
//!
//! `image2` (the pattern/glob demuxer in [`crate::multi`]) opens *files by
//! name*; these demuxers frame a *byte stream* that may already hold several
//! concatenated images (`cat a.png b.png c.png | ffmpeg -f png_pipe -i -`).
//! They are registered as their own [`vaco_format_core::DemuxerDesc`]s so
//! `-f png_pipe` (and content probing) can select one directly, exactly as
//! the reference does — `png_pipe` and `image2` are different demuxers there
//! too, not one demuxer with a mode flag.
//!
//! # Count
//!
//! `ffmpeg -demuxers | awk '{print $2}' | grep -E '_pipe$'` on ffmpeg 8.1
//! lists **37**, not the 42 `planning/20-roadmap.md` names — a roadmap count
//! that was wrong, per `planning/AGENT-CONSTRAINTS.md`'s own warning that
//! this has happened before. `image2pipe` and `yuv4mpegpipe` also match
//! `*pipe` but are not per-codec splitters (`yuv4mpegpipe` lives in
//! `vaco-demux-raw`, matching the reference's own module boundary; this
//! crate does not register `image2pipe` at all — see the crate docs).
//!
//! # How to add a splitter
//!
//! Pick a [`framing::ImageFraming`] strategy (or add one), then one line in
//! [`PIPE_DEMUXERS`] via the `pipe!` macro. If the format's boundary needs a
//! byte-counting algorithm this module does not have yet, add it to
//! [`framing`] first — the framing lives here, in the format layer, never in
//! a decoder.

pub mod framing;

use framing::{ImageFraming, Span};
use vaco_codec_core::CodecId;
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{
    Demuxer, DemuxerDesc, FormatFlags, ParserProvider, SeekFlags, SeekTarget, Stream,
};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

/// Largest input a pipe splitter will buffer whole, on top of the caller's
/// own [`Limits`] (which are charged first and usually bind first). Mirrors
/// `vaco-demux-raw::bitstream`'s `MAX_BUFFERED` for the same reason: this
/// demuxer computes its whole packet table up front rather than streaming.
const MAX_BUFFERED: u64 = 512 << 20;

/// Reference default for every pipe splitter's `-framerate`, measured via
/// `ffmpeg -h demuxer=png_pipe` (and cross-checked on `jpeg_pipe`): both
/// print `(default "25")`.
const DEFAULT_FRAMERATE: Rational = Rational::new(25, 1);

/// One content-signature requirement: `data[offset..offset+bytes.len()]`
/// must equal `bytes`. A [`PipeSpec::magic_sets`] alternative is every
/// element of its inner slice matching (AND); the outer slice is
/// alternatives (OR) — needed for WebP, whose signature is `"RIFF"` at 0
/// *and* `"WEBP"` at 8, and for TIFF/GIF, which have two valid magics.
pub type MagicPart = (usize, &'static [u8]);

/// Static description of one pipe splitter.
#[derive(Debug, Clone, Copy)]
pub struct PipeSpec {
    pub name: &'static str,
    pub long_name: &'static str,
    pub extensions: &'static [&'static str],
    pub framing: ImageFraming,
    /// `None` when `vaco_codec_core::CodecId` has no variant for this format
    /// yet — see [`vaco_demux_raw`](../../vaco_demux_raw/index.html)'s crate
    /// docs for the same convention. [`PipeSpec::raw_codec_name`] carries the
    /// reference's exact name in that case.
    pub codec_id: Option<CodecId>,
    pub raw_codec_name: &'static str,
    /// Empty means "no reliable content signature": the registry falls back
    /// to extension matching only. See `docs/format/vaco-demux-image2.md`
    /// for which of the 37 that applies to.
    pub magic_sets: &'static [&'static [MagicPart]],
}

fn pipe_probe(spec: &PipeSpec, data: &ProbeData<'_>) -> ProbeScore {
    for set in spec.magic_sets {
        if set.iter().all(|&(off, bytes)| data.matches_at(off, bytes)) {
            return ProbeScore::MAX;
        }
    }
    if spec.magic_sets.is_empty() && data.extension_matches(spec.extensions) {
        return ProbeScore::EXTENSION;
    }
    ProbeScore::NONE
}

/// Options a pipe splitter reads directly, for a caller holding one rather
/// than going through the registry's frozen `open` (which — like every
/// options-driven format in `vaco-demux-raw` — has no options parameter, so
/// the registry path always gets these defaults).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PipeOptions {
    pub framerate: Rational,
    /// `-loop`: once the last framed image is read, start again from the
    /// first rather than reporting end of stream.
    pub loop_input: bool,
}

impl Default for PipeOptions {
    fn default() -> Self {
        Self {
            framerate: DEFAULT_FRAMERATE,
            loop_input: false,
        }
    }
}

/// The pipe splitter: buffers the whole input once, frames it into spans per
/// [`PipeSpec::framing`], and reads them back out as packets.
#[derive(Debug)]
pub struct PipeDemuxer {
    data: Vec<u8>,
    spans: Vec<Span>,
    next: usize,
    loops_done: u64,
    budget: Budget,
    options: PipeOptions,
    stream: Stream,
    stride_ticks: i64,
}

impl PipeDemuxer {
    /// # Errors
    /// I/O failure, or the input exceeds [`MAX_BUFFERED`] or the caller's own
    /// budget.
    pub fn open(spec: &PipeSpec, src: Box<dyn MediaSource>) -> Result<Self> {
        Self::open_with_options(spec, src, PipeOptions::default())
    }

    /// # Errors
    /// As [`PipeDemuxer::open`].
    pub fn open_with_options(
        spec: &PipeSpec,
        src: Box<dyn MediaSource>,
        options: PipeOptions,
    ) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let mut budget = Budget::new(Limits::permissive());
        let data = read_all(&mut io, &mut budget)?;
        let spans = framing::compute_spans(spec.framing, &data);

        let mut stream = Stream::new(0, MediaType::Video, crate::multi::time_base_for(options.framerate));
        stream.params.media_type = Some(MediaType::Video);
        stream.params.codec_id = spec.codec_id;
        stream.params.video = Some(crate::multi::stream_video(options.framerate));
        if spec.codec_id.is_none() {
            stream.metadata_set("raw_codec_name", spec.raw_codec_name);
        }

        // Exactly one tick of `stream.time_base` (`1/framerate`, by
        // construction) — i.e. one frame period — rather than a
        // `duration_from_rate` value in a different, fixed base. Used only
        // for `seek`'s timestamp-to-frame-index arithmetic below:
        // `read_packet` states no real timeline (see its own docs), so this
        // never reaches a displayed duration.
        let stride_ticks: i64 = 1;

        Ok(Self {
            data,
            spans,
            next: 0,
            loops_done: 0,
            budget,
            options,
            stream,
            stride_ticks,
        })
    }
}

impl Demuxer for PipeDemuxer {
    fn streams(&self) -> &[Stream] {
        std::slice::from_ref(&self.stream)
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if self.next >= self.spans.len() {
            if self.options.loop_input && !self.spans.is_empty() {
                self.next = 0;
                self.loops_done = self.loops_done.saturating_add(1);
            } else {
                return Err(Error::Eof);
            }
        }
        let index = self.next;
        let (start, end) = self.spans.get(index).copied().ok_or(Error::Eof)?;
        let slice = self.data.get(start..end).ok_or(Error::Eof)?;
        let mut packet = Packet::from_slice(&mut self.budget, slice)?;
        // No timeline at all, single image or concatenated many — measured
        // directly, `ffprobe -f png_pipe` on three concatenated PNGs reports
        // `start_time`/`duration` as unset exactly as it does for one, unlike
        // `image2`'s own file-pattern path (`crate::multi`), which is a real
        // sequence a caller named on purpose. A byte stream this crate merely
        // *split* states no playback rate of its own.
        packet.pts = Timestamp::NONE;
        packet.dts = Timestamp::NONE;
        packet.duration = Duration::ZERO;
        packet.pos = Some(start as u64);
        packet.flags = PacketFlags::KEY;
        self.next += 1;
        Ok(packet)
    }

    fn seek(&mut self, target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        // Every span is already known and independent (a decoder keyframe),
        // so a frame-number seek is exact; nothing else is supported.
        match target {
            SeekTarget::Frame { frame, .. } => {
                let idx = usize::try_from(frame).map_err(|_| Error::NotSeekable)?;
                if idx >= self.spans.len() {
                    return Err(Error::NotSeekable);
                }
                self.next = idx;
                self.loops_done = 0;
                Ok(())
            }
            SeekTarget::Timestamp { ts, .. } => {
                let Some(ticks) = ts.ticks() else {
                    return Err(Error::NotSeekable);
                };
                if self.stride_ticks <= 0 {
                    return Err(Error::NotSeekable);
                }
                #[allow(
                    clippy::integer_division,
                    reason = "deliberately floors a timestamp to the frame index that contains it"
                )]
                let frame_index = ticks / self.stride_ticks;
                let idx = frame_index.max(0) as usize;
                if idx >= self.spans.len() {
                    return Err(Error::NotSeekable);
                }
                self.next = idx;
                self.loops_done = 0;
                Ok(())
            }
            SeekTarget::Byte(_) => Err(Error::NotSeekable),
        }
    }

    // No override: the default `None` is correct here, matching
    // `read_packet`'s "no timeline" packets — see that method's docs. This
    // used to derive a duration from `stride_ticks * span_count`, which is
    // exactly the container-level input `estimate_duration` prefers, and fed
    // a `duration`/`bit_rate` the reference never states for a `_pipe`
    // format.
}

/// Charge-and-collect the whole remaining input, bounded by `budget`. Mirrors
/// `vaco-demux-raw::bitstream::read_all`; duplicated rather than shared
/// because the two crates do not depend on each other (D14.1 is about
/// layering, not code reuse, but there is no format-layer "utils" crate to
/// put this in, and inventing one for fifteen lines is not worth the extra
/// registered crate).
fn read_all(io: &mut IoContext, budget: &mut Budget) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut chunk = budget.alloc::<u8>(64 * 1024)?;
    loop {
        let n = io.read_partial(&mut chunk)?;
        if n == 0 {
            break;
        }
        let Some(taken) = chunk.get(..n) else {
            return Err(Error::InvalidData(
                "short read reported more bytes than taken",
            ));
        };
        budget.charge(taken.len() as u64)?;
        if out.len() as u64 + taken.len() as u64 > MAX_BUFFERED {
            return Err(Error::LimitExceeded {
                limit: "image2_pipe_buffer",
                requested: out.len() as u64 + taken.len() as u64,
                cap: MAX_BUFFERED,
            });
        }
        out.extend_from_slice(taken);
    }
    Ok(out)
}

/// Declare one `*_pipe` [`DemuxerDesc`] plus its [`PipeSpec`].
///
/// Both the registered name and the long name are **derived from `base`**:
/// every one of the 37 lines in `ffmpeg -demuxers` is `<base>_pipe` with the
/// long name `"piped <base> sequence"`, so neither is typed 37 times.
///
/// The name used to be a separate `name = "..."` argument, and nine of the 37
/// were written without the `_pipe` suffix — so `bmp_pipe` in the
/// `vaco-component.toml` fragment pointed at a descriptor calling itself
/// `bmp`, which broke `vaco-registry`'s cross-check. Deriving it means the two
/// cannot disagree, which is a better answer than fixing nine of them.
macro_rules! pipe {
    (
        $desc:ident, $spec:ident,
        base = $base:literal,
        extensions = $exts:expr,
        framing = $framing:expr,
        codec = $codec:expr,
        raw_name = $raw:literal,
        magics = $magics:expr $(,)?
    ) => {
        #[doc = concat!("`", $base, "_pipe`: piped ", $base, " sequence.")]
        pub const $spec: PipeSpec = PipeSpec {
            name: concat!($base, "_pipe"),
            long_name: concat!("piped ", $base, " sequence"),
            extensions: $exts,
            framing: $framing,
            codec_id: $codec,
            raw_codec_name: $raw,
            magic_sets: $magics,
        };

        #[doc = concat!("Registry entry for [`", stringify!($spec), "`].")]
        pub const $desc: DemuxerDesc = DemuxerDesc {
            name: $spec.name,
            long_name: $spec.long_name,
            extensions: $spec.extensions,
            mime_types: &[],
            // Not `empty()`. Every packet is a whole image and therefore a keyframe, the
            // timestamps are synthesised from `-framerate` rather than read from the
            // stream, and the only exact seek is by frame number — so all three
            // timestamp-search strategies are inapplicable and say so, rather than
            // being left unstated. `empty()` would *suppress* nothing and *express*
            // nothing: it reads as "the field was forgotten", which is exactly what
            // `vaco-probe`'s `every_registered_demuxer_declares_flags` is looking for.
            //
            // `NOTIMESTAMPS` is deliberately absent: this demuxer does stamp its
            // packets. They are derived rather than carried, which is a different
            // thing from having none.
            flags: FormatFlags::NOBINSEARCH
                .union(FormatFlags::NOGENSEARCH)
                .union(FormatFlags::NO_BYTE_SEEK),
            probe: |data: &ProbeData<'_>| pipe_probe(&$spec, data),
            open: |src: Box<dyn MediaSource>, parsers: &dyn ParserProvider| {
                let _ = parsers;
                Ok(Box::new(PipeDemuxer::open(&$spec, src)?) as Box<dyn Demuxer>)
            },
        };
    };
}

const JPEG_MARKER: ImageFraming = ImageFraming::Marker {
    start: [0xFF, 0xD8],
    end: [0xFF, 0xD9],
    skip_stuffing: true,
};
const J2K_MARKER: ImageFraming = ImageFraming::Marker {
    start: [0xFF, 0x4F],
    end: [0xFF, 0xD9],
    skip_stuffing: false,
};

pipe!(
    DEMUXER_BMP,
    SPEC_BMP,
    base = "bmp",
    extensions = &["bmp"],
    framing = ImageFraming::BmpSized,
    codec = Some(CodecId::Bmp),
    raw_name = "bmp",
    magics = &[&[(0, b"BM")]]
);

pipe!(
    DEMUXER_CRI,
    SPEC_CRI,
    base = "cri",
    extensions = &["cri"],
    framing = ImageFraming::WholeRemaining,
    codec = None,
    raw_name = "cri",
    magics = &[]
);

pipe!(
    DEMUXER_DDS,
    SPEC_DDS,
    base = "dds",
    extensions = &["dds"],
    framing = ImageFraming::WholeRemaining,
    codec = None,
    raw_name = "dds",
    magics = &[&[(0, b"DDS ")]]
);

pipe!(
    DEMUXER_DPX,
    SPEC_DPX,
    base = "dpx",
    extensions = &["dpx"],
    framing = ImageFraming::WholeRemaining,
    codec = None,
    raw_name = "dpx",
    magics = &[&[(0, b"SDPX")], &[(0, b"XPDS")]]
);

pipe!(
    DEMUXER_EXR,
    SPEC_EXR,
    base = "exr",
    extensions = &["exr"],
    framing = ImageFraming::WholeRemaining,
    codec = None,
    raw_name = "exr",
    magics = &[&[(0, &[0x76, 0x2f, 0x31, 0x01])]]
);

pipe!(
    DEMUXER_GEM,
    SPEC_GEM,
    base = "gem",
    extensions = &["gem"],
    framing = ImageFraming::WholeRemaining,
    codec = None,
    raw_name = "gem",
    magics = &[]
);

pipe!(
    DEMUXER_GIF,
    SPEC_GIF,
    base = "gif",
    extensions = &["gif"],
    framing = ImageFraming::WholeRemaining,
    codec = Some(CodecId::Gif),
    raw_name = "gif",
    magics = &[&[(0, b"GIF87a")], &[(0, b"GIF89a")]]
);

pipe!(
    DEMUXER_HDR,
    SPEC_HDR,
    base = "hdr",
    extensions = &["hdr"],
    framing = ImageFraming::Radiance,
    codec = None,
    raw_name = "hdr",
    magics = &[&[(0, b"#?RADIANCE")], &[(0, b"#?RGBE")]]
);

pipe!(
    DEMUXER_J2K,
    SPEC_J2K,
    base = "j2k",
    extensions = &["j2k"],
    framing = J2K_MARKER,
    codec = None,
    raw_name = "j2k",
    magics = &[&[(0, &[0xFF, 0x4F])]]
);

pipe!(
    DEMUXER_JPEG,
    SPEC_JPEG,
    base = "jpeg",
    extensions = &["jpg", "jpeg"],
    framing = JPEG_MARKER,
    codec = Some(CodecId::Jpeg),
    raw_name = "mjpeg",
    magics = &[&[(0, &[0xFF, 0xD8])]]
);

pipe!(
    DEMUXER_JPEGLS,
    SPEC_JPEGLS,
    base = "jpegls",
    extensions = &["jls"],
    framing = JPEG_MARKER,
    codec = Some(CodecId::JpegLs),
    raw_name = "jpegls",
    magics = &[&[(0, &[0xFF, 0xD8])]]
);

pipe!(
    DEMUXER_JPEGXL,
    SPEC_JPEGXL,
    base = "jpegxl",
    extensions = &["jxl"],
    framing = ImageFraming::WholeRemaining,
    codec = None,
    raw_name = "jpegxl",
    magics = &[
        &[(0, &[0xFF, 0x0A])],
        &[(
            0,
            &[
                0x00, 0x00, 0x00, 0x0C, 0x4A, 0x58, 0x4C, 0x20, 0x0D, 0x0A, 0x87, 0x0A
            ]
        )]
    ]
);

pipe!(
    DEMUXER_JPEGXS,
    SPEC_JPEGXS,
    base = "jpegxs",
    extensions = &["jxs"],
    framing = ImageFraming::WholeRemaining,
    codec = None,
    raw_name = "jpegxs",
    magics = &[]
);

pipe!(
    DEMUXER_PAM,
    SPEC_PAM,
    base = "pam",
    extensions = &["pam"],
    framing = ImageFraming::Netpbm,
    codec = Some(CodecId::Pam),
    raw_name = "pam",
    magics = &[&[(0, b"P7")]]
);

pipe!(
    DEMUXER_PBM,
    SPEC_PBM,
    base = "pbm",
    extensions = &["pbm"],
    framing = ImageFraming::Netpbm,
    codec = Some(CodecId::Pbm),
    raw_name = "pbm",
    magics = &[&[(0, b"P4")], &[(0, b"P1")]]
);

pipe!(
    DEMUXER_PCX,
    SPEC_PCX,
    base = "pcx",
    extensions = &["pcx"],
    framing = ImageFraming::WholeRemaining,
    codec = Some(CodecId::Pcx),
    raw_name = "pcx",
    magics = &[&[(0, &[0x0A])]]
);

pipe!(
    DEMUXER_PFM,
    SPEC_PFM,
    base = "pfm",
    extensions = &["pfm"],
    framing = ImageFraming::Netpbm,
    codec = Some(CodecId::Pfm),
    raw_name = "pfm",
    magics = &[&[(0, b"PF")], &[(0, b"Pf")]]
);

pipe!(
    DEMUXER_PGM,
    SPEC_PGM,
    base = "pgm",
    extensions = &["pgm"],
    framing = ImageFraming::Netpbm,
    codec = Some(CodecId::Pgm),
    raw_name = "pgm",
    magics = &[&[(0, b"P5")], &[(0, b"P2")]]
);

pipe!(
    DEMUXER_PGMYUV,
    SPEC_PGMYUV,
    base = "pgmyuv",
    extensions = &["pgmyuv"],
    framing = ImageFraming::Netpbm,
    codec = None,
    raw_name = "pgmyuv",
    // Byte-identical to pgm's own P5 header (see `pipe::netpbm`'s docs);
    // no content signature distinguishes them, so this relies on extension.
    magics = &[]
);

pipe!(
    DEMUXER_PGX,
    SPEC_PGX,
    base = "pgx",
    extensions = &["pgx"],
    framing = ImageFraming::Pgx,
    codec = None,
    raw_name = "pgx",
    magics = &[&[(0, b"PG")]]
);

pipe!(
    DEMUXER_PHM,
    SPEC_PHM,
    base = "phm",
    extensions = &["phm"],
    framing = ImageFraming::Netpbm,
    codec = Some(CodecId::Phm),
    raw_name = "phm",
    magics = &[&[(0, b"PH")], &[(0, b"Ph")]]
);

pipe!(
    DEMUXER_PHOTOCD,
    SPEC_PHOTOCD,
    base = "photocd",
    extensions = &["pcd"],
    framing = ImageFraming::WholeRemaining,
    codec = None,
    raw_name = "photocd",
    magics = &[]
);

pipe!(
    DEMUXER_PICTOR,
    SPEC_PICTOR,
    base = "pictor",
    extensions = &["pic"],
    framing = ImageFraming::WholeRemaining,
    codec = None,
    raw_name = "pictor",
    magics = &[]
);

pipe!(
    DEMUXER_PNG,
    SPEC_PNG,
    base = "png",
    extensions = &["png"],
    framing = ImageFraming::Png,
    codec = Some(CodecId::Png),
    raw_name = "png",
    magics = &[&[(0, &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A])]]
);

pipe!(
    DEMUXER_PPM,
    SPEC_PPM,
    base = "ppm",
    extensions = &["ppm"],
    framing = ImageFraming::Netpbm,
    codec = Some(CodecId::Ppm),
    raw_name = "ppm",
    magics = &[&[(0, b"P6")], &[(0, b"P3")]]
);

pipe!(
    DEMUXER_PSD,
    SPEC_PSD,
    base = "psd",
    extensions = &["psd"],
    framing = ImageFraming::WholeRemaining,
    codec = None,
    raw_name = "psd",
    magics = &[&[(0, b"8BPS")]]
);

pipe!(
    DEMUXER_QDRAW,
    SPEC_QDRAW,
    base = "qdraw",
    extensions = &["pict", "pct"],
    framing = ImageFraming::WholeRemaining,
    codec = None,
    raw_name = "qdraw",
    magics = &[]
);

pipe!(
    DEMUXER_QOI,
    SPEC_QOI,
    base = "qoi",
    extensions = &["qoi"],
    framing = ImageFraming::Qoi,
    codec = Some(CodecId::Qoi),
    raw_name = "qoi",
    magics = &[&[(0, b"qoif")]]
);

pipe!(
    DEMUXER_SGI,
    SPEC_SGI,
    base = "sgi",
    extensions = &["sgi"],
    framing = ImageFraming::WholeRemaining,
    codec = Some(CodecId::Sgi),
    raw_name = "sgi",
    magics = &[&[(0, &[0x01, 0xDA])]]
);

pipe!(
    DEMUXER_SUNRAST,
    SPEC_SUNRAST,
    base = "sunrast",
    extensions = &["sun", "ras"],
    framing = ImageFraming::WholeRemaining,
    codec = None,
    raw_name = "sunrast",
    magics = &[&[(0, &[0x59, 0xA6, 0x6A, 0x95])]]
);

pipe!(
    DEMUXER_SVG,
    SPEC_SVG,
    base = "svg",
    extensions = &["svg"],
    framing = ImageFraming::SvgText,
    codec = None,
    raw_name = "svg",
    magics = &[]
);

pipe!(
    DEMUXER_TIFF,
    SPEC_TIFF,
    base = "tiff",
    extensions = &["tiff", "tif"],
    framing = ImageFraming::WholeRemaining,
    codec = Some(CodecId::Tiff),
    raw_name = "tiff",
    magics = &[&[(0, b"II*\0")], &[(0, b"MM\0*")]]
);

pipe!(
    DEMUXER_VBN,
    SPEC_VBN,
    base = "vbn",
    extensions = &["vbn"],
    framing = ImageFraming::WholeRemaining,
    codec = None,
    raw_name = "vbn",
    magics = &[]
);

pipe!(
    DEMUXER_WEBP,
    SPEC_WEBP,
    base = "webp",
    extensions = &["webp"],
    framing = ImageFraming::RiffSized,
    codec = Some(CodecId::Webp),
    raw_name = "webp",
    magics = &[&[(0, b"RIFF"), (8, b"WEBP")]]
);

pipe!(
    DEMUXER_XBM,
    SPEC_XBM,
    base = "xbm",
    extensions = &["xbm"],
    framing = ImageFraming::CArrayText,
    codec = Some(CodecId::Xbm),
    raw_name = "xbm",
    magics = &[]
);

pipe!(
    DEMUXER_XPM,
    SPEC_XPM,
    base = "xpm",
    extensions = &["xpm"],
    framing = ImageFraming::CArrayText,
    codec = None,
    raw_name = "xpm",
    magics = &[&[(0, b"/* XPM */")]]
);

pipe!(
    DEMUXER_XWD,
    SPEC_XWD,
    base = "xwd",
    extensions = &["xwd"],
    framing = ImageFraming::Xwd,
    codec = Some(CodecId::Xwd),
    raw_name = "xwd",
    magics = &[]
);

/// Every pipe splitter this crate registers, in `ffmpeg -demuxers`' own
/// alphabetical order.
pub const PIPE_DEMUXERS: &[&DemuxerDesc] = &[
    &DEMUXER_BMP,
    &DEMUXER_CRI,
    &DEMUXER_DDS,
    &DEMUXER_DPX,
    &DEMUXER_EXR,
    &DEMUXER_GEM,
    &DEMUXER_GIF,
    &DEMUXER_HDR,
    &DEMUXER_J2K,
    &DEMUXER_JPEG,
    &DEMUXER_JPEGLS,
    &DEMUXER_JPEGXL,
    &DEMUXER_JPEGXS,
    &DEMUXER_PAM,
    &DEMUXER_PBM,
    &DEMUXER_PCX,
    &DEMUXER_PFM,
    &DEMUXER_PGM,
    &DEMUXER_PGMYUV,
    &DEMUXER_PGX,
    &DEMUXER_PHM,
    &DEMUXER_PHOTOCD,
    &DEMUXER_PICTOR,
    &DEMUXER_PNG,
    &DEMUXER_PPM,
    &DEMUXER_PSD,
    &DEMUXER_QDRAW,
    &DEMUXER_QOI,
    &DEMUXER_SGI,
    &DEMUXER_SUNRAST,
    &DEMUXER_SVG,
    &DEMUXER_TIFF,
    &DEMUXER_VBN,
    &DEMUXER_WEBP,
    &DEMUXER_XBM,
    &DEMUXER_XPM,
    &DEMUXER_XWD,
];

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "test code")]
mod tests {
    use super::*;
    use vaco_format_core::discovery::NoParsers;
    use vaco_io::MemorySource;

    #[test]
    fn there_are_exactly_thirty_seven_registrations() {
        assert_eq!(PIPE_DEMUXERS.len(), 37);
    }

    #[test]
    fn every_name_is_unique() {
        let mut names: Vec<&str> = PIPE_DEMUXERS.iter().map(|d| d.name).collect();
        names.sort_unstable();
        let mut dedup = names.clone();
        dedup.dedup();
        assert_eq!(names, dedup, "duplicate demuxer name registered");
    }

    #[test]
    fn every_probe_is_total_over_hostile_buffers() {
        use vaco_format_core::probe::ProbeData;
        for d in PIPE_DEMUXERS {
            let _ = (d.probe)(&ProbeData::new(&[]));
            let _ = (d.probe)(&ProbeData::new(&[0u8; 64]));
            let _ = (d.probe)(&ProbeData::new(&[0xFFu8; 64]));
        }
    }

    #[test]
    fn png_pipe_reads_three_concatenated_images_as_three_packets() {
        let mut one = framing::tests_support_png();
        let mut data = one.clone();
        data.append(&mut one.clone());
        data.append(&mut one);
        let src = Box::new(MemorySource::new(data));
        let mut d = PipeDemuxer::open(&SPEC_PNG, src).unwrap();
        let mut count = 0;
        loop {
            match d.read_packet() {
                Ok(_) => count += 1,
                Err(Error::Eof) => break,
                Err(e) => panic!("unexpected error: {e:?}"),
            }
        }
        assert_eq!(count, 3);
    }

    #[test]
    fn eof_is_stable_after_the_last_packet() {
        let src = Box::new(MemorySource::new(framing::tests_support_png()));
        let mut d = PipeDemuxer::open(&SPEC_PNG, src).unwrap();
        assert!(d.read_packet().is_ok());
        for _ in 0..3 {
            assert!(matches!(d.read_packet(), Err(Error::Eof)));
        }
    }

    #[test]
    fn loop_input_restarts_from_the_first_span() {
        let one = framing::tests_support_png();
        let src = Box::new(MemorySource::new(one));
        let mut d = PipeDemuxer::open_with_options(
            &SPEC_PNG,
            src,
            PipeOptions {
                framerate: DEFAULT_FRAMERATE,
                loop_input: true,
            },
        )
        .unwrap();
        for _ in 0..5 {
            assert!(d.read_packet().is_ok());
        }
    }

    #[test]
    fn a_demuxer_can_be_opened_via_its_own_descriptor() {
        let src = Box::new(MemorySource::new(framing::tests_support_png()));
        let mut d = (DEMUXER_PNG.open)(src, &NoParsers).unwrap();
        assert_eq!(d.streams().len(), 1);
        assert!(d.read_packet().is_ok());
    }

    #[test]
    fn whole_remaining_formats_produce_exactly_one_packet_regardless_of_content() {
        let data = vec![0x0Au8; 128]; // pcx-ish leading byte, arbitrary body
        let src = Box::new(MemorySource::new(data));
        let mut d = PipeDemuxer::open(&SPEC_PCX, src).unwrap();
        assert!(d.read_packet().is_ok());
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }
}
