//! `image2`: the pattern/glob/sequence multi-file demuxer.
//!
//! # The registry seam does not fit this format
//!
//! [`vaco_format_core::DemuxerDesc::open`] is
//! `fn(Box<dyn MediaSource>, &dyn ParserProvider) -> Result<Box<dyn Demuxer>>`
//! — one already-open source, no filename. `image2`'s entire job is opening
//! *many* files by a name pattern the caller has not been given anywhere to
//! put. `vaco-demux-raw` already documented the milder version of this gap
//! (`-pixel_format`/`-video_size` have nowhere to go through the registry);
//! this format's gap is structural rather than "defaults only; a direct
//! caller can override," because there is no `MediaSource::path()` at all —
//! see `docs/format/vaco-demux-image2.md` for what a fix would need.
//!
//! So there are two ways into this crate's demuxing, deliberately:
//!
//! * [`Image2Demuxer::open_pattern`] — the real thing. A pattern string plus
//!   [`Image2Options`], for a caller that has both (an embedder, the eventual
//!   CLI layer, this crate's own tests).
//! * [`DEMUXER_IMAGE2`]'s `open` — the registry path. It receives one already
//!   -open source and treats it exactly like `-pattern_type none` pointed at
//!   a single already-resolved file: the whole source is one packet, and a
//!   second `read_packet` reports end of stream. That is not a guess; it is
//!   the literal, correct behaviour for "one source, no pattern to speak of."

use std::path::PathBuf;

use vaco_codec_core::{FieldOrder, VideoParameters};
use vaco_core::{Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::time::TIME_BASE_Q;
use vaco_format_core::{
    Demuxer, DemuxerDesc, FormatFlags, ParserProvider, SeekFlags, SeekTarget, Stream,
};
use vaco_io::MediaSource;
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

use crate::fsutil;
use crate::pattern::SequencePattern;

/// A still image has no interlacing concept at all — measured, the reference
/// prints `field_order=unknown` for a bare PNG through `image2`, never
/// `progressive` — so this crate states [`FieldOrder::Unknown`] itself rather
/// than leaving [`VideoParameters::default`]'s `Progressive` in place, which
/// `fill_from` would otherwise read as "no opinion" and happily inherit.
///
/// `1/framerate`, for a stream time base: `-framerate`'s default of `25/1`
/// gives `1/25`, matching the reference's own `image2` time base. A `0`
/// numerator (an explicit `-framerate 0`, or an unset option this crate never
/// produces) falls back to [`TIME_BASE_Q`] rather than dividing by zero.
pub(crate) fn stream_video(framerate: Rational) -> VideoParameters {
    VideoParameters {
        frame_rate: framerate,
        field_order: FieldOrder::Unknown,
        ..VideoParameters::default()
    }
}

pub(crate) fn time_base_for(framerate: Rational) -> Rational {
    if framerate.num > 0 {
        Rational::new(framerate.den, framerate.num)
    } else {
        TIME_BASE_Q
    }
}

/// `-pattern_type`. Measured via `ffmpeg -h demuxer=image2` against ffmpeg
/// 8.1: the reference prints named constants `glob` (1), `sequence` (2) and
/// `none` (3), and a default numeric value of 4 with **no name attached** —
/// `-pattern_type 0` (the historical `glob_sequence`) is rejected outright
/// (`Unknown value '0' for pattern_type option`, measured), so a build of
/// this vintage genuinely has no way to select it by name or number.
/// `planning/20-roadmap.md`'s `sequence, glob, glob_sequence, none` list is
/// wrong for this reference build; [`PatternType::Auto`] reproduces the
/// unnamed default's *observed* behaviour (try sequence-style number
/// matching; a pattern with no `%d` falls through to a literal file) rather
/// than the name that used to exist for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternType {
    /// The reference's unset default: numeric value 4, no CLI name.
    Auto,
    Glob,
    Sequence,
    /// `-pattern_type none`: the path is a literal filename.
    Disabled,
}

/// `-ts_from_file`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TsFromFile {
    #[default]
    None,
    Sec,
    Ns,
}

/// Options private to `image2`, read directly by [`Image2Demuxer::open_pattern`].
#[derive(Debug, Clone, PartialEq)]
pub struct Image2Options {
    pub pattern_type: PatternType,
    /// Default `0`, per `-h demuxer=image2`.
    pub start_number: i64,
    /// Default `5`.
    pub start_number_range: i64,
    /// Default `25/1`.
    pub framerate: Rational,
    pub loop_input: bool,
    pub ts_from_file: TsFromFile,
}

