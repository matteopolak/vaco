//! Exit codes and the error vocabulary, measured rather than recalled.
//!
//! # The rule
//!
//! The reference's process status is **the negative `AVERROR` truncated to
//! eight bits**, not a small table of curated codes. Every value below was
//! observed from `ffmpeg` 8.1 with no pipe in the way (plan 13 §1b: a pipe
//! swallows `$?`, and the usual `${PIPESTATUS[0]}` repair is *bash* — `zsh`
//! spells it `$pipestatus[1]`, so in `zsh` the bash form expands to nothing and
//! the comparison silently passes):
//!
//! | invocation | `$?` | why |
//! |---|---|---|
//! | `ffmpeg` | 1 | usage, printed with the banner |
//! | `ffmpeg -i in.mkv` | 1 | "At least one output file must be specified" |
//! | `ffmpeg -i nope.mkv -f null -` | 254 | `ENOENT` = -2, `-2 as u8` = 254 |
//! | `ffmpeg -i . -f null -` | 235 | `EISDIR` = -21 |
//! | `ffmpeg -i script.sh -f null -` | 183 | `INVALIDDATA` = -1094995529, low byte `0xB7` |
//! | `ffmpeg -f null -` | 234 | `EINVAL` = -22 |
//! | `ffmpeg -qwerty 3 …` | 8 | `AVERROR_OPTION_NOT_FOUND`, low byte `0xF8` |
//! | `ffmpeg -i in.mkv -c:v nosuch -f null -` | 8 | `AVERROR_ENCODER_NOT_FOUND` |
//! | `ffmpeg -i nosuchproto://x -f null -` | 8 | `AVERROR_PROTOCOL_NOT_FOUND` |
//!
//! The four `FFERRTAG` values all end in `0xF8`, which is why four unrelated
//! failures share exit 8. That is a property of the tag construction, not a
//! coincidence, and [`AvError::exit`] reproduces it by arithmetic rather than by
//! a lookup table.
//!
//! Two failures are **not** an `AVERROR` at all and exit `1`: an empty argument
//! vector, and an argument vector with no output file. Both are the reference's
//! `show_usage`/`exit_program(1)` path.

use core::fmt;

/// The process exit status.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ExitCode(i32);

impl ExitCode {
    /// Success.
    pub const OK: Self = Self(0);
    /// The usage path: no arguments, or no output file.
    pub const USAGE: Self = Self(1);
    /// A run that reached `Finish::Complete` — no node errored — but a video
    /// stream real content was decoded for ended up with zero packets muxed.
    ///
    /// Not one more `AvError` truncation: measured directly, with no pipe in
    /// the way (see `exit.rs`'s own module doc for why that matters), from
    /// `ffmpeg -f h264 -i garbage.264 -c:v rawvideo -f rawvideo out.raw` and
    /// two variant reproductions (a different output container; a stream
    /// with a valid SPS/PPS but garbage slice data). All three print
    /// `Nothing was written into output file, because at least one of its
    /// streams received no packets.` then `Conversion failed!`, and all
    /// three exit **69** regardless of which distinct internal `AVERROR`
    /// each per-task thread hit (`Invalid data found when processing
    /// input`, `Invalid argument`, an internal decode-thread code) — a fixed
    /// top-level status the reference falls back to once a stream that
    /// should have received packets received none, not a `FFERRTAG` this
    /// crate's usual low-byte-of-the-negative-code arithmetic would produce.
    ///
    /// This build has no `-ss`/`-t`/`-frames` (nothing that could legitimately
    /// trim a stream to empty), so unlike the reference — which still exits 0
    /// with "Output file is empty, nothing was encoded" when the *cause* is a
    /// seek past end of input — every zero-packet video encode leg here is a
    /// real decode failure, not a user-requested empty range.
    pub const CONVERSION_FAILED: Self = Self(69);

    /// The value to hand `std::process::exit`.
    #[must_use]
    pub const fn code(self) -> i32 {
        self.0
    }

    #[must_use]
    pub const fn is_ok(self) -> bool {
        self.0 == 0
    }
}

impl fmt::Display for ExitCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One of the reference's error codes, with the string `av_strerror` prints
/// for it.
///
/// The *codes* are interface facts — they are observable in `$?` and in
/// ffprobe's `error.code` field — and the *strings* are fixed text keyed by the
/// code rather than prose, so both are reproduced under D9. The construction of
/// the four-character tags is `libavutil`'s documented `FFERRTAG`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AvError {
    /// The negative code, as ffprobe's `error.code` prints it.
    pub code: i64,
    /// What `av_strerror` renders it as.
    pub text: &'static str,
}

