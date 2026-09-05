//! Binding a split command line onto the options this binary understands.
//!
//! `vaco-cli-core` decides what a command line *means*: which options are
//! global, which bind to a file, which side of their file they must be on, and
//! how each value is spelled. This module is the next stage — taking that
//! structure and producing the two lists the run is built from.
//!
//! # The `AVOption` oracle
//!
//! The reference accepts `-crf 20` before any encoder is chosen because *some*
//! `AVOption` class in the process declares `crf`, and rejects `-qwerty 3`
//! because none does. That decision needs the component registry, so
//! `vaco-cli-core` takes it as an injected [`AvOptionOracle`].
//!
//! [`Oracle`] answers it from **what this build actually contains**, which is
//! the same rule the reference applies to itself. That was `FormatOptions` —
//! `probesize`, `fflags`, `protocol_whitelist` and the rest — and nothing
//! else for a long time, because there were no encoders with real options to
//! ask. `-b`/`-qscale` never needed this at all: they are generic options in
//! the reference's own `OptionDef` table (`crate::exec::codec_options_of`'s
//! own doc), not `AVOption`s discovered by search, so they live in
//! `vaco-cli-core`'s static option table instead. `vaco-codec-png`/`-tiff`/
//! `-exr`/`-ffv1`/`-vp8` are the first encoders with real *private*
//! `AVOption`-shaped options, and there is still no registry-wide reflection
//! that would let this crate search them the way the reference searches
//! every codec's `AVClass` — `Encoder::set_option` is a runtime "try this
//! key" interface, not something a caller can list ahead of construction —
//! so [`crate::exec::PRIVATE_ENCODER_OPTIONS`] is a hand-maintained mirror of
//! exactly what those encoders implement, grown as their own `set_option`
//! grows. `vaco -crf 20 …` still reports `Unrecognized option 'crf'` where
//! the reference accepts it (no encoder here implements `crf`), which is a
//! real, narrower divergence than before and still caused by the build's
//! contents rather than by the parser. The alternative — accepting every
//! unknown name — makes `-qwrty 3` a silent no-op, which is worse in exactly
//! the case a user needs help.

use std::ffi::OsStr;

use vaco_cli_core::num::strtol_base0;
use vaco_cli_core::split::AvOptionOracle;
use vaco_cli_core::{
    CliError, CommandLine, GroupKind, OptionGroup, ParsedOption, table::ArgFlags, table::ffmpeg,
};
use vaco_format_core::{FFlags, FormatOptions};
use vaco_opts::{Options as _, OptionsExt as _};

use crate::exit::{AvError, Diagnostic};
use crate::select::{MapEntry, Suppressed};

/// Answers "could this name be a component option?" from the components this
/// build has.
#[derive(Debug, Clone, Copy, Default)]
pub struct Oracle;

impl AvOptionOracle for Oracle {
    fn knows(&self, name: &str) -> bool {
        FormatOptions::default().schema().find(name).is_some()
            || crate::exec::PRIVATE_ENCODER_OPTIONS.contains(&name)
            || PRIVATE_DEMUXER_OPTIONS.contains(&name)
    }
}

/// Demuxer-private option names with a complete path through this binary.
const PRIVATE_DEMUXER_OPTIONS: &[&str] = &["decryption_key", "decryption_keys"];

/// One `-i` group, bound.
#[derive(Debug, Clone, Default)]
pub struct InputSpec {
    pub index: u32,
    pub url: String,
    /// `-f` on the input side.
    pub format: Option<String>,
    /// `-protocol_whitelist`, split on `,`.
    pub whitelist: Option<Vec<String>>,
    /// `-protocol_blacklist`, split on `,`.
    pub blacklist: Option<Vec<String>>,
    /// Every `-probesize`/`-analyzeduration`/`-fflags`/… (FW-11, the generic
    /// `AVFormatContext` options) this group named, folded onto the default —
    /// see [`format_options_of`]. Fed to [`crate::input::open`] and, through
    /// it, to [`vaco_format_core::Demuxer::reconfigure`] via
    /// [`vaco_format_core::Discovery::run`].
    pub format_opts: FormatOptions,
    /// MP4 `-decryption_key`, decoded from exactly 32 hexadecimal digits.
    pub decryption_key: Option<[u8; 16]>,
    /// MP4 `-decryption_keys`, decoded from `KID=KEY` dictionary entries.
    pub decryption_keys: Vec<vaco_demux_mp4::DecryptionKey>,
    /// `-ss` on this input: seek this far into the file before demuxing
    /// anything. See [`crate::seek_trim`].
    pub seek: Option<vaco_core::Duration>,
    /// `-t`/`-to` on this input, already resolved to one bound: [`EndBound`]
    /// tells [`crate::seek_trim`] whether it is relative to `seek` (`-t`) or
    /// absolute from the file's own start (`-to`). The reference gives `-t`
    /// priority when both are given on the same group.
    pub end: Option<EndBound>,
}

/// What `-t`/`-to` resolved to for one input group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndBound {
    /// `-t duration`: stop `duration` after `-ss` (or the file start, with
    /// no `-ss`).
    AfterSeek(vaco_core::Duration),
    /// `-to position`: stop at `position`, measured from the file's own
    /// start regardless of `-ss`.
    Absolute(vaco_core::Duration),
}

/// One output group, bound.
#[derive(Debug, Clone, Default)]
pub struct OutputSpec {
    pub index: u32,
    pub url: String,
    /// `-f` on the output side.
    pub format: Option<String>,
    pub maps: Vec<MapEntry>,
    pub blocked: Suppressed,
    /// Every generic `AVFormatContext` option this group named — the output
    /// side's share of FW-11 (`-avoid_negative_ts`, `-max_interleave_delta`,
    /// and the rest [`vaco_format_core::interleave`] and
    /// [`vaco_sched::spec::PipelineSpec::add_output_with`] consume).
    pub format_opts: FormatOptions,
    /// `-map_chapters <n>`. `None` when unstated (the reference's own
    /// default: copy from the first input file that has chapters); `Some(-1)`
    /// disables chapter copying outright.
    pub map_chapters: Option<i64>,
    /// `-map_metadata <n>`, the leading integer only — the reference's
    /// `outfile[,metadata]:infile[,metadata]` per-scope qualifiers are not
    /// implemented (CL-16 breadth phase; global-to-global is the overwhelming
    /// common case). `None` defaults to input `0`; `Some(-1)` disables.
    pub map_metadata: Option<i64>,
    /// `-attach <filename>`, in argv order — repeatable, one entry per
    /// attachment. `exec::metadata_of` reads the file and matches
    /// `-metadata:s:t:N mimetype=…` against its position in this list.
    pub attach: Vec<String>,
    /// `-t`/`-to` on this output: stop **writing** once a packet's own
    /// presentation time (in the muxer's chosen time base) reaches this
    /// bound. Unlike [`InputSpec::end`], there is no output-side `-ss` to be
    /// relative to — the reference's output-side `-ss` is a materially
    /// different feature (decode-and-discard until the timestamp, not a seek)
    /// that nothing here implements, and is refused explicitly rather than
    /// silently accepted (see `refuse_unimplemented_options`) — so
    /// [`EndBound::AfterSeek`] and [`EndBound::Absolute`] mean the same thing
    /// here: a bound measured from the output's own start. See
    /// [`crate::output_trim`].
    pub end: Option<EndBound>,
}

