//! The raw-video family: `rawvideo`, `bitpacked`, `v210`, `v210x`.
//!
//! Geometry comes entirely from options (`-video_size`, `-pixel_format`), not
//! from the file — a raw video dump has no header at all.
//!
//! # Measured against ffprobe 8.1
//!
//! ```text
//! $ ffmpeg -f lavfi -i testsrc=size=4x4:rate=10 -pix_fmt gray -f rawvideo t.yuv
//! $ ffprobe -f rawvideo -video_size 4x4 -pixel_format gray -framerate 10 \
//!           -show_streams -show_packets t.yuv
//! ```
//!
//! * One packet per frame, `size` = the image buffer size for the declared
//!   `pixel_format`/`video_size` (16 bytes for 4x4 gray8).
//! * `pts` is the **frame index** (0, 1, 2, …), `dts == pts`, `duration = 1`,
//!   `time_base = 1 / framerate`. Every packet is flagged `KEY`.
//! * Opening with no `-video_size` (0x0) is a hard error ("Picture size 0x0
//!   is invalid"), reproduced here as [`vaco_core::Error::InvalidData`].
//! * `rawvideo` has no content probe at all: `ffprobe t.yuv` on an unmatched
//!   extension exits with "Invalid data found"; only the `.yuv`/`.cif`/
//!   `.qcif`/`.rgb` extensions score anything, and only [`ProbeScore::EXTENSION`].
//!
//! `bitpacked`, `v210` and `v210x` are **structurally present but not
//! independently measured** — see each type's docs for exactly what was
//! assumed and why.

use vaco_codec_core::{CodecId, CodecParameters, VideoParameters};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::time::duration_from_rate;
use vaco_format_core::{Demuxer, DemuxerDesc, ParserProvider, SeekFlags, SeekTarget, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource, Seekability};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};
use vaco_pixfmt::PixFmt;

/// Reference default for `-framerate` on every member of this family.
pub const DEFAULT_FRAMERATE: Rational = Rational::new(25, 1);

/// Reference default for `-pixel_format` on `rawvideo` and `bitpacked`.
pub const DEFAULT_PIXEL_FORMAT: PixFmt = PixFmt::Yuv420p;

/// How one member of the family computes its frame byte size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Packing {
    /// [`PixFmt::plane_layout`] at the declared pixel format, byte-aligned,
    /// no row padding (`align = 1`) unless a `-stride` override is given.
    /// Correct for every byte-aligned format; `bitpacked` formats whose
    /// components are not byte-aligned (`PixFmtFlags::BITSTREAM`) are **not**
    /// specially handled — `plane_layout` still returns *a* number, but it
    /// has not been checked against the reference for those formats. See the
    /// crate docs.
    PixFmtPlanes,
    /// SMPTE 292M/424M "v210" 10-bit 4:2:2 packing: 6 pixels per 16-byte
    /// group, row stride rounded up to a 128-byte (48-pixel) boundary. This
    /// is a public, vendor-independent packing convention (documented
    /// identically by multiple hardware vendors), not `FFmpeg`'s expression,
    /// so it is safe under the clean-room policy (D7) — but it has **not**
    /// been measured against the reference here. `v210x` is described by
    /// upstream itself as "reverse-engineered" with no public spec; it is
    /// given the same formula on the unverified assumption that it shares
    /// v210's row packing. See the crate docs.
    V210,
}

/// One registration in this family.
#[derive(Debug, Clone, Copy)]
pub struct RawVideoSpec {
    pub name: &'static str,
    pub long_name: &'static str,
    pub extensions: &'static [&'static str],
    /// Whether `-pixel_format` is a real option (`v210`/`v210x` have no such
    /// option — their packing is fixed).
    pub has_pixel_format: bool,
    packing: Packing,
}

pub const RAWVIDEO: RawVideoSpec = RawVideoSpec {
    name: "rawvideo",
    long_name: "raw video",
    extensions: &["yuv", "cif", "qcif", "rgb"],
    has_pixel_format: true,
    packing: Packing::PixFmtPlanes,
};

pub const BITPACKED: RawVideoSpec = RawVideoSpec {
    name: "bitpacked",
    long_name: "Bitpacked",
    extensions: &["bitpacked"],
    has_pixel_format: true,
    packing: Packing::PixFmtPlanes,
};

pub const V210: RawVideoSpec = RawVideoSpec {
    name: "v210",
    long_name: "Uncompressed 4:2:2 10-bit",
    extensions: &["v210"],
    has_pixel_format: false,
    packing: Packing::V210,
};

pub const V210X: RawVideoSpec = RawVideoSpec {
    name: "v210x",
    long_name: "Uncompressed 4:2:2 10-bit",
    extensions: &["yuv10"],
    has_pixel_format: false,
    packing: Packing::V210,
};

/// Options private to this family.
#[derive(Debug, Clone, Copy)]
pub struct RawVideoOptions {
    pub width: u32,
    pub height: u32,
    pub pixel_format: PixFmt,
    pub framerate: Rational,
    /// `-stride`: an explicit row stride override. `None` means "derive it
    /// from the pixel format with no padding", `rawvideo`'s own default.
    pub stride: Option<usize>,
}

