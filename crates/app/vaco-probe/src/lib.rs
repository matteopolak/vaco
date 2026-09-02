//! The `ffprobe`-equivalent: open a container, describe it, print nothing else.
//!
//! # What it is
//!
//! `vaco-probe` turns an argument vector and a media file into bytes on a
//! stream. It is the v0.1 acceptance surface (D5), and the acceptance criterion
//! is **byte identity** with the reference (D6) — not "equivalent information",
//! not "structurally the same JSON". A trailing space is a failure.
//!
//! # How it works
//!
//! Four crates do most of the work, and this one is the wiring plus one table:
//!
//! | Crate | Owns |
//! |---|---|
//! | `vaco-cli-core` | the option table, the specifier grammar, the scope model |
//! | `vaco-registry` | which demuxers exist |
//! | `vaco-format-core` | probing, and the [`Demuxer`](vaco_format_core::Demuxer) trait |
//! | `vaco-textformat` | the six writers, and every number that reaches them |
//!
//! What is left, and what this crate is really *for*, is
//! [`fields`] — **which** fields, in **what order**, with **what spelling**,
//! and **integer or string**. Nothing else decides that, nothing derives it,
//! and it was measured rather than reasoned about. Read [`fields`] first.
//!
//! The run is:
//!
//! ```text
//! argv ──▶ [cli]      the option set                          (vaco-cli-core)
//!      ──▶ [listing]  -formats/-sections/… print and exit
//!      ──▶ [open]     protocol → IoContext → probe → demuxer  (vaco-io, -format-core)
//!      ──▶ [show]     one section per -show_* flag             (this crate)
//!      ──▶ [packets]  one pass serving -show_packets,
//!                     -count_packets, -select_streams and
//!                     -read_intervals together               (this crate)
//!      ──▶ [writer]   bytes                                    (vaco-textformat)
//! ```
//!
//! [`packets`] is one pass because the reference makes it one pass, and the
//! observable consequence is that `-count_packets -read_intervals '%+#3'`
//! reports 3 rather than the file's total.
//!
//! # A correction worth stating plainly
//!
//! **`ffprobe file.mp4`, with no other options, prints nothing on stdout.**
//! Everything it shows — the version banner, the build configuration, the
//! `Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'file.mp4':` block — goes to
//! *stderr*, from the logging layer, not from the section writers. Verified:
//!
//! ```sh
//! ffprobe av.mp4 2>/dev/null | wc -c   # 0
//! ```
//!
//! So the stdout acceptance target is `-show_streams` / `-show_format` and
//! their relatives, which is what this crate implements byte-identically. The
//! stderr banner is a *different* target, and half of it — `ffprobe version
//! 8.1`, the Homebrew configure line, `libavutil 60. 26.100` — is `FFmpeg`'s
//! identity, which we must not print. [`banner`] emits Vaco's own, in the same
//! shape. See `docs/app/vaco-probe.md`.
//!
//! # How to change it
//!
//! A change to observable output needs a reference run in the commit. The
//! captured bytes live in `tests/reference.rs` with the invocation that
//! produced each one; `tests/fields.rs` asserts the emitters follow
//! [`fields`]'s tables in order. Neither will let a field move quietly.
//!
//! # Configuration
//!
//! No environment variables and no config files. Everything is an option, and
//! every option is in `vaco_cli_core::table::ffprobe()`.

#![forbid(unsafe_code)]

pub mod cli;
pub mod dump;
pub mod emit;
pub mod fields;
pub mod intervals;
pub mod listing;
pub mod packets;
pub mod show;

use std::io::Write;

use vaco_core::{Error, Result};
use vaco_format_core::{Demuxer, DemuxerDesc, Discovery, FormatOptions, Probe, Stream};
use vaco_io::{IoContext, IoOptions};
use vaco_limits::Limits;
use vaco_textformat::sections::SectionId;
use vaco_textformat::{EntryFilterSet, TextFormat, writers};

pub use cli::{Listing, Options, Show};
pub use emit::{Emit, Val};

/// Why `-show_frames` and `-count_frames` fail instead of printing nothing.
///
/// D5 gives v0.1 zero decoders, and a frame section reports **decoded** frame
/// properties. D14.4 moved `-show_frames`, `-count_frames` and
/// `-analyze_frames` to v0.2 for exactly that reason.
///
/// The alternative — an empty `[FRAMES]` array and exit 0 — is worse than a
/// refusal, because it is indistinguishable from "this file has no frames".
/// A differential harness records that as a pass. `vaco-cli` sets the
/// precedent with `AvError::ENOSYS` for its unimplemented listings: a gap you
/// can see beats a gap that looks like an empty answer.
pub const FRAMES_UNSUPPORTED: &str = "-show_frames/-count_frames need a decoder; v0.1 has none (D5, D14.4 \u{2014} roadmap CL-34b/v0.2)";