/// `FFERRTAG` — a negated little-endian four-character code.
const fn fferrtag(a: u8, b: u8, c: u8, d: u8) -> i64 {
    let tag = (a as i64) | ((b as i64) << 8) | ((c as i64) << 16) | ((d as i64) << 24);
    -tag
}

impl AvError {
    pub const ENOENT: Self = Self {
        code: -2,
        text: "No such file or directory",
    };
    pub const EACCES: Self = Self {
        code: -13,
        text: "Permission denied",
    };
    pub const EISDIR: Self = Self {
        code: -21,
        text: "Is a directory",
    };
    pub const EINVAL: Self = Self {
        code: -22,
        text: "Invalid argument",
    };
    pub const ENOSYS: Self = Self {
        code: -38,
        text: "Function not implemented",
    };
    pub const INVALIDDATA: Self = Self {
        code: -1_094_995_529,
        text: "Invalid data found when processing input",
    };
    pub const OPTION_NOT_FOUND: Self = Self {
        code: fferrtag(0xF8, b'O', b'P', b'T'),
        text: "Option not found",
    };
    pub const ENCODER_NOT_FOUND: Self = Self {
        code: fferrtag(0xF8, b'E', b'N', b'C'),
        text: "Encoder not found",
    };
    pub const DECODER_NOT_FOUND: Self = Self {
        code: fferrtag(0xF8, b'D', b'E', b'C'),
        text: "Decoder not found",
    };
    pub const MUXER_NOT_FOUND: Self = Self {
        code: fferrtag(0xF8, b'M', b'X', b'R'),
        text: "Muxer not found",
    };
    pub const DEMUXER_NOT_FOUND: Self = Self {
        code: fferrtag(0xF8, b'D', b'E', b'M'),
        text: "Demuxer not found",
    };
    pub const PROTOCOL_NOT_FOUND: Self = Self {
        code: fferrtag(0xF8, b'P', b'R', b'O'),
        text: "Protocol not found",
    };

    /// The process status this code produces: the low eight bits of the
    /// negative value, exactly as `exit(ret)` truncates it.
    #[must_use]
    pub const fn exit(self) -> ExitCode {
        ExitCode(((self.code as i32) as u8) as i32)
    }

    /// Translate a Vaco error into the reference's vocabulary.
    ///
    /// The `io::ErrorKind` split is load-bearing: `ffprobe -show_error` prints
    /// `code=-2`, `-13` and `-21` for three failures that are one
    /// [`vaco_core::Error`] variant, so collapsing them would change observable
    /// output.
    #[must_use]
    pub fn of(e: &vaco_core::Error) -> Self {
        match e {
            vaco_core::Error::Io(io) => match io.kind() {
                std::io::ErrorKind::NotFound => Self::ENOENT,
                std::io::ErrorKind::PermissionDenied => Self::EACCES,
                std::io::ErrorKind::IsADirectory => Self::EISDIR,
                _ => Self::EINVAL,
            },
            vaco_core::Error::Unsupported(_) => Self::ENOSYS,
            vaco_core::Error::InvalidData(_)
            | vaco_core::Error::Eof
            | vaco_core::Error::UnexpectedEof => Self::INVALIDDATA,
            _ => Self::EINVAL,
        }
    }
}

/// A failure, as the user sees it: the exact stderr text and the exit status.
///
/// Every failure path in this crate constructs one of these instead of writing
/// to stderr directly, which is what makes the wording and the status testable
/// without spawning a process.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Diagnostic {
    /// stderr lines, in order, without trailing newlines.
    pub lines: Vec<String>,
    pub exit: ExitCode,
}

impl Diagnostic {
    /// A failure reported with `err`'s status and text.
    #[must_use]
    pub fn new(err: AvError, lines: Vec<String>) -> Self {
        Self {
            lines,
            exit: err.exit(),
        }
    }

    /// The usage path: exit 1, not an `AVERROR`.
    #[must_use]
    pub fn usage(lines: Vec<String>) -> Self {
        Self {
            lines,
            exit: ExitCode::USAGE,
        }
    }

    /// A clean-completing run (`Finish::Complete`, no node error) that still
    /// produced no packets for a video stream it decoded. See
    /// [`ExitCode::CONVERSION_FAILED`] for what this reproduces and why it is
    /// not an `AvError`.
    #[must_use]
    pub fn conversion_failed() -> Self {
        Self {
            lines: vec![
                "Nothing was written into output file, because at least one \
                 of its streams received no packets."
                    .to_owned(),
                "Conversion failed!".to_owned(),
            ],
            exit: ExitCode::CONVERSION_FAILED,
        }
    }

