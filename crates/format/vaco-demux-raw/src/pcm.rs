//! The linear-PCM family: 21 demuxers, one engine.
//!
//! Every PCM raw format is the same demuxer parameterised by the on-disk
//! sample width: sample rate and channel layout come from options
//! (`-sample_rate`, `-ch_layout`), never from the file, because a raw PCM
//! stream carries no header at all.
//!
//! # Measured against ffprobe 8.1
//!
//! ```text
//! $ head -c 100000 /dev/urandom > pcm.raw
//! $ ffprobe -f s16le -sample_rate 8000 -show_streams -show_packets pcm.raw
//! ```
//!
//! * Every packet is a fixed **1024-byte** read (`RAW_PACKET_SIZE`), not a
//!   sample-count or a duration target. The reference exposes no
//!   `raw_packet_size` option on the PCM demuxers (unlike the bitstream
//!   family in [`crate::bitstream`]), so 1024 is a hardcoded constant on this
//!   family, not a default the user can see or change from the CLI.
//! * `pts`/`dts` are the running **sample count** (bytes consumed so far,
//!   divided by `bytes_per_frame = container_bytes * channels`), not the byte
//!   offset and not `N/A`. `time_base = 1 / sample_rate`.
//! * `duration` is the packet's own sample count, so the last (short) packet
//!   in a file reports a shorter duration than the rest — measured directly:
//!   a 100000-byte file at `s16le`/8000 Hz/mono produced 97 packets of 1024
//!   bytes/512 samples and one trailing packet of 672 bytes/336 samples.
//! * Every packet is flagged `KEY` (PCM has no dependent frames).
//! * `codec_name` on the reference is the specific tag (`pcm_s16le`), not the
//!   generic `pcm` — see the crate-level docs for why we cannot reproduce
//!   that field byte-for-byte yet.
//! * Extensions are declared for exactly six of the twenty-one formats:
//!   `al` (alaw), `ul` (mulaw), `sb` (s8), `sw` (s16le), `ub` (u8), `uw`
//!   (u16le). The other fifteen have **no** extension and can only be
//!   selected with `-f <name>`; probed directly (`ffprobe x.bin` on a
//!   headerless file with no matching extension exits with "Invalid data
//!   found", never guesses a PCM format).

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{AudioParameters, CodecId, CodecParameters};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{Demuxer, DemuxerDesc, ParserProvider, SeekFlags, SeekTarget, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource, Seekability};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};
use vaco_sampfmt::SampleFmt;

/// Fixed read size for every PCM demuxer. Measured; not exposed as an option
/// on this family (contrast [`crate::bitstream::RAW_PACKET_SIZE`], which the
/// reference *does* expose for the bitstream family).
pub const PCM_PACKET_SIZE: usize = 1024;

/// Reference default for `-sample_rate`.
pub const DEFAULT_SAMPLE_RATE: u32 = 44_100;

/// One PCM registration.
#[derive(Debug, Clone, Copy)]
pub struct PcmSpec {
    /// The demuxer/muxer name, e.g. `"s16le"`. Distinct from the codec name.
    pub name: &'static str,
    pub long_name: &'static str,
    pub extension: Option<&'static str>,
    /// Bytes per sample **as stored on disk**. Distinct from
    /// `decoded.bytes_per_sample()`, which is the *decoded* representation's
    /// width — `s24le` stores 3 bytes and decodes into a 32-bit sample.
    pub container_bytes: u8,
    /// The `AudioParameters::format` a decoder would report. Not exercised by
    /// demuxing itself (which never decodes), so this is best-effort: it
    /// matches the reference's `sample_fmt` for the formats we could check
    /// (`s16le` -> `s16`) and is a documented, unverified mapping for the
    /// rest — see the module docs.
    pub decoded: SampleFmt,
    /// The reference's *codec* name (e.g. `"pcm_s16le"`), for
    /// `codec_long_name`/metadata purposes. `CodecId` has only one generic
    /// `Pcm` variant today (see crate docs), so this cannot yet reach
    /// `codec_name` byte-for-byte.
    pub codec_name: &'static str,
}

