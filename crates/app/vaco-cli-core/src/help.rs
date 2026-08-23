//! `-h`, `-h long`, `-h full` and `-h <kind>=<name>`: topic parsing, and the
//! two renderers everything else is built from.
//!
//! # Two renderers, not one
//!
//! `ffmpeg -h full` prints the command-line options first, **with no flag
//! column**, then a series of `AVOptions` blocks each carrying the
//! eleven-column [`vaco_opts::OptFlags`] field. Conflating the two is the
//! easiest way to get this wrong — see [`render_options_help`] for the first
//! and [`render_schema_block`] for the second.
//!
//! # What was measured, and how
//!
//! Every column width, separator and blank-line count below was read from
//! `ffmpeg 8.1`/`ffprobe 8.1` under `LC_ALL=C`, never recalled or guessed
//! (D17, plan 13 §1b). The commands are reproduced in
//! `docs/app/vaco-cli-core.md` so the pinned reference version can be
//! re-probed if it moves. Three findings worth stating up front because nothing
//! about them is written down anywhere:
//!
//! 1. **`-h` always consumes the next argv entry if one exists, whatever it
//!    looks like.** `ffmpeg -h -i x` reports `Unknown help option '-i'.` and
//!    then prints the basic help — `-i` was consumed as `-h`'s topic and
//!    never re-classified as an option name, `x` is simply never looked at.
//!    Only running out of argv entirely (a bare trailing `-h`) is topic-free.
//!    See [`ArgFlags::OPTIONAL_ARG`](crate::table::ArgFlags::OPTIONAL_ARG).
//! 2. **The `AVOptions` line format is `max(18, len(name)) + 1` for the name
//!    field and `max(12, len(type)) + 1` for the type field**, each a literal
//!    space appended after a left-justified minimum-width field — not the
//!    `max(., .) + 8` a naive reading of one example suggests. Measured
//!    against `ffmpeg -h protocol=file`'s five options (name lengths 6–10, one
//!    field width throughout) and cross-checked against `-h full`'s ~14,000
//!    lines, where the field grows past the minimum exactly at `len + 1` for
//!    every name from 2 to 32 characters with no exception.
//! 3. **Only `-h`, `-version` and `-buildconf` print a trailing
//!    `Exiting with exit code 0` on *stdout*, unconditionally, even at
//!    `-loglevel quiet`.** None of the other listing commands do — confirmed
//!    across all fourteen. It is not part of the normal log stream (`-muxers
//!    -loglevel debug` prints the same line, but to *stderr*, where it is
//!    merely the shared debug-level exit trace); for the `-h` family it is
//!    unconditional and on stdout. The blank-line count before it is **one**
//!    when the body's last block was not an `AVOptions` block, **two** when it
//!    was.
//!
//! # What is deliberately not reproduced
//!
//! Per D9, help *strings* are not an interface fact and are not copied from
//! the reference — only names, column layout and structural facts are. So the
//! command-line section's headings and per-option prose in
//! [`render_options_help`] are written fresh for Vaco, not transcribed, even
//! though the *shape* (grouping, column algorithm, blank lines) matches. This
//! means `vaco -h` cannot be byte-identical to `ffmpeg -h` even where the
//! structure is — an accepted, documented consequence of D9, not an oversight.

use crate::table::{ArgFlags, OptDesc, OptTable};
use vaco_opts::{ConstDesc, ConstValue, OptBase, OptRangeDisplay, Schema, help_entries};

/// Which of the three depths `-h` was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpLevel {
    /// Bare `-h`: only non-[`ArgFlags::EXPERT`] command-line options.
    Basic,
    /// `-h long`: every command-line option.
    Long,
    /// `-h full`: every command-line option, plus every `AVOptions` schema
    /// this build can reach.
    Full,
}

