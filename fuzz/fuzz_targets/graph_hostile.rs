//! Plausible-but-hostile filtergraph descriptions, generated from the grammar.
//!
//! `graph_parse` and `graph_build` feed the parser arbitrary bytes. That finds
//! scanner bugs quickly and structural ones slowly: a byte mutator rarely
//! produces a label that is defined twice three chains apart, a `split` whose
//! `outputs=` is a 30-digit number, or a chain whose last filter feeds the
//! first through a forward reference. This target builds those shapes on
//! purpose, from a small pool of names and labels so that collisions are the
//! common case rather than the lucky one, and then injects raw
//! metacharacters so the well-formed skeleton is never quite well formed.
//!
//! It also drives every **error** through `GraphError::render`, which the
//! byte-level targets never did. The caret renderer slices the source by byte
//! offsets a span recorded earlier; an offset that lands inside a multibyte
//! character, or past the end after the printer normalised escaping, is a
//! panic the success path can never reach.
//!
//! What is asserted, in order:
//!
//! 1. `parse` never panics; on error, rendering the diagnostic never panics.
//! 2. On success, `print` then `parse` gives the same tree, and printing is
//!    idempotent.
//! 3. `build` against the mock registry never panics; on error, rendering the
//!    diagnostic never panics. On success the pad bookkeeping is consistent.
//! 4. Attaching sources and sinks to every open pad and configuring — with
//!    and without auto-conversion — never panics.
//!
//! fuzz-crate: vaco-filter-graph
#![no_main]

use arbitrary::Arbitrary;
use core::fmt::Write as _;
use libfuzzer_sys::fuzz_target;
use vaco_core::{MediaType, Rational};
use vaco_filter_core::mock::{
    any_audio_sink, any_video_sink, audio_link, audio_source_formats, gray_link,
    video_source_formats,
};
use vaco_filter_core::negotiate::AutoConvert;
use vaco_filter_graph::mock::MockRegistry;
use vaco_filter_graph::{GraphError, build, parse};
use vaco_pixfmt::PixFmt;

/// Everything the mock registry knows, so most generated filters instantiate
/// and the interesting failures happen *after* the name lookup.
const KNOWN: &[&str] = &[
    "counter",
    "null",
    "anull",
    "invert",
    "format",
    "aformat",
    "split",
    "merge",
    "amerge",
    "scale",
    "aresample",
];

/// Near misses and non-names: the suggestion engine, the `@` split, the
/// `sws_flags=` prefix misplaced, and the shapes the reference is documented
/// to accept as (unknown) names.
const TYPOS: &[&str] = &[
    "scael",
    "nul",
    "spilt",
    "",
    "sws_flags",
    "sws_flags=bicubic",
    "@",
    "@x",
    "null@",
    "null@@x",
    "Parsed_null_0",
    "null ",
    "null]x",
    "nu ll",
    "日本語",
    "ｎｕｌｌ",
    "null\u{0}",
];

/// A pool small enough that the same label is reused constantly: duplicates,
/// forward references, cycles, and a label consumed twice are all one draw
/// away. Some entries are hostile in themselves.
const LABELS: &[&str] = &[
    "a", "b", "c", "in", "out", "0:v", "0:a", "a:0", "日本", "é", "", " ", "\\]", "x'y", "'a'",
    "a b", "[", "aa",
];

const PIXFMTS: &[&str] = &[
    "gray",
    "yuv420p",
    "rgb24",
    "nope",
    "",
    "gray|rgb24",
    "gray|",
    "|",
    "yuv420p|yuv444p|rgb24",
    "gray\\|rgb24",
];

const SAMPLEFMTS: &[&str] = &["s16", "fltp", "nope", "s16|flt", "", "|"];

/// Bare words for option keys and values.
const WORDS: &[&str] = &[
    "n",
    "w",
    "h",
    "inputs",
    "outputs",
    "pix_fmts",
    "sample_fmts",
    "sample_rates",
    "flags",
    "text",
    "",
    "=",
    "key=value",
    "a:b",
    "x|y",
];

#[derive(Arbitrary, Debug)]
enum Name {
    Known(u8),
    Typo(u8),
    /// A long run of one letter: the suggestion engine's edit distance is
    /// quadratic in the two lengths.
    Long(u8),
    Raw(String),
}