/// A whole invocation, bound.
#[derive(Debug, Default)]
pub struct Cli {
    pub hide_banner: bool,
    /// The resolved name of the first `-version`/`-formats`/`-h`/… option
    /// seen, if any. Resolved through [`ParsedOption::resolved`] rather than
    /// taken from the raw descriptor, because `-?`/`-help`/`--help` are all
    /// aliases of `-h` and must dispatch identically.
    pub listing: Option<&'static str>,
    /// The raw value that followed the listing option, if it took one.
    /// `-h`'s topic argument lives here (`-h full`, `-h decoder=x264`); the
    /// other listing options never take a value, so this is `None` for them.
    pub listing_value: Option<std::ffi::OsString>,
    pub inputs: Vec<InputSpec>,
    pub outputs: Vec<OutputSpec>,
    /// Trailing per-file options with no file after them. The reference drops
    /// these silently; kept so `vaco` can warn.
    pub orphaned: Vec<String>,
    /// The split command line, kept because per-stream option resolution needs
    /// the original groups.
    pub line: CommandLine,
    /// CL-25: every `-filter_complex`/`-lavfi` occurrence's text, in argv
    /// order — each is a separate, repeatable global graph. Parsed here so
    /// the option does not raise "unrecognized option"; **not yet wired into
    /// a real run** — see `crate::complexgraph`'s module doc for exactly
    /// what that would still take.
    pub complex_filters: Vec<String>,
    /// `-threads N`, the codec-side thread count. `None` when unstated, which
    /// resolves to [`default_thread_count`] -- **not** the reference's
    /// "auto": `-threads` unstated still means the same fixed small count on
    /// every machine, so a run's output provenance never depends on where it
    /// ran. `-threads 0` is explicitly stated and taken as one, matching the
    /// reference's own wording for that value rather than its "auto"
    /// behaviour, since nothing here auto-detects.
    pub threads: Option<usize>,
    /// `-filter_threads N` (D2). `None` when unstated, which resolves to
    /// [`default_thread_count`] via
    /// [`Cli::filter_thread_count`] -- reusing `-threads`' own small-fixed-
    /// count reasoning rather than the reference's raw-core-count "auto",
    /// for the same determinism reason. Declared `ValueKind::Str` in the
    /// option table (matching the reference's own declaration for it), so
    /// this is parsed with [`leading_int`] rather than [`ParsedOption::number`].
    pub filter_threads: Option<usize>,
    /// `-frame_drop_threshold`, in output-frame intervals. A negative value
    /// disables late-frame dropping; `None` keeps the reference default.
    pub frame_drop_threshold: Option<f64>,
}

impl Cli {
    /// The output group for `index`, for per-stream option lookups.
    #[must_use]
    pub fn output_group(&self, index: u32) -> Option<&OptionGroup> {
        self.line
            .of_kind(GroupKind::Output)
            .find(|g| g.index == index)
    }

    /// The input group for `index`, for per-stream option lookups against an
    /// *input* file's own stream numbering -- `-display_rotation`,
    /// `-display_hflip`, `-display_vflip` and `-autorotate` are all
    /// `INPUT, PER_STREAM` (measured, `ffmpeg 9.0.1 -h full`), unlike
    /// `-vf`/`-s`/`-aspect`/`-pix_fmt`, which [`Cli::output_group`] serves.
    #[must_use]
    pub fn input_group(&self, index: u32) -> Option<&OptionGroup> {
        self.line
            .of_kind(GroupKind::Input)
            .find(|g| g.index == index)
    }

    /// The thread count a run actually uses: `-threads N` if stated
    /// (including `-threads 1`, which forces serial), else
    /// [`default_thread_count`].
    #[must_use]
    pub fn thread_count(&self) -> usize {
        self.threads.unwrap_or_else(default_thread_count)
    }

    /// The filter/scale thread count a run actually uses: `-filter_threads
    /// N` if stated, else [`default_thread_count`] (D2) -- see
    /// [`Cli::filter_threads`]'s own doc for why this is the same default
    /// derivation `-threads` uses rather than the reference's raw core
    /// count. `vaco_scale::ScaleOptions::threads` (and every other consumer
    /// this reaches) treats `0` and `1` identically as "run on the calling
    /// thread", so there is no separate "0 means one" special case to make
    /// here the way [`Cli::thread_count`] has for decode.
    #[must_use]
    pub fn filter_thread_count(&self) -> usize {
        self.filter_threads.unwrap_or_else(default_thread_count)
    }
}

/// `-threads` unstated: `min(available_parallelism, 4)`.
///
/// H.264 frame threading is bit-identical to the serial decoder at every
/// thread count (`docs/codec/frame-threading.md`), but the count itself is
/// deliberately **not** `available_parallelism()` alone. Two reasons, both
/// measured on the row-granularity decoder that made this the default:
///
/// * **The scaling curve is already nearly flat past four.** 3.37x at four
///   threads against 3.78x at eight on a 4K all-P fixture, for roughly double
///   the memory -- eight buys 12% more speed for 100% more concurrent
///   pictures in flight.
/// * **The memory ceiling would otherwise be machine-dependent.** Each
///   in-flight picture is charged exactly to `Limits::max_alloc_total`
///   (`docs/codec/frame-threading.md`'s "Memory" section), so the thread
///   count is the one knob that decides how many multiply that charge.
///   `available_parallelism()` alone would make that ceiling depend on the
///   core count of whatever machine happens to run the decode, which is
///   exactly the kind of machine-dependence this decoder's own determinism
///   claim exists to avoid.
///
/// Falls back to one if the platform cannot report a core count at all
/// (`std::thread::available_parallelism`'s own documented failure case,
/// observed on some containers and restricted sandboxes) -- the same
/// single-threaded call sequence this decoder always had, never a hang or an
/// error over a knob nobody turned.
#[must_use]
pub fn default_thread_count() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get().min(4))
}

/// Whether argv asks for the banner to be printed.
///
/// A textual pre-scan, because the reference decides this *before* it parses:
/// `ffmpeg -qwerty 3` prints the banner and then the error, so the banner
/// cannot wait for a successful parse.
///
/// Re-exported rather than reimplemented: `ffprobe` answers the same question
/// the same way, and the answer turned out to depend on `-v`/`-loglevel` as
/// well as on `-hide_banner`. One definition, per D19 — see
/// [`vaco_cli_core::loglevel`] for the measurements behind it.
pub use vaco_cli_core::loglevel::wants_banner;