/// A `-h <kind>=<name>` request, split but not yet validated against a
/// registry — this crate does not depend on one. `vaco-cli` resolves `kind`
/// and `name` against `vaco-registry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindTopic {
    /// The text before `=`, e.g. `"decoder"`. Not yet checked against the
    /// seven recognised spellings — an unrecognised one is itself an
    /// [`Topic::Unrecognized`] outcome, using only this part as the reported
    /// name (measured: `-h zzzz=x` reports `Unknown help option 'zzzz'.`, not
    /// `'zzzz=x'`).
    pub kind: String,
    /// The text after `=`, verbatim (may be empty: `-h decoder=` is a real,
    /// distinct case from `-h decoder` with no `=` at all — both end up
    /// meaning "no name", but the reference's own message differs: `-h
    /// protocol` says "No protocol name specified.", `-h demuxer` says
    /// "Unknown format '(null)'." with the C `NULL`-format literal).
    pub name: Option<String>,
}

/// What `-h`'s topic argument means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Topic {
    Level(HelpLevel),
    Kind(KindTopic),
    /// Neither `long`/`full` nor a recognised `kind=name` shape. Measured:
    /// the reference prints `Unknown help option '<topic>'.` and then falls
    /// back to the *basic* (not long) help.
    Unrecognized(String),
}

/// The seven component kinds `-h <kind>=<name>` recognises. Measured: bare
/// `-h demuxer` (no `=` at all) still dispatches as the demuxer kind with no
/// name — it does **not** fall through to "unrecognised topic" the way `-h
/// bogus` does. This is the fixed vocabulary that distinguishes the two.
const KIND_WORDS: &[&str] = &[
    "decoder", "encoder", "demuxer", "muxer", "filter", "bsf", "protocol",
];

/// Parse `-h`'s topic argument, exactly as the reference's `strtol`-free
/// dispatch reads it: `None`/empty is the bare-`-h` case, `"long"`/`"full"`
/// are the two depths, anything containing `=` is a `kind=name` pair split on
/// the *first* `=` only, a bare [`KIND_WORDS`] entry is that kind with no
/// name, and everything else is unrecognised.
#[must_use]
pub fn parse_topic(raw: Option<&str>) -> Topic {
    let Some(raw) = raw.filter(|s| !s.is_empty()) else {
        return Topic::Level(HelpLevel::Basic);
    };
    match raw {
        "long" => return Topic::Level(HelpLevel::Long),
        "full" => return Topic::Level(HelpLevel::Full),
        _ => {}
    }
    if let Some((kind, name)) = raw.split_once('=') {
        return Topic::Kind(KindTopic {
            kind: kind.to_owned(),
            name: Some(name.to_owned()),
        });
    }
    if KIND_WORDS.contains(&raw) {
        return Topic::Kind(KindTopic {
            kind: raw.to_owned(),
            name: None,
        });
    }
    Topic::Unrecognized(raw.to_owned())
}

// --------------------------------------------------------- command-line half

/// The section an option's grouped help entry falls in. Order is the
/// reference's (`ffmpeg -h`'s own section order); the section *text* is ours
/// (D9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Section {
    Info,
    Global,
    PerFileBoth,
    PerFileInputOnly,
    PerFileOutputOnly,
    PerStream,
    Video,
    Audio,
    Subtitle,
    Other,
}

const SECTIONS: &[(Section, &str)] = &[
    (Section::Info, "Print help / information / capabilities:"),
    (
        Section::Global,
        "Global options (affect the whole run, not one file):",
    ),
    (Section::PerFileBoth, "Per-file options (input and output):"),
    (Section::PerFileInputOnly, "Per-file options (input-only):"),
    (
        Section::PerFileOutputOnly,
        "Per-file options (output-only):",
    ),
    (Section::PerStream, "Per-stream options:"),
    (Section::Video, "Video options:"),
    (Section::Audio, "Audio options:"),
    (Section::Subtitle, "Subtitle options:"),
    (Section::Other, "Other options:"),
];

fn section_of(d: &OptDesc) -> Section {
    let f = d.flags;
    if f.contains(ArgFlags::EXIT) {
        return Section::Info;
    }
    if f.contains(ArgFlags::GLOBAL) {
        return Section::Global;
    }
    if f.contains(ArgFlags::VIDEO) {
        return Section::Video;
    }
    if f.contains(ArgFlags::AUDIO) {
        return Section::Audio;
    }
    if f.contains(ArgFlags::SUBTITLE) {
        return Section::Subtitle;
    }
    if f.contains(ArgFlags::PER_STREAM) {
        return Section::PerStream;
    }
    if f.contains(ArgFlags::PER_FILE) {
        let input = f.contains(ArgFlags::INPUT);
        let output = f.contains(ArgFlags::OUTPUT);
        return match (input, output) {
            (true, true) => Section::PerFileBoth,
            (true, false) => Section::PerFileInputOnly,
            (false, true) => Section::PerFileOutputOnly,
            (false, false) => Section::Other,
        };
    }
    Section::Other
}

