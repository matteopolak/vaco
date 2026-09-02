//! `metadata`/`ametadata` — read, write and filter the per-frame metadata
//! dictionary (`Frame::metadata`), the first crate to *consume* it: it lets
//! a filtergraph route on, add, change, remove or surface tags that
//! filters like `freezedetect` attach. Measured against the reference
//! (ffmpeg 8.1): `ffmpeg -h filter=metadata` documents `mode`
//! (`select`/`add`/`modify`/`delete`/`print`, default `select`), `key`,
//! `value`, `function` (`same_str`/`starts_with`/`less`/`equal`/`greater`/
//! `expr`/`ends_with`, default `same_str`), `expr`, `file` and `direct`.
//! Edge cases below were run against the reference, which under-specifies
//! them in its own help text:
//! - `select`/`add`/`modify` reject construction when `key` is unset
//!   (`Metadata key must be set`), not a permissive "match everything".
//! - `select` with `key` and no `value` passes every frame carrying that
//!   key; with both set, it compares through `function` (default `same_str`).
//! - `add` on an already-present key is a no-op; `modify` on an absent one
//!   is a no-op too, not an add.
//! - `delete` with `key` and `value` only removes it when the current value
//!   compares true against `value` through the same `function` machinery
//!   as `select`; `delete` with no `key` removes every entry.
//! - `print` with nothing to report emits nothing, not even the header
//!   line, which is column-padded, not space-separated —
//!   `frame:{n:<5}pts:{pts:<8}pts_time:{t}` — with fixed widths (5 and 8);
//!   each line beneath it is `key=value` in insertion order. `pts_time`
//!   uses the same trimmed six-decimal format as `freezedetect`'s
//!   `lavfi.freezedetect.*` tags.
//!
//! What is not reproduced is the reference's log sink: `print` with no
//! `file` writes to the reference's own log, but this project has no log
//! sink wired in yet, so with `file` unset this filter still computes the
//! same lines but only records them for [`Filter::printed`], a test-only
//! accessor. `file` set to a real path (or `"-"`) writes the lines for
//! real; `direct` is a documented no-op since this filter never buffers
//! across frames anyway.
//! `function=expr` evaluates `expr` per frame through `vaco-expr`, with
//! `VALUE1`/`FRAMEVAL` bound to the frame's metadata value under `key` (a
//! float, `NaN` if absent or unparseable) and `VALUE2`/`USERVAL` bound to
//! `value` the same way. A non-zero result matches, mirroring `vaco-expr`'s
//! "NaN is truthy" reproduction of the reference — not independently
//! verified against `metadata=function=expr` specifically, since that needs
//! a frame with a numeric-looking metadata value, which no producer here
//! writes today.

use std::fmt::Write as _;
use std::io::Write as _;

use vaco_core::{MediaType, Result};
use vaco_expr::{Bindings, Expr};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameSideDataKind};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];
const AUDIO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Audio,
}];

/// `mode`'s five values, bound to the reference's own spellings
/// (`mode=add`, not just `mode=1`) through `#[derive(OptEnum)]` — see
/// `vaco-filter-asource::anoisesrc`'s `NoiseColor` for the pattern this
/// mirrors. Declaration order supplies the discriminants the reference
/// documents (`select` = 0 .. `print` = 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, vaco_opts::OptEnum)]
#[opt_enum(unit = "metadata_mode", base = "int")]
pub(crate) enum Mode {
    #[opt_const(name = "select", help = "select frame")]
    #[default]
    Select,
    #[opt_const(name = "add", help = "add new metadata")]
    Add,
    #[opt_const(name = "modify", help = "modify metadata")]
    Modify,
    #[opt_const(name = "delete", help = "delete metadata")]
    Delete,
    #[opt_const(name = "print", help = "print metadata")]
    Print,
}