/// Split and bind `argv`.
///
/// # Errors
///
/// A [`Diagnostic`] carrying the reference's wording and exit status for a
/// parse failure, an option on the wrong side of its file, or a `-map` value
/// that does not parse.
pub fn parse<S: AsRef<OsStr>>(argv: &[S]) -> Result<Cli, Diagnostic> {
    let table = ffmpeg();
    let line = vaco_cli_core::split_with(&table, argv, &Oracle).map_err(|e| split_error(&e))?;
    line.validate().map_err(|e| split_error(&e))?;

    let listing_opt = line
        .global
        .iter()
        .find(|o| o.desc.is_some_and(|d| d.flags.contains(ArgFlags::EXIT)));

    let mut cli = Cli {
        hide_banner: line.last_global("hide_banner").is_some(),
        // The *resolved* name, not the raw descriptor's own: `-?`, `-help`
        // and `--help` are `alias_of`-redirected to `h`, so their own
        // `OptDesc::name` ("?", "help", "-help") would dispatch nowhere.
        // Every option reaching `listing_opt` has `desc.is_some()` by
        // construction (the `find` above requires it), so `d.name` is always
        // `'static` here — no need to go through `resolved()`, which would
        // borrow from `o` instead.
        listing: listing_opt
            .and_then(|o| o.desc)
            .map(|d| d.alias_of.map_or(d.name, |(target, _)| target)),
        listing_value: listing_opt.and_then(|o| o.value.clone()),
        orphaned: line
            .orphaned
            .iter()
            .map(|o| format!("-{}", o.name))
            .collect(),
        complex_filters: line
            .global
            .iter()
            .filter(|o| o.resolved().0 == "filter_complex")
            .map(value_str)
            .collect::<Result<Vec<_>, _>>()?,
        // `ParsedOption::number` applies the option's own declared grammar
        // and range (`ValueKind::Int`), so `-threads abc` is rejected with
        // the reference's wording rather than quietly meaning one.
        threads: line
            .last_global("threads")
            .map(|o| o.number().map_err(|e| split_error(&e)))
            .transpose()?
            .flatten()
            .map(|n| if n < 1.0 { 1 } else { n as usize }),
        // `filter_threads` is declared `ValueKind::Str` in the option table
        // (matching the reference's own declaration), so `ParsedOption::
        // number` -- gated to `Int`/`Int64`/`Float`/`Expr` -- always returns
        // `Ok(None)` for it; `leading_int` (the reference's own lenient
        // `strtol`-style parse for a numeric-looking string option) is the
        // right tool here, not a `split_error` on the first non-digit.
        filter_threads: line
            .last_global("filter_threads")
            .map(value_str)
            .transpose()?
            .map(|s| leading_int(&s).max(0) as usize),
        frame_drop_threshold: line
            .last_global("frame_drop_threshold")
            .map(|o| o.number().map_err(|e| split_error(&e)))
            .transpose()?
            .flatten(),
        ..Cli::default()
    };

    for g in line.of_kind(GroupKind::Input) {
        let url = url_of(g)?;
        let seek = duration_of(g, "ss", "input")?;
        let end = end_bound_of(g, "input")?;
        validate_bounds(g.index, seek, end, &url, "input")?;
        cli.inputs.push(InputSpec {
            index: g.index,
            url,
            format: last_value(g, "f")?,
            whitelist: last_value(g, "protocol_whitelist")?.map(|v| split_list(&v)),
            blacklist: last_value(g, "protocol_blacklist")?.map(|v| split_list(&v)),
            format_opts: format_options_of(g)?,
            decryption_key: decryption_key_of(g)?,
            decryption_keys: decryption_keys_of(g)?,
            seek,
            end,
        });
    }

    for g in line.of_kind(GroupKind::Output) {
        if g.last("decryption_key").is_some() {
            return Err(unimplemented_option("decryption_key"));
        }
        if g.last("decryption_keys").is_some() {
            return Err(unimplemented_option("decryption_keys"));
        }
        let url = url_of(g)?;
        let end = end_bound_of(g, "output")?;
        validate_bounds(g.index, None, end, &url, "output")?;
        let format_opts = format_options_of(g)?;
        if let Some(name) = format_opts.configured_mpegts_option() {
            return Err(input_only_option(name));
        }
        cli.outputs.push(OutputSpec {
            index: g.index,
            url,
            format: last_value(g, "f")?,
            maps: maps_of(g)?,
            blocked: Suppressed {
                video: g.last("vn").is_some(),
                audio: g.last("an").is_some(),
                subtitle: g.last("sn").is_some(),
                data: g.last("dn").is_some(),
            },
            format_opts,
            map_chapters: last_value(g, "map_chapters")?.as_deref().map(leading_int),
            map_metadata: last_value(g, "map_metadata")?.as_deref().map(leading_int),
            attach: attach_of(g)?,
            end,
        });
    }

    cli.line = line;
    refuse_unimplemented_options(&cli.line)?;
    Ok(cli)
}