impl Default for Image2Options {
    fn default() -> Self {
        Self {
            pattern_type: PatternType::Auto,
            start_number: 0,
            start_number_range: 5,
            framerate: Rational::new(25, 1),
            loop_input: false,
            ts_from_file: TsFromFile::None,
        }
    }
}

#[derive(Debug)]
enum Plan {
    Sequence {
        seq: SequencePattern,
        display_pattern: String,
        current: i64,
    },
    Glob {
        files: Vec<PathBuf>,
        index: usize,
    },
    Disabled {
        path: PathBuf,
        done: bool,
    },
}

/// The real `image2` demuxer: a name pattern resolved against the
/// filesystem, one whole file per packet.
#[derive(Debug)]
pub struct Image2Demuxer {
    dir: PathBuf,
    plan: Plan,
    loops_done: u64,
    frame_number: u64,
    options: Image2Options,
    stream: Stream,
    budget: Budget,
}

impl Image2Demuxer {
    /// Resolve `pattern` against the filesystem per `options.pattern_type`
    /// (searching `[start_number, start_number + start_number_range)` for
    /// sequence mode) and open the first image.
    ///
    /// # Errors
    /// [`Error::Io`] (`NotFound`) when no file matches; propagates whatever
    /// [`SequencePattern::parse`] and the filesystem report otherwise.
    pub fn open_pattern(pattern: &str, options: Image2Options) -> Result<Self> {
        let (dir, name) = fsutil::split_dir_and_name(pattern);
        let effective = match options.pattern_type {
            PatternType::Auto => {
                if SequencePattern::looks_like_one(name) {
                    PatternType::Sequence
                } else {
                    PatternType::Disabled
                }
            }
            other => other,
        };

        let plan = match effective {
            PatternType::Disabled => Plan::Disabled {
                path: PathBuf::from(pattern),
                done: false,
            },
            PatternType::Sequence => {
                let seq = SequencePattern::parse(name)?;
                let current = fsutil::find_sequence_start(
                    &dir,
                    pattern,
                    &seq,
                    options.start_number,
                    options.start_number_range,
                )?;
                Plan::Sequence {
                    seq,
                    display_pattern: pattern.to_owned(),
                    current,
                }
            }
            PatternType::Glob => {
                let files = fsutil::glob_list(&dir, name)?;
                if files.is_empty() {
                    return Err(Error::Io(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("glob pattern '{pattern}' did not match any file"),
                    )));
                }
                Plan::Glob { files, index: 0 }
            }
            PatternType::Auto => unreachable!("resolved above"),
        };

        let mut stream = Stream::new(0, MediaType::Video, time_base_for(options.framerate));
        stream.params.media_type = Some(MediaType::Video);
        stream.params.video = Some(stream_video(options.framerate));
        // The reference names the codec from the filename here too
        // (`image2`'s header read, not its probe): nothing else in this path
        // ever looks at the bytes, so without it a `.tga` opened as `image2`
        // reached the CLI with no codec at all and failed with "a stream being
        // transcoded has no known input codec". `None` for an extension this
        // build has no `CodecId` for, which leaves the stream exactly as
        // undescribed as it was.
        stream.params.codec_id =
            fsutil::extension_of(name).and_then(vaco_codec_core::image_codec_for_extension);
        // A literal filename (no pattern at all) is one still image, and the
        // reference states no *stream* start time for it — while still
        // timestamping its packet. Measured, ffmpeg 9.0.1 on a single PNG:
        // `stream start_pts=N/A start_time=N/A duration_ts=N/A duration=N/A`
        // beside `packet pts=0 dts=0 duration=1`. A `Sequence`/`Glob` plan is a
        // real timeline the caller asked for (`start_time=0`, `duration_ts=3`
        // for three images), and `-ts_from_file` states one from the file's
        // mtime (`start_pts=1788374309`), so both of those are left to be
        // derived from the packets as usual.
        if matches!(plan, Plan::Disabled { .. }) && options.ts_from_file == TsFromFile::None {
            stream.state_no_start_time();
        }