/// CLI-option audit: `-analyze_frames`, `-cpuflags`, `-find_stream_info`,
/// `-max_alloc`, `-report`, `-show_log`, `-sinks`, `-sources` are declared
/// by this build's option table, parsed, and validated, and none of them
/// reaches any consumer -- the ffprobe half of the same gap `vaco-cli`'s
/// own `refuse_unimplemented_options` closed. [`Error::Unsupported`] takes
/// `&'static str`, so this is a fixed table rather than a `format!`, one
/// literal per name in [`crate::cli::UNIMPLEMENTED`] -- kept next to each
/// other so an added name that is missing here is a compile-time-obvious
/// gap (the `unreachable!` below), not a silently generic message.
#[must_use]
pub fn unimplemented_option_message(name: &str) -> &'static str {
    match name {
        "analyze_frames" => "-analyze_frames is accepted by this build's option table but not implemented yet.",
        "cpuflags" => "-cpuflags is accepted by this build's option table but not implemented yet.",
        "find_stream_info" => "-find_stream_info is accepted by this build's option table but not implemented yet.",
        "max_alloc" => "-max_alloc is accepted by this build's option table but not implemented yet.",
        "report" => "-report is accepted by this build's option table but not implemented yet.",
        "show_log" => "-show_log is accepted by this build's option table but not implemented yet.",
        "sinks" => "-sinks is accepted by this build's option table but not implemented yet.",
        "sources" => "-sources is accepted by this build's option table but not implemented yet.",
        // Not reachable while `cli::UNIMPLEMENTED` and this match agree, but
        // a mismatch is a bug in this file, not malformed input -- so it
        // gets a generic-but-honest message instead of a panic, the same
        // "never panic on a state this code itself controls" rule as
        // everywhere else in this tree.
        _ => "an option is accepted by this build's table but not implemented yet.",
    }
}

/// This program's version, as `program_version.version` prints it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The process exit code. `0` for success, `1` for **any** failure — including
/// "no input file specified" and an unopenable URL. Observed:
/// `ffprobe nonexistent.mp4; echo $?` prints `1`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Exit {
    Ok,
    Failure,
}

impl Exit {
    #[must_use]
    pub const fn code(self) -> i32 {
        match self {
            Self::Ok => 0,
            Self::Failure => 1,
        }
    }
}

/// Run one invocation.
///
/// `argv` must not include the program name. Everything the program would print
/// goes to `out` or `err`; nothing is written to the real stdio, which is what
/// makes the whole binary testable and fuzzable.
pub fn run<S, O, E>(argv: &[S], out: &mut O, err: &mut E) -> Exit
where
    S: AsRef<std::ffi::OsStr>,
    O: Write,
    E: Write,
{
    run_with_limits(argv, out, err, Limits::permissive())
}

/// [`run`], with the packet loop's safety bound supplied by the caller.
///
/// Exists for the fuzz target, which needs a hostile input to terminate in
/// milliseconds rather than in the four billion packets `permissive` allows.
/// It is a *safety* bound, never a correctness one: no real file comes near it,
/// and `-read_intervals` is what a user reaches for. See [`packets`].
pub fn run_with_limits<S, O, E>(argv: &[S], out: &mut O, err: &mut E, limits: Limits) -> Exit
where
    S: AsRef<std::ffi::OsStr>,
    O: Write,
    E: Write,
{
    let opts = match cli::parse(argv) {
        Ok(o) => o,
        Err(e) => {
            let _ = writeln!(err, "{e}");
            return Exit::Failure;
        }
    };
    for warning in &opts.interval_warnings {
        let _ = writeln!(err, "{warning}");
    }
    match execute(&opts, out, err, limits) {
        Ok(x) => x,
        Err(e) => {
            let _ = writeln!(err, "{e}");
            Exit::Failure
        }
    }
}

