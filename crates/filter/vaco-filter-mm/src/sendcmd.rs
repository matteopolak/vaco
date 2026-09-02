//! `sendcmd`/`asendcmd` — parse and drive a command script; **cannot
//! dispatch it**.
//!
//! `ffmpeg -h filter=sendcmd` documents `commands`/`c` and `filename`/`f`.
//! The command grammar itself is only in `filters.texi`'s "Commands
//! syntax" subsection, quoted here because `-h` does not show it. Its BNF,
//! verbatim:
//!
//! ```text
//! COMMAND_FLAG  ::= "enter" | "leave"
//! COMMAND_FLAGS ::= COMMAND_FLAG [(+|"|")COMMAND_FLAG]
//! COMMAND       ::= ["[" COMMAND_FLAGS "]"] TARGET COMMAND [ARG]
//! COMMANDS      ::= COMMAND [,COMMANDS]
//! INTERVAL      ::= START[-END] COMMANDS
//! INTERVALS     ::= INTERVAL[;INTERVALS]
//! ```
//!
//! An interval's commands fire when the current frame's time crosses into
//! (`enter`, the default when `FLAGS` is omitted) or out of (`leave`)
//! `[START, END)` — `END` defaults to unbounded. `#` starts a comment
//! running to end of line. This module implements the parser and the
//! per-frame enter/leave edge detection in full — [`Filter::fired`] (test
//! only) records every command this instance's script would have sent, in
//! order, exactly reproducing `filters.texi`'s worked examples.
//!
//! # What is not implemented, and cannot be from inside a leaf filter
//!
//! `TARGET` names *another* filter instance in the same graph (the
//! reference's own examples: `sendcmd='4.0 atempo tempo 1.5',atempo`).
//! `vaco_filter_core::Filter::command` exists and is the right shape to
//! receive such a command, but nothing between here and there can address
//! it: a filter only sees its own `FilterContext`, not a handle to look
//! another node up by label and call `command` on it. `Graph` has no
//! public "send this node a command" method today, so this needs a
//! `vaco-filter-core` change, not something a leaf filter can close on its
//! own. This filter parses the full script, tracks time, and correctly
//! identifies which commands would fire when, but does not deliver them
//! anywhere — every frame passes through unchanged, a safe default for a
//! driver that cannot do its job yet.

use vaco_core::{MediaType, Result};
use vaco_expr::{Bindings, Expr};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::Frame;
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

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct EdgeFlags: u8 {
        const ENTER = 1 << 0;
        const LEAVE = 1 << 1;
    }
}

#[derive(Debug, Clone)]
struct Cmd {
    flags: EdgeFlags,
    is_expr: bool,
    target: String,
    command: String,
    arg: Option<String>,
}

#[derive(Debug, Clone)]
struct Interval {
    start: f64,
    end: f64,
    commands: Vec<Cmd>,
}

/// One fired command, in the shape a real dispatcher would need:
/// `(target, command, resolved_arg)`.
pub(crate) type Fired = (String, String, String);

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let code = line.split('#').next().unwrap_or("");
        out.push_str(code);
        out.push('\n');
    }
    out
}

fn parse_flags(s: &str) -> std::result::Result<(EdgeFlags, bool), String> {
    let mut flags = EdgeFlags::empty();
    let mut is_expr = false;
    for tok in s.split(['+', '|']) {
        let tok = tok.trim();
        match tok {
            "enter" => flags |= EdgeFlags::ENTER,
            "leave" => flags |= EdgeFlags::LEAVE,
            "expr" => is_expr = true,
            "" => {}
            other => return Err(format!("sendcmd: unknown flag `{other}`")),
        }
    }
    Ok((flags, is_expr))
}