        Ok(Self {
            dir,
            plan,
            loops_done: 0,
            frame_number: 0,
            options,
            stream,
            budget: Budget::new(Limits::permissive()),
        })
    }

    /// The path the next `read_packet` will read, and whether it exists —
    /// without reading it. `None` means "the sequence/glob/single-file plan
    /// is exhausted."
    fn peek_next(&self) -> Option<PathBuf> {
        match &self.plan {
            Plan::Sequence { seq, current, .. } => {
                let path = self.dir.join(seq.format(*current));
                fsutil::sequence_file_exists(&self.dir, seq, *current).then_some(path)
            }
            Plan::Glob { files, index } => files.get(*index).cloned(),
            Plan::Disabled { path, done } => (!done).then(|| path.clone()),
        }
    }

    fn advance(&mut self) {
        match &mut self.plan {
            Plan::Sequence { current, .. } => *current = current.saturating_add(1),
            Plan::Glob { index, .. } => *index += 1,
            Plan::Disabled { done, .. } => *done = true,
        }
    }

    fn restart(&mut self) -> Result<()> {
        match &mut self.plan {
            Plan::Sequence {
                current,
                display_pattern,
                seq,
            } => {
                *current = fsutil::find_sequence_start(
                    &self.dir,
                    display_pattern,
                    seq,
                    self.options.start_number,
                    self.options.start_number_range,
                )?;
            }
            Plan::Glob { index, .. } => *index = 0,
            Plan::Disabled { done, .. } => *done = false,
        }
        self.loops_done = self.loops_done.saturating_add(1);
        Ok(())
    }

    fn pts_ticks(&self, path: &std::path::Path) -> i64 {
        match self.options.ts_from_file {
            // The frame index, because `time_base` is `1/framerate` by
            // construction — one frame is exactly one tick. Measured on
            // `img_%03d.png`: `pts=0,1,2` with `duration=1` at `time_base=1/25`,
            // and `pts=0,1,2` at `1/10` under `-framerate 10`, so the index is
            // the answer at any rate.
            //
            // This used to be `duration_from_rate(..) * frame_number`, a
            // *microsecond* count handed to a field counted in `1/framerate`
            // ticks — 40 000× too large at the default rate, which made every
            // timestamp-driven option silently inert: measured, `-t 0.04`,
            // `-t 0.08` and `-t 0.12` on a three-image sequence all wrote
            // 27 648 bytes where the reference wrote 9 216 / 18 432 / 27 648.
            TsFromFile::None => i64::try_from(self.frame_number).unwrap_or(i64::MAX),
            TsFromFile::Sec => {
                fsutil::file_mtime_unix(path).map_or(0, |(secs, _)| secs.saturating_mul(1_000_000))
            }
            TsFromFile::Ns => fsutil::file_mtime_unix(path)
                .map_or(0, |(secs, nanos)| secs_and_nanos_to_micros(secs, nanos)),
        }
    }
}

/// `secs.nanos` to microseconds, floored. A plain `/ 1_000` division is
/// exactly what this is, and is deliberately isolated here (rather than
/// inline at the one call site) so the `integer_division` lint's blanket
/// deny stays meaningful for every *other* division in this crate.
#[allow(
    clippy::integer_division,
    reason = "converting a nanosecond count to microseconds always floors"
)]
fn secs_and_nanos_to_micros(secs: i64, nanos: u32) -> i64 {
    secs.saturating_mul(1_000_000)
        .saturating_add(i64::from(nanos) / 1_000)
}

impl Demuxer for Image2Demuxer {
    fn streams(&self) -> &[Stream] {
        std::slice::from_ref(&self.stream)
    }

    fn read_packet(&mut self) -> Result<Packet> {
        let path = match self.peek_next() {
            Some(p) => p,
            None if self.options.loop_input => {
                self.restart()?;
                match self.peek_next() {
                    Some(p) => p,
                    None => return Err(Error::Eof), // an empty plan looping forever
                }
            }
            None => return Err(Error::Eof),
        };
        let bytes = fsutil::read_file(&path)?;
        let mut packet = Packet::from_slice(&mut self.budget, &bytes)?;
        packet.pts = Timestamp::new(self.pts_ticks(&path));
        packet.dts = packet.pts;
        packet.flags = PacketFlags::KEY;
        self.advance();
        self.frame_number = self.frame_number.saturating_add(1);
        Ok(packet)
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        // Each "packet" is an independent whole-file read with no shared
        // index; nothing here has measured the reference's own seek support
        // for image2, and getting it wrong silently would be worse than
        // reporting it honestly as unsupported.
        Err(Error::NotSeekable)
    }
}