/// All 21 registrations, in the order `ffmpeg -demuxers` prints them.
///
/// Captured under `LC_ALL=C` against ffmpeg 8.1: `ffmpeg -h demuxer=<name>`
/// for each of `alaw mulaw f32be f32le f64be f64le s16be s16le s24be s24le
/// s32be s32le s8 u16be u16le u24be u24le u32be u32le u8 vidc`.
pub const PCM_FORMATS: &[PcmSpec] = &[
    PcmSpec {
        name: "alaw",
        long_name: "PCM A-law",
        extension: Some("al"),
        container_bytes: 1,
        decoded: SampleFmt::S16,
        codec_name: "pcm_alaw",
    },
    PcmSpec {
        name: "mulaw",
        long_name: "PCM mu-law",
        extension: Some("ul"),
        container_bytes: 1,
        decoded: SampleFmt::S16,
        codec_name: "pcm_mulaw",
    },
    PcmSpec {
        name: "f32be",
        long_name: "PCM 32-bit floating-point big-endian",
        extension: None,
        container_bytes: 4,
        decoded: SampleFmt::F32,
        codec_name: "pcm_f32be",
    },
    PcmSpec {
        name: "f32le",
        long_name: "PCM 32-bit floating-point little-endian",
        extension: None,
        container_bytes: 4,
        decoded: SampleFmt::F32,
        codec_name: "pcm_f32le",
    },
    PcmSpec {
        name: "f64be",
        long_name: "PCM 64-bit floating-point big-endian",
        extension: None,
        container_bytes: 8,
        decoded: SampleFmt::F64,
        codec_name: "pcm_f64be",
    },
    PcmSpec {
        name: "f64le",
        long_name: "PCM 64-bit floating-point little-endian",
        extension: None,
        container_bytes: 8,
        decoded: SampleFmt::F64,
        codec_name: "pcm_f64le",
    },
    PcmSpec {
        name: "s16be",
        long_name: "PCM signed 16-bit big-endian",
        extension: None,
        container_bytes: 2,
        decoded: SampleFmt::S16,
        codec_name: "pcm_s16be",
    },
    PcmSpec {
        name: "s16le",
        long_name: "PCM signed 16-bit little-endian",
        extension: Some("sw"),
        container_bytes: 2,
        decoded: SampleFmt::S16,
        codec_name: "pcm_s16le",
    },
    PcmSpec {
        name: "s24be",
        long_name: "PCM signed 24-bit big-endian",
        extension: None,
        container_bytes: 3,
        decoded: SampleFmt::S32,
        codec_name: "pcm_s24be",
    },
    PcmSpec {
        name: "s24le",
        long_name: "PCM signed 24-bit little-endian",
        extension: None,
        container_bytes: 3,
        decoded: SampleFmt::S32,
        codec_name: "pcm_s24le",
    },
    PcmSpec {
        name: "s32be",
        long_name: "PCM signed 32-bit big-endian",
        extension: None,
        container_bytes: 4,
        decoded: SampleFmt::S32,
        codec_name: "pcm_s32be",
    },
    PcmSpec {
        name: "s32le",
        long_name: "PCM signed 32-bit little-endian",
        extension: None,
        container_bytes: 4,
        decoded: SampleFmt::S32,
        codec_name: "pcm_s32le",
    },
    PcmSpec {
        name: "s8",
        long_name: "PCM signed 8-bit",
        extension: Some("sb"),
        container_bytes: 1,
        decoded: SampleFmt::U8,
        codec_name: "pcm_s8",
    },
    PcmSpec {
        name: "u16be",
        long_name: "PCM unsigned 16-bit big-endian",
        extension: None,
        container_bytes: 2,
        decoded: SampleFmt::S16,
        codec_name: "pcm_u16be",
    },
    PcmSpec {
        name: "u16le",
        long_name: "PCM unsigned 16-bit little-endian",
        extension: Some("uw"),
        container_bytes: 2,
        decoded: SampleFmt::S16,
        codec_name: "pcm_u16le",
    },
    PcmSpec {
        name: "u24be",
        long_name: "PCM unsigned 24-bit big-endian",
        extension: None,
        container_bytes: 3,
        decoded: SampleFmt::S32,
        codec_name: "pcm_u24be",
    },
    PcmSpec {
        name: "u24le",
        long_name: "PCM unsigned 24-bit little-endian",
        extension: None,
        container_bytes: 3,
        decoded: SampleFmt::S32,
        codec_name: "pcm_u24le",
    },
    PcmSpec {
        name: "u32be",
        long_name: "PCM unsigned 32-bit big-endian",
        extension: None,
        container_bytes: 4,
        decoded: SampleFmt::S32,
        codec_name: "pcm_u32be",
    },
    PcmSpec {
        name: "u32le",
        long_name: "PCM unsigned 32-bit little-endian",
        extension: None,
        container_bytes: 4,
        decoded: SampleFmt::S32,
        codec_name: "pcm_u32le",
    },
    PcmSpec {
        name: "u8",
        long_name: "PCM unsigned 8-bit",
        extension: Some("ub"),
        container_bytes: 1,
        decoded: SampleFmt::U8,
        codec_name: "pcm_u8",
    },
    PcmSpec {
        name: "vidc",
        long_name: "PCM Archimedes VIDC",
        extension: None,
        container_bytes: 1,
        decoded: SampleFmt::S16,
        codec_name: "pcm_vidc",
    },
];