/// Options this build's table declares and `split`/`Oracle` therefore accept,
/// but that nothing downstream of [`parse`] ever reads.
///
/// Before this existed they parsed successfully, exited 0, and had exactly
/// zero effect — the same defect `-ar` had: silently wrong is worse than
/// refusing, which applies just as much to an option that does nothing as
/// to one that resamples nothing.
/// Each of these is tracked as deferred work in `docs/app/vaco-cli.md`
/// (`-hwaccel`: CL-34a) — refusing does not
/// close any of those issues, it only stops the gap between "accepted" and
/// "implemented" from reading as "works".
/// `-print_graphs` (CL-27) is no longer in this list: `crate::print_graphs`
/// implements it, gated by [`crate::print_graphs::PrintGraphsSpec::resolve`]
/// rather than a blanket refusal. `-fps_mode`/`-enc_time_base` (CL-21) are
/// also gone: `crate::fps_mode`/`crate::enc_time_base` implement them, and
/// `-frame_drop_threshold` is consumed by `crate::fps_mode`'s late-frame
/// stage.
///
/// Deliberately narrow: this names exactly the options measured to have no
/// consuming code anywhere in `vaco-cli`/`vaco-cli-core`, not a general
/// unused-option lint — a table entry gains an exemption here only when it is
/// confirmed dead, the same bar `-ar` itself was held to before it was wired
/// up instead of listed here.
///
/// # Errors
/// [`Diagnostic`] naming the option, once any occurrence of one is found.
fn refuse_unimplemented_options(line: &CommandLine) -> Result<(), Diagnostic> {
    // Overwrite policy and seek/trim (CLI-option audit, first batch):
    // `-y`/`-n` and `-ss`/`-t`/`-to` are no longer refused -- they are
    // implemented, by `crate::overwrite` and `crate::seek_trim`
    // respectively, and `crate::lib`'s per-input loop wires both in. `-y`
    // was never refused even before that (see its own history): this
    // build always overwrote unconditionally before `crate::overwrite`
    // existed, which is exactly what `-y` itself asks for, so accepting
    // and ignoring it produced no divergence from its own request.
    // `-sseof`/`-itsoffset`/`-itsscale`/`-seek_timestamp`/`-accurate_seek`
    // remain refused: none of them reaches `crate::seek_trim`, and
    // silently ignoring any of them means every invocation processes the
    // whole input regardless of what it says -- including, potentially,
    // past measurements: checked against `planning/PERF-BASELINE.md` and
    // `scripts/`, neither uses any of these, so no recorded ratio is
    // affected.
    //
    // `-t`/`-to`, **output**-positioned, were a second, narrower instance of
    // the same silent-no-op shape: the option table has always declared them
    // `INPUT, OUTPUT` and `crate::cli::parse` split them into the right
    // `OptionGroup` either way, but the binding loop above only ever read an
    // *input* group's occurrence -- `vaco -i in.mp4 -t 10 out.mp4` parsed,
    // ran and exited 0 with the whole file written, never erroring the way
    // this list would for a genuinely unimplemented option. Fixed by giving
    // `OutputSpec` its own `end` ([`OutputSpec::end`]), read the same way
    // just above, and enforced by wrapping each output's muxer in
    // `crate::output_trim::OutputTrim`. Output-positioned `-ss` is the
    // opposite fix: the reference's own output-side `-ss` decodes and
    // discards up to the timestamp rather than seeking, which is a
    // materially different, unimplemented feature -- refused just below
    // rather than left to silently do nothing the way `-t`/`-to` used to.
    // CLI-option audit, second batch: everything below changes output bytes
    // or silently drops a requested behaviour if ignored (coordinator's own
    // triage, priority 1) -- as opposed to the third batch (below), which
    // only loses diagnostics.
    const GLOBAL: &[&str] = &[
        "copyts",
        "start_at_zero",
        "copytb",
        // Third batch: diagnostics-only (triage group 2) -- ignoring these
        // never changes an output byte, only whether a requested report
        // ever appears. Still a lie to accept silently, just a cheaper one.
        "benchmark",
        "benchmark_all",
        "dump",
        "hex",
        "debug_ts",
        "vstats",
        "vstats_file",
        "vstats_version",
        "stats_period",
        // Fourth batch: structural absence (triage group 4) -- no hardware
        // acceleration path exists at all yet, same reason `-hwaccel`
        // itself is already refused above.
        "init_hw_device",
        // Fifth batch: the single most severe item found in the whole
        // audit. Silently ignoring `-filter_complex_script` means the user
        // hands us an entire filtergraph in a file and we process the
        // input with NO filtering at all, then exit 0 having written a
        // plausible-looking output -- the same silent-wrong-output shape
        // as the truncated-ALAC-decode defect. Refused ahead of every
        // other item below, including the rest of this same priority
        // group.
        "filter_complex_script",
        // Sixth batch: rest of triage group 1 (changes output / robustness
        // gap), plus `max_alloc` moved here from group 2 -- it is not a
        // diagnostic, it is a safety bound for untrusted input (the same
        // concern `vaco-limits` exists for), and silently not honouring a
        // requested cap is a robustness gap, not a missing convenience.
        // The rest govern what happens when something goes wrong
        // (`abort_on`, `xerror`, `timelimit`, `ignore_unknown`,
        // `copy_unknown`, `recast_media`) or otherwise change output bytes.
        "max_alloc",
        "dts_delta_threshold",
        "dts_error_threshold",
        "sdp_file",
        "abort_on",
        "xerror",
        "timelimit",
        "ignore_unknown",
        "copy_unknown",
        "recast_media",
        // Seventh batch: rest of triage group 2 (diagnostics/tuning-only)
        // and group 4 (structural, hwaccel-adjacent).
        "filter_buffered_frames",
        "filter_complex_threads",
        "filter_hw_device",
        // Missed in the seventh batch: same diagnostics/compat-only
        // reasoning as the rest of group 2.
        "cpuflags",
        // Eighth batch: a second independent measurement (an
        // `xtask option-consumption-check`, hand-verified in a clean
        // worktree) found these still silently accepted after the audit
        // above -- the checker itself had a bug (reading vaco-cli's own
        // refusal list for vaco-probe too, missing vaco-probe's separate
        // `UNIMPLEMENTED`), but these are real, confirmed by byte-identical
        // output with and without them. `-vsync` in particular is not
        // ffmpeg's own documented no-op the way it looked at first: unlike
        // `-top`/`-qphist`, it is a *separate* table entry from `-fps_mode`,
        // not an alias, so `-vsync cfr` does nothing where `-fps_mode cfr`
        // works -- our own gap wearing ffmpeg's "deprecated" costume, not
        // the real thing.
        "cpucount",
        "max_error_rate",
        "adrift_threshold",
        "vsync",
    ];
    const PER_FILE: &[&str] = &[
        "hwaccel",
        "sseof",
        "itsoffset",
        "itsscale",
        "seek_timestamp",
        "accurate_seek",
        "stream_loop",
        "r",
        "re",
        "fpsmax",
        "force_fps",
        "apply_cropping",
        "autoscale",
        "muxdelay",
        "muxpreload",
        "time_base",
        "timecode",
        "tag",
        "discard",
        "copyinkf",
        "copypriorss",
        "intra_matrix",
        "inter_matrix",
        "chroma_intra_matrix",
        "profile",
        "target",
        "channel_layout",
        "ch_layout",
        "fix_sub_duration",
        "canvas_size",
        "fs",
        "pass",
        "passlogfile",
        "rc_override",
        // Third batch (group 2, diagnostics-only).
        "stats_enc_pre",
        "stats_enc_post",
        "stats_mux_pre",
        "stats_enc_pre_fmt",
        "stats_enc_post_fmt",
        "stats_mux_pre_fmt",
        // Fourth batch (group 4, structural -- no hardware acceleration
        // path exists, same reason as the `-hwaccel` refusal above).
        "hwaccel_device",
        "hwaccel_output_format",
        // Fifth batch: `-filter_script`'s per-file twin of
        // `-filter_complex_script` above -- same silent-no-filtering defect.
        "filter_script",
        // Sixth batch: rest of triage group 1 (per-file/per-stream half).
        "isync",
        "readrate",
        "readrate_initial_burst",
        "readrate_catchup",
        "reinit_filter",
        "drop_changed",
        "fpre",
        "pre",
        "bits_per_raw_sample",
        "stream_group",
        "streamid",
        "dump_attachment",
        "apad",
        "guess_layout_max",
        "fix_sub_duration_heartbeat",
        "find_stream_info",
        // Seventh batch: rest of triage group 2 (per-file half).
        "max_muxing_queue_size",
        "muxing_queue_data_threshold",
        // Eighth batch (see the GLOBAL half's comment above for why).
        // `-frames`/its aliases `-aframes`/`-dframes`/`-vframes` and
        // `-shortest` are the two urgent ones: hand-verified byte-identical
        // output with and without `-frames:v 2` (2794 bytes, frame=25
        // either way) and with and without `-shortest` on two
        // different-length WAV inputs (64568 bytes, time=00:00:03.00
        // either way) -- both silently produce output of the wrong length,
        // not a missing convenience.
        "thread_queue_size",
        "timestamp",
        "shortest",
        "shortest_buf_duration",
        "frames",
    ];

    for &name in GLOBAL {
        if line.last_global(name).is_some() {
            return Err(unimplemented_option(name));
        }
    }
    for g in &line.groups {
        for &name in PER_FILE {
            if g.last(name).is_some() {
                return Err(unimplemented_option(name));
            }
        }
    }
    // `-ss`, output-positioned only: see this function's own doc for why
    // this is a targeted refusal rather than an addition to `PER_FILE`
    // above, which would also (wrongly) refuse the already-implemented
    // input-side `-ss`.
    for g in line.of_kind(GroupKind::Output) {
        if g.last("ss").is_some() {
            return Err(unimplemented_option("ss"));
        }
    }
    Ok(())
}