// --------------------------------------------------------- the frozen seam

/// Adapts one already-open [`MediaSource`] into a one-packet [`Demuxer`], for
/// [`DEMUXER_IMAGE2`]'s registry-mandated `open` signature. See the module
/// docs for why this — not the real multi-file logic — is what the registry
/// path gets.
#[derive(Debug)]
struct SingleSourceDemuxer {
    stream: Stream,
    remaining: Option<Vec<u8>>,
    budget: Budget,
}

impl Demuxer for SingleSourceDemuxer {
    fn streams(&self) -> &[Stream] {
        std::slice::from_ref(&self.stream)
    }

    fn read_packet(&mut self) -> Result<Packet> {
        let Some(bytes) = self.remaining.take() else {
            return Err(Error::Eof);
        };
        let mut packet = Packet::from_slice(&mut self.budget, &bytes)?;
        // One still image, exactly like `Image2Demuxer`'s `Plan::Disabled`
        // case: the packet is timestamped and the *stream* states no start
        // time (set in `open_boxed`). See [`Stream::state_no_start_time`].
        packet.pts = Timestamp::ZERO;
        packet.dts = Timestamp::ZERO;
        packet.flags = PacketFlags::KEY;
        Ok(packet)
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        Err(Error::NotSeekable)
    }
}

/// The `image2` demuxer as reached through the registry.
///
/// Starts as [`SingleSourceDemuxer`], because that is all
/// [`DemuxerDesc::open`]'s frozen signature can construct from one
/// already-open source. Becomes the real [`Image2Demuxer`] the moment a
/// caller supplies the pattern's URL through [`Demuxer::bind_url`] —
/// [`Image2Demuxer::open_pattern`] already resolves `img_%03d.png`-style
/// patterns against the filesystem correctly; it was simply unreachable
/// from the registry path before this method existed.
#[derive(Debug)]
enum RegistryDemuxer {
    Single(SingleSourceDemuxer),
    Pattern(Image2Demuxer),
}

impl Demuxer for RegistryDemuxer {
    fn streams(&self) -> &[Stream] {
        match self {
            Self::Single(d) => d.streams(),
            Self::Pattern(d) => d.streams(),
        }
    }

    fn read_packet(&mut self) -> Result<Packet> {
        match self {
            Self::Single(d) => d.read_packet(),
            Self::Pattern(d) => d.read_packet(),
        }
    }

    fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()> {
        match self {
            Self::Single(d) => d.seek(target, flags),
            Self::Pattern(d) => d.seek(target, flags),
        }
    }

    /// Replace the placeholder single-source state with the real
    /// pattern-resolved demuxer.
    ///
    /// Options (`-pattern_type`, `-start_number`, …) have no channel to this
    /// point yet either — the read-side twin of gap 5, not attempted here —
    /// so this binds with [`Image2Options::default`]; a caller that needs
    /// non-default options still has [`Image2Demuxer::open_pattern`] itself.
    ///
    /// # Errors
    /// [`Error::Unsupported`] if already bound: this method is documented as
    /// a one-time call, and re-resolving against a second, possibly
    /// different URL is not this crate's decision to make silently.
    /// Otherwise whatever [`Image2Demuxer::open_pattern`] finds wrong with
    /// `url` (no match, an unparseable pattern).
    fn bind_url(&mut self, url: &str) -> Result<()> {
        if matches!(self, Self::Pattern(_)) {
            return Err(Error::Unsupported(
                "this image2 demuxer is already bound to a pattern",
            ));
        }
        *self = Self::Pattern(Image2Demuxer::open_pattern(url, Image2Options::default())?);
        Ok(())
    }
}

/// Largest single source this adapter will buffer, mirroring
/// `pipe::MAX_BUFFERED` for the same "computed once, up front" trade-off.
const MAX_SINGLE_SOURCE: u64 = 512 << 20;

fn read_source_to_end(mut src: Box<dyn MediaSource>) -> Result<Vec<u8>> {
    let mut budget = Budget::new(Limits::permissive());
    let mut out = Vec::new();
    let mut chunk = budget.alloc::<u8>(64 * 1024)?;
    loop {
        let n = src.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        let Some(taken) = chunk.get(..n) else {
            return Err(Error::InvalidData(
                "short read reported more bytes than taken",
            ));
        };
        budget.charge(taken.len() as u64)?;
        if out.len() as u64 + taken.len() as u64 > MAX_SINGLE_SOURCE {
            return Err(Error::LimitExceeded {
                limit: "image2_single_source_buffer",
                requested: out.len() as u64 + taken.len() as u64,
                cap: MAX_SINGLE_SOURCE,
            });
        }
        out.extend_from_slice(taken);
    }
    Ok(out)
}

