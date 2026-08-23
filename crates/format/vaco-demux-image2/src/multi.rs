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

use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::time::{TIME_BASE_Q, duration_from_rate};
use vaco_format_core::{
    Demuxer, DemuxerDesc, FormatFlags, ParserProvider, SeekFlags, SeekTarget, Stream,
};
use vaco_io::MediaSource;
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

use crate::fsutil;
use crate::pattern::SequencePattern;

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

        let mut stream = Stream::new(0, MediaType::Video, TIME_BASE_Q);
        stream.params.media_type = Some(MediaType::Video);

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
            TsFromFile::None => duration_from_rate(self.options.framerate)
                .unwrap_or(Duration::ZERO)
                .0
                .saturating_mul(i64::try_from(self.frame_number).unwrap_or(i64::MAX)),
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
        let pts = self.pts_ticks(&path);
        let mut packet = Packet::from_slice(&mut self.budget, &bytes)?;
        packet.pts = Timestamp::new(pts);
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
        packet.pts = Timestamp::ZERO;
        packet.dts = Timestamp::ZERO;
        packet.flags = PacketFlags::KEY;
        Ok(packet)
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        Err(Error::NotSeekable)
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
    let mut stream = Stream::new(0, MediaType::Video, TIME_BASE_Q);
    stream.params.media_type = Some(MediaType::Video);
    let remaining = (!bytes.is_empty()).then_some(bytes);
    Ok(Box::new(SingleSourceDemuxer {
        stream,
        remaining,
        budget: Budget::new(Limits::permissive()),
    }))
}

fn probe_image2(data: &ProbeData<'_>) -> ProbeScore {
    // The reference selects `image2` almost entirely by filename pattern
    // (`%d` in the path) rather than content, which this crate cannot see
    // from `ProbeData` alone reliably; fall back to the extension list every
    // still-image codec this crate knows about shares.
    if data.extension_matches(&[
        "png", "jpg", "jpeg", "bmp", "gif", "tif", "tiff", "webp", "ppm", "pgm", "pbm", "pam",
        "sgi", "dpx", "exr", "qoi",
    ]) {
        ProbeScore::EXTENSION
    } else {
        ProbeScore::NONE
    }
}

/// The `image2` demuxer's registry entry. See the module docs for the split
/// between this and [`Image2Demuxer::open_pattern`].
pub const DEMUXER_IMAGE2: DemuxerDesc = DemuxerDesc {
    name: "image2",
    long_name: "image2 sequence",
    extensions: &[],
    mime_types: &[],
    flags: FormatFlags::empty(),
    probe: probe_image2,
    open: open_boxed,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use std::fs;

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
}