fn execute<O: Write, E: Write>(
    opts: &Options,
    out: &mut O,
    err: &mut E,
    limits: Limits,
) -> Result<Exit> {
    if !opts.hide_banner && opts.listing != Some(Listing::Version) {
        banner(err)?;
    }
    if let Some(which) = opts.listing {
        version_listing(out, which)?;
        listing::render(out, which)?;
        return Ok(Exit::Ok);
    }

    // CLI-option audit: an accepted-but-never-consumed option, refused
    // before anything is written for the same reason as the two checks
    // below -- see [`Options::unimplemented`]'s own doc.
    if let Some(name) = opts.unimplemented {
        return Err(Error::Unsupported(unimplemented_option_message(name)));
    }

    // Refuse before anything is written, so the failure cannot be mistaken for
    // an empty section. See [`FRAMES_UNSUPPORTED`].
    if opts.show.frames || opts.show.count_frames {
        return Err(Error::Unsupported(FRAMES_UNSUPPORTED));
    }
    // Same rule for the five hash algorithms this build knows the name of but
    // cannot compute. See [`dump::HASH_UNSUPPORTED`].
    if opts.show_data_hash.is_some_and(|a| !a.implemented()) {
        return Err(Error::Unsupported(dump::HASH_UNSUPPORTED));
    }

    // The writer is built *before* the input is checked, because the reference
    // resolves it first: `ffprobe -of nonesuch` with no input at all reports
    // the unknown writer, not the missing file. Observed.
    let mut writer = Writer::new(opts, out)?;

    let Some(input) = opts.input.clone() else {
        // Exit 1, and the empty document still goes to stdout: the three
        // writers with a document prologue emit it even though nothing was
        // read. Observed, `ffprobe -hide_banner -of <w>` with stdin closed:
        //
        //   default/compact/csv/flat  ->  (nothing)
        //   ini                       ->  "# ffprobe output\n\n"
        //   json                      ->  "{\n\n}\n"
        //   xml                       ->  prologue + "<ffprobe>\n</ffprobe>\n"
        //
        // The two usage lines are ours, not the reference's (D9); the exit
        // code and the stdout bytes are behaviour and are reproduced.
        writer.empty_document()?;
        writer.finish()?;
        writeln!(err, "You have to specify one input file.")?;
        writeln!(err, "Use -h to get full help.")?;
        return Ok(Exit::Failure);
    };
    let url = input.to_string_lossy().into_owned();

    let opened = open(&url, opts.force_format.as_deref());

    match opened {
        Err(e) => {
            // `-show_error` turns the failure into a section. Without it the
            // message goes to stderr — but the document is still *opened*, so a
            // writer with a prologue still emits one. Observed:
            // `ffprobe -hide_banner -of json nope.mp4` prints "{\n\n}\n", and
            // `-of xml` prints the prologue and an empty `<ffprobe>` element.
            //
            // Getting this wrong produced a bare "\n" from the json writer —
            // `fini` running without `init`, which is not merely a missing
            // document but a malformed one.
            if opts.show.error {
                writer.error_document(&e)?;
            } else {
                writer.empty_document()?;
            }
            writer.finish()?;
            // `<url>: <strerror>`, exactly as the reference words it. The
            // underlying `Error`'s own Display carries a Rust-shaped
            // "(os error 2)" tail the reference does not print.
            writeln!(err, "{url}: {}", error_report(&e).1)?;
            Ok(Exit::Failure)
        }
        Ok(mut input) => {
            writer.document(opts, &mut input, &url, limits)?;
            writer.finish()?;
            Ok(Exit::Ok)
        }
    }
}

/// A successfully opened input, with everything the `format` section needs.
struct Input {
    demuxer: Box<dyn Demuxer>,
    /// Copied out of the registry rather than borrowed: `Probe` ties the
    /// descriptor's lifetime to the `FormatOptions` it was built with, and
    /// `DemuxerDesc` is `Copy`, so taking a copy is both cheaper and simpler
    /// than keeping that borrow alive.
    desc: DemuxerDesc,
    probe_score: i64,
    size: Option<u64>,
}