fn open_boxed(src: Box<dyn MediaSource>, parsers: &dyn ParserProvider) -> Result<Box<dyn Demuxer>> {
    let _ = parsers;
    let bytes = read_source_to_end(src)?;
    let framerate = Image2Options::default().framerate;
    let mut stream = Stream::new(0, MediaType::Video, time_base_for(framerate));
    stream.params.media_type = Some(MediaType::Video);
    stream.params.video = Some(stream_video(framerate));
    stream.state_no_start_time();
    let remaining = (!bytes.is_empty()).then_some(bytes);
    Ok(Box::new(RegistryDemuxer::Single(SingleSourceDemuxer {
        stream,
        remaining,
        budget: Budget::new(Limits::permissive()),
    })))
}

fn probe_image2(data: &ProbeData<'_>) -> ProbeScore {
    // The reference selects `image2` almost entirely by filename pattern
    // (`%d` in the path) rather than content, which this crate cannot see
    // from `ProbeData` alone reliably; fall back to the extension list.
    //
    // That list used to be written out here by hand, and was missing `tga`
    // among others — measured, `t.tga` matched no demuxer at all and
    // `vaco-probe` reported `Invalid data found when processing input` where
    // `ffprobe` reported `image2`. TGA is one of only a handful of image
    // formats with no `*_pipe` splitter of its own (the reference has none
    // either), so `image2` is the *only* thing that can open it. Reading
    // `vaco-codec-core`'s single list is what stops that gap reopening.
    //
    // Scoring [`ProbeScore::EXTENSION`] leaves every `*_pipe` splitter's
    // content match (`MAGIC` or `MAX`) ahead of this, so `image2` only wins
    // where nothing recognised the bytes.
    ProbeScore::from_extension(data, vaco_codec_core::IMAGE_EXTENSIONS)
}