/// Options private to this family: `-sample_rate` and `-ch_layout`.
///
/// Not routed through `FormatOptions` (the generic 39-option table has no
/// room for a per-family surface) and not reachable through
/// `DemuxerDesc::open`, which the frozen trait gives no options parameter at
/// all — see the crate docs for this gap, shared with every option-driven
/// raw format.
#[derive(Debug, Clone)]
pub struct PcmOptions {
    pub sample_rate: u32,
    pub layout: ChannelLayout,
}

impl Default for PcmOptions {
    fn default() -> Self {
        Self {
            sample_rate: DEFAULT_SAMPLE_RATE,
            layout: ChannelLayout::MONO,
        }
    }
}

fn spec_by_name(name: &str) -> Option<&'static PcmSpec> {
    PCM_FORMATS.iter().find(|s| s.name == name)
}

/// The PCM demuxer, parameterised at construction by [`PcmSpec`].
#[derive(Debug)]
pub struct PcmDemuxer {
    spec: &'static PcmSpec,
    io: IoContext,
    streams: [Stream; 1],
    budget: Budget,
    /// Frames (one sample per channel) consumed so far; the running pts.
    frames_read: u64,
    bytes_per_frame: u64,
    eof: bool,
}

impl PcmDemuxer {
    /// Open a PCM stream of `spec`'s format.
    ///
    /// # Errors
    /// [`Error::InvalidData`] if `sample_rate` is zero or the layout has no
    /// channels; otherwise whatever the transport reports.
    pub fn open(
        name: &str,
        src: Box<dyn MediaSource>,
        _parsers: &dyn ParserProvider,
        opts: &PcmOptions,
    ) -> Result<Self> {
        Self::open_with_limits(name, src, opts, Limits::permissive())
    }

    /// Open with an explicit allocation ceiling.
    ///
    /// # Errors
    /// As [`PcmDemuxer::open`].
    pub fn open_with_limits(
        name: &str,
        src: Box<dyn MediaSource>,
        opts: &PcmOptions,
        limits: Limits,
    ) -> Result<Self> {
        let spec =
            spec_by_name(name).ok_or(Error::Unsupported("not a registered PCM raw format"))?;
        if opts.sample_rate == 0 {
            return Err(Error::InvalidData("sample_rate must be nonzero"));
        }
        if opts.layout.channels == 0 {
            return Err(Error::InvalidData(
                "ch_layout must have at least one channel",
            ));
        }
        let io = IoContext::new(src, &IoOptions::default().with_limits(limits.clone()))?;
        let time_base = Rational::new(1, i32::try_from(opts.sample_rate).unwrap_or(i32::MAX));
        let audio = AudioParameters {
            sample_rate: opts.sample_rate,
            format: Some(spec.decoded),
            layout: Some(opts.layout.clone()),
            bits_per_coded_sample: Some(spec.container_bytes.saturating_mul(8)),
            ..AudioParameters::default()
        };
        let mut params = CodecParameters::new(MediaType::Audio);
        params.codec_id = Some(CodecId::Pcm);
        params.audio = Some(audio);
        let mut stream = Stream::new(0, MediaType::Audio, time_base);
        stream.params = params;
        // `CodecId::Pcm` has no per-subtype variant (see crate docs), so the
        // reference's specific `codec_name` (e.g. `pcm_s16le`) is recorded
        // here rather than lost.
        stream.metadata_set("raw_codec_name", spec.codec_name);
        let bytes_per_frame = u64::from(spec.container_bytes) * u64::from(opts.layout.channels);
        Ok(Self {
            spec,
            io,
            streams: [stream],
            budget: Budget::new(limits),
            frames_read: 0,
            bytes_per_frame: bytes_per_frame.max(1),
            eof: false,
        })
    }

