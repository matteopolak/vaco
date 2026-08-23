//! `pan` — remix channels with coefficients (panning).
//!
//! `ffmpeg -h filter=pan` documents exactly one option, `args`, a positional
//! string: `LAYOUT|OUTSPEC|OUTSPEC|...` where each `OUTSPEC` is
//! `NAME=[GAIN*]IN[[+-][GAIN*]IN...]` — a linear combination of input
//! channels, nothing else (no functions, no parentheses, in the documented
//! grammar).
//!
//! # Does `vaco-expr` fit?
//!
//! Yes, and by a trick worth recording. `vaco-expr` is a general arithmetic
//! engine, strictly richer than `pan`'s tiny grammar, so parsing an `OUTSPEC`
//! with [`Expr::parse`] accepts a harmless superset (e.g. a stray
//! parenthesis) rather than rejecting it — the same permissive-superset shape
//! D17 already accepts for the colour table. What it does *not* give for
//! free is the coefficient matrix `pan` actually needs: `Expr::eval` returns
//! one number for one set of input values, not a set of linear coefficients.
//!
//! The fix is exact for a genuinely linear expression: evaluate it once per
//! input channel with that channel's binding set to `1.0` and every other
//! binding set to `0.0`. Homogeneity of a linear form means the result *is*
//! that channel's coefficient. Evaluating at every unit basis vector this way
//! reconstructs the whole row of the mixing matrix without writing a second
//! parser. It would silently produce a wrong (but still linear-in-appearance)
//! answer if a user wrote a non-linear `OUTSPEC` such as `c0*c1` — the
//! reference's own mini-parser cannot express that either, so this is not a
//! new gap.
//!
//! **Not implemented**: resolving a channel by name (`FL`, `FR`, ...) on
//! either side of `=`. Both `OUTSPEC`'s left-hand name and every input
//! reference on the right must be the numeric `cN` form. `LAYOUT` itself
//! (the first `|`-separated field) is a full layout name or `<n>c`, exactly
//! as the reference accepts, via [`ChannelLayout::from_name`].

use smallvec::SmallVec;
use vaco_chlayout::ChannelLayout;
use vaco_core::{Error, MediaType, Result};
use vaco_expr::{Bindings, Expr};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

const AUDIO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Audio,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "pan",
    description: "remix channels with coefficients (panning)",
    inputs: AUDIO_PAD,
    outputs: AUDIO_PAD,
    flags: FilterFlags::empty(),
};

/// A generous ceiling on `cN` indices, so a malicious `NAME` cannot turn into
/// a multi-exabyte `Vec::resize` — the same class of finding
/// `channelmap`'s fuzz target caught (see its `MAX_CHANNEL_INDEX` doc).
const MAX_CHANNEL_INDEX: usize = 4096;

fn resolve_out_index(name: &str, seq: usize) -> usize {
    name.strip_prefix('c')
        .and_then(|n| n.parse::<usize>().ok())
        .filter(|&n| n <= MAX_CHANNEL_INDEX)
        .unwrap_or(seq)
}

#[derive(Debug)]
pub(crate) struct Filter {
    out_layout: ChannelLayout,
    /// One `OUTSPEC` right-hand side per output channel, still unparsed:
    /// parsing needs the input channel count, known only at `configure`.
    specs: Vec<String>,
    /// `matrix[out][in]`, built in `configure`.
    matrix: Vec<Vec<f64>>,
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let in_channels = match ctx.input_link(0) {
            Some(LinkFormat::Audio { layout, .. }) => layout.channels.max(1) as usize,
            _ => 1,
        };
        let names: Vec<String> = (0..in_channels).map(|i| format!("c{i}")).collect();
        let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let bindings = Bindings::new(&name_refs);

        let mut matrix = Vec::new();
        for spec in &self.specs {
            let expr = Expr::parse(spec, &bindings)
                .map_err(|_| Error::InvalidData("pan: bad channel expression"))?;
            let mut row = Vec::new();
            for i in 0..in_channels {
                let mut vars = vec![0.0; in_channels];
                if let Some(slot) = vars.get_mut(i) {
                    *slot = 1.0;
                }
                row.push(expr.eval(&vars));
            }
            matrix.push(row);
        }
        self.matrix = matrix;

        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Audio { layout, .. } = &mut out {
                *layout = self.out_layout.clone();
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, samples, _, channels) = crate::sample::decode(&input)?;
        let mut out_channels: SmallVec<[Vec<f64>; 8]> = SmallVec::new();
        for row in &self.matrix {
            let mut ch = vec![0.0f64; samples as usize];
            for (in_idx, &gain) in row.iter().enumerate() {
                if gain == 0.0 {
                    continue;
                }
                let Some(src) = channels.get(in_idx) else {
                    continue;
                };
                for (k, slot) in ch.iter_mut().enumerate() {
                    *slot += gain * src.get(k).copied().unwrap_or(0.0);
                }
            }
            out_channels.push(ch);
        }
        let mut out = crate::sample::encode(
            &vaco_frame::FramePool::default(),
            fmt,
            self.out_layout.clone(),
            rate,
            &out_channels,
        )?;
        out.pts = input.pts;
        out.time_base = input.time_base;
        out.duration = input.duration;
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    // `pan`'s entire grammar is one opaque `LAYOUT|OUTSPEC|...` blob with no
    // top-level `:`, so it is the raw pre-`:`-split text the DSL scanner
    // handed over — not something to re-derive from the parsed positional
    // list, which a test built by hand may not have populated consistently
    // with `args`.
    let raw = req
        .args
        .map(str::to_owned)
        .or_else(|| req.positional(0))
        .or_else(|| req.named("args"))
        .ok_or_else(|| "pan: missing arguments".to_owned())?;
    let mut fields = raw.split('|');
    let layout_spec = fields.next().unwrap_or_default();
    let out_layout = ChannelLayout::from_name(layout_spec)
        .ok_or_else(|| format!("pan: bad layout `{layout_spec}`"))?;

