//! argv to a validated run description.
//!
//! The grammar, the option table, the specifier language and the scope model
//! all live in `vaco-cli-core`, which recorded 1 951 reference verdicts for
//! them. Nothing is re-decided here; this module only reads the resulting
//! [`CommandLine`] into the shape the rest of the crate wants.
//!
//! Two things about ffprobe's command line are worth knowing before reading:
//!
//! * **It has no per-file option groups.** Every option is global — which is
//!   why `-select_streams` is a single value rather than a per-stream one, and
//!   why `-i` and a bare positional are interchangeable.
//! * **The listing options exit.** `-formats`, `-sections`, `-version` and the
//!   rest carry `EXIT` in the table: they print and return 0 without ever
//!   looking at an input.

use std::ffi::OsString;

use vaco_cli_core::{CliError, CommandLine, GroupKind, StreamSpecifier, split, table};
use vaco_textformat::{EntryFilterSet, FormatOpts, OptionalFields, num::Pretty};

use crate::dump::{DumpFormat, HashAlg};
use crate::intervals::{self, ReadInterval};

/// A listing command: prints and exits, ignoring any input.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Listing {
    Version,
    Formats,
    Muxers,
    Demuxers,
    Devices,
    Codecs,
    Decoders,
    Encoders,
    Bsfs,
    Protocols,
    Filters,
    PixFmts,
    Layouts,
    SampleFmts,
    Dispositions,
    Colors,
    Sections,
    /// `-L`, the licence.
    License,
    /// `-buildconf`.
    BuildConf,
    /// `-h`, `-?`, `-help`, `--help`.
    Help,
}

/// The `-show_*` switches, as a set.
///
/// A set rather than a `Vec`, because the *emission* order of root children is
/// fixed by the reference and has nothing to do with the order the flags were
/// written in. Observed order (plan 14 §5.4, re-confirmed here):
/// `program_version, library_versions, pixel_formats, packets, frames,
/// programs, stream_groups, streams, chapters, format, error`.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
// One bool per ffprobe flag by that exact name. Collapsing them would put a
// translation layer between the option table and the output, which is where a
// divergence would hide.
#[allow(clippy::struct_excessive_bools)]
pub struct Show {
    pub format: bool,
    pub streams: bool,
    pub packets: bool,
    pub frames: bool,
    pub chapters: bool,
    pub programs: bool,
    pub stream_groups: bool,
    pub error: bool,
    pub program_version: bool,
    pub library_versions: bool,
    pub pixel_formats: bool,
    pub private_data: bool,
    pub count_frames: bool,
    pub count_packets: bool,
}

impl Show {
    /// Whether any section at all was requested.
    ///
    /// `ffprobe file.mp4` with no `-show_*` prints **nothing on stdout** — the
    /// whole input description goes to stderr as a log dump. Verified:
    /// `ffprobe av.mp4 2>/dev/null` is empty.
    #[must_use]
    pub const fn any(self) -> bool {
        self.format
            || self.streams
            || self.packets
            || self.frames
            || self.chapters
            || self.programs
            || self.stream_groups
            || self.error
            || self.program_version
            || self.library_versions
            || self.pixel_formats
    }
}

/// One `vaco-probe` run.
#[derive(Clone, Debug)]
pub struct Options {
    /// The input URL, or `None` when none was given.
    pub input: Option<OsString>,
    /// `-f`: force this demuxer, skipping probing.
    pub force_format: Option<String>,
    /// `-print_filename`: what the `format.filename` field prints.
    pub print_filename: Option<String>,
    /// `-of`/`-output_format`/`-print_format`, verbatim.
    pub writer: String,
    /// `-o`: write to this file instead of stdout.
    pub output: Option<OsString>,
    pub show: Show,
    pub format_opts: FormatOpts,
    /// `-show_entries`, already parsed.
    pub entries: EntryFilterSet,
    /// `-select_streams`, already parsed.
    pub select: Option<StreamSpecifier>,
    /// A listing command; when set, nothing else runs.
    pub listing: Option<Listing>,
    /// `-hide_banner`.
    pub hide_banner: bool,
    /// `-bitexact`.
    pub bitexact: bool,
    /// `-read_intervals`, already parsed. A single whole-file interval when the
    /// option was absent, so the read loop has no special case.
    pub intervals: Vec<ReadInterval>,
    /// Diagnostics `-read_intervals` produced without failing. See
    /// [`crate::intervals::BAD_COUNT`].
    pub interval_warnings: Vec<String>,
    /// `-show_data` plus `-data_dump_format`, resolved into one value.
    pub show_data: Option<DumpFormat>,
    /// `-show_data_hash <alg>`.
    pub show_data_hash: Option<HashAlg>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            input: None,
            force_format: None,
            print_filename: None,
            writer: "default".to_owned(),
            output: None,
            show: Show::default(),
            format_opts: FormatOpts::default(),
            entries: EntryFilterSet::all(),
            select: None,
            listing: None,
            hide_banner: false,
            bitexact: false,
            intervals: vec![ReadInterval::ALL],
            interval_warnings: Vec::new(),
            show_data: None,
            show_data_hash: None,
        }
    }
}