/// Open a URL, probe it, and construct its demuxer.
///
/// `file:` and `pipe:` are registered directly rather than through the
/// registry: neither ships a `vaco-component.toml`, so
/// `vaco_registry::protocol_registry()` is empty. That is a fragment those
/// crates owe, not something to paper over here — but a probe tool that cannot
/// open a file is useless, so the two are added explicitly and the gap is
/// reported. See `docs/app/vaco-probe.md`.
fn open(url: &str, force: Option<&str>) -> Result<Input> {
    use vaco_io::CancelToken;
    use vaco_opts::Dict;
    use vaco_protocol_core::{IoFlags, ProtocolEnv};

    let mut protocols = vaco_registry::protocol_registry();
    vaco_protocol_file::register(&mut protocols);

    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&protocols, &cancel);
    let opener = |url: &str| -> Result<Box<dyn vaco_io::MediaSource>> {
        protocols
            .open(url, IoFlags::READ, &Dict::new(), &env)
            .map_err(|e| match e {
                // Unwrapped rather than re-wrapped: `error.code` is a POSIX
                // errno, and wrapping the transport failure in `Error::Option`
                // would lose the `io::ErrorKind` that decides it. Measured:
                // `ffprobe -show_error nonexistent.mp4` prints `code=-2`, and
                // it prints `-13` for a permission failure and `-21` for a
                // directory — three different kinds of the same variant.
                vaco_protocol_core::ProtocolError::Io(inner) => inner,
                other => Error::Option {
                    name: "i".to_owned(),
                    detail: other.to_string(),
                },
            })
    };

    let format_opts = FormatOptions::default();
    let probe = Probe::new(vaco_registry::demuxers(), &format_opts);

    // Probing and demuxing each need to own a source, and `IoContext` has no
    // way to give one back — no `into_inner`, no `into_source`. So a probed
    // open reads the URL twice: once through an `IoContext` that peeks (and
    // therefore leaves the position alone) and is then dropped, once for the
    // demuxer. `-f` skips probing entirely and opens once.
    //
    // Correct for a seekable transport and wrong for a pipe, which cannot be
    // reopened. The fix belongs in `vaco-io` as `IoContext::into_source`;
    // reported, and recorded in the doc file.
    let (desc, score, size): (DemuxerDesc, _, _) = if let Some(name) = force {
        let detected = probe.force(name)?;
        let io = IoContext::new(opener(url)?, &IoOptions::default())?;
        (*detected.desc, detected.score, io.size())
    } else {
        let mut io = IoContext::new(opener(url)?, &IoOptions::default())?;
        let size = io.size();
        let detected = probe.detect(&mut io, Some(url), None)?;
        (*detected.desc, detected.score, size)
    };

    let inner = (desc.open)(opener(url)?, &vaco_registry::Parsers)?;

    // Run stream discovery before anyone reads `streams()`.
    //
    // `read_header` is only allowed to report what the header states, and a
    // container that under-describes itself — Matroska's `start_time`,
    // MPEG-TS's codec parameters, anyone's frame rate — needs packets to fill
    // the rest in. `Discovery` is that pass: it reads a bounded prefix, refines
    // what it can, and replays every packet it consumed, so wrapping is
    // transparent to everything downstream.
    //
    // It has to be composed *here* because it is a wrapper, not a driver: a
    // demuxer owns its own I/O, so nothing below this point can run the loop.
    // Composing it once covers every container at once rather than pushing the
    // same derivation into each demuxer, where the shared rule would then be
    // disabled per-container by whoever filled the field in first.
    let mut discovery = Discovery::new(inner, desc.flags, &format_opts);
    // A failed pass is not a failed open: it keeps whatever it learned, and
    // `read_header` already gave us a usable stream list. Reporting six streams
    // of seven beats reporting a broken file.
    let _ = discovery.run(&vaco_registry::Parsers);

    Ok(Input {
        demuxer: Box::new(discovery),
        desc,
        probe_score: i64::from(score.value()),
        size,
    })
}

/// Everything that turns sections into bytes for one run.
struct Writer<'a, O: Write> {
    tf: TextFormat<&'a mut O>,
    policy: vaco_textformat::OptionalFields,
    /// `-bitexact`, which drops every `*_long_name` — see [`Emit::bitexact`].
    bitexact: bool,
}

impl<'a, O: Write> Writer<'a, O> {
    fn new(opts: &Options, out: &'a mut O) -> Result<Self> {
        let w = writers::make(&opts.writer)?;
        let filter: EntryFilterSet = opts.entries.clone();
        let tf = TextFormat::with_filter(w, out, opts.format_opts.clone(), filter);
        tf.validate()?;
        Ok(Self {
            tf,
            policy: opts.format_opts.show_optional_fields,
            bitexact: opts.bitexact,
        })
    }

    fn finish(self) -> Result<()> {
        self.tf.finish().map(|_| ())
    }

    /// A document with no sections in it at all.
    ///
    /// Not a no-op: `json`, `xml` and `ini` carry a document prologue and
    /// epilogue, so an empty run still produces bytes.
    fn empty_document(&mut self) -> Result<()> {
        self.tf.open(SectionId::ROOT)?;
        self.tf.close()
    }

    /// The `error` section as a whole document.
    fn error_document(&mut self, e: &Error) -> Result<()> {
        let (code, text) = error_report(e);
        self.tf.open(SectionId::ROOT)?;
        {
            let mut emit = Emit::new(&mut self.tf, self.policy).bitexact(self.bitexact);
            show::error(&mut emit, code, text)?;
        }
        self.tf.close()
    }