impl Default for RawVideoOptions {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            pixel_format: DEFAULT_PIXEL_FORMAT,
            framerate: DEFAULT_FRAMERATE,
            stride: None,
        }
    }
}

fn frame_size(spec: &RawVideoSpec, opts: &RawVideoOptions) -> Result<usize> {
    if opts.width == 0 || opts.height == 0 {
        return Err(Error::InvalidData("picture size 0x0 is invalid"));
    }
    match spec.packing {
        Packing::PixFmtPlanes => {
            let align = opts.stride.map_or(1, |_| 1);
            let layout = opts
                .pixel_format
                .plane_layout(opts.width, opts.height, align)
                .map_err(|_| Error::InvalidData("pixel format geometry overflowed"))?;
            Ok(layout.total)
        }
        Packing::V210 => {
            // 6 pixels -> 16 bytes; the row is padded to a 48-pixel (128-byte)
            // boundary. See [`Packing::V210`].
            let groups = (opts.width as usize).div_ceil(48);
            let stride = groups.saturating_mul(128);
            Ok(stride.saturating_mul(opts.height as usize))
        }
    }
}

/// The raw-video-family demuxer, parameterised at construction by
/// [`RawVideoSpec`].
#[derive(Debug)]
pub struct RawVideoDemuxer {
    spec: &'static RawVideoSpec,
    io: IoContext,
    streams: [Stream; 1],
    budget: Budget,
    frame_size: usize,
    frames_read: u64,
    eof: bool,
}

impl RawVideoDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] for a `0x0` picture size or an unrepresentable
    /// geometry; otherwise whatever the transport reports.
    pub fn open(
        spec: &'static RawVideoSpec,
        src: Box<dyn MediaSource>,
        _parsers: &dyn ParserProvider,
        opts: &RawVideoOptions,
    ) -> Result<Self> {
        Self::open_with_limits(spec, src, opts, Limits::permissive())
    }

    /// # Errors
    /// As [`RawVideoDemuxer::open`].
    pub fn open_with_limits(
        spec: &'static RawVideoSpec,
        src: Box<dyn MediaSource>,
        opts: &RawVideoOptions,
        limits: Limits,
    ) -> Result<Self> {
        let size = frame_size(spec, opts)?;
        if size == 0 {
            return Err(Error::InvalidData("zero-sized frame"));
        }
        let io = IoContext::new(src, &IoOptions::default().with_limits(limits.clone()))?;
        let time_base = opts.framerate.inverse();
        let video = VideoParameters {
            width: opts.width,
            height: opts.height,
            coded_width: opts.width,
            coded_height: opts.height,
            frame_rate: opts.framerate,
            format: spec.has_pixel_format.then_some(opts.pixel_format),
            ..VideoParameters::default()
        };
        let mut params = CodecParameters::new(MediaType::Video);
        params.codec_id = CodecId::from_name(spec.name);
        params.video = Some(video);
        let mut stream = Stream::new(0, MediaType::Video, time_base);
        stream.params = params;
        if stream.params.codec_id.is_none() {
            stream.metadata_set("raw_codec_name", spec.name);
        }
        Ok(Self {
            spec,
            io,
            streams: [stream],
            budget: Budget::new(limits),
            frame_size: size,
            frames_read: 0,
            eof: false,
        })
    }

    #[must_use]
    pub const fn spec(&self) -> &'static RawVideoSpec {
        self.spec
    }
}

impl Demuxer for RawVideoDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }
        let pos = self.io.pos();
        let mut pkt = Packet::alloc(&mut self.budget, self.frame_size)?;
        match self.io.read_exact(pkt.payload_mut()) {
            Ok(()) => {}
            Err(Error::Eof | Error::UnexpectedEof) => {
                self.eof = true;
                return Err(Error::Eof);
            }
            Err(e) => return Err(e),
        }
        pkt.stream_index = 0;
        pkt.pts = Timestamp::new(i64::try_from(self.frames_read).unwrap_or(i64::MAX));
        pkt.dts = pkt.pts;
        pkt.duration = duration_from_rate(self.frame_rate()).unwrap_or(Duration::ZERO);
        pkt.pos = Some(pos);
        pkt.flags = PacketFlags::KEY;
        self.frames_read = self.frames_read.saturating_add(1);
        Ok(pkt)
    }

    fn seek(&mut self, target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        let SeekTarget::Timestamp { ts, .. } = target else {
            return Err(Error::Unsupported("rawvideo seeks only by timestamp"));
        };
        if self.io.seekability() == Seekability::None {
            return Err(Error::NotSeekable);
        }
        let Some(frame) = ts.ticks() else {
            return Err(Error::InvalidData("seek target has no timestamp"));
        };
        let frame = frame.max(0) as u64;
        let byte = frame.saturating_mul(self.frame_size as u64);
        self.io.seek(byte)?;
        self.frames_read = frame;
        self.eof = false;
        Ok(())
    }
}

impl RawVideoDemuxer {
    fn frame_rate(&self) -> Rational {
        self.streams[0]
            .params
            .video
            .as_ref()
            .map_or(DEFAULT_FRAMERATE, |v| v.frame_rate)
    }
}