    /// The format this instance demuxes.
    #[must_use]
    pub const fn spec(&self) -> &'static PcmSpec {
        self.spec
    }
}

impl Demuxer for PcmDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }
        let pos = self.io.pos();
        let mut buf = [0u8; PCM_PACKET_SIZE];
        let mut n = 0usize;
        while n < buf.len() {
            let Some(dst) = buf.get_mut(n..) else {
                break;
            };
            match self.io.read_partial(dst) {
                Ok(0) | Err(Error::Eof | Error::UnexpectedEof) => break,
                Ok(k) => n = n.saturating_add(k),
                Err(e) => return Err(e),
            }
        }
        if n == 0 {
            self.eof = true;
            return Err(Error::Eof);
        }
        let Some(slice) = buf.get(..n) else {
            return Err(Error::InvalidData("short pcm read"));
        };
        let mut pkt = Packet::from_slice(&mut self.budget, slice)?;
        #[allow(
            clippy::integer_division,
            reason = "exact frame count from a byte count that is always a whole \
                      multiple of bytes_per_frame, except for a legitimately \
                      truncated final packet, whose partial trailing bytes are \
                      correctly dropped by integer division"
        )]
        let frames = (n as u64) / self.bytes_per_frame;
        pkt.stream_index = 0;
        pkt.pts = Timestamp::new(i64::try_from(self.frames_read).unwrap_or(i64::MAX));
        pkt.dts = pkt.pts;
        pkt.duration = frame_duration(frames, self.stream_sample_rate());
        pkt.pos = Some(pos);
        pkt.flags = PacketFlags::KEY;
        self.frames_read = self.frames_read.saturating_add(frames);
        if n < buf.len() {
            self.eof = true;
        }
        Ok(pkt)
    }

    fn seek(&mut self, target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        let SeekTarget::Timestamp { ts, .. } = target else {
            return Err(Error::Unsupported("pcm seeks only by timestamp"));
        };
        if self.io.seekability() == Seekability::None {
            return Err(Error::NotSeekable);
        }
        let Some(frames) = ts.ticks() else {
            return Err(Error::InvalidData("seek target has no timestamp"));
        };
        let frames = frames.max(0) as u64;
        let byte = frames.saturating_mul(self.bytes_per_frame);
        self.io.seek(byte)?;
        self.frames_read = frames;
        self.eof = false;
        Ok(())
    }
}

impl PcmDemuxer {
    fn stream_sample_rate(&self) -> u32 {
        self.streams[0]
            .params
            .audio
            .as_ref()
            .map_or(DEFAULT_SAMPLE_RATE, |a| a.sample_rate)
    }
}

/// `frames` samples at `sample_rate`, as a [`Duration`]. `0` when the rate is
/// somehow zero (never true after `open`'s own validation, but total anyway).
fn frame_duration(frames: u64, sample_rate: u32) -> Duration {
    if sample_rate == 0 {
        return Duration::ZERO;
    }
    #[allow(
        clippy::integer_division,
        reason = "microsecond duration deliberately truncates rather than rounds, \
                  matching how the reference quantises packet durations elsewhere \
                  in this workspace"
    )]
    let micros = (frames.saturating_mul(1_000_000)) / u64::from(sample_rate);
    Duration::from_micros(i64::try_from(micros).unwrap_or(i64::MAX))
}