    /// One full document, in the reference's root-child order.
    ///
    /// The order is **not** the order the flags were written in, and not the
    /// order plan 14's contract §3.5 gives either. Observed with everything
    /// enabled at once (plan 14 §5.4):
    /// `program_version, library_versions, pixel_formats, packets, frames,
    /// programs, stream_groups, streams, chapters, format, error`.
    fn document(
        &mut self,
        opts: &Options,
        input: &mut Input,
        url: &str,
        limits: vaco_limits::Limits,
    ) -> Result<()> {
        let streams: Vec<Stream> = input.demuxer.streams().to_vec();
        let selected = select(opts, &streams);
        let selected_ids: Vec<u32> = selected.iter().map(|s| s.index).collect();

        self.tf.open(SectionId::ROOT)?;
        let mut emit = Emit::new(&mut self.tf, self.policy).bitexact(self.bitexact);

        if opts.show.program_version {
            program_version(&mut emit)?;
        }
        if opts.show.library_versions {
            library_versions(&mut emit)?;
        }
        if opts.show.pixel_formats {
            // Needs an "every pixel format" iterator that `vaco-pixfmt` does
            // not expose; the array is opened so the document shape is right.
            emit.tf().open(SectionId::PIXEL_FORMATS)?;
            emit.tf().close()?;
        }
        // One pass serves `-show_packets` and `-count_packets` both, which is
        // why `-count_packets` alone still reads the whole file. `packets`
        // opens and closes the array itself, so the root-child position is
        // this statement's position.
        let counts = if opts.show.packets || opts.show.count_packets {
            packets::read(
                &mut emit,
                input.demuxer.as_mut(),
                &streams,
                packets::ReadOpts {
                    intervals: &opts.intervals,
                    selected: &selected_ids,
                    emit_packets: opts.show.packets,
                    payload: show::PayloadOpts {
                        data: opts.show_data,
                        hash: opts.show_data_hash,
                    },
                    limits,
                    // The demuxer's own flags, not a default: an empty
                    // `FormatFlags` is the *strictest* container, and the
                    // timestamp rules read it.
                    format_flags: input.desc.flags,
                    format_options: FormatOptions::default(),
                },
            )?
        } else {
            Vec::new()
        };
        // `-count_packets` is what puts a number in `nb_read_packets`; without
        // it the field is `N/A` even though the count is knowable. Observed,
        // and the reason the counter is an `Option` rather than a `u64`.
        let count_of = |index: u32| show::Counts {
            read_packets: opts.show.count_packets.then(|| {
                streams
                    .iter()
                    .position(|s| s.index == index)
                    .and_then(|i| counts.get(i))
                    .copied()
                    .unwrap_or(0)
            }),
            // `-count_frames` never gets here: `execute` refuses it.
            read_frames: None,
        };
        // `-show_frames` never gets here either.
        let show_ids = input
            .desc
            .flags
            .contains(vaco_format_core::FormatFlags::SHOW_IDS);
        if opts.show.programs {
            emit.tf().open(SectionId::PROGRAMS)?;
            for p in input.demuxer.programs() {
                show::program(&mut emit, p, &streams, show_ids, &count_of)?;
            }
            emit.tf().close()?;
        }
        if opts.show.stream_groups {
            emit.tf().open(SectionId::STREAM_GROUPS)?;
            emit.tf().close()?;
        }
        if opts.show.streams {
            emit.tf().open(SectionId::STREAMS)?;
            for s in selected.iter().copied() {
                show::stream(&mut emit, s, show_ids, count_of(s.index))?;
            }
            emit.tf().close()?;
        }
        if opts.show.chapters {
            emit.tf().open(SectionId::CHAPTERS)?;
            for c in input.demuxer.chapters() {
                show::chapter(&mut emit, c)?;
            }
            emit.tf().close()?;
        }
        if opts.show.format {
            let info = show::FormatInfo {
                filename: opts.print_filename.as_deref().unwrap_or(url),
                format_name: input.desc.name,
                format_long_name: input.desc.long_name,
                probe_score: input.probe_score,
                size: input.size,
                nb_programs: input.demuxer.programs().len(),
                nb_stream_groups: 0,
            };
            // Through `Discovery`, deliberately. It applies R14 — when a
            // container-level duration and per-stream durations disagree the
            // longest stream wins — which is the shared rule, and reading the
            // inner demuxer's field directly would bypass it.
            //
            // `vaco-probe` did read it directly for a while, because
            // `Discovery::duration()` preferred a `from_pts` input that
            // discovery filled from the *head* of the file while
            // `estimate_duration` treats it as a tail scan; every container
            // then reported the length of its own probe window. That is fixed
            // upstream — `from_pts` is left unset — and re-measured here across
            // twelve containers, all twelve agreeing with the reference either
            // way. The workaround is gone rather than left in place "just in
            // case", since a redundant one hides the next regression.
            let duration = input
                .demuxer
                .duration()
                .map(vaco_core::Duration::as_secs_f64);
            show::format(
                &mut emit,
                &info,
                &streams,
                duration,
                input.demuxer.metadata(),
            )?;
        }
        self.tf.close()
    }
}