    let mut specs: Vec<String> = Vec::new();
    for (seq, field) in fields.enumerate() {
        let Some((lhs, rhs)) = field.split_once('=') else {
            return Err(format!("pan: `{field}` is not `NAME=expr`"));
        };
        let idx = resolve_out_index(lhs.trim(), seq);
        if specs.len() <= idx {
            specs.resize(idx + 1, "0".to_owned());
        }
        if let Some(slot) = specs.get_mut(idx) {
            rhs.trim().clone_into(slot);
        }
    }
    if specs.is_empty() {
        return Err("pan: no output channel specs".to_owned());
    }

    let filter = Filter {
        out_layout: out_layout.clone(),
        specs,
        matrix: Vec::new(),
    };

    Ok(Instance {
        desc: DESC,
        formats: NodeFormats {
            inputs: vec![FormatSet::default()],
            outputs: vec![FormatSet {
                channel_layouts: Some(vaco_filter_core::negotiate::Constraint::Exact(out_layout)),
                ..FormatSet::default()
            }],
            // Sample format and rate pass straight through unchanged — only
            // the channel layout is this filter's business, so that alone is
            // deliberately left untied (an `Any` output with no tie is a
            // requirement to solve, not a default; see the crate doc).
            ties: vec![
                vaco_filter_core::negotiate::Tie {
                    property: vaco_filter_core::negotiate::Property::SampleFormat,
                    pads: vec![
                        (vaco_filter_core::link::Direction::Input, 0),
                        (vaco_filter_core::link::Direction::Output, 0),
                    ],
                },
                vaco_filter_core::negotiate::Tie {
                    property: vaco_filter_core::negotiate::Property::SampleRate,
                    pads: vec![
                        (vaco_filter_core::link::Direction::Input, 0),
                        (vaco_filter_core::link::Direction::Output, 0),
                    ],
                },
            ],
            label: req.instance.to_owned(),
        },
        filter: Box::new(Simple::new(filter)),
    })
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
    use vaco_filter_core::mock::audio_link;
    use vaco_filter_core::{Graph, GraphStatus};

    /// `pan=stereo|c0=c1|c1=c0` swaps left and right — the one-line linear
    /// combination the reference documents (`OUT=IN`, gain 1), and an exact
    /// check that the unit-basis-vector coefficient extraction this module's
    /// docs describe actually recovers a permutation matrix rather than
    /// something merely close to it.
    #[test]
    fn swaps_left_and_right() {
        let req = Instantiate {
            name: "pan",
            instance: "pan",
            args: Some("stereo|c0=c1|c1=c0"),
            arguments: &[],
        };
        let instance = create(&req).unwrap();

        let mut graph = Graph::new();
        let src = graph.add_source(
            "in",
            MediaType::Audio,
            vaco_filter_core::mock::audio_source_formats("in", 8000),
        );
        let node = graph.add(instance.desc, instance.formats, instance.filter);
        let sink = graph.add_sink(
            "out",
            MediaType::Audio,
            vaco_filter_core::mock::any_audio_sink("out"),
        );
        graph.connect(src, 0, node, 0).unwrap();
        graph.connect(node, 0, sink, 0).unwrap();
        graph.set_source_format(src, audio_link(8000)).unwrap();
        graph.configure().unwrap();

        // One S16 stereo sample: left = 1000, right = -1000.
        let pool = vaco_frame::FramePool::default();
        let mut frame = pool
            .acquire_audio(vaco_sampfmt::SampleFmt::S16, ChannelLayout::STEREO, 1, 8000)
            .unwrap();
        if let Some(mut plane) = frame.plane_mut(0)
            && let Some(row) = plane.row_mut(0)
        {
            row[0..2].copy_from_slice(&1000i16.to_le_bytes());
            row[2..4].copy_from_slice(&(-1000i16).to_le_bytes());
        }
        frame.pts = vaco_core::Timestamp::new(0);
        frame.time_base = vaco_core::Rational::new(1, 8000);

        graph.send(src, frame).unwrap();
        graph
            .close_source(src, vaco_core::Timestamp::new(1))
            .unwrap();
        // `run()` may legitimately stop at `HasOutput` before a sink drains
        // rather than `Eof` — that is backpressure, not failure.
        match graph.run().unwrap() {
            GraphStatus::Eof | GraphStatus::HasOutput(_) => {}
            other => panic!("unexpected graph status: {other:?}"),
        }

        let out = graph.recv(sink).unwrap();
        let plane = out.plane(0).unwrap();
        let bytes = plane.as_slice();
        let left = i16::from_le_bytes([bytes[0], bytes[1]]);
        let right = i16::from_le_bytes([bytes[2], bytes[3]]);
        assert_eq!(left, -1000, "c0=c1 should have copied the right channel");
        assert_eq!(right, 1000, "c1=c0 should have copied the left channel");
    }
}