fn unimplemented_option(name: &str) -> Diagnostic {
    Diagnostic::new(
        AvError::ENOSYS,
        vec![format!(
            "-{name} is accepted by this build's option table but not implemented yet; see docs/app/vaco-cli.md."
        )],
    )
}

fn input_only_option(name: &str) -> Diagnostic {
    Diagnostic::new(
        AvError::EINVAL,
        vec![format!(
            "Option {name} cannot be applied to an output file: it is an input-only MPEG-TS demuxer option."
        )],
    )
}

fn url_of(g: &OptionGroup) -> Result<String, Diagnostic> {
    g.url.to_str().map(str::to_owned).ok_or_else(|| {
        // The reference opens non-UTF-8 paths; we cannot, because every layer
        // below takes a `&str`. Recorded in the doc file as a known divergence
        // rather than hidden behind a lossy conversion that would open the
        // wrong file.
        Diagnostic::new(
            AvError::EINVAL,
            vec![format!(
                "Filename is not valid UTF-8: {}",
                g.url.to_string_lossy()
            )],
        )
    })
}

/// `-ss`/`-t`/`-to` (CLI-option audit): parse one group's occurrence of
/// `name`, under the reference's own duration grammar
/// ([`vaco_core::parse::duration`], the same parser `-force_key_frames`
/// already uses). `what` is `"input"` or `"output"`, matching whichever side
/// of the file `g` binds to -- it only ever affects the wording of a parse
/// error, never the grammar itself.
///
/// # Errors
/// OBSERVED (`ffmpeg -ss notatime -i in.wav -f null -`, exit 234):
/// ```text
/// Invalid duration for option ss: notatime
/// Error parsing options for input file in.wav.
/// Error opening input files: Invalid argument
/// ```
/// and, output-positioned (`ffmpeg -i in.mp4 -t notatime -c copy bad.mp4`,
/// exit 234):
/// ```text
/// Invalid duration for option t: notatime
/// Error parsing options for output file bad.mp4.
/// Error opening output files: Invalid argument
/// ```
fn duration_of(
    g: &OptionGroup,
    name: &str,
    what: &str,
) -> Result<Option<vaco_core::Duration>, Diagnostic> {
    let Some(raw) = last_value(g, name)? else {
        return Ok(None);
    };
    vaco_core::parse::duration(&raw)
        .map(Some)
        .ok_or_else(|| invalid_duration(name, &raw, &g.url.to_string_lossy(), what))
}

fn invalid_duration(name: &str, value: &str, url: &str, what: &str) -> Diagnostic {
    Diagnostic::new(
        AvError::EINVAL,
        vec![
            format!("Invalid duration for option {name}: {value}"),
            format!("Error parsing options for {what} file {url}."),
            format!("Error opening {what} files: {}", AvError::EINVAL.text),
        ],
    )
}

/// `-t`/`-to` together: the reference gives `-t` priority when both are on
/// the same group. Measured against a 10 s fixture, input-positioned:
/// `-ss 2 -t 3 -to 100 -f null -` reports `time=00:00:03.00` regardless of
/// `-to`'s own value, and `-ss 2 -to 5` alone reports `time=00:00:03.00` too
/// (`5 - 2`, confirming `-to` is measured from the file's own start, not
/// from `-ss`). Output-positioned, the same priority holds with no `-ss` to
/// be relative to: `ffmpeg -i in.mp4 -t 3 -to 100 -c copy out.mp4` and
/// `ffmpeg -i in.mp4 -t 3 -c copy out.mp4` both report `duration=3.08`.
fn end_bound_of(g: &OptionGroup, what: &str) -> Result<Option<EndBound>, Diagnostic> {
    if let Some(d) = duration_of(g, "t", what)? {
        return Ok(Some(EndBound::AfterSeek(d)));
    }
    Ok(duration_of(g, "to", what)?.map(EndBound::Absolute))
}

/// `-to` is absolute from the file's own start (see [`end_bound_of`]), so a
/// value at or before `-ss` (default 0 with no `-ss`, and always 0 on the
/// output side -- there is no output `-ss`) names an empty or backwards
/// range. OBSERVED, input-positioned (`ffmpeg -ss 5 -to 5 -i in.wav -f null
/// -`, exit 234 -- and the same at `-to` values below `-ss` too, including
/// with no `-ss` at all and `-to 0`):
/// ```text
/// [in#0] -to value smaller than -ss; aborting.
/// Error opening input file in.wav.
/// Error opening input files: Invalid argument
/// ```
/// and OBSERVED, output-positioned (`ffmpeg -i in.mp4 -to 0 -c copy out.mp4`,
/// exit 234 -- the reference reuses its input-side wording verbatim, `-ss`
/// mention included, even though there is no output `-ss` to name):
/// ```text
/// [out#0] -to value smaller than -ss; aborting.
/// Error opening output file out.mp4.
/// Error opening output files: Invalid argument
/// ```
/// `-t` (`EndBound::AfterSeek`) cannot trigger this: it is a duration added
/// to `-ss` (or to 0, output-side), never a point that can fall before it.
fn validate_bounds(
    index: u32,
    seek: Option<vaco_core::Duration>,
    end: Option<EndBound>,
    url: &str,
    what: &str,
) -> Result<(), Diagnostic> {
    let Some(EndBound::Absolute(to)) = end else {
        return Ok(());
    };
    let start = seek.unwrap_or(vaco_core::Duration(0));
    if to <= start {
        let tag = if what == "input" { "in" } else { "out" };
        return Err(Diagnostic::opening(
            AvError::EINVAL,
            vec![format!(
                "[{tag}#{index}] -to value smaller than -ss; aborting."
            )],
            what,
            url,
        ));
    }
    Ok(())
}

fn last_value(g: &OptionGroup, name: &str) -> Result<Option<String>, Diagnostic> {
    let Some(opt) = g.last(name) else {
        return Ok(None);
    };
    value_str(opt).map(Some)
}