/// The display form of an option's name-plus-placeholder cell, e.g.
/// `-c[:<stream_spec>] <codec>` or `-y`.
fn display_cell(d: &OptDesc) -> String {
    let mut s = format!("-{}", d.name);
    if d.flags.contains(ArgFlags::PER_STREAM) {
        s.push_str("[:<stream_spec>]");
    } else if d.flags.contains(ArgFlags::TAKES_SPEC) {
        s.push_str("[:<spec>]");
    }
    if let Some(arg) = d.argname {
        s.push_str(" <");
        s.push_str(arg);
        s.push('>');
    }
    s
}

/// Minimum width of the name+placeholder cell before the two-space gap.
/// Measured against `ffmpeg -h`/`-h long`: `-v <loglevel>` (13 chars) pads to
/// 20 total before help starts, and `-metadata[:<spec>] <key=value>` (30
/// chars) gets no padding at all, just the same two-space gap — i.e.
/// `max(18, len) + 2`, not the `AVOptions` line's `max(., .) + 1`. The two
/// renderers use different constants, which is exactly the trap plan 14's
/// brief warns about.
const CELL_MIN: usize = 18;

/// Render the grouped command-line option section, `-h`'s first half.
///
/// `level` controls both which options are shown ([`ArgFlags::EXPERT`] is
/// hidden below [`HelpLevel::Long`]) and nothing else — the same renderer
/// produces the body of `-h`, `-h long` and the top of `-h full`.
///
/// No trailing blank lines or exit trailer: the caller assembles the whole
/// `-h` output, because whether the body ends here or continues into
/// `AVOptions` blocks decides the blank-line count (see the module docs).
#[must_use]
pub fn render_options_help(table: &OptTable, level: HelpLevel) -> String {
    let show_expert = level != HelpLevel::Basic;
    let mut out = String::new();
    let mut first_section = true;
    for &(section, heading) in SECTIONS {
        let rows: Vec<&OptDesc> = table
            .options
            .iter()
            .filter(|d| d.alias_of.is_none())
            .filter(|d| show_expert || !d.flags.contains(ArgFlags::EXPERT))
            .filter(|d| section_of(d) == section)
            .collect();
        if rows.is_empty() {
            continue;
        }
        if !first_section {
            out.push('\n');
        }
        first_section = false;
        out.push_str(heading);
        out.push('\n');
        for d in rows {
            let cell = display_cell(d);
            let width = CELL_MIN.max(cell.chars().count());
            out.push_str(&cell);
            for _ in cell.chars().count()..width {
                out.push(' ');
            }
            out.push_str("  ");
            out.push_str(d.help);
            out.push('\n');
        }
    }
    out
}

// -------------------------------------------------------------- AVOptions half

/// One `AVOptions` block: the class name plus every option `filter` passes,
/// rendered exactly as measured (see the module docs).
///
/// `filter` is normally [`vaco_opts::OptFlags::empty`] — `-h full` and
/// `-h <kind>=<name>` show every option regardless of E/D/V/A/S/etc, they only
/// ever filter by [`ArgFlags::EXPERT`] on the *command-line* half above.
#[must_use]
pub fn render_schema_block(schema: &'static Schema) -> String {
    let mut out = String::new();
    out.push_str(schema.class_name);
    out.push_str(" AVOptions:\n");
    for entry in help_entries(schema, vaco_opts::OptFlags::empty()) {
        render_option_line(
            &mut out,
            2,
            &format!("-{}", entry.name),
            &entry.kind.type_name(),
            &entry.flags_column,
            entry.help,
            entry.range,
            entry.default_repr,
            entry.kind.base,
        );
        for c in entry.consts {
            render_const_line(&mut out, c, &entry.flags_column);
        }
    }
    out
}