/// Apply `-select_streams`.
///
/// It restricts the stream-scoped sections only: `format`, `chapter` and the
/// version sections are unaffected, and a program's membership list still names
/// every member. Plan 14 §5.4.
fn select<'a>(opts: &Options, streams: &'a [Stream]) -> Vec<&'a Stream> {
    let Some(spec) = &opts.select else {
        return streams.iter().collect();
    };
    let infos: Vec<vaco_cli_core::StreamInfo> = streams.iter().map(stream_info).collect();
    let ctx = vaco_cli_core::MatchCtx::streams(&infos);
    streams
        .iter()
        .filter(|s| spec.matches(&ctx, s.index))
        .collect()
}

/// The specifier grammar's view of a container stream.
fn stream_info(s: &Stream) -> vaco_cli_core::StreamInfo {
    let mut tags = vaco_core::Dict::new();
    for (k, v) in &s.metadata {
        tags.set(k, v);
    }
    vaco_cli_core::StreamInfo {
        index: s.index,
        id: s.id.unwrap_or(0),
        media_type: s.media_type(),
        disposition: vaco_cli_core::Disposition::from_bits(s.disposition.bits()),
        tags,
        codec_known: s.params.codec_id.is_some(),
        width: s.params.video.as_ref().map_or(0, |v| v.width),
        height: s.params.video.as_ref().map_or(0, |v| v.height),
        sample_rate: s.params.audio.as_ref().map_or(0, |a| a.sample_rate),
    }
}

/// `error.code` and `error.string`, which are one decision and not two.
///
/// The reference prints a negative POSIX errno and the matching `strerror`
/// text, and for its own failures a four-character-code error and the text that
/// goes with it. Observed, under `LC_ALL=C`:
///
/// ```sh
/// ffprobe -v quiet -show_error -of flat nonexistent.mp4  # -2           No such file or directory
/// ffprobe -v quiet -show_error -of flat a-directory      # -21          Is a directory
/// ffprobe -v quiet -show_error -of flat unreadable.bin   # -13          Permission denied
/// ffprobe -v quiet -show_error -of flat one-byte.bin     # -1094995529  Invalid data found when processing input
/// ```
///
/// The last one is `AVERROR_INVALIDDATA`, a negated `MKTAG('I','N','D','A')`
/// rather than an errno. It is reproduced as the number it is: scripts read
/// `error.code`, and D17 says to match the reference in observable output even
/// where the value is an artefact of how the reference is built. The *text* is
/// reproduced for the same reason — it is a fixed string keyed by the code, not
/// prose we would be paraphrasing.
///
/// Anything we cannot place maps to `EINVAL`, the reference's own catch-all.
fn error_report(e: &Error) -> (i64, &'static str) {
    const ENOENT: (i64, &str) = (-2, "No such file or directory");
    const EACCES: (i64, &str) = (-13, "Permission denied");
    const EISDIR: (i64, &str) = (-21, "Is a directory");
    const EINVAL: (i64, &str) = (-22, "Invalid argument");
    const ENOSYS: (i64, &str) = (-38, "Function not implemented");
    const INVALIDDATA: (i64, &str) = (-1_094_995_529, "Invalid data found when processing input");

    match e {
        Error::Io(io) => match io.kind() {
            std::io::ErrorKind::NotFound => ENOENT,
            std::io::ErrorKind::PermissionDenied => EACCES,
            std::io::ErrorKind::IsADirectory => EISDIR,
            _ => EINVAL,
        },
        Error::Unsupported(_) => ENOSYS,
        // A file that exists and is not a container this build recognises.
        Error::InvalidData(_) | Error::Eof | Error::UnexpectedEof => INVALIDDATA,
        _ => EINVAL,
    }
}

/// The stderr banner.
///
/// The reference prints its version, its configure line and nine library
/// versions here. We print ours: reproducing `FFmpeg`'s would be claiming to be
/// `FFmpeg`, and D9 puts help and identity text outside what is reproduced. The
/// *shape* is the same so that `-hide_banner` means the same thing.
///
/// # Errors
/// Propagates the sink's I/O error.
pub fn banner<W: Write>(w: &mut W) -> Result<()> {
    writeln!(
        w,
        "vaco-probe version {VERSION} Copyright (c) 2026 the Vaco authors"
    )?;
    Ok(())
}

/// `-version`, `-L` and `-buildconf`, which print before any listing.
fn version_listing<W: Write>(w: &mut W, which: Listing) -> Result<()> {
    match which {
        Listing::Version => {
            writeln!(w, "vaco-probe version {VERSION}")?;
        }
        Listing::License => {
            writeln!(w, "vaco-probe is licensed under MIT OR Apache-2.0.")?;
        }
        Listing::BuildConf => {
            writeln!(w, "configuration:")?;
        }
        Listing::Help => {
            writeln!(w, "Simple multimedia streams analyzer")?;
            writeln!(w, "usage: vaco-probe [OPTIONS] INPUT_FILE")?;
        }
        _ => {}
    }
    Ok(())
}