/// Fold every generic `AVFormatContext` option (FW-11: `-probesize`,
/// `-analyzeduration`, `-fflags`, `-avoid_negative_ts`, and the rest
/// [`FormatOptions`]'s schema names) named in `g` onto the default, in argv
/// order — so a later occurrence of the same name wins, and `-fflags
/// +genpts -fflags +ignidx` accumulates rather than one replacing the other,
/// exactly as [`vaco_opts::OptionsExt::set_str`] already does for any other
/// caller of an `#[derive(Options)]` struct.
///
/// Filtered by [`Oracle::knows`]'s own rule — a schema-field match — so this
/// never touches an option this binary models itself (`-map`, `-f`, `-c`, …
/// are never [`FormatOptions`] fields and so never reach `set_str` here).
///
/// # Errors
///
/// A [`Diagnostic`] carrying [`vaco_opts::OptError`]'s message when a value
/// this option's own type rejects reaches `set_str` (an out-of-range
/// `-probesize`, an unrecognised `-strict` name, and the like).
fn format_options_of(g: &OptionGroup) -> Result<FormatOptions, Diagnostic> {
    let mut opts = FormatOptions::default();
    let schema = opts.schema();
    for opt in &g.opts {
        let (name, _spec) = opt.resolved();
        // The top-level `-bitexact` is not itself a `FormatOptions` field —
        // it is not one of the reference's 39 `AVFormatContext` options, so
        // `Oracle::knows` and the `schema.find` below both say no — but the
        // reference treats it as sugar that also sets `AVFMT_FLAG_BITEXACT`
        // (`fflags`'s own `bitexact` bit) on every context in this file
        // group. Folding it in here, rather than teaching `FormatOptions`
        // a field that duplicates one already in `fflags`, means
        // `vaco-mux-hash`'s `Muxer::set_bitexact` (reached through
        // `MuxBuilder::open` from `opts.fflags.contains(FFlags::BITEXACT)`)
        // sees the same bit regardless of which spelling asked for it —
        // measured (`ffmpeg 8.1`) to suppress `framecrc`'s `#software` line
        // identically for `-bitexact` and `-fflags +bitexact` on the output.
        if name == "bitexact" {
            opts.fflags.insert(FFlags::BITEXACT);
            continue;
        }
        if schema.find(name).is_none() {
            continue;
        }
        let value = value_str(opt)?;
        opts.set_str(name, &value).map_err(|e| {
            Diagnostic::new(
                AvError::EINVAL,
                vec![format!(
                    "Error parsing option '{name}' with value '{value}': {e}"
                )],
            )
        })?;
    }
    Ok(opts)
}

fn decryption_key_of(g: &OptionGroup) -> Result<Option<[u8; 16]>, Diagnostic> {
    let Some(value) = last_value(g, "decryption_key")? else {
        return Ok(None);
    };
    parse_hex_16(&value)
        .map(Some)
        .ok_or_else(|| invalid_decryption_key(&value))
}

fn invalid_decryption_key(value: &str) -> Diagnostic {
    Diagnostic::new(
        AvError::EINVAL,
        vec![format!(
            "Invalid decryption key '{value}': expected 32 hexadecimal digits."
        )],
    )
}

fn decryption_keys_of(g: &OptionGroup) -> Result<Vec<vaco_demux_mp4::DecryptionKey>, Diagnostic> {
    let Some(value) = last_value(g, "decryption_keys")? else {
        return Ok(Vec::new());
    };
    let mut keys = Vec::new();
    for entry in value.split(':') {
        let Some((kid, key)) = entry.split_once('=') else {
            return Err(invalid_decryption_keys(&value));
        };
        let Some(kid) = parse_hex_16(kid) else {
            return Err(invalid_decryption_keys(&value));
        };
        let Some(key) = parse_hex_16(key) else {
            return Err(invalid_decryption_keys(&value));
        };
        keys.push(vaco_demux_mp4::DecryptionKey { kid, key });
    }
    if keys.is_empty() {
        return Err(invalid_decryption_keys(&value));
    }
    Ok(keys)
}

fn parse_hex_16(value: &str) -> Option<[u8; 16]> {
    if value.len() != 32 {
        return None;
    }
    let mut bytes = [0; 16];
    for (dst, pair) in bytes.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        let text = core::str::from_utf8(pair).ok()?;
        *dst = u8::from_str_radix(text, 16).ok()?;
    }
    Some(bytes)
}

fn invalid_decryption_keys(value: &str) -> Diagnostic {
    Diagnostic::new(
        AvError::EINVAL,
        vec![format!(
            "Invalid decryption keys '{value}': expected KID=KEY entries of 32 hexadecimal digits separated by ':'."
        )],
    )
}

/// The leading `strtol`-base-0 integer of a value string, ignoring anything
/// after the first non-numeric character — the same leniency
/// [`vaco_cli_core::metaspec`]'s `c:`/`p:` parsing documents, applied here to
/// `-map_chapters`/`-map_metadata`'s leading file index so a `,metadata`
/// qualifier this crate does not implement does not turn into a parse error.
fn leading_int(s: &str) -> i64 {
    strtol_base0(s).value
}

pub(crate) fn value_str(opt: &ParsedOption) -> Result<String, Diagnostic> {
    opt.value
        .as_ref()
        .and_then(|v| v.to_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            Diagnostic::new(
                AvError::EINVAL,
                vec![format!(
                    "Invalid value for option '{}': not valid UTF-8",
                    opt.name
                )],
            )
        })
}