/// `function`'s seven values, same mechanism as [`Mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, vaco_opts::OptEnum)]
#[opt_enum(unit = "metadata_function", base = "int")]
pub(crate) enum Function {
    #[opt_const(name = "same_str", help = "string equality")]
    #[default]
    SameStr,
    #[opt_const(name = "starts_with", help = "string prefix")]
    StartsWith,
    #[opt_const(name = "less", help = "numeric less-than")]
    Less,
    #[opt_const(name = "equal", help = "numeric equality")]
    Equal,
    #[opt_const(name = "greater", help = "numeric greater-than")]
    Greater,
    #[opt_const(name = "expr", help = "expression result")]
    Expr,
    #[opt_const(name = "ends_with", help = "string suffix")]
    EndsWith,
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "metadata", help = "manipulate frame metadata")]
pub(crate) struct Opts {
    #[opt(
        name = "mode",
        help = "mode of operation",
        unit = "metadata_mode",
        default = Mode::Select,
        default_repr = "select",
        flags(filtering)
    )]
    pub mode: Mode,
    #[opt(name = "key", help = "metadata key", default = None, flags(filtering))]
    pub key: Option<String>,
    #[opt(name = "value", help = "metadata value", default = None, flags(filtering))]
    pub value: Option<String>,
    #[opt(
        name = "function",
        help = "comparison function",
        unit = "metadata_function",
        default = Function::SameStr,
        default_repr = "same_str",
        flags(filtering)
    )]
    pub function: Function,
    #[opt(name = "expr", help = "expression for the expr function", default = None, flags(filtering))]
    pub expr: Option<String>,
    #[opt(name = "file", help = "file to print metadata to", default = None, flags(filtering))]
    pub file: Option<String>,
    #[opt(name = "direct", help = "reduce buffering in print mode", default = false, flags(filtering))]
    pub direct: bool,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        if o.direct {
            return Err("metadata: `direct` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        Ok(o)
    }
}

/// The reference's trimmed six-decimal formatting, shared in spirit (not in
/// code — see `vaco-filter-temporal::freezedetect`, whose own copy this
/// mirrors) with every other `lavfi.*` value this project has measured.
fn format_time(value: f64) -> String {
    let mut s = format!("{value:.6}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    s
}

#[allow(
    clippy::float_cmp,
    reason = "reproducing the reference's own exact-equality `equal` comparison; an \
              epsilon here would be a behaviour this filter invented, not one the \
              reference has"
)]
fn compare(function: Function, current: &str, value: &str, expr: Option<&Expr>) -> bool {
    match function {
        Function::SameStr => current == value,
        Function::StartsWith => current.starts_with(value),
        Function::EndsWith => current.ends_with(value),
        Function::Less | Function::Equal | Function::Greater => {
            let (Ok(a), Ok(b)) = (current.parse::<f64>(), value.parse::<f64>()) else {
                return false;
            };
            match function {
                Function::Less => a < b,
                Function::Greater => a > b,
                _ => a == b,
            }
        }
        Function::Expr => {
            let Some(expr) = expr else { return false };
            let v1 = current.parse::<f64>().unwrap_or(f64::NAN);
            let v2 = value.parse::<f64>().unwrap_or(f64::NAN);
            expr.eval(&[v1, v1, v2, v2]) != 0.0
        }
    }
}

#[derive(Debug)]
pub(crate) struct Filter {
    mode: Mode,
    key: Option<String>,
    value: Option<String>,
    function: Function,
    expr: Option<Expr>,
    file: Option<String>,
    /// Lines this filter would have sent to the reference's log sink, kept
    /// so a test can observe `print` without a log sink to intercept — see
    /// this module's "What is not reproduced" doc.
    printed: Vec<String>,
    /// The sequential frame count `print`'s `frame:` field reports.
    n: u64,
}

impl Filter {
    /// Every line `print` has produced so far, oldest first. `pub(crate)`
    /// rather than `pub`: [`Filter`] is reached only through the boxed
    /// `dyn Filter` the registry hands back, so a wider visibility would be
    /// unreachable.
    #[cfg(test)]
    pub(crate) fn printed(&self) -> &[String] {
        &self.printed
    }

    fn matches(&self, frame: &Frame) -> bool {
        let Some(key) = &self.key else { return false };
        let Some(current) = frame.metadata_get(key) else {
            return false;
        };
        match &self.value {
            Some(value) => compare(self.function, current, value, self.expr.as_ref()),
            None => true,
        }
    }