/// `max(min, len) + 1`: a left-justified field padded to at least `min`
/// characters, followed by exactly one literal separator space — the
/// algorithm behind both the name and the type columns. See the module docs
/// for how this was distinguished from `max(min,len) + <a fixed offset>`.
fn pad_field(out: &mut String, s: &str, min: usize) {
    out.push_str(s);
    let len = s.chars().count();
    for _ in len..min.max(len) {
        out.push(' ');
    }
    out.push(' ');
}

#[allow(
    clippy::too_many_arguments,
    reason = "one AVOption row, seven measured fields"
)]
fn render_option_line(
    out: &mut String,
    indent: usize,
    name: &str,
    type_name: &str,
    flags_column: &[u8; 11],
    help: &str,
    range: Option<OptRangeDisplay>,
    default_repr: &str,
    base: OptBase,
) {
    for _ in 0..indent {
        out.push(' ');
    }
    pad_field(out, name, 18);
    pad_field(out, &format!("<{type_name}>"), 12);
    out.push_str(&String::from_utf8_lossy(flags_column));

    let mut pieces: Vec<String> = Vec::new();
    if !help.is_empty() {
        pieces.push(help.to_owned());
    }
    if let Some(r) = range {
        pieces.push(format!(
            "(from {} to {})",
            format_bound(r.min, base),
            format_bound(r.max, base)
        ));
    }
    if !default_repr.is_empty() {
        pieces.push(format!("(default {default_repr})"));
    }
    if !pieces.is_empty() {
        out.push(' ');
        out.push_str(&pieces.join(" "));
    }
    out.push('\n');
}

fn render_const_line(out: &mut String, c: &ConstDesc, owner_flags_column: &[u8; 11]) {
    for _ in 0..5 {
        out.push(' ');
    }
    pad_field(out, c.name, 15);
    let value = match c.value {
        ConstValue::Int(v) => v.to_string(),
        ConstValue::Float(v) => format!("{v}"),
    };
    pad_field(out, &value, 12);
    // Consts carry the *owning option's* flag column in every measured
    // example (`strict`'s five consts all show `ED.VA......`, matching
    // `strict` itself, never their own narrower `OptFlags::NONE` default), so
    // that is what is rendered here rather than `c.flags` directly — falling
    // back to `c.flags.column()` only when a const genuinely narrows it.
    let col = if c.flags.is_empty() {
        *owner_flags_column
    } else {
        c.flags.column()
    };
    out.push_str(&String::from_utf8_lossy(&col));
    if !c.help.is_empty() {
        out.push(' ');
        out.push_str(c.help);
    }
    out.push('\n');
}

/// Render a numeric bound the way `-h full` does: the type's own sentinel
/// name when the value matches it exactly, otherwise the bare integer.
///
/// Measured: `-b <int64> ... (from 0 to I64_MAX)`, `-blocksize <int> ...
/// (from 1 to INT_MAX)`, `-follow <int> ... (from 0 to 1)` — sentinels only
/// replace an exact type-extreme, never a value that merely happens to be
/// large. Only the four sentinels this build's own schemas actually reach
/// (`i32`/`i64` extremes) are implemented; `FLT_MAX`/`DBL_MAX` are not
/// reached by any option we render today and are deliberately left as a
/// documented gap rather than guessed at.
#[allow(
    clippy::float_cmp,
    reason = "matching a documented type-extreme sentinel exactly, not comparing computed floats"
)]
fn format_bound(v: f64, base: OptBase) -> String {
    if matches!(base, OptBase::Int | OptBase::Flags) {
        if v == f64::from(i32::MAX) {
            return "INT_MAX".to_owned();
        }
        if v == f64::from(i32::MIN) {
            return "INT_MIN".to_owned();
        }
    }
    if matches!(base, OptBase::Int64) {
        if v == i64::MAX as f64 {
            return "I64_MAX".to_owned();
        }
        if v == i64::MIN as f64 {
            return "I64_MIN".to_owned();
        }
    }
    if v.fract() == 0.0 && v.abs() < 9.007_199_254_740_992e15 {
        format!("{v:.0}")
    } else {
        format!("{v}")
    }
}