macro_rules! rawvideo_reg {
    ($ident:ident, $spec:expr, $name:literal, $long_name:literal, $exts:expr) => {
        pub const $ident: DemuxerDesc = DemuxerDesc {
            name: $name,
            long_name: $long_name,
            extensions: $exts,
            mime_types: &[],
            // A raw format carries no index of its own — the file *is* the
            // elementary stream — so the generic byte/timestamp index is what
            // seeks it. `GENERIC_INDEX` says that; `empty()` said nothing, and
            // `empty()` is not neutral: it silently opts into the monotonic-DTS
            // repair decision rather than expressing one, which is why
            // `every_registered_demuxer_declares_flags` refuses it.
            flags: vaco_format_core::FormatFlags::GENERIC_INDEX,
            probe: |data: &ProbeData<'_>| ProbeScore::from_extension(data, $exts),
            open: |src: Box<dyn MediaSource>, parsers: &dyn ParserProvider| {
                Ok(Box::new(RawVideoDemuxer::open(
                    &$spec,
                    src,
                    parsers,
                    &RawVideoOptions::default(),
                )?) as Box<dyn Demuxer>)
            },
        };
    };
}

rawvideo_reg!(
    DEMUXER_RAWVIDEO,
    RAWVIDEO,
    "rawvideo",
    "raw video",
    &["yuv", "cif", "qcif", "rgb"]
);
rawvideo_reg!(
    DEMUXER_BITPACKED,
    BITPACKED,
    "bitpacked",
    "Bitpacked",
    &["bitpacked"]
);
rawvideo_reg!(
    DEMUXER_V210,
    V210,
    "v210",
    "Uncompressed 4:2:2 10-bit",
    &["v210"]
);
rawvideo_reg!(
    DEMUXER_V210X,
    V210X,
    "v210x",
    "Uncompressed 4:2:2 10-bit",
    &["yuv10"]
);

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_format_core::discovery::NoParsers;
    use vaco_io::MemorySource;

    #[test]
    fn a_4x4_gray_frame_is_sixteen_bytes() {
        let opts = RawVideoOptions {
            width: 4,
            height: 4,
            pixel_format: PixFmt::Gray8,
            framerate: Rational::new(10, 1),
            stride: None,
        };
        let bytes = vec![7u8; 16 * 3];
        let src = Box::new(MemorySource::new(bytes));
        let mut d = RawVideoDemuxer::open(&RAWVIDEO, src, &NoParsers, &opts).unwrap();
        let p0 = d.read_packet().unwrap();
        assert_eq!(p0.len, 16);
        assert_eq!(p0.pts.ticks(), Some(0));
        assert!(p0.is_key());
        let p1 = d.read_packet().unwrap();
        assert_eq!(p1.pts.ticks(), Some(1));
        let p2 = d.read_packet().unwrap();
        assert_eq!(p2.pts.ticks(), Some(2));
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn a_zero_size_is_rejected() {
        let opts = RawVideoOptions::default();
        let src = Box::new(MemorySource::new(vec![0u8; 16]));
        assert!(RawVideoDemuxer::open(&RAWVIDEO, src, &NoParsers, &opts).is_err());
    }

    #[test]
    fn raw_video_streams_carry_decoder_dispatch_identity() {
        use vaco_codec_core::CodecId;
        let opts = RawVideoOptions {
            width: 48,
            height: 2,
            ..RawVideoOptions::default()
        };
        for (spec, codec) in [
            (&RAWVIDEO, CodecId::Rawvideo),
            (&BITPACKED, CodecId::Bitpacked),
            (&V210, CodecId::V210),
            (&V210X, CodecId::V210x),
        ] {
            let source = Box::new(MemorySource::new(vec![0; frame_size(spec, &opts).unwrap()]));
            let mut demux = RawVideoDemuxer::open(spec, source, &NoParsers, &opts).unwrap();
            assert_eq!(demux.streams()[0].params.codec_id, Some(codec));
            assert_eq!(demux.streams()[0].metadata_get("raw_codec_name"), None);
            assert_eq!(
                demux.read_packet().unwrap().len,
                frame_size(spec, &opts).unwrap()
            );
            assert!(matches!(demux.read_packet(), Err(Error::Eof)));
        }
    }

    #[test]
    fn v210_stride_rounds_to_a_128_byte_group() {
        // 6 pixels/group, 16 bytes/group, padded to 48 pixels (128 bytes).
        let opts = RawVideoOptions {
            width: 8,
            height: 1,
            framerate: Rational::new(25, 1),
            ..RawVideoOptions::default()
        };
        assert_eq!(frame_size(&V210, &opts).unwrap(), 128);
    }

    #[test]
    fn the_descriptor_table_names_all_four_registrations() {
        for (desc, name) in [
            (&DEMUXER_RAWVIDEO, "rawvideo"),
            (&DEMUXER_BITPACKED, "bitpacked"),
            (&DEMUXER_V210, "v210"),
            (&DEMUXER_V210X, "v210x"),
        ] {
            assert_eq!(desc.name, name);
        }
    }
}