/// The `program_version` section.
///
/// Never byte-compared: plan 13 §1.3.2's `strip-sections` normaliser removes
/// this and `library_versions` from both sides, because they identify the
/// producing software.
fn program_version<W: Write>(e: &mut Emit<'_, W>) -> Result<()> {
    let t = fields::PROGRAM_VERSION;
    e.tf().open(SectionId::PROGRAM_VERSION)?;
    e.field(t, "version", &Val::s(VERSION))?;
    e.field(
        t,
        "copyright",
        &Val::s("Copyright (c) 2026 the Vaco authors"),
    )?;
    e.field(t, "compiler_ident", &Val::s("rustc"))?;
    e.field(t, "configuration", &Val::s(""))?;
    e.tf().close()
}

/// The `library_versions` array.
///
/// Vaco has no `libav*`, so the array is empty rather than populated with
/// invented names. Same normalisation note as [`program_version`].
fn library_versions<W: Write>(e: &mut Emit<'_, W>) -> Result<()> {
    e.tf().open(SectionId::LIBRARY_VERSIONS)?;
    e.tf().close()
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    fn run_str(args: &[&str]) -> (Exit, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(args, &mut out, &mut err);
        (
            code,
            String::from_utf8_lossy(&out).into_owned(),
            String::from_utf8_lossy(&err).into_owned(),
        )
    }

    /// No input file: **exit 1**, usage on stderr, and the writer's empty
    /// document on stdout.
    ///
    /// The exit code here was reported as 0 in an earlier revision of this
    /// crate, from `ffprobe 2>&1 | tail -5; echo $?` — which is `tail`'s status,
    /// not `ffprobe`'s. Plan 13 §1b applied to the exit-code channel: the layer
    /// between you and the answer has opinions. Measured directly:
    ///
    /// ```sh
    /// ffprobe </dev/null >/dev/null 2>/dev/null; echo $?   # 1
    /// ```
    #[test]
    fn no_input_exits_one_with_usage_on_stderr() {
        let (code, out, err) = run_str(&["-hide_banner"]);
        assert_eq!(code, Exit::Failure);
        assert!(out.is_empty(), "{out:?}");
        assert!(err.contains("input file"), "{err:?}");
    }

    /// The empty document is per writer, and three of the seven produce bytes.
    #[test]
    fn no_input_still_emits_the_writers_empty_document() {
        for (spec, want) in [
            ("default", ""),
            ("compact", ""),
            ("csv", ""),
            ("flat", ""),
            ("ini", "# ffprobe output\n\n"),
            ("json", "{\n\n}\n"),
            (
                "xml",
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ffprobe>\n</ffprobe>\n",
            ),
        ] {
            let (code, out, _) = run_str(&["-hide_banner", "-of", spec]);
            assert_eq!(code, Exit::Failure, "{spec}");
            assert_eq!(out, want, "{spec}");
        }
    }

    /// A failed open still produces the writer's document, with or without
    /// `-show_error`.
    ///
    /// The empty-document half of this was missing until the error-path corpus
    /// was widened past `-show_error` and the default writer; `json` was
    /// emitting a bare newline.
    #[test]
    fn a_failed_open_still_emits_a_well_formed_document() {
        for (spec, want) in [
            ("default", ""),
            ("flat", ""),
            ("ini", "# ffprobe output\n\n"),
            ("json", "{\n\n}\n"),
            (
                "xml",
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ffprobe>\n</ffprobe>\n",
            ),
        ] {
            let (code, out, _) = run_str(&["-hide_banner", "-of", spec, "/nonexistent/x.mp4"]);
            assert_eq!(code, Exit::Failure, "{spec}");
            assert_eq!(out, want, "{spec}");
        }
    }

    /// Every registered demuxer declares behavioural flags.
    ///
    /// `DemuxerDesc::flags` is a plain field, so a descriptor written without
    /// it compiles and silently reports `empty()` — and `empty()` is not a
    /// neutral answer. `TS_DISCONT` is the flag the discovery path reads and it
    /// *suppresses* the monotonic-DTS repair, so a container that lost it would
    /// have genuinely discontinuous timestamps quietly rewritten, with nothing
    /// in the output to show for it.
    ///
    /// Every real container declares something, so "declares nothing" is a
    /// reliable signal that the field was forgotten. This replaces the
    /// name-keyed transcription `vaco-probe` carried before the field existed;
    /// the check it guarded is worth keeping either way.
    #[test]
    fn every_registered_demuxer_declares_flags() {
        for d in vaco_registry::demuxers() {
            assert!(
                !d.flags.is_empty(),
                "demuxer `{}` declares no FormatFlags; `empty()` disables the \
                 monotonic-DTS repair decision rather than expressing one",
                d.name
            );
        }
    }

    /// The one flag whose value changes what we do, pinned per container.
    ///
    /// Not a restatement of the descriptors: an empty-flags regression would
    /// pass the test above only if it also lost `SHOW_IDS`, and a descriptor
    /// wired to the *wrong* crate's constant would pass it outright. These two
    /// assertions are the ones that would actually catch a mis-wire.
    #[test]
    fn timestamp_discontinuity_is_declared_per_container() {
        use vaco_format_core::FormatFlags;
        // MPEG-TS timestamps jump legitimately; the repair must stay off.
        if let Some(ts) = vaco_registry::demuxer_by_name("mpegts") {
            assert!(ts.flags.contains(FormatFlags::TS_DISCONT), "mpegts");
        }
        // MP4's do not; the repair must stay available.
        if let Some(mp4) = vaco_registry::demuxer_by_name("mp4") {
            assert!(!mp4.flags.contains(FormatFlags::TS_DISCONT), "mp4");
            assert!(mp4.flags.contains(FormatFlags::SHOW_IDS), "mp4 SHOW_IDS");
        }
    }

    /// An unrecognised writer name beats the missing-input check, because the
    /// reference resolves the writer first. Observed:
    /// `ffprobe -hide_banner -of nonesuch` reports the writer, not the file.
    #[test]
    fn an_unknown_writer_outranks_the_missing_input() {
        let (code, out, err) = run_str(&["-hide_banner", "-of", "nonesuch"]);
        assert_eq!(code, Exit::Failure);
        assert!(out.is_empty(), "{out:?}");
        assert!(!err.contains("input file"), "{err:?}");
    }

    #[test]
    fn a_missing_file_exits_one_and_says_so() {
        let (code, out, err) = run_str(&["-hide_banner", "/nonexistent/x.mp4"]);
        assert_eq!(code, Exit::Failure);
        assert!(out.is_empty(), "{out:?}");
        assert!(err.contains("/nonexistent/x.mp4"), "{err:?}");
    }

    #[test]
    fn a_missing_file_with_show_error_still_writes_the_section() {
        let (code, out, _) = run_str(&[
            "-hide_banner",
            "-show_error",
            "-of",
            "json",
            "/nonexistent/x.mp4",
        ]);
        assert_eq!(code, Exit::Failure);
        assert!(out.contains("\"error\""), "{out:?}");
        assert!(out.contains("\"code\""), "{out:?}");
    }

    #[test]
    fn the_error_codes_and_texts_are_the_observed_ones() {
        let io = |k| Error::Io(std::io::Error::from(k));
        assert_eq!(
            error_report(&io(std::io::ErrorKind::NotFound)),
            (-2, "No such file or directory")
        );
        assert_eq!(
            error_report(&io(std::io::ErrorKind::PermissionDenied)),
            (-13, "Permission denied")
        );
        assert_eq!(
            error_report(&io(std::io::ErrorKind::IsADirectory)),
            (-21, "Is a directory")
        );
        // AVERROR_INVALIDDATA, which is not an errno at all.
        assert_eq!(
            error_report(&Error::InvalidData("no demuxer recognised this input")),
            (-1_094_995_529, "Invalid data found when processing input")
        );
        assert_eq!(error_report(&Error::NotSeekable).0, -22);
    }

    #[test]
    fn sections_listing_exits_zero_without_an_input() {
        let (code, out, _) = run_str(&["-hide_banner", "-sections"]);
        assert_eq!(code, Exit::Ok);
        assert!(out.starts_with("Sections:\n"), "{out:?}");
    }

    #[test]
    fn the_banner_goes_to_stderr_and_hide_banner_removes_it() {
        let (_, _, err) = run_str(&["-sections"]);
        assert!(err.contains("vaco-probe version"), "{err:?}");
        let (_, _, err) = run_str(&["-hide_banner", "-sections"]);
        assert!(!err.contains("vaco-probe version"), "{err:?}");
    }

    #[test]
    fn an_unusable_writer_name_is_a_failure_not_a_panic() {
        let (code, _, err) = run_str(&["-hide_banner", "-of", "nonesuch", "x.mp4"]);
        assert_eq!(code, Exit::Failure);
        assert!(!err.is_empty());
    }

    #[test]
    fn arbitrary_argv_never_panics() {
        for args in [
            vec![],
            vec!["-"],
            vec!["--"],
            vec!["-of"],
            vec!["-of", "json"],
            vec!["-show_entries", "stream="],
            vec!["-select_streams", "!!"],
            vec!["-show_format", "-show_streams", "/dev/null"],
            vec!["-f", "nonesuch", "/dev/null"],
            vec!["-show_error", "/dev/null"],
        ] {
            let (mut o, mut e) = (Vec::new(), Vec::new());
            let _ = run(&args, &mut o, &mut e);
        }
    }
}