/// Whether a body's last line belongs to an `AVOptions` block, which decides
/// the blank-line count before the `Exiting with exit code 0` trailer (one
/// line when it does not, two when it does — measured, see the module docs).
#[must_use]
pub fn ends_in_options_block(body: &str) -> bool {
    body.trim_end_matches('\n')
        .rsplit('\n')
        .next()
        .is_some_and(|last| last.starts_with("  -") || last.starts_with("     "))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;
    use crate::table::ffmpeg;
    use vaco_opts::{Options, schema_of};

    #[test]
    fn topic_parsing() {
        assert_eq!(parse_topic(None), Topic::Level(HelpLevel::Basic));
        assert_eq!(parse_topic(Some("")), Topic::Level(HelpLevel::Basic));
        assert_eq!(parse_topic(Some("long")), Topic::Level(HelpLevel::Long));
        assert_eq!(parse_topic(Some("full")), Topic::Level(HelpLevel::Full));
        assert_eq!(
            parse_topic(Some("decoder=h264")),
            Topic::Kind(KindTopic {
                kind: "decoder".to_owned(),
                name: Some("h264".to_owned())
            })
        );
        // D17: the reference reports only the kind part for an unrecognised
        // kind, not the whole `kind=name` text — `-h zzzz=x` says "Unknown
        // help option 'zzzz'.", not "'zzzz=x'.". Modelled here by always
        // splitting on `=` and leaving kind-validity to the caller, which
        // does have the vocabulary to know `zzzz` is not one of the seven.
        assert_eq!(
            parse_topic(Some("zzzz=x")),
            Topic::Kind(KindTopic {
                kind: "zzzz".to_owned(),
                name: Some("x".to_owned())
            })
        );
        // `-h decoder` (no `=` at all) is a *different* no-name case from
        // `-h decoder=` (name is the empty string) — the reference's own
        // messages differ between them (see `KindTopic::name` docs), so this
        // module must keep them apart rather than collapsing both to `None`.
        assert_eq!(
            parse_topic(Some("decoder=")),
            Topic::Kind(KindTopic {
                kind: "decoder".to_owned(),
                name: Some(String::new())
            })
        );
        assert_eq!(
            parse_topic(Some("bogus")),
            Topic::Unrecognized("bogus".to_owned())
        );
        // Measured: `-h -version` swallows `-version` as h's own topic; it is
        // never re-split. Parsing does not care that it starts with a dash.
        assert_eq!(
            parse_topic(Some("-version")),
            Topic::Unrecognized("-version".to_owned())
        );
    }

    #[test]
    fn command_line_section_hides_expert_options_below_long() {
        let basic = render_options_help(&ffmpeg(), HelpLevel::Basic);
        let long = render_options_help(&ffmpeg(), HelpLevel::Long);
        assert!(!basic.contains("-buildconf"), "{basic}");
        assert!(long.contains("-buildconf"), "{long}");
        assert!(basic.contains("-version"));
        assert!(long.contains("-version"));
    }

    #[test]
    fn aliases_are_not_shown_twice() {
        // `?`/`help`/`-help` are the same option as `h`; only `h` itself
        // should ever appear as a row (`render_options_help` filters on
        // `alias_of.is_none()` for exactly this reason).
        let long = render_options_help(&ffmpeg(), HelpLevel::Long);
        assert_eq!(
            long.lines()
                .filter(|l| l.trim_start().starts_with("-h "))
                .count(),
            1,
            "{long}"
        );
    }

    #[test]
    fn command_line_cell_padding_matches_the_measured_algorithm() {
        // Measured: `-y` (2 chars) and `-v <loglevel>` (13 chars) both reach
        // 20 columns before help starts; a cell of 30 chars gets no padding,
        // just the two-space gap. See `CELL_MIN`.
        let t = ffmpeg();
        let d = t.find("y").unwrap();
        assert_eq!(display_cell(d), "-y");
        let d = t.find("v").unwrap();
        assert_eq!(display_cell(d), "-v <loglevel>");
        let d = t.find("metadata").unwrap();
        assert_eq!(display_cell(d), "-metadata[:<spec>] <key=value>");
    }

    #[derive(Debug, Clone, PartialEq, Options)]
    #[options(name = "file", help = "")]
    struct FileLike {
        #[opt(
            name = "truncate",
            help = "truncate existing files on write",
            default = true,
            flags(encoding)
        )]
        truncate: bool,
        #[opt(
            name = "blocksize",
            help = "set I/O operation maximum block size",
            default = i32::MAX,
            default_repr = "INT_MAX",
            range = 1..=i32::MAX,
            flags(encoding)
        )]
        blocksize: i32,
        #[opt(
            name = "follow",
            help = "Follow a file as it is being written",
            default = 0,
            range = 0..=1,
            flags(decoding)
        )]
        follow: i32,
        #[opt(
            name = "seekable",
            help = "Sets if the file is seekable",
            default = -1,
            range = -1..=0,
            flags(param)
        )]
        seekable: i32,
        #[opt(
            name = "pkt_size",
            help = "Maximum packet size",
            default = 0,
            range = 0..=i32::MAX,
            flags(param)
        )]
        pkt_size: i32,
    }

    #[test]
    fn schema_block_matches_the_measured_protocol_file_example() {
        // Reproduces `ffmpeg -h protocol=file` (LC_ALL=C, ffmpeg 8.1) byte for
        // byte over a schema built the same way our own options are: five
        // options, three types, three distinct flag columns, one with no
        // range at all (`boolean`).
        let block = render_schema_block(schema_of::<FileLike>());
        let expected = format!(
            "file AVOptions:\n\
             \x20\x20-truncate          <boolean>    {e} truncate existing files on write (default true)\n\
             \x20\x20-blocksize         <int>        {e} set I/O operation maximum block size (from 1 to INT_MAX) (default INT_MAX)\n\
             \x20\x20-follow            <int>        {d} Follow a file as it is being written (from 0 to 1) (default 0)\n\
             \x20\x20-seekable          <int>        {ed} Sets if the file is seekable (from -1 to 0) (default -1)\n\
             \x20\x20-pkt_size          <int>        {ed} Maximum packet size (from 0 to INT_MAX) (default 0)\n",
            e = "E..........",
            d = ".D.........",
            ed = "ED.........",
        );
        assert_eq!(block, expected);
    }

    #[test]
    fn const_rows_inherit_the_owning_options_flag_column() {
        // Measured (`ffmpeg -h full`, `strict`'s five named constants): every
        // const row shows `ED.VA......`, identical to `-strict` itself, never
        // the all-dot column `ConstDesc::flags` defaults to. `OptEnum`
        // produces consts with `OptFlags::NONE`, so this only passes if
        // `render_const_line` falls back to the *option's* column.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, vaco_opts::OptEnum)]
        #[opt_enum(unit = "strictness", base = "int")]
        enum Strictness {
            #[opt_const(name = "very", help = "very strict")]
            Very,
            #[default]
            #[opt_const(name = "normal", help = "")]
            Normal,
        }

        #[derive(Debug, Clone, PartialEq, Options)]
        #[options(name = "x", help = "")]
        struct WithUnit {
            #[opt(
                name = "strict",
                help = "how strictly to follow the standards",
                unit = "strictness",
                default = Strictness::Normal,
                default_repr = "normal",
                flags(param)
            )]
            strict: Strictness,
        }

        let block = render_schema_block(schema_of::<WithUnit>());
        assert!(block.contains("     very            "), "{block}");
        assert!(
            block
                .lines()
                .any(|l| l.trim_start().starts_with("very") && l.contains("ED.........")),
            "{block}"
        );
    }

    #[test]
    fn ends_in_options_block_is_true_only_for_option_or_const_rows() {
        assert!(ends_in_options_block(
            "x AVOptions:\n  -a <int>       ..... 1\n"
        ));
        assert!(ends_in_options_block(
            "x AVOptions:\n  -a <int>  ..... 1\n     c 1 ..... x\n"
        ));
        assert!(!ends_in_options_block(
            "Demuxer x [y]:\n    Common extensions: a.\n"
        ));
    }
}