#[derive(Arbitrary, Debug)]
enum Text {
    Word(u8),
    Number(u64),
    /// Beyond `u64`, beyond `usize`, beyond anything `parse` accepts.
    Huge,
    Negative(u32),
    Float,
    Spaces,
    Quoted(Box<Text>),
    Escaped(Box<Text>),
    Raw(String),
}

#[derive(Arbitrary, Debug)]
enum Arg {
    Named(Text, Text),
    Positional(Text),
    Outputs(Text),
    Inputs(Text),
    PixFmts(u8),
    SampleFmts(u8),
    SampleRates(Text),
    Count(Text),
    /// An argument list that is pure noise: `=`, `:`, and `|` in every order.
    Noise(u8),
}

#[derive(Arbitrary, Debug)]
enum LabelShape {
    Ok,
    /// `[a` with no `]`.
    Unterminated,
    /// `[[a]]`.
    Doubled,
    /// `[]`.
    Empty,
    /// `[ a ]`.
    Spaced,
    /// `[a\]b]`: a `]` that must not close the label.
    EscapedClose,
}

#[derive(Arbitrary, Debug)]
struct Label {
    name: u8,
    shape: LabelShape,
}

#[derive(Arbitrary, Debug)]
enum Sep {
    Comma,
    Semi,
    Nothing,
    DoubleComma,
    DoubleSemi,
    Newline,
    /// `, ;`, which the reference reads as an empty filter name.
    CommaSemi,
}

#[derive(Arbitrary, Debug)]
struct Filter {
    inputs: Vec<Label>,
    name: Name,
    instance: Option<Text>,
    args: Option<Vec<Arg>>,
    outputs: Vec<Label>,
    /// Bare whitespace in the places the grammar permits it.
    ws: u8,
    sep: Sep,
}

/// Raw characters to splice into the rendered string.
const GARBAGE: &[&str] = &[
    "[", "]", ",", ";", "=", ":", "'", "\\", "@", "\n", "\t", "\u{0}", "é", "日", "|", " ",
];

#[derive(Arbitrary, Debug)]
struct Input {
    sws: Option<Text>,
    filters: Vec<Filter>,
    /// Repeat the whole body this many times (plus one): a graph with the same
    /// labels defined over and over, and the instance counter climbing.
    repeat: u8,
    /// `(position, garbage index)` splices, applied after rendering.
    garbage: Vec<(u16, u8)>,
    auto_convert: bool,
}

/// Enough to hold hundreds of filters; small enough that libFuzzer explores
/// shapes rather than length.
const MAX_LEN: usize = 32 * 1024;
const MAX_FILTERS: usize = 256;

fn pick<'a>(pool: &[&'a str], i: u8) -> &'a str {
    pool.get(usize::from(i) % pool.len().max(1))
        .copied()
        .unwrap_or("")
}

fn render_text(out: &mut String, t: &Text) {
    // `Escaped(Escaped(..))` doubles the text per level; without this the
    // generator itself timed out on a 129-byte input.
    if out.len() > MAX_LEN {
        return;
    }
    match t {
        Text::Word(i) => out.push_str(pick(WORDS, *i)),
        Text::Number(n) => {
            let _ = write!(out, "{n}");
        }
        Text::Huge => out.push_str("999999999999999999999999999999"),
        Text::Negative(n) => {
            let _ = write!(out, "-{n}");
        }
        Text::Float => out.push_str("2.5"),
        Text::Spaces => out.push_str("   "),
        Text::Quoted(inner) => {
            out.push('\'');
            render_text(out, inner);
            out.push('\'');
        }
        Text::Escaped(inner) => {
            let mut tmp = String::new();
            render_text(&mut tmp, inner);
            for c in tmp.chars().take(512) {
                out.push('\\');
                out.push(c);
            }
        }
        Text::Raw(s) => out.push_str(s),
    }
}