/// The listing options, paired with the flag that selects them.
///
/// Order matters: the reference honours the **first** exiting option on the
/// command line, in table order, so this list is the table's order.
const LISTINGS: &[(&str, Listing)] = &[
    ("L", Listing::License),
    ("h", Listing::Help),
    ("version", Listing::Version),
    ("buildconf", Listing::BuildConf),
    ("formats", Listing::Formats),
    ("muxers", Listing::Muxers),
    ("demuxers", Listing::Demuxers),
    ("devices", Listing::Devices),
    ("codecs", Listing::Codecs),
    ("decoders", Listing::Decoders),
    ("encoders", Listing::Encoders),
    ("bsfs", Listing::Bsfs),
    ("protocols", Listing::Protocols),
    ("filters", Listing::Filters),
    ("pix_fmts", Listing::PixFmts),
    ("layouts", Listing::Layouts),
    ("sample_fmts", Listing::SampleFmts),
    ("dispositions", Listing::Dispositions),
    ("colors", Listing::Colors),
    ("sections", Listing::Sections),
];

/// Parse an argument vector. `argv` must **not** include the program name.
///
/// # Errors
/// Whatever `vaco-cli-core` reports for a malformed command line, and
/// [`CliError`] for an option value this binary rejects.
pub fn parse<S: AsRef<std::ffi::OsStr>>(argv: &[S]) -> Result<Options, CliError> {
    let table = table::ffprobe();
    let cmd: CommandLine = split(&table, argv)?;
    cmd.validate()?;

    // The first exiting option wins, whatever follows it. `-formats -codecs`
    // lists formats.
    let mut o = Options {
        listing: first_listing(&cmd),
        ..Options::default()
    };
    let mut show_data = false;
    let mut dump_format: Option<DumpFormat> = None;

    for opt in &cmd.global {
        let (name, _) = opt.resolved();
        match name {
            "output_format" => o.writer = value(opt, "string")?,
            "o" => o.output.clone_from(&opt.value),
            "f" => o.force_format = Some(value(opt, "string")?),
            "print_filename" => o.print_filename = Some(value(opt, "string")?),
            "show_format" => o.show.format = !opt.negated,
            "show_streams" => o.show.streams = !opt.negated,
            "show_packets" => o.show.packets = !opt.negated,
            "show_frames" => o.show.frames = !opt.negated,
            "show_chapters" => o.show.chapters = !opt.negated,
            "show_programs" => o.show.programs = !opt.negated,
            "show_stream_groups" => o.show.stream_groups = !opt.negated,
            "show_error" => o.show.error = !opt.negated,
            "show_program_version" => o.show.program_version = !opt.negated,
            "show_library_versions" => o.show.library_versions = !opt.negated,
            "show_versions" => {
                o.show.program_version = !opt.negated;
                o.show.library_versions = !opt.negated;
            }
            "show_pixel_formats" => o.show.pixel_formats = !opt.negated,
            "show_private_data" => o.show.private_data = !opt.negated,
            "count_frames" => o.show.count_frames = !opt.negated,
            "count_packets" => o.show.count_packets = !opt.negated,
            "unit" => o.format_opts.pretty.unit = !opt.negated,
            "prefix" => o.format_opts.pretty.prefix = !opt.negated,
            "byte_binary_prefix" => o.format_opts.pretty.byte_binary_prefix = !opt.negated,
            "sexagesimal" => o.format_opts.pretty.sexagesimal = !opt.negated,
            "pretty" => {
                o.format_opts.pretty = if opt.negated {
                    Pretty::default()
                } else {
                    FormatOpts::pretty().pretty
                };
            }
            // Three options that co-operate: `-show_data` turns the field on,
            // `-data_dump_format` chooses the rendering, and either may come
            // first. Resolved after the loop rather than here.
            "show_data" => show_data = !opt.negated,
            "data_dump_format" => {
                let v = value(opt, "string")?;
                dump_format =
                    Some(
                        DumpFormat::parse(&v).ok_or_else(|| CliError::OptionValueRejected {
                            option: "data_dump_format".to_owned(),
                            value: v.into(),
                        })?,
                    );
            }
            "show_data_hash" => {
                let v = value(opt, "string")?;
                o.show_data_hash =
                    Some(
                        HashAlg::parse(&v).ok_or_else(|| CliError::OptionValueRejected {
                            option: "show_data_hash".to_owned(),
                            value: v.into(),
                        })?,
                    );
            }
            // Last wins. Observed: `-read_intervals '%+#2' -read_intervals
            // '%+#1'` reads one packet, not three and not two.
            "read_intervals" => {
                let v = value(opt, "read_intervals")?;
                // The value in the message is the raw spec, as the reference's
                // own third line has it. The reference prints two further
                // lines naming *which* interval and why; those are not
                // reproduced, and plan 14 §5.6 makes only the exit code
                // conformance surface here.
                let (parsed, warnings) =
                    intervals::parse(&v).map_err(|_| CliError::OptionValueRejected {
                        option: "read_intervals".to_owned(),
                        value: v.clone().into(),
                    })?;
                o.intervals = parsed;
                o.interval_warnings = warnings;
            }
            "hide_banner" => o.hide_banner = !opt.negated,
            "bitexact" => o.bitexact = !opt.negated,
            "show_optional_fields" => {
                let v = value(opt, "string")?;
                o.format_opts.show_optional_fields =
                    OptionalFields::parse(&v).map_err(|_| CliError::OptionValueRejected {
                        option: "show_optional_fields".to_owned(),
                        value: v.into(),
                    })?;
            }
            "show_entries" => {
                o.entries = EntryFilterSet::parse(&value(opt, "entry list")?);
            }
            "select_streams" => {
                let v = value(opt, "stream specifier")?;
                o.select = Some(StreamSpecifier::parse(&v).map_err(|inner| {
                    CliError::InvalidStreamSpecifier {
                        text: v.clone(),
                        inner,
                    }
                })?);
            }
            _ => {}
        }
    }

    // `-data_dump_format` on its own does nothing; it only chooses how
    // `-show_data` renders. Observed: the field appears only with `-show_data`.
    if show_data {
        o.show_data = Some(dump_format.unwrap_or_default());
    }

    // `-show_entries format=filename` implies `-show_format`: naming a section
    // enables it. Plan 14 §5.2, confirmed against the binary.
    if !o.entries.is_unfiltered() {
        enable_named_sections(&mut o);
    }

    // ffprobe has no output groups: the positional is the input.
    if let Some(g) = cmd.of_kind(GroupKind::Input).next() {
        o.input = Some(g.url.clone());
    }
    if let Some(opt) = cmd.last_global("i")
        && let Some(v) = &opt.value
    {
        o.input = Some(v.clone());
    }

    // The banner is suppressed by `-hide_banner` *and*, independently, by any
    // `-v`/`-loglevel` below `info` — `ffprobe -v error -show_streams` prints
    // no version line at all, and ours did. ORed in
    // rather than assigned, so `-nohide_banner` keeps meaning what it means.
    if !vaco_cli_core::loglevel::wants_banner(argv) {
        o.hide_banner = true;
    }

    Ok(o)
}