fn split_list(v: &str) -> Vec<String> {
    v.split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

fn maps_of(g: &OptionGroup) -> Result<Vec<MapEntry>, Diagnostic> {
    let mut out = Vec::new();
    for opt in &g.opts {
        if opt.resolved().0 != "map" {
            continue;
        }
        // The reference prints the specifier grammar's own complaint first, then
        // the generic "Failed to set value" line; `MapEntry::parse` owns both.
        out.push(MapEntry::parse(&value_str(opt)?)?);
    }
    Ok(out)
}

/// CL-16: every `-attach <filename>` on this output group, in argv order.
fn attach_of(g: &OptionGroup) -> Result<Vec<String>, Diagnostic> {
    let mut out = Vec::new();
    for opt in &g.opts {
        if opt.resolved().0 != "attach" {
            continue;
        }
        out.push(value_str(opt)?);
    }
    Ok(out)
}

/// Translate a `vaco-cli-core` parse failure into the reference's two-line
/// shape and exit status.
///
/// Measured (`ffmpeg 8.1`, no pipe):
///
/// ```text
/// ffmpeg -qwerty 3 …   -> exit 8    "Unrecognized option 'qwerty'."
///                                    "Error splitting the argument list: Option not found"
/// ffmpeg -i            -> exit 234  "Missing argument for option 'i'."
///                                    "Error splitting the argument list: Invalid argument"
/// ```
pub(crate) fn split_error(e: &CliError) -> Diagnostic {
    let (err, first) = match e {
        CliError::UnrecognizedOption { name } => (
            AvError::OPTION_NOT_FOUND,
            format!("Unrecognized option '{}'.", name.to_string_lossy()),
        ),
        CliError::MissingArgument { name } => (
            AvError::EINVAL,
            format!("Missing argument for option '{name}'."),
        ),
        other => (AvError::EINVAL, other.to_string()),
    };
    let second = match e {
        CliError::WrongSide { .. } => None,
        _ => Some(format!("Error splitting the argument list: {}", err.text)),
    };
    let mut lines = vec![first];
    lines.extend(second);
    Diagnostic::new(err, lines)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn inputs_and_outputs_are_bound_in_order() {
        let cli = parse(&["-i", "a.mkv", "-i", "b.mkv", "-f", "null", "-"]).unwrap();
        assert_eq!(
            cli.inputs
                .iter()
                .map(|i| i.url.as_str())
                .collect::<Vec<_>>(),
            vec!["a.mkv", "b.mkv"]
        );
        assert_eq!(cli.outputs.len(), 1);
        assert_eq!(
            cli.outputs.first().map(|o| o.format.as_deref()),
            Some(Some("null"))
        );
        assert_eq!(cli.outputs.first().map(|o| o.url.as_str()), Some("-"));
    }

    #[test]
    fn an_unknown_option_is_rejected_with_the_reference_wording() {
        let e = parse(&["-qwerty", "3", "-i", "a.mkv", "-f", "null", "-"]).unwrap_err();
        assert_eq!(
            e.render(),
            "Unrecognized option 'qwerty'.\n\
             Error splitting the argument list: Option not found\n"
        );
        assert_eq!(e.exit.code(), 8);
    }

    #[test]
    fn a_missing_value_is_einval_not_option_not_found() {
        // OBSERVED: `ffmpeg -i` exits 234, not 8.
        let e = parse(&["-i"]).unwrap_err();
        assert_eq!(
            e.render(),
            "Missing argument for option 'i'.\n\
             Error splitting the argument list: Invalid argument\n"
        );
        assert_eq!(e.exit.code(), 234);
    }

    #[test]
    fn a_format_option_name_is_accepted_because_this_build_has_one() {
        // `probesize` is a real `FormatOptions` field, so the oracle knows it.
        assert!(Oracle.knows("probesize"));
        assert!(Oracle.knows("protocol_whitelist"));
        assert!(Oracle.knows("decryption_key"));
        assert!(Oracle.knows("decryption_keys"));
        // No encoder in this build implements `crf`. Divergence, documented.
        assert!(!Oracle.knows("crf"));
        assert!(!Oracle.knows("qwerty"));
    }

    #[test]
    fn decryption_key_is_decoded_on_its_input_group() {
        let cli = parse(&[
            "-decryption_key",
            "00112233445566778899AaBbCcDdEeFf",
            "-i",
            "encrypted.mp4",
            "-c",
            "copy",
            "-f",
            "null",
            "-",
        ])
        .unwrap();
        assert_eq!(
            cli.inputs.first().unwrap().decryption_key,
            Some([
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff,
            ])
        );
    }

    #[test]
    fn malformed_decryption_key_is_rejected_before_open() {
        let error = parse(&[
            "-decryption_key",
            "00112233-not-a-128-bit-key",
            "-i",
            "encrypted.mp4",
            "-f",
            "null",
            "-",
        ])
        .unwrap_err();
        assert_eq!(error.exit.code(), 234);
        assert!(
            error.render().contains("expected 32 hexadecimal digits"),
            "{}",
            error.render()
        );
    }

    #[test]
    fn decryption_keys_are_decoded_on_their_input_group() {
        let cli = parse(&[
            "-decryption_keys",
            "0f1e2d3c4b5a69788796a5b4c3d2e1f0=00112233445566778899aabbccddeeff:\
             ffeeddccbbaa99887766554433221100=102132435465768798a9bacbdcedfe0f",
            "-i",
            "encrypted.mp4",
            "-f",
            "null",
            "-",
        ])
        .unwrap();
        let keys = &cli.inputs.first().unwrap().decryption_keys;
        assert_eq!(keys.len(), 2);
        assert_eq!(
            *keys.first().unwrap(),
            vaco_demux_mp4::DecryptionKey {
                kid: [
                    0x0f, 0x1e, 0x2d, 0x3c, 0x4b, 0x5a, 0x69, 0x78, 0x87, 0x96, 0xa5, 0xb4, 0xc3,
                    0xd2, 0xe1, 0xf0,
                ],
                key: [
                    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc,
                    0xdd, 0xee, 0xff,
                ],
            }
        );
        assert_eq!(
            keys.get(1).unwrap().kid,
            [
                0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22,
                0x11, 0x00,
            ]
        );
    }

    #[test]
    fn malformed_decryption_keys_are_rejected_before_open() {
        let error = parse(&[
            "-decryption_keys",
            "0f1e2d3c4b5a69788796a5b4c3d2e1f0=not-a-key",
            "-i",
            "encrypted.mp4",
            "-f",
            "null",
            "-",
        ])
        .unwrap_err();
        assert_eq!(error.exit.code(), 234);
        assert!(
            error.render().contains("expected KID=KEY entries"),
            "{}",
            error.render()
        );
    }

    #[test]
    fn output_scoped_decryption_key_is_refused() {
        let error = parse(&[
            "-i",
            "clear.mp4",
            "-decryption_key",
            "00112233445566778899aabbccddeeff",
            "-f",
            "null",
            "-",
        ])
        .unwrap_err();
        assert_eq!(error.exit.code(), 218);
        assert!(error.render().contains("not implemented yet"));
    }

    #[test]
    fn output_scoped_decryption_keys_are_refused() {
        let error = parse(&[
            "-i",
            "clear.mp4",
            "-decryption_keys",
            "0f1e2d3c4b5a69788796a5b4c3d2e1f0=00112233445566778899aabbccddeeff",
            "-f",
            "null",
            "-",
        ])
        .unwrap_err();
        assert_eq!(error.exit.code(), 218);
        assert!(error.render().contains("not implemented yet"));
    }

    /// Every name in [`crate::exec::PRIVATE_ENCODER_OPTIONS`] must be known
    /// -- that list and this method are meant to stay in lockstep, and a
    /// mismatch here is exactly the failure mode `codec_options_of`'s doc
    /// warns about: a value resolved from the split option set with nowhere
    /// to have come from, or an option the split stage rejects before
    /// `codec_options_of` ever sees it.
    #[test]
    fn every_private_encoder_option_name_is_known() {
        for name in crate::exec::PRIVATE_ENCODER_OPTIONS {
            assert!(Oracle.knows(name), "Oracle does not know {name:?}");
        }
    }

    #[test]
    fn generic_format_options_reach_input_spec() {
        // FW-11: `-probesize`/`-analyzeduration`/`-fflags` are `FormatOptions`
        // schema fields, so `format_options_of` must have applied them —
        // where `input::open` (untested here; see that module) reads them
        // from, once opened.
        let cli = parse(&[
            "-probesize",
            "12345",
            "-analyzeduration",
            "999",
            "-fflags",
            "+genpts",
            "-i",
            "a.mkv",
            "-f",
            "null",
            "-",
        ])
        .unwrap();
        let opts = &cli.inputs.first().unwrap().format_opts;
        assert_eq!(opts.probesize, 12345);
        assert_eq!(opts.analyzeduration, 999);
        assert!(
            opts.fflags
                .contains(vaco_format_core::options::FFlags::GENPTS)
        );
    }

    #[test]
    fn merge_pmt_versions_reaches_the_input_format_options() {
        let cli = parse(&[
            "-merge_pmt_versions",
            "1",
            "-f",
            "mpegts",
            "-i",
            "in.ts",
            "-f",
            "null",
            "-",
        ])
        .unwrap();
        assert!(cli.inputs[0].format_opts.merge_pmt_versions);
    }

    #[test]
    fn merge_pmt_versions_is_refused_on_an_output() {
        let error =
            parse(&["-i", "in.ts", "-merge_pmt_versions", "1", "-f", "null", "-"]).unwrap_err();
        assert!(error.render().contains("merge_pmt_versions"));
        assert!(error.render().contains("input"));
    }

    #[test]
    fn generic_format_options_reach_output_spec_too() {
        // FW-11's output-side share: `-avoid_negative_ts` is `encoding`-flagged
        // (see `vaco-format-core`'s own doc table), so it only ever matters on
        // an output group.
        let cli = parse(&[
            "-i",
            "a.mkv",
            "-avoid_negative_ts",
            "make_zero",
            "-f",
            "null",
            "-",
        ])
        .unwrap();
        assert_eq!(
            cli.outputs.first().unwrap().format_opts.avoid_negative_ts,
            2
        );
    }

    #[test]
    fn top_level_bitexact_folds_onto_fflags() {
        // `-bitexact` is not a `FormatOptions` schema field (it is not one of
        // the reference's 39 `AVFormatContext` options), so the generic
        // schema-match loop in `format_options_of` would otherwise silently
        // drop it — the same silent drop this test would have caught before
        // the fix, when nothing consumed `-bitexact` at all.
        // `Muxer::set_bitexact` (`vaco-format-core`) is what a muxer actually
        // reads this bit from, via `MuxBuilder::open`.
        let cli = parse(&["-i", "a.mkv", "-c", "copy", "-bitexact", "-f", "null", "-"]).unwrap();
        assert!(
            cli.outputs
                .first()
                .unwrap()
                .format_opts
                .fflags
                .contains(vaco_format_core::options::FFlags::BITEXACT)
        );
    }

    #[test]
    fn bitexact_via_fflags_still_works_directly() {
        // The spelling that already worked before this fix (`fflags` is a
        // literal schema field) must keep working identically.
        let cli = parse(&[
            "-i",
            "a.mkv",
            "-c",
            "copy",
            "-fflags",
            "+bitexact",
            "-f",
            "null",
            "-",
        ])
        .unwrap();
        assert!(
            cli.outputs
                .first()
                .unwrap()
                .format_opts
                .fflags
                .contains(vaco_format_core::options::FFlags::BITEXACT)
        );
    }

    #[test]
    fn an_option_this_build_does_not_model_is_left_alone() {
        // `-map`/`-f`/`-c` are never `FormatOptions` fields; `format_options_of`
        // must not choke on them or silently absorb their values.
        let cli = parse(&["-i", "a.mkv", "-map", "0", "-c", "copy", "-f", "null", "-"]).unwrap();
        assert_eq!(
            cli.outputs.first().unwrap().format_opts.probesize,
            5_000_000
        );
    }

    #[test]
    fn map_chapters_and_map_metadata_parse_their_leading_index() {
        let cli = parse(&[
            "-i",
            "a.mkv",
            "-map_chapters",
            "-1",
            "-map_metadata",
            "0",
            "-f",
            "null",
            "-",
        ])
        .unwrap();
        let o = cli.outputs.first().unwrap();
        assert_eq!(o.map_chapters, Some(-1));
        assert_eq!(o.map_metadata, Some(0));
    }

    #[test]
    fn map_chapters_and_map_metadata_default_to_unstated() {
        let cli = parse(&["-i", "a.mkv", "-f", "null", "-"]).unwrap();
        let o = cli.outputs.first().unwrap();
        assert_eq!(o.map_chapters, None);
        assert_eq!(o.map_metadata, None);
    }

    #[test]
    fn drop_flags_bind_to_their_output() {
        let cli = parse(&["-i", "a.mkv", "-vn", "-dn", "-f", "null", "-"]).unwrap();
        let o = cli.outputs.first().unwrap();
        assert_eq!(
            o.blocked,
            Suppressed {
                video: true,
                audio: false,
                subtitle: false,
                data: true
            }
        );
    }

    #[test]
    fn maps_keep_their_order_and_their_text() {
        let cli = parse(&[
            "-i", "a.mkv", "-map", "0:a", "-map", "-0:a:1", "-f", "null", "-",
        ])
        .unwrap();
        let o = cli.outputs.first().unwrap();
        assert_eq!(
            o.maps.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
            vec!["0:a", "-0:a:1"]
        );
    }

    #[test]
    fn a_protocol_whitelist_reaches_the_input() {
        let cli = parse(&[
            "-protocol_whitelist",
            "file,crypto",
            "-i",
            "a.mkv",
            "-f",
            "null",
            "-",
        ])
        .unwrap();
        assert_eq!(
            cli.inputs.first().and_then(|i| i.whitelist.clone()),
            Some(vec!["file".to_owned(), "crypto".to_owned()])
        );
    }

    #[test]
    fn trailing_per_file_options_are_orphaned_not_fatal() {
        // OBSERVED: `ffmpeg -i a -f null - -c:v libx264` exits 0.
        let cli = parse(&["-i", "a.mkv", "-f", "null", "-", "-c:v", "libx264"]).unwrap();
        assert_eq!(cli.orphaned, vec!["-c"]);
    }

    #[test]
    fn an_exit_option_is_reported() {
        let cli = parse(&["-version"]).unwrap();
        assert_eq!(cli.listing, Some("version"));
        let cli = parse(&["-formats"]).unwrap();
        assert_eq!(cli.listing, Some("formats"));
    }

    #[test]
    fn filter_complex_and_lavfi_occurrences_are_captured_in_argv_order() {
        let cli = parse(&[
            "-i",
            "a.mkv",
            "-filter_complex",
            "[0:v]scale=320:240[out]",
            "-lavfi",
            "anull",
            "-f",
            "null",
            "-",
        ])
        .unwrap();
        assert_eq!(
            cli.complex_filters,
            vec!["[0:v]scale=320:240[out]".to_owned(), "anull".to_owned()]
        );
    }

    #[test]
    fn hide_banner_is_seen_by_the_pre_scan_and_by_the_parse() {
        assert!(!wants_banner(&["-hide_banner", "-i", "x"]));
        assert!(wants_banner(&["-i", "x"]));
        assert!(
            parse(&["-hide_banner", "-i", "x", "-f", "null", "-"])
                .unwrap()
                .hide_banner
        );
    }

    #[test]
    fn frame_drop_threshold_binds_its_last_global_value() {
        let cli = parse(&[
            "-frame_drop_threshold",
            "0.25",
            "-i",
            "a.mkv",
            "-frame_drop_threshold",
            "1.5",
            "-f",
            "null",
            "-",
        ])
        .unwrap();
        assert_eq!(cli.frame_drop_threshold, Some(1.5));
    }
}