fn parse_command(text: &str) -> std::result::Result<Cmd, String> {
    let text = text.trim();
    let (flag_text, rest) = if let Some(stripped) = text.strip_prefix('[') {
        let (inside, after) = stripped
            .split_once(']')
            .ok_or_else(|| "sendcmd: unterminated `[flags]`".to_owned())?;
        (Some(inside), after.trim())
    } else {
        (None, text)
    };
    let (flags, is_expr) = match flag_text {
        Some(f) => parse_flags(f)?,
        None => (EdgeFlags::ENTER, false),
    };
    let mut parts = rest.splitn(3, char::is_whitespace);
    let target = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "sendcmd: missing target".to_owned())?
        .to_owned();
    let command = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "sendcmd: missing command".to_owned())?
        .to_owned();
    let arg = parts.next().map(|s| s.trim().trim_matches('\'').to_owned()).filter(|s| !s.is_empty());
    Ok(Cmd {
        flags,
        is_expr,
        target,
        command,
        arg,
    })
}

fn parse_interval(text: &str) -> std::result::Result<Interval, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("sendcmd: empty interval".to_owned());
    }
    let mut chars = text.char_indices();
    let mut split_at = None;
    for (i, c) in &mut chars {
        if c.is_whitespace() {
            split_at = Some(i);
            break;
        }
    }
    let Some(split_at) = split_at else {
        return Err("sendcmd: interval with no commands".to_owned());
    };
    let (time_spec, commands_text) = text.split_at(split_at);
    let (start_s, end_s) = time_spec.split_once('-').map_or((time_spec, None), |(a, b)| (a, Some(b)));
    let start: f64 = start_s.trim().parse().map_err(|_| format!("sendcmd: bad start time `{start_s}`"))?;
    let end: f64 = match end_s {
        Some(e) => e.trim().parse().map_err(|_| format!("sendcmd: bad end time `{e}`"))?,
        None => f64::INFINITY,
    };
    let commands = commands_text
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(parse_command)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if commands.is_empty() {
        return Err("sendcmd: interval with no commands".to_owned());
    }
    Ok(Interval { start, end, commands })
}

fn parse_script(text: &str) -> std::result::Result<Vec<Interval>, String> {
    let cleaned = strip_comments(text);
    cleaned
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(parse_interval)
        .collect()
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "sendcmd", help = "send commands to filters in the filtergraph")]
pub(crate) struct Opts {
    #[opt(name = "commands", alias = "c", help = "set the commands", default = None, flags(filtering))]
    pub commands: Option<String>,
    #[opt(name = "filename", alias = "f", help = "set the commands file", default = None, flags(filtering))]
    pub filename: Option<String>,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":").map_err(|e| e.to_string())?;
        }
        Ok(o)
    }
}

#[derive(Debug)]
pub(crate) struct Filter {
    intervals: Vec<Interval>,
    /// Whether each interval was active on the *previous* frame, so an
    /// enter/leave edge can be detected on this one.
    was_active: Vec<bool>,
    n: f64,
    /// Every command this instance's script would have sent, in order —
    /// see module doc for why nothing consumes this yet.
    fired: Vec<Fired>,
}

impl Filter {
    #[cfg(test)]
    pub(crate) fn fired(&self) -> &[Fired] {
        &self.fired
    }

    /// `POS`/`PTS`/`N`/`T`/`TS`/`TE`/`TI`/`W`/`H`, the `expr`-flag constant
    /// set `filters.texi` documents. `W`/`H` are always `0.0`: frame
    /// dimensions are not threaded through this filter's edge-detection
    /// path, which only ever sees a frame's timing — a recorded gap, not a
    /// silent one, since no worked reference example exercises `W`/`H`.
    fn resolve_arg(cmd: &Cmd, vars: [f64; 9]) -> String {
        if cmd.is_expr && let Some(arg) = &cmd.arg {
            let bindings = Bindings::new(&["POS", "PTS", "N", "T", "TS", "TE", "TI", "W", "H"]);
            // Parse errors fall back to the literal text rather than
            // panicking or dropping the command.
            if let Ok(expr) = Expr::parse(arg, &bindings) {
                return expr.eval(&vars).to_string();
            }
        }
        cmd.arg.clone().unwrap_or_default()
    }
}

