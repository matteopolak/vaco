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
    }
}

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
}

impl Cli {
    /// The output group for `index`, for per-stream option lookups.
    #[must_use]
    pub fn output_group(&self, index: u32) -> Option<&OptionGroup> {
        self.line
            .of_kind(GroupKind::Output)
            .find(|g| g.index == index)
    }

    /// The thread count a run actually uses: `-threads N` if stated
    /// (including `-threads 1`, which forces serial), else
    /// [`default_thread_count`].
    #[must_use]
    pub fn thread_count(&self) -> usize {
        self.threads.unwrap_or_else(default_thread_count)
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
/// well as on `-hide_banner` (CONFORMANCE-FINDINGS 34). One definition, per
/// D19 — see [`vaco_cli_core::loglevel`] for the measurements behind it.
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
        ..Cli::default()
    };

    for g in line.of_kind(GroupKind::Input) {
        cli.inputs.push(InputSpec {
            index: g.index,
            url: url_of(g)?,
            format: last_value(g, "f")?,
            whitelist: last_value(g, "protocol_whitelist")?.map(|v| split_list(&v)),
            blacklist: last_value(g, "protocol_blacklist")?.map(|v| split_list(&v)),
            format_opts: format_options_of(g)?,
        });
    }

    for g in line.of_kind(GroupKind::Output) {
        cli.outputs.push(OutputSpec {
            index: g.index,
            url: url_of(g)?,
            format: last_value(g, "f")?,
            maps: maps_of(g)?,
            blocked: Suppressed {
                video: g.last("vn").is_some(),
                audio: g.last("an").is_some(),
                subtitle: g.last("sn").is_some(),
                data: g.last("dn").is_some(),
            },
            format_opts: format_options_of(g)?,
            map_chapters: last_value(g, "map_chapters")?.as_deref().map(leading_int),
            map_metadata: last_value(g, "map_metadata")?.as_deref().map(leading_int),
            attach: attach_of(g)?,
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
/// zero effect — the same defect `-ar` had (`planning/AGENT-CONSTRAINTS.md`'s
/// standing rule that silently wrong is worse than refusing applies just as
/// much to an option that does nothing as to one that resamples nothing).
/// Each of these is tracked as deferred work in `docs/app/vaco-cli.md`
/// (`-hwaccel`: CL-34a; `-print_graphs`: CL-27; `-fps_mode`/`-enc_time_base`/
/// `-frame_drop_threshold`: CL-21) — refusing does not close any of those
/// issues, it only stops the gap between "accepted" and "implemented" from
/// reading as "works".
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
    const GLOBAL: &[&str] = &["print_graphs", "frame_drop_threshold"];
    const PER_FILE: &[&str] = &["hwaccel", "fps_mode", "enc_time_base"];

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
fn split_error(e: &CliError) -> Diagnostic {
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
        // No encoder in this build implements `crf`. Divergence, documented.
        assert!(!Oracle.knows("crf"));
        assert!(!Oracle.knows("qwerty"));
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
        // issue #634's fix, when nothing consumed `-bitexact` at all.
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
}