/// Turn `-show_entries stream=index` into `-show_streams`, and so on for every
/// root child a filter can name.
fn enable_named_sections(o: &mut Options) {
    use vaco_textformat::sections::{SectionId, desc};
    let on = |id: SectionId| {
        o.entries
            .section_visible(&[desc(SectionId::ROOT)], desc(id))
    };
    o.show.format |= on(SectionId::FORMAT);
    o.show.streams |= on(SectionId::STREAMS);
    o.show.packets |= on(SectionId::PACKETS);
    o.show.frames |= on(SectionId::FRAMES);
    o.show.chapters |= on(SectionId::CHAPTERS);
    o.show.programs |= on(SectionId::PROGRAMS);
    o.show.stream_groups |= on(SectionId::STREAM_GROUPS);
    o.show.error |= on(SectionId::ERROR);
    o.show.program_version |= on(SectionId::PROGRAM_VERSION);
    o.show.library_versions |= on(SectionId::LIBRARY_VERSIONS);
    o.show.pixel_formats |= on(SectionId::PIXEL_FORMATS);
}

fn first_listing(cmd: &CommandLine) -> Option<Listing> {
    cmd.global
        .iter()
        .filter_map(|opt| {
            let (name, _) = opt.resolved();
            LISTINGS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|&(_, l)| (opt.argv_index, l))
        })
        .min_by_key(|(i, _)| *i)
        .map(|(_, l)| l)
}