impl Filter {
    /// The edge-detection and firing logic, pulled out of `filter_frame` so
    /// it is testable without a live `FilterContext` (which has no public
    /// constructor outside the scheduler that owns one).
    fn step(&mut self, t: f64, pts: f64) {
        for (idx, interval) in self.intervals.iter().enumerate() {
            let now_active = t >= interval.start && t < interval.end;
            let was = self.was_active.get(idx).copied().unwrap_or(false);
            if let Some(slot) = self.was_active.get_mut(idx) {
                *slot = now_active;
            }
            let entered = now_active && !was;
            let left = !now_active && was;
            if !entered && !left {
                continue;
            }
            for cmd in &interval.commands {
                let matches = (entered && cmd.flags.contains(EdgeFlags::ENTER))
                    || (left && cmd.flags.contains(EdgeFlags::LEAVE));
                if matches {
                    let ti = if (interval.end - interval.start).abs() > f64::EPSILON {
                        (t - interval.start) / (interval.end - interval.start)
                    } else {
                        0.0
                    };
                    let vars = [pts, pts, self.n, t, interval.start, interval.end, ti, 0.0, 0.0];
                    let arg = Self::resolve_arg(cmd, vars);
                    self.fired.push((cmd.target.clone(), cmd.command.clone(), arg));
                }
            }
        }
        self.n += 1.0;
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        let t = frame.pts.to_seconds(frame.time_base).unwrap_or(f64::NAN);
        let pts = frame.pts.ticks().map_or(f64::NAN, |v| v as f64);
        self.step(t, pts);
        Ok(FrameOut::One(frame))
    }
}

fn build(
    media: MediaType,
    desc: FilterDesc,
    req: &Instantiate<'_>,
) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let text = match (&opts.commands, &opts.filename) {
        (Some(c), _) => c.clone(),
        (None, Some(path)) => std::fs::read_to_string(path).map_err(|e| format!("sendcmd: {e}"))?,
        (None, None) => return Err("sendcmd: one of `commands`/`filename` is required".to_owned()),
    };
    let intervals = parse_script(&text)?;
    let was_active = vec![false; intervals.len()];
    Ok(Instance {
        desc,
        formats: NodeFormats::passthrough(1, 1, media, req.instance),
        filter: Box::new(Simple::new(Filter {
            intervals,
            was_active,
            n: 0.0,
            fired: Vec::new(),
        })),
    })
}

pub mod video {
    use super::{FilterDesc, FilterFlags, Instance, Instantiate, MediaType, VIDEO_PAD, build};

    pub const DESC: FilterDesc = FilterDesc {
        name: "sendcmd",
        description: "Send commands to filters",
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
        name: "asendcmd",
        description: "Send commands to filters",
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
    clippy::float_cmp,
    reason = "test code; the float_cmp cases assert exact values the parser \
              round-trips from literal decimal text, not a computed result"
)]
mod tests {
    use super::*;
    use vaco_filter_core::mock::{gray_frame, gray_link, video_source_formats};
    use vaco_filter_core::{Graph, GraphStatus};