    /// The reference's three-line shape for a failure while opening inputs or
    /// outputs: the component's own line, the per-file line, then the summary.
    #[must_use]
    pub fn opening(err: AvError, detail: Vec<String>, what: &str, url: &str) -> Self {
        let mut lines = detail;
        lines.push(format!("Error opening {what} file {url}."));
        lines.push(format!("Error opening {what} files: {}", err.text));
        Self::new(err, lines)
    }

    /// The whole message, newline-terminated, as it reaches stderr.
    #[must_use]
    pub fn render(&self) -> String {
        let mut s = String::new();
        for l in &self.lines {
            s.push_str(l);
            s.push('\n');
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversion_failed_matches_the_reference() {
        // Measured 2026-08-29 on ffmpeg 9.0.1, no pipe in the way (`$?` read
        // directly): `ffmpeg -f h264 -i garbage.264 -c:v rawvideo -f
        // rawvideo out.raw` and two variant reproductions all exit 69 with
        // this exact two-line tail. See `ExitCode::CONVERSION_FAILED`'s doc.
        let d = Diagnostic::conversion_failed();
        assert_eq!(d.exit.code(), 69);
        assert_eq!(
            d.render(),
            "Nothing was written into output file, because at least one of \
             its streams received no packets.\n\
             Conversion failed!\n"
        );
    }

    #[test]
    fn measured_exit_codes() {
        // Every row here was read from `$?` of a real ffmpeg 8.1 run with no
        // pipe between the binary and the shell. See the module docs.
        assert_eq!(AvError::ENOENT.exit().code(), 254);
        assert_eq!(AvError::EISDIR.exit().code(), 235);
        assert_eq!(AvError::EINVAL.exit().code(), 234);
        assert_eq!(AvError::EACCES.exit().code(), 243);
        assert_eq!(AvError::INVALIDDATA.exit().code(), 183);
        assert_eq!(ExitCode::USAGE.code(), 1);
        assert_eq!(ExitCode::OK.code(), 0);
    }

    #[test]
    fn every_four_character_tag_exits_eight() {
        // Not a coincidence and not four separate observations: the tags all
        // begin 0xF8, so the truncation lands on 8 for all of them. Observed
        // for OPTION, ENCODER and PROTOCOL; asserted here for the rest.
        for e in [
            AvError::OPTION_NOT_FOUND,
            AvError::ENCODER_NOT_FOUND,
            AvError::DECODER_NOT_FOUND,
            AvError::MUXER_NOT_FOUND,
            AvError::DEMUXER_NOT_FOUND,
            AvError::PROTOCOL_NOT_FOUND,
        ] {
            assert_eq!(e.exit().code(), 8, "{e:?}");
            assert!(e.code < 0);
        }
    }

    #[test]
    fn option_not_found_has_the_documented_value() {
        // -0x54504FF8: the negated MKTAG of (0xF8, 'O', 'P', 'T').
        assert_eq!(AvError::OPTION_NOT_FOUND.code, -0x5450_4FF8);
    }

    #[test]
    fn error_translation_keeps_the_three_io_kinds_apart() {
        use std::io::{Error as IoError, ErrorKind};
        let e = |k| vaco_core::Error::Io(IoError::from(k));
        assert_eq!(AvError::of(&e(ErrorKind::NotFound)), AvError::ENOENT);
        assert_eq!(
            AvError::of(&e(ErrorKind::PermissionDenied)),
            AvError::EACCES
        );
        assert_eq!(AvError::of(&e(ErrorKind::IsADirectory)), AvError::EISDIR);
        assert_eq!(AvError::of(&e(ErrorKind::BrokenPipe)), AvError::EINVAL);
        assert_eq!(
            AvError::of(&vaco_core::Error::Unsupported("x")),
            AvError::ENOSYS
        );
        assert_eq!(
            AvError::of(&vaco_core::Error::InvalidData("x")),
            AvError::INVALIDDATA
        );
    }

    #[test]
    fn opening_renders_the_reference_shape() {
        let d = Diagnostic::opening(
            AvError::ENOENT,
            vec!["[in#0] Error opening input: No such file or directory".to_owned()],
            "input",
            "nope.mkv",
        );
        assert_eq!(
            d.render(),
            "[in#0] Error opening input: No such file or directory\n\
             Error opening input file nope.mkv.\n\
             Error opening input files: No such file or directory\n"
        );
        assert_eq!(d.exit.code(), 254);
    }
}