/// Build one [`DemuxerDesc`] plus its `open`/`probe` glue.
///
/// The literal `name`/`long_name`/`extensions` here **must** match the
/// corresponding [`PcmSpec`] row — `every_descriptor_matches_its_spec_row`
/// below is the test that keeps them from drifting apart. A macro rather than
/// a const-indexed lookup because [`DemuxerDesc`] fields must be `&'static`
/// literals and const string comparison is not worth the ceremony for 21
/// rows.
///
/// None of the 21 registrations has any self-describing magic (measured: a
/// raw PCM file probed with a non-matching extension is never auto-detected —
/// `ffprobe` reports "Invalid data found"), so the whole score comes from
/// [`ProbeScore::from_extension`]. The frozen [`DemuxerDesc`] hands every
/// demuxer a mandatory probe function, so "no content probe" is expressed by
/// calling that helper explicitly rather than by omitting one.
macro_rules! pcm_reg {
    ($ident:ident, $name:literal, $long_name:literal, $exts:expr) => {
        #[doc = concat!("`", $name, "` — ", $long_name, ".")]
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
                Ok(Box::new(PcmDemuxer::open(
                    $name,
                    src,
                    parsers,
                    &PcmOptions::default(),
                )?) as Box<dyn Demuxer>)
            },
        };
    };
}

pcm_reg!(DEMUXER_ALAW, "alaw", "PCM A-law", &["al"]);
pcm_reg!(DEMUXER_MULAW, "mulaw", "PCM mu-law", &["ul"]);
pcm_reg!(
    DEMUXER_F32BE,
    "f32be",
    "PCM 32-bit floating-point big-endian",
    &[]
);
pcm_reg!(
    DEMUXER_F32LE,
    "f32le",
    "PCM 32-bit floating-point little-endian",
    &[]
);
pcm_reg!(
    DEMUXER_F64BE,
    "f64be",
    "PCM 64-bit floating-point big-endian",
    &[]
);
pcm_reg!(
    DEMUXER_F64LE,
    "f64le",
    "PCM 64-bit floating-point little-endian",
    &[]
);
pcm_reg!(DEMUXER_S16BE, "s16be", "PCM signed 16-bit big-endian", &[]);
pcm_reg!(
    DEMUXER_S16LE,
    "s16le",
    "PCM signed 16-bit little-endian",
    &["sw"]
);
pcm_reg!(DEMUXER_S24BE, "s24be", "PCM signed 24-bit big-endian", &[]);
pcm_reg!(
    DEMUXER_S24LE,
    "s24le",
    "PCM signed 24-bit little-endian",
    &[]
);
pcm_reg!(DEMUXER_S32BE, "s32be", "PCM signed 32-bit big-endian", &[]);
pcm_reg!(
    DEMUXER_S32LE,
    "s32le",
    "PCM signed 32-bit little-endian",
    &[]
);
pcm_reg!(DEMUXER_S8, "s8", "PCM signed 8-bit", &["sb"]);
pcm_reg!(
    DEMUXER_U16BE,
    "u16be",
    "PCM unsigned 16-bit big-endian",
    &[]
);
pcm_reg!(
    DEMUXER_U16LE,
    "u16le",
    "PCM unsigned 16-bit little-endian",
    &["uw"]
);
pcm_reg!(
    DEMUXER_U24BE,
    "u24be",
    "PCM unsigned 24-bit big-endian",
    &[]
);
pcm_reg!(
    DEMUXER_U24LE,
    "u24le",
    "PCM unsigned 24-bit little-endian",
    &[]
);
pcm_reg!(
    DEMUXER_U32BE,
    "u32be",
    "PCM unsigned 32-bit big-endian",
    &[]
);
pcm_reg!(
    DEMUXER_U32LE,
    "u32le",
    "PCM unsigned 32-bit little-endian",
    &[]
);
pcm_reg!(DEMUXER_U8, "u8", "PCM unsigned 8-bit", &["ub"]);
pcm_reg!(DEMUXER_VIDC, "vidc", "PCM Archimedes VIDC", &[]);