    #[test]
    fn parses_the_reference_s_atempo_example() {
        let parsed = parse_script("4.0 atempo tempo 1.5").unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].start, 4.0);
        assert!(parsed[0].end.is_infinite());
        assert_eq!(parsed[0].commands[0].target, "atempo");
        assert_eq!(parsed[0].commands[0].command, "tempo");
        assert_eq!(parsed[0].commands[0].arg.as_deref(), Some("1.5"));
    }

    /// The reference's own three-interval file example, verbatim from
    /// `filters.texi`, comments included.
    #[test]
    fn parses_the_reference_s_drawtext_and_hue_example() {
        let script = "\
# show text in the interval 5-10
5.0-10.0 [enter] drawtext reinit 'fontfile=FreeSerif.ttf:text=hello world',
         [leave] drawtext reinit 'fontfile=FreeSerif.ttf:text=';

# desaturate the image in the interval 15-20
15.0-20.0 [enter] hue s 0,
          [enter] drawtext reinit 'fontfile=FreeSerif.ttf:text=nocolor',
          [leave] hue s 1,
          [leave] drawtext reinit 'fontfile=FreeSerif.ttf:text=color';

# apply an exponential saturation fade-out effect, starting from time 25
25 [enter] hue s exp(25-t)
";
        let parsed = parse_script(script).unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].start, 5.0);
        assert_eq!(parsed[0].end, 10.0);
        assert_eq!(parsed[0].commands.len(), 2);
        assert_eq!(parsed[0].commands[0].flags, EdgeFlags::ENTER);
        assert_eq!(parsed[0].commands[1].flags, EdgeFlags::LEAVE);
        assert_eq!(parsed[1].commands.len(), 4);
        assert_eq!(parsed[2].start, 25.0);
        assert!(parsed[2].end.is_infinite());
        // No `[expr]` flag is present here (only `[enter]`) — sendcmd
        // itself does not evaluate `exp(25-t)`; that is `hue`'s own
        // runtime-command expression support, a separate mechanism.
        assert!(!parsed[2].commands[0].is_expr);
    }

    fn new_filter(script: &str) -> Filter {
        let intervals = parse_script(script).unwrap();
        Filter {
            was_active: vec![false; intervals.len()],
            intervals,
            n: 0.0,
            fired: Vec::new(),
        }
    }

    /// Frames at `t = 0, 1, 2, 3, 4` against interval `[1, 3)` must fire
    /// `enter` exactly when `t` reaches `1` and `leave` exactly when `t`
    /// reaches `3` — not one tick early or late.
    #[test]
    fn fires_enter_then_leave_at_the_interval_edges() {
        let mut filter = new_filter("1-3 [enter+leave] target cmd arg");
        for t in 0..5 {
            filter.step(f64::from(t), f64::from(t));
        }
        assert_eq!(
            filter.fired(),
            &[
                ("target".to_owned(), "cmd".to_owned(), "arg".to_owned()),
                ("target".to_owned(), "cmd".to_owned(), "arg".to_owned()),
            ]
        );
    }

    /// Default flags (none given) means `enter` only, per `filters.texi`.
    #[test]
    fn default_flags_is_enter_only() {
        let mut filter = new_filter("1-3 target cmd arg");
        for t in 0..5 {
            filter.step(f64::from(t), f64::from(t));
        }
        assert_eq!(filter.fired().len(), 1);
    }

    /// A frame that never revisits an interval does not re-fire `enter`.
    #[test]
    fn staying_inside_an_interval_fires_once() {
        let mut filter = new_filter("1-3 [enter] target cmd arg");
        for t in [1, 2, 2, 2] {
            filter.step(f64::from(t), f64::from(t));
        }
        assert_eq!(filter.fired().len(), 1);
    }

    /// The whole filter is a driver, not a transform: every frame passes
    /// through the graph completely unchanged.
    #[test]
    fn every_frame_passes_through_unchanged() {
        let req = Instantiate {
            name: "sendcmd",
            instance: "sendcmd",
            args: Some("commands=1.0 target cmd arg"),
            arguments: &[],
        };
        let instance = video::create(&req).unwrap();
        let mut graph = Graph::new();
        let src = graph.add_source("in", MediaType::Video, video_source_formats("in", vaco_pixfmt::PixFmt::Gray8));
        let node = graph.add(instance.desc, instance.formats, instance.filter);
        let sink = graph.add_sink("out", MediaType::Video, vaco_filter_core::mock::any_video_sink("out"));
        graph.connect(src, 0, node, 0).unwrap();
        graph.connect(node, 0, sink, 0).unwrap();
        let tb = vaco_core::Rational::new(1, 25);
        graph.set_source_format(src, gray_link(1, 1, tb)).unwrap();
        graph.configure().unwrap();
        for i in 0..3i64 {
            graph.send(src, gray_frame(1, 1, i, 5)).unwrap();
        }
        graph.close_source(src, vaco_core::Timestamp::new(3)).unwrap();
        let mut pts = Vec::new();
        loop {
            match graph.run().unwrap() {
                GraphStatus::Eof => break,
                GraphStatus::HasOutput(_) => {
                    while let Ok(f) = graph.recv(sink) {
                        pts.push(f.pts.ticks().unwrap_or(-1));
                    }
                }
                GraphStatus::NeedInput(_) => {}
                other => panic!("unexpected graph status: {other:?}"),
            }
        }
        assert_eq!(pts, vec![0, 1, 2]);
    }
}