fn render_arg(out: &mut String, a: &Arg) {
    match a {
        Arg::Named(k, v) => {
            render_text(out, k);
            out.push('=');
            render_text(out, v);
        }
        Arg::Positional(v) => render_text(out, v),
        Arg::Outputs(v) => {
            out.push_str("outputs=");
            render_text(out, v);
        }
        Arg::Inputs(v) => {
            out.push_str("inputs=");
            render_text(out, v);
        }
        Arg::PixFmts(i) => {
            out.push_str("pix_fmts=");
            out.push_str(pick(PIXFMTS, *i));
        }
        Arg::SampleFmts(i) => {
            out.push_str("sample_fmts=");
            out.push_str(pick(SAMPLEFMTS, *i));
        }
        Arg::SampleRates(v) => {
            out.push_str("sample_rates=");
            render_text(out, v);
        }
        Arg::Count(v) => {
            out.push_str("n=");
            render_text(out, v);
        }
        Arg::Noise(i) => out.push_str(pick(
            &["=", ":", "|", "==", "::", ":=", "=:", "'", "\\"],
            *i,
        )),
    }
}

fn render_label(out: &mut String, l: &Label) {
    let name = pick(LABELS, l.name);
    match l.shape {
        LabelShape::Ok => {
            let _ = write!(out, "[{name}]");
        }
        LabelShape::Unterminated => {
            let _ = write!(out, "[{name}");
        }
        LabelShape::Doubled => {
            let _ = write!(out, "[[{name}]]");
        }
        LabelShape::Empty => out.push_str("[]"),
        LabelShape::Spaced => {
            let _ = write!(out, "[ {name} ]");
        }
        LabelShape::EscapedClose => {
            let _ = write!(out, "[{name}\\]b]");
        }
    }
}

fn render_filter(out: &mut String, f: &Filter) {
    let ws = |out: &mut String, bit: u8| {
        if f.ws & bit != 0 {
            out.push(' ');
        }
    };
    for l in &f.inputs {
        render_label(out, l);
    }
    ws(out, 1);
    match &f.name {
        Name::Known(i) => out.push_str(pick(KNOWN, *i)),
        Name::Typo(i) => out.push_str(pick(TYPOS, *i)),
        Name::Long(n) => out.extend(core::iter::repeat_n('n', usize::from(*n) * 16)),
        Name::Raw(s) => out.push_str(s),
    }
    if let Some(id) = &f.instance {
        out.push('@');
        render_text(out, id);
    }
    ws(out, 2);
    if let Some(args) = &f.args {
        out.push('=');
        for (i, a) in args.iter().enumerate() {
            if i > 0 {
                out.push(':');
            }
            render_arg(out, a);
        }
    }
    ws(out, 4);
    for l in &f.outputs {
        render_label(out, l);
    }
    ws(out, 8);
    out.push_str(match f.sep {
        Sep::Comma => ",",
        Sep::Semi => ";",
        Sep::Nothing => "",
        Sep::DoubleComma => ",,",
        Sep::DoubleSemi => ";;",
        Sep::Newline => ",\n",
        Sep::CommaSemi => ", ;",
    });
}

fn render(input: &Input) -> String {
    let mut body = String::new();
    for f in input.filters.iter().take(MAX_FILTERS) {
        render_filter(&mut body, f);
        if body.len() > MAX_LEN {
            break;
        }
    }
    let mut out = String::new();
    if let Some(sws) = &input.sws {
        out.push_str("sws_flags=");
        render_text(&mut out, sws);
        out.push(';');
    }
    for _ in 0..=input.repeat {
        out.push_str(&body);
        if out.len() > MAX_LEN {
            break;
        }
    }
    for (pos, g) in &input.garbage {
        let at = usize::from(*pos) % out.len().saturating_add(1);
        if out.is_char_boundary(at) {
            out.insert_str(at, pick(GARBAGE, *g));
        }
    }
    // Truncate on a boundary so the string is still valid UTF-8.
    let mut cut = out.len().min(MAX_LEN);
    while !out.is_char_boundary(cut) {
        cut = cut.saturating_sub(1);
    }
    out.truncate(cut);
    out
}

/// Every way a caller can look at an error, none of which may panic.
fn exercise_error(e: &GraphError, src: &str) {
    let rendered = e.render(src);
    assert!(rendered.starts_with("error: "), "{rendered:?}");
    let _ = e.to_string();
    let _ = vaco_core::Error::from(e.clone());
}