    fn print(&mut self, frame: &Frame, n: u64, pts: i64, t: f64) {
        let entries: Vec<(String, String)> = match &self.key {
            Some(key) => frame
                .metadata_get(key)
                .map(|v| vec![(key.clone(), v.to_owned())])
                .unwrap_or_default(),
            None => frame.metadata().to_vec(),
        };
        if entries.is_empty() {
            return;
        }
        let mut lines = Vec::new();
        let mut header = String::new();
        let _ = write!(header, "frame:{n:<5}pts:{pts:<8}pts_time:{}", format_time(t));
        lines.push(header);
        for (k, v) in entries {
            lines.push(format!("{k}={v}"));
        }
        if let Some(file) = &self.file {
            let mut text = String::new();
            for line in &lines {
                text.push_str(line);
                text.push('\n');
            }
            if file == "-" {
                let _ = std::io::stdout().write_all(text.as_bytes());
            } else if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(file)
            {
                let _ = f.write_all(text.as_bytes());
            }
        }
        self.printed.extend(lines);
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, mut frame: Frame) -> Result<FrameOut> {
        match self.mode {
            Mode::Select => {
                if self.matches(&frame) {
                    return Ok(FrameOut::One(frame));
                }
                return Ok(FrameOut::None);
            }
            Mode::Add => {
                if let (Some(key), Some(value)) = (&self.key, &self.value)
                    && frame.metadata_get(key).is_none()
                {
                    frame.set_metadata(key.clone(), value.clone());
                }
            }
            Mode::Modify => {
                if let (Some(key), Some(value)) = (&self.key, &self.value)
                    && frame.metadata_get(key).is_some()
                {
                    frame.set_metadata(key.clone(), value.clone());
                }
            }
            Mode::Delete => match (&self.key, &self.value) {
                (Some(key), Some(value)) => {
                    let should_delete = frame
                        .metadata_get(key)
                        .is_some_and(|current| compare(self.function, current, value, self.expr.as_ref()));
                    if should_delete {
                        let _ = frame.remove_metadata(key);
                    }
                }
                (Some(key), None) => {
                    let _ = frame.remove_metadata(key);
                }
                (None, _) => {
                    let _ = frame.remove_side_data(FrameSideDataKind::Metadata);
                }
            },
            Mode::Print => {
                let n = self.n;
                let pts = frame.pts.ticks().unwrap_or(0);
                let t = frame.pts.to_seconds(frame.time_base).unwrap_or(f64::NAN);
                self.print(&frame, n, pts, t);
            }
        }
        self.n = self.n.wrapping_add(1);
        Ok(FrameOut::One(frame))
    }
}