/// The `image2` demuxer's registry entry. See the module docs for the split
/// between this and [`Image2Demuxer::open_pattern`].
pub const DEMUXER_IMAGE2: DemuxerDesc = DemuxerDesc {
    name: "image2",
    long_name: "image2 sequence",
    extensions: &[],
    mime_types: &[],
    // See the note in `pipe/mod.rs`: derived timestamps, whole-image keyframes,
    // exact frame-number seeking only. Stating the three inapplicable search
    // strategies is a decision; `empty()` is an omission that reads like one.
    // `NEEDNUMBER` is the CLI's signal that this descriptor's `open` cannot
    // be reached with a literally-opened source at all — the URL is a `%d`
    // pattern — and must instead get a placeholder plus a
    // `Demuxer::bind_url` call. See `RegistryDemuxer::bind_url` above.
    flags: FormatFlags::NOBINSEARCH
        .union(FormatFlags::NOGENSEARCH)
        .union(FormatFlags::NO_BYTE_SEEK)
        .union(FormatFlags::NEEDNUMBER),
    probe: probe_image2,
    open: open_boxed,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use std::fs;

    /// TGA has no `*_pipe` splitter in the reference or here, so `image2` is
    /// the only demuxer that can open one. Its probe used to carry a
    /// hand-written extension list that omitted `tga` (and `pcx`, `xbm`,
    /// `xwd`, `jls`), so a `.tga` file matched nothing at all.
    #[test]
    fn the_image2_probe_covers_every_declared_image_extension() {
        for ext in vaco_codec_core::IMAGE_EXTENSIONS {
            let name = format!("frame.{ext}");
            let data = ProbeData::new(&[0u8; 32]).with_filename(&name);
            assert_eq!(
                (DEMUXER_IMAGE2.probe)(&data),
                ProbeScore::EXTENSION,
                "{ext} is declared but does not probe"
            );
        }
        let other = ProbeData::new(&[0u8; 32]).with_filename("movie.mkv");
        assert!((DEMUXER_IMAGE2.probe)(&other).is_none());
    }

    /// Nothing else in the `image2` path reads the bytes, so without the
    /// filename the stream reaches the CLI with no codec and the transcode
    /// fails with "no known input codec".
    #[test]
    fn a_bound_pattern_names_its_codec_from_the_extension() {
        let dir = std::env::temp_dir().join(format!(
            "vaco-image2-codec-id-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (name, want) in [
            ("a.tga", Some(vaco_codec_core::CodecId::Targa)),
            ("a.png", Some(vaco_codec_core::CodecId::Png)),
            ("a.dpx", None),
        ] {
            let path = dir.join(name);
            fs::write(&path, b"x").unwrap();
            let d = Image2Demuxer::open_pattern(path.to_str().unwrap(), Image2Options::default())
                .unwrap();
            assert_eq!(d.streams()[0].params.codec_id, want, "{name}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// Packet timestamps and the stream's `start_time` are independent in the
    /// reference, and this crate used to answer the second by dropping the
    /// first — which left every single still image untranscodable
    /// ("this container needs timestamps and the packet has none").
    ///
    /// Measured, ffmpeg 9.0.1 on one PNG: `packet pts=0 dts=0 duration=1`
    /// beside `stream start_pts=N/A start_time=N/A`.
    #[test]
    fn a_still_is_timestamped_while_its_stream_states_no_start() {
        let dir = scratch_dir("still_ts");
        fs::write(dir.join("only.png"), b"one").unwrap();
        let path = dir.join("only.png");
        let mut d =
            Image2Demuxer::open_pattern(path.to_str().unwrap(), Image2Options::default()).unwrap();

        assert!(d.streams()[0].start_time.is_none());
        assert!(
            !d.streams()[0].start_time_underived(),
            "the absence must be stated, or discovery derives 0 from the first packet"
        );

        let p = d.read_packet().unwrap();
        assert_eq!(p.pts.ticks(), Some(0));
        assert_eq!(p.dts.ticks(), Some(0));
        let _ = fs::remove_dir_all(&dir);
    }

    /// `time_base` is `1/framerate`, so one frame is one tick and the *n*th
    /// packet's pts is *n*. Measured: `pts=0,1,2 duration=1` at `1/25`, and
    /// the same `0,1,2` at `1/10` under `-framerate 10`.
    ///
    /// This used to be `duration_from_rate(framerate) * frame_number` — a
    /// microsecond count in a field counted in `1/framerate` ticks, 40 000×
    /// too large at the default rate.
    #[test]
    fn a_sequence_counts_ticks_not_microseconds() {
        for (rate, tag) in [
            (Rational::new(25, 1), "seq_tb25"),
            (Rational::new(10, 1), "seq_tb10"),
        ] {
            let dir = scratch_dir(tag);
            for n in 1..=3 {
                fs::write(dir.join(format!("f{n:03}.png")), b"x").unwrap();
            }
            let pattern = dir.join("f%03d.png");
            let mut d = Image2Demuxer::open_pattern(
                pattern.to_str().unwrap(),
                Image2Options {
                    start_number: 1,
                    framerate: rate,
                    ..Image2Options::default()
                },
            )
            .unwrap();
            assert_eq!(d.streams()[0].time_base, Rational::new(rate.den, rate.num));
            assert!(
                d.streams()[0].start_time_underived(),
                "a real sequence has a timeline; the reference reports start_time=0"
            );
            for want in 0..3 {
                assert_eq!(d.read_packet().unwrap().pts.ticks(), Some(want), "{rate:?}");
            }
            let _ = fs::remove_dir_all(&dir);
        }
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vaco-image2-multi-test-{}-{tag}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sequence_mode_reads_files_in_order() {
        let dir = scratch_dir("seq_order");
        fs::write(dir.join("out001.png"), b"one").unwrap();
        fs::write(dir.join("out002.png"), b"two").unwrap();
        fs::write(dir.join("out003.png"), b"three").unwrap();
        let pattern = dir.join("out%03d.png");
        let pattern = pattern.to_str().unwrap();

        let mut d = Image2Demuxer::open_pattern(
            pattern,
            Image2Options {
                start_number: 1,
                ..Image2Options::default()
            },
        )
        .unwrap();
        assert_eq!(d.read_packet().unwrap().payload(), b"one");
        assert_eq!(d.read_packet().unwrap().payload(), b"two");
        assert_eq!(d.read_packet().unwrap().payload(), b"three");
        assert!(matches!(d.read_packet(), Err(Error::Eof)));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sequence_mode_searches_the_start_number_range() {
        let dir = scratch_dir("seq_range");
        fs::write(dir.join("out010.png"), b"x").unwrap();
        let pattern = dir.join("out%03d.png");
        let pattern = pattern.to_str().unwrap();

        assert!(
            Image2Demuxer::open_pattern(
                pattern,
                Image2Options {
                    start_number: 0,
                    start_number_range: 5,
                    ..Image2Options::default()
                }
            )
            .is_err()
        );

        assert!(
            Image2Demuxer::open_pattern(
                pattern,
                Image2Options {
                    start_number: 6,
                    start_number_range: 5,
                    ..Image2Options::default()
                }
            )
            .is_ok()
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn glob_mode_reads_matches_in_sorted_order() {
        let dir = scratch_dir("glob_order");
        fs::write(dir.join("b.png"), b"B").unwrap();
        fs::write(dir.join("a.png"), b"A").unwrap();
        let pattern = dir.join("*.png");
        let pattern = pattern.to_str().unwrap();

        let mut d = Image2Demuxer::open_pattern(
            pattern,
            Image2Options {
                pattern_type: PatternType::Glob,
                ..Image2Options::default()
            },
        )
        .unwrap();
        assert_eq!(d.read_packet().unwrap().payload(), b"A");
        assert_eq!(d.read_packet().unwrap().payload(), b"B");
        assert!(matches!(d.read_packet(), Err(Error::Eof)));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn disabled_pattern_reads_the_literal_file_once() {
        let dir = scratch_dir("disabled");
        fs::write(dir.join("out.png"), b"only").unwrap();
        let path = dir.join("out.png");

        let mut d = Image2Demuxer::open_pattern(
            path.to_str().unwrap(),
            Image2Options {
                pattern_type: PatternType::Disabled,
                ..Image2Options::default()
            },
        )
        .unwrap();
        assert_eq!(d.read_packet().unwrap().payload(), b"only");
        assert!(matches!(d.read_packet(), Err(Error::Eof)));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn loop_input_restarts_the_sequence() {
        let dir = scratch_dir("loop");
        fs::write(dir.join("out001.png"), b"one").unwrap();
        let pattern = dir.join("out%03d.png");
        let pattern = pattern.to_str().unwrap();

        let mut d = Image2Demuxer::open_pattern(
            pattern,
            Image2Options {
                start_number: 1,
                loop_input: true,
                ..Image2Options::default()
            },
        )
        .unwrap();
        for _ in 0..5 {
            assert_eq!(d.read_packet().unwrap().payload(), b"one");
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn registry_open_reads_the_single_given_source_once() {
        use vaco_format_core::discovery::NoParsers;
        use vaco_io::MemorySource;
        let src = Box::new(MemorySource::new(b"whole file".to_vec()));
        let mut d = (DEMUXER_IMAGE2.open)(src, &NoParsers).unwrap();
        assert_eq!(d.read_packet().unwrap().payload(), b"whole file");
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    /// A pattern reached through the registry's frozen `open` cannot open
    /// anything real (the pattern string is not a file), but
    /// `Demuxer::bind_url` — called with the same URL right after `open` —
    /// rebinds to the real, already-correct multi-file resolution.
    #[test]
    fn registry_open_then_bind_url_reads_the_whole_pattern() {
        use vaco_format_core::discovery::NoParsers;
        use vaco_io::MemorySource;

        let dir = scratch_dir("registry-bind-url");
        fs::write(dir.join("img001.png"), b"one").unwrap();
        fs::write(dir.join("img002.png"), b"two").unwrap();
        let pattern = dir.join("img%03d.png");
        let pattern = pattern.to_str().unwrap();

        // The registry path's only option: a throwaway placeholder source,
        // exactly as a caller unable to open the literal pattern string
        // would pass.
        let placeholder = Box::new(MemorySource::new(Vec::new()));
        let mut d = (DEMUXER_IMAGE2.open)(placeholder, &NoParsers).unwrap();
        d.bind_url(pattern).unwrap();

        assert_eq!(d.read_packet().unwrap().payload(), b"one");
        assert_eq!(d.read_packet().unwrap().payload(), b"two");
        assert!(matches!(d.read_packet(), Err(Error::Eof)));

        // A second bind is refused rather than silently re-resolving.
        assert!(matches!(d.bind_url(pattern), Err(Error::Unsupported(_))));

        let _ = fs::remove_dir_all(&dir);
    }
}