fn check(src: &str, auto_convert: bool) {
    let ast = match parse(src) {
        Ok(ast) => ast,
        Err(e) => {
            exercise_error(&e, src);
            return;
        }
    };

    assert!(
        !ast.chains.is_empty(),
        "a parse that succeeded has no chains"
    );
    for chain in &ast.chains {
        assert!(!chain.filters.is_empty(), "empty chain: {src:?}");
        for filter in &chain.filters {
            assert!(!filter.name.is_empty(), "empty filter name: {src:?}");
            if let Err(e) = filter.arguments() {
                exercise_error(&e, src);
            }
        }
    }

    let printed = ast.to_string();
    let reparsed = match parse(&printed) {
        Ok(a) => a,
        Err(e) => panic!(
            "printing {src:?} produced unparseable {printed:?}:\n{}",
            e.render(&printed)
        ),
    };
    assert_eq!(
        ast.without_spans(),
        reparsed.without_spans(),
        "round trip changed the tree: {src:?} -> {printed:?}"
    );
    assert_eq!(
        printed,
        reparsed.to_string(),
        "printing is not idempotent: {src:?}"
    );

    let registry = MockRegistry::new();
    let mut built = match build(&ast, &registry) {
        Ok(b) => b,
        Err(e) => {
            exercise_error(&e, src);
            // The printed form must fail the same way, or the printer changed
            // the meaning of the description.
            let again = build(&reparsed, &registry);
            assert!(
                again.is_err(),
                "{src:?} fails to build but {printed:?} builds"
            );
            return;
        }
    };

    let node_count = built.graph.node_count();
    assert_eq!(built.nodes.len(), node_count, "node bookkeeping diverged");
    let mut inputs: Vec<(u32, u32)> = Vec::new();
    let mut outputs: Vec<(u32, u32)> = Vec::new();
    for link in built.graph.links().iter() {
        let src_pad = (link.src().node.0, link.src().pad);
        let dst_pad = (link.dst().node.0, link.dst().pad);
        assert!((link.src().node.0 as usize) < node_count);
        assert!((link.dst().node.0 as usize) < node_count);
        assert!(!inputs.contains(&dst_pad), "input pad fed twice: {src:?}");
        assert!(
            !outputs.contains(&src_pad),
            "output pad read twice: {src:?}"
        );
        inputs.push(dst_pad);
        outputs.push(src_pad);
    }
    for open in &built.open_inputs {
        assert!(
            !inputs.contains(&(open.node.0, open.pad)),
            "open input is connected: {src:?}"
        );
    }
    for open in &built.open_outputs {
        assert!(
            !outputs.contains(&(open.node.0, open.pad)),
            "open output is connected: {src:?}"
        );
    }
    let _ = built.to_dot();
    let _ = built.dump();

    // Attach something to every open pad, then negotiate. `attach_*` only
    // fails when the scheduler refuses a link it was just told about, so that
    // is a finding too.
    while let Some(open) = built.open_inputs.first() {
        let result = match open.media {
            MediaType::Video => built.attach_source(
                0,
                video_source_formats("in", PixFmt::Gray8),
                gray_link(16, 16, Rational::new(1, 25)),
            ),
            MediaType::Audio => {
                built.attach_source(0, audio_source_formats("in", 48_000), audio_link(48_000))
            }
            _ => return,
        };
        if let Err(e) = result {
            panic!("attaching a source to an open input failed: {e:?} for {src:?}");
        }
    }
    while let Some(open) = built.open_outputs.first() {
        let result = match open.media {
            MediaType::Video => built.attach_sink(0, any_video_sink("out")),
            MediaType::Audio => built.attach_sink(0, any_audio_sink("out")),
            _ => return,
        };
        if let Err(e) = result {
            panic!("attaching a sink to an open output failed: {e:?} for {src:?}");
        }
    }
    let mode = if auto_convert {
        AutoConvert::All
    } else {
        AutoConvert::None
    };
    // Negotiation may legitimately fail (`format=pix_fmts=rgb24` behind a gray
    // source with conversion off). It must not panic.
    let _ = built.configure(&registry, mode);
}

fuzz_target!(|input: Input| {
    let src = render(&input);
    if src.is_empty() {
        return;
    }
    // The artifact is the `arbitrary` byte stream, not the graph string; this
    // is how to see what a crashing input actually said.
    if std::env::var_os("GRAPH_HOSTILE_DUMP").is_some() {
        eprintln!("graph_hostile: {src:?}");
    }
    check(&src, input.auto_convert);
});