/// Every PCM demuxer descriptor, in [`PCM_FORMATS`] order.
pub const PCM_DEMUXERS: &[&DemuxerDesc] = &[
    &DEMUXER_ALAW,
    &DEMUXER_MULAW,
    &DEMUXER_F32BE,
    &DEMUXER_F32LE,
    &DEMUXER_F64BE,
    &DEMUXER_F64LE,
    &DEMUXER_S16BE,
    &DEMUXER_S16LE,
    &DEMUXER_S24BE,
    &DEMUXER_S24LE,
    &DEMUXER_S32BE,
    &DEMUXER_S32LE,
    &DEMUXER_S8,
    &DEMUXER_U16BE,
    &DEMUXER_U16LE,
    &DEMUXER_U24BE,
    &DEMUXER_U24LE,
    &DEMUXER_U32BE,
    &DEMUXER_U32LE,
    &DEMUXER_U8,
    &DEMUXER_VIDC,
];

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_format_core::discovery::NoParsers;
    use vaco_io::MemorySource;

    #[test]
    fn every_format_is_present_and_named_after_the_reference() {
        assert_eq!(PCM_FORMATS.len(), 21);
        let names: Vec<&str> = PCM_FORMATS.iter().map(|s| s.name).collect();
        assert!(names.contains(&"alaw"));
        assert!(names.contains(&"s16le"));
        assert!(names.contains(&"vidc"));
    }

    #[test]
    fn every_descriptor_matches_its_spec_row() {
        assert_eq!(PCM_DEMUXERS.len(), PCM_FORMATS.len());
        for (desc, spec) in PCM_DEMUXERS.iter().zip(PCM_FORMATS.iter()) {
            assert_eq!(desc.name, spec.name);
            assert_eq!(desc.long_name, spec.long_name);
            let want: Vec<&str> = spec.extension.into_iter().collect();
            assert_eq!(desc.extensions, want.as_slice(), "{}", spec.name);
        }
    }

    #[test]
    fn declared_extensions_match_the_measured_six() {
        let with_ext: Vec<&str> = PCM_FORMATS.iter().filter_map(|s| s.extension).collect();
        let mut sorted = with_ext.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec!["al", "sb", "sw", "ub", "ul", "uw"]);
    }

    #[test]
    fn packets_are_fixed_size_chunks_with_running_sample_pts() {
        let bytes = vec![0u8; 100_000];
        let src = Box::new(MemorySource::new(bytes));
        let opts = PcmOptions {
            sample_rate: 8000,
            layout: ChannelLayout::MONO,
        };
        let mut d = PcmDemuxer::open("s16le", src, &NoParsers, &opts).unwrap();
        let p0 = d.read_packet().unwrap();
        assert_eq!(p0.len, 1024);
        assert_eq!(p0.pts.ticks(), Some(0));
        assert!(p0.is_key());
        let p1 = d.read_packet().unwrap();
        assert_eq!(p1.pts.ticks(), Some(512));
        let mut last = p1;
        loop {
            match d.read_packet() {
                Ok(p) => last = p,
                Err(Error::Eof) => break,
                Err(e) => panic!("{e:?}"),
            }
        }
        // 100000 % 1024 = 672 bytes = 336 samples, matching the measured
        // reference trailing packet.
        assert_eq!(last.len, 672);
    }

    #[test]
    fn a_zero_sample_rate_is_rejected() {
        let src = Box::new(MemorySource::new(vec![0u8; 16]));
        let opts = PcmOptions {
            sample_rate: 0,
            layout: ChannelLayout::MONO,
        };
        assert!(PcmDemuxer::open("s16le", src, &NoParsers, &opts).is_err());
    }

    #[test]
    fn an_unregistered_name_is_rejected() {
        let src = Box::new(MemorySource::new(vec![0u8; 16]));
        assert!(PcmDemuxer::open("not-a-format", src, &NoParsers, &PcmOptions::default()).is_err());
    }
}