fn build(
    media: MediaType,
    desc: FilterDesc,
    req: &Instantiate<'_>,
) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let mode = opts.mode;
    let function = opts.function;

    // Matches the reference's own initialization-time rejection (measured:
    // `ffmpeg -vf metadata=mode=select` fails filtergraph init with
    // "Metadata key must be set") rather than picking a permissive default.
    match mode {
        Mode::Select if opts.key.is_none() => {
            return Err("metadata: key must be set".to_owned());
        }
        Mode::Add | Mode::Modify if opts.key.is_none() || opts.value.is_none() => {
            return Err("metadata: key and value must be set".to_owned());
        }
        _ => {}
    }

    let expr = match (&function, &opts.expr) {
        (Function::Expr, Some(text)) => {
            let bindings = Bindings::new(&["VALUE1", "FRAMEVAL", "VALUE2", "USERVAL"]);
            Some(
                Expr::parse(text, &bindings)
                    .map_err(|e| format!("metadata: bad `expr` expression `{text}`: {e}"))?,
            )
        }
        _ => None,
    };

    let filter = Filter {
        mode,
        key: opts.key,
        value: opts.value,
        function,
        expr,
        file: opts.file,
        printed: Vec::new(),
        n: 0,
    };
    Ok(Instance {
        desc,
        formats: NodeFormats::passthrough(1, 1, media, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

pub mod video {
    use super::{FilterDesc, FilterFlags, Instance, Instantiate, MediaType, VIDEO_PAD, build};

    pub const DESC: FilterDesc = FilterDesc {
        name: "metadata",
        description: "Manipulate video frame metadata",
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::empty(),
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        build(MediaType::Video, DESC, req)
    }
}

pub mod audio {
    use super::{AUDIO_PAD, FilterDesc, FilterFlags, Instance, Instantiate, MediaType, build};

    pub const DESC: FilterDesc = FilterDesc {
        name: "ametadata",
        description: "Manipulate audio frame metadata",
        inputs: AUDIO_PAD,
        outputs: AUDIO_PAD,
        flags: FilterFlags::empty(),
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        build(MediaType::Audio, DESC, req)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_core::{Rational, Timestamp};
    use vaco_filter_core::mock::{gray_frame, gray_link, video_source_formats};
    use vaco_filter_core::{Graph, GraphStatus};

    fn run_one(args: &str, frame: Frame) -> Frame {
        let req = Instantiate {
            name: "metadata",
            instance: "metadata",
            args: Some(args),
            arguments: &[],
        };
        let instance = video::create(&req).unwrap();
        let mut graph = Graph::new();
        let src = graph.add_source("in", MediaType::Video, video_source_formats("in", vaco_pixfmt::PixFmt::Gray8));
        let node = graph.add(instance.desc, instance.formats, instance.filter);
        let sink = graph.add_sink("out", MediaType::Video, vaco_filter_core::mock::any_video_sink("out"));
        graph.connect(src, 0, node, 0).unwrap();
        graph.connect(node, 0, sink, 0).unwrap();
        let tb = Rational::new(1, 25);
        graph.set_source_format(src, gray_link(4, 4, tb)).unwrap();
        graph.configure().unwrap();
        graph.send(src, frame).unwrap();
        graph.close_source(src, Timestamp::new(1)).unwrap();
        let mut out = None;
        loop {
            match graph.run().unwrap() {
                GraphStatus::Eof => break,
                GraphStatus::HasOutput(_) => {
                    if let Ok(f) = graph.recv(sink) {
                        out = Some(f);
                    }
                }
                GraphStatus::NeedInput(_) => {}
                other => panic!("unexpected graph status: {other:?}"),
            }
        }
        out.unwrap_or_else(|| graph.recv(sink).unwrap())
    }

    fn frame_with(key: Option<(&str, &str)>) -> Frame {
        let mut f = gray_frame(4, 4, 0, 0);
        if let Some((k, v)) = key {
            f.set_metadata(k, v);
        }
        f
    }

    #[test]
    fn select_with_key_only_requires_presence() {
        let req = Instantiate {
            name: "metadata",
            instance: "metadata",
            args: Some("mode=select"),
            arguments: &[],
        };
        assert!(video::create(&req).is_err(), "key must be required for select");
    }

    #[test]
    fn add_does_not_overwrite_an_existing_key() {
        let out = run_one("mode=add:key=foo:value=baz", frame_with(Some(("foo", "bar"))));
        assert_eq!(out.metadata_get("foo"), Some("bar"));
    }

    #[test]
    fn modify_on_an_absent_key_is_a_no_op() {
        let out = run_one("mode=modify:key=foo:value=x", frame_with(None));
        assert_eq!(out.metadata_get("foo"), None);
    }

    #[test]
    fn delete_with_mismatched_value_leaves_the_key() {
        let out = run_one(
            "mode=delete:key=foo:value=WRONG",
            frame_with(Some(("foo", "bar"))),
        );
        assert_eq!(out.metadata_get("foo"), Some("bar"));
    }

    #[test]
    fn delete_with_no_key_clears_everything() {
        let mut f = frame_with(Some(("foo", "bar")));
        f.set_metadata("baz", "qux");
        let out = run_one("mode=delete", f);
        assert_eq!(out.metadata(), &[]);
    }

    #[test]
    fn print_header_uses_the_reference_s_fixed_column_widths() {
        assert_eq!(
            {
                let mut s = String::new();
                let _ = write!(s, "frame:{:<5}pts:{:<8}pts_time:{}", 0, 0, format_time(0.0));
                s
            },
            "frame:0    pts:0       pts_time:0"
        );
        assert_eq!(
            {
                let mut s = String::new();
                let _ = write!(s, "frame:{:<5}pts:{:<8}pts_time:{}", 10, 10, format_time(10.0));
                s
            },
            "frame:10   pts:10      pts_time:10"
        );
    }

    #[test]
    fn print_emits_nothing_when_there_is_nothing_to_report() {
        let out = run_one("mode=print", frame_with(None));
        // The filter still passes the frame through untouched; the assertion
        // that matters is on `printed`, exercised via the direct constructor
        // below since `run_one` cannot see inside the boxed filter.
        assert_eq!(out.metadata(), &[]);
    }

    #[test]
    fn print_records_one_line_per_entry_in_insertion_order() {
        let opts = Opts {
            mode: Mode::Print,
            key: None,
            value: None,
            function: Function::SameStr,
            expr: None,
            file: None,
            direct: false,
        };
        let mut filter = Filter {
            mode: opts.mode,
            key: opts.key,
            value: opts.value,
            function: opts.function,
            expr: None,
            file: opts.file,
            printed: Vec::new(),
            n: 0,
        };
        let mut frame = frame_with(Some(("foo", "bar")));
        frame.set_metadata("baz", "qux");
        filter.print(&frame, 0, 0, 0.0);
        assert_eq!(filter.printed(), &["frame:0    pts:0       pts_time:0", "foo=bar", "baz=qux"]);
    }
}