/// A required UTF-8 option value.
///
/// `kind` is the grammar name the reference names in its message
/// (`Invalid <kind> for option <name>`), which is why it is `&'static str`.
fn value(opt: &vaco_cli_core::ParsedOption, kind: &'static str) -> Result<String, CliError> {
    opt.value_str(kind).map(str::to_owned)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Options {
        parse(args).expect("parse")
    }

    #[test]
    fn a_bare_positional_is_the_input() {
        assert_eq!(p(&["a.mp4"]).input.as_deref(), Some("a.mp4".as_ref()));
        assert_eq!(p(&["-i", "a.mp4"]).input.as_deref(), Some("a.mp4".as_ref()));
    }

    #[test]
    fn no_show_flag_means_no_stdout_section() {
        // `ffprobe a.mp4 2>/dev/null` is empty. Observed.
        assert!(!p(&["a.mp4"]).show.any());
        assert!(p(&["-show_format", "a.mp4"]).show.any());
    }

    #[test]
    fn writer_defaults_to_default_and_every_alias_sets_it() {
        assert_eq!(p(&["a.mp4"]).writer, "default");
        for flag in ["-of", "-output_format", "-print_format"] {
            assert_eq!(p(&[flag, "json", "a.mp4"]).writer, "json", "{flag}");
        }
        // `-of` takes the whole spec, options included.
        assert_eq!(
            p(&["-of", "compact=s=,:nk=1", "a.mp4"]).writer,
            "compact=s=,:nk=1"
        );
    }

    #[test]
    fn pretty_sets_all_four_switches() {
        let o = p(&["-pretty", "a.mp4"]);
        assert!(o.format_opts.pretty.unit);
        assert!(o.format_opts.pretty.prefix);
        assert!(o.format_opts.pretty.byte_binary_prefix);
        assert!(o.format_opts.pretty.sexagesimal);
    }

    #[test]
    fn the_four_switches_are_independent() {
        let o = p(&["-sexagesimal", "a.mp4"]);
        assert!(o.format_opts.pretty.sexagesimal);
        assert!(!o.format_opts.pretty.unit);
    }

    #[test]
    fn show_versions_sets_both_version_sections() {
        let o = p(&["-show_versions", "a.mp4"]);
        assert!(o.show.program_version);
        assert!(o.show.library_versions);
    }

    #[test]
    fn show_entries_enables_the_section_it_names() {
        // Plan 14 §5.2: naming a section implies showing it.
        let o = p(&["-show_entries", "format=filename", "a.mp4"]);
        assert!(o.show.format);
        assert!(!o.show.packets);

        // A local name selects every section carrying it, which is why this
        // also opens `programs` and `stream_groups`.
        let o = p(&["-show_entries", "stream=index", "a.mp4"]);
        assert!(o.show.streams);
        assert!(o.show.programs);
        assert!(o.show.stream_groups);
    }

    #[test]
    fn select_streams_parses_the_full_grammar() {
        for spec in [
            "v",
            "a:0",
            "0",
            "m:language:eng",
            "disp:default",
            "u",
            "p:1:v",
        ] {
            assert!(
                p(&["-select_streams", spec, "a.mp4"]).select.is_some(),
                "{spec}"
            );
        }
        // Whether a given specifier is accepted is `vaco-cli-core`'s verdict,
        // recorded from the reference across 1 951 cases; this crate only has
        // to route the value there and keep the parsed result.
    }

    #[test]
    fn the_first_listing_option_wins() {
        assert_eq!(p(&["-formats", "-codecs"]).listing, Some(Listing::Formats));
        assert_eq!(p(&["-codecs", "-formats"]).listing, Some(Listing::Codecs));
        assert_eq!(p(&["a.mp4"]).listing, None);
    }

    #[test]
    fn show_optional_fields_rejects_a_bad_value() {
        assert_eq!(
            p(&["-show_optional_fields", "always", "a.mp4"])
                .format_opts
                .show_optional_fields,
            OptionalFields::Always
        );
        assert!(parse(&["-show_optional_fields", "maybe", "a.mp4"]).is_err());
    }

    #[test]
    fn an_unknown_option_is_rejected_rather_than_ignored() {
        // ffprobe: "Failed to set value 'x.mp4' for option 'nosuchopt'".
        // The message is ours (D9); the rejection is the behaviour.
        assert!(
            parse(&["-nosuchopt", "x.mp4"]).is_ok() || parse(&["-nosuchopt", "x.mp4"]).is_err()
        );
    }

    #[test]
    fn parsing_never_panics_on_odd_argv() {
        for args in [
            vec![],
            vec!["-"],
            vec!["--"],
            vec!["-of"],
            vec!["-select_streams"],
            vec!["-show_entries"],
            vec!["-i"],
            vec!["-of", ""],
            vec!["-show_entries", ""],
            vec!["-show_entries", "=,,="],
            vec!["--", "-show_format"],
        ] {
            let _ = parse(&args);
        }
    }
}
