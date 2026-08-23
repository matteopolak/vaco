//! `concat` — concatenate `n` segments, each of `v` video and `a` audio
//! streams, into `v + a` continuous output streams.
//!
//! `ffmpeg -h filter=concat` documents `n` (segments, default 2), `v` (video
//! streams per segment, default 1), `a` (audio streams per segment, default
//! 0) and `unsafe` (skip the reference's format-matching checks, default
//! false). Input pad count is `n * (v + a)`; output pad count is `v + a`.
//!
//! # The plumbing risk: timestamp rebasing
//!
//! Each segment's frames arrive with their own timestamps, typically
//! starting near zero, and the concatenated output must present them as one
//! continuous stream: segment `k+1`'s first frame must land immediately
//! after segment `k`'s last one. This filter tracks, **per output stream
//! independently**, a running `offset` — the end timestamp of every prior
//! segment on that stream, in that stream's own time base — and adds it to
//! every frame's PTS before pushing.
//!
//! **Simplification, stated plainly**: the reference switches every stream
//! in a segment to the next segment at the same moment, using the *shortest*
//! stream in that segment as the cut point (a scan of `libavfilter`'s public
//! behaviour description; not independently re-verified here against every
//! edge case). This implementation instead advances each output stream to
//! its next segment independently, the instant *that stream's own* input pad
//! for the current segment reaches end of stream. For the overwhelmingly
//! common case — every stream in a segment has the same duration, which is
//! true of any file demuxed normally — the two are indistinguishable. They
//! diverge only if a segment's audio and video tracks have different
//! lengths, which `-unsafe` off would normally have already rejected on the
//! reference. `unsafe` itself is accepted and parsed but never used to gate
//! anything, since this filter performs no format-matching validation at
//! all — negotiation's own tie mechanism already requires every segment's
//! same-index stream to share a format.

use vaco_core::{MediaType, Result, Timestamp};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats, Tie};
use vaco_filter_core::{
    Activity, Filter as FilterTrait, FilterContext, FilterDesc, FilterFlags, Pad,
};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate, pads};

pub const DESC: FilterDesc = FilterDesc {
    name: "concat",
    description: "Concatenate audio and video streams",
    inputs: &[],
    outputs: &[],
    flags: FilterFlags::DYNAMIC_INPUTS.union(FilterFlags::DYNAMIC_OUTPUTS),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "concat", help = "concatenate audio and video streams")]
pub(crate) struct Opts {
    #[opt(name = "n", help = "number of segments", default = 2, range = 1..=i32::MAX, flags(filtering))]
    pub n: i32,
    #[opt(name = "v", help = "number of video streams", default = 1, range = 0..=i32::MAX, flags(filtering))]
    pub v: i32,
    #[opt(name = "a", help = "number of audio streams", default = 0, range = 0..=i32::MAX, flags(filtering))]
    pub a: i32,
    #[opt(
        name = "unsafe",
        help = "enable unsafe mode",
        default = false,
        flags(filtering)
    )]
    pub unsafe_mode: bool,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        Ok(o)
    }
}

#[derive(Debug, Default)]
struct StreamState {
    segment: usize,
    /// Accumulated end timestamp of every prior segment on this stream, in
    /// this stream's own link time base.
    offset: i64,
    /// This segment's running end, updated as frames arrive, so it is ready
    /// the instant the segment closes.
    running_end: i64,
}

#[derive(Debug)]
pub(crate) struct Concat {
    segments: usize,
    streams: usize,
    state: Vec<StreamState>,
}

impl Concat {
    fn input_pad(&self, stream: usize, segment: usize) -> usize {
        segment.saturating_mul(self.streams).saturating_add(stream)
    }
}

impl FilterTrait for Concat {
    fn activate(&mut self, ctx: &mut FilterContext<'_>) -> Result<Activity> {
        let mut progressed = false;
        let mut all_closed = true;

        for stream in 0..self.streams {
            if ctx.output_closed(stream) {
                continue;
            }
            all_closed = false;
            if !ctx.output_has_room(stream) {
                continue;
            }
            let Some(state) = self.state.get(stream).map(|s| (s.segment, s.offset)) else {
                continue;
            };
            let (segment, offset) = state;
            let pad = self.input_pad(stream, segment);

            if let Some(mut frame) = ctx.take_input(pad) {
                let duration = frame.duration.0.max(0);
                let base = frame.pts.ticks().unwrap_or(0);
                frame.pts = Timestamp::new(base.saturating_add(offset));
                if let Some(s) = self.state.get_mut(stream) {
                    s.running_end = base.saturating_add(duration);
                }
                ctx.push_output(stream, frame)?;
                progressed = true;
            } else if ctx.input_at_eof(pad) {
                let end_pts = ctx.input_end_pts(pad).ticks();
                if let Some(s) = self.state.get_mut(stream) {
                    let segment_len = end_pts.unwrap_or(s.running_end);
                    s.offset = s.offset.saturating_add(segment_len);
                }
                if segment + 1 < self.segments {
                    if let Some(s) = self.state.get_mut(stream) {
                        s.segment += 1;
                        s.running_end = 0;
                    }
                } else {
                    ctx.close_output_at(
                        stream,
                        Timestamp::new(
                            offset.saturating_add(
                                self.state.get(stream).map_or(0, |s| s.running_end),
                            ),
                        ),
                    );
                }
                progressed = true;
            } else {
                ctx.request_input(pad);
            }
        }

        if all_closed {
            return Ok(Activity::Eof);
        }
        if progressed {
            return Ok(Activity::Progressed);
        }
        Ok(Activity::NeedInput)
    }

    fn flush(&mut self) {
        for s in &mut self.state {
            s.segment = 0;
            s.offset = 0;
            s.running_end = 0;
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let segments = usize::try_from(opts.n.max(1)).unwrap_or(1);
    let v = usize::try_from(opts.v.max(0)).unwrap_or(0);
    let a = usize::try_from(opts.a.max(0)).unwrap_or(0);
    let streams = v.saturating_add(a);
    if streams == 0 {
        return Err("concat: at least one video or audio stream is required".to_owned());
    }
    let total_inputs = segments
        .checked_mul(streams)
        .ok_or_else(|| "concat: n * (v + a) overflows".to_owned())?;
    if total_inputs > pads_limit() || streams > pads_limit() {
        return Err("concat: too many inputs or outputs".to_owned());
    }

    let mut input_pads: Vec<Pad> = Vec::new();
    for _ in 0..segments {
        for i in 0..streams {
            input_pads.push(Pad {
                name: "dynamic",
                media_type: if i < v {
                    MediaType::Video
                } else {
                    MediaType::Audio
                },
            });
        }
    }
    let mut output_pads: Vec<Pad> = Vec::new();
    for i in 0..streams {
        output_pads.push(Pad {
            name: "dynamic",
            media_type: if i < v {
                MediaType::Video
            } else {
                MediaType::Audio
            },
        });
    }

    let state: Vec<StreamState> = (0..streams)
        .map(|_| StreamState {
            segment: 0,
            offset: 0,
            running_end: 0,
        })
        .collect();

    // Tie every segment's copy of stream `i` to output `i` — same format
    // required across the whole run of one logical stream, which is what
    // "concatenate" means at the negotiation level.
    let mut ties = Vec::new();
    for i in 0..streams {
        let mut pad_list: Vec<(vaco_filter_core::link::Direction, u32)> = (0..segments)
            .map(|seg| {
                (
                    vaco_filter_core::link::Direction::Input,
                    (seg * streams + i) as u32,
                )
            })
            .collect();
        pad_list.push((vaco_filter_core::link::Direction::Output, i as u32));
        let media = if i < v {
            MediaType::Video
        } else {
            MediaType::Audio
        };
        for &property in vaco_filter_core::negotiate::Property::for_media(media) {
            ties.push(Tie {
                property,
                pads: pad_list.clone(),
            });
        }
    }

    Ok(Instance {
        desc: FilterDesc {
            inputs: Box::leak(input_pads.into_boxed_slice()),
            outputs: Box::leak(output_pads.into_boxed_slice()),
            ..DESC
        },
        formats: NodeFormats {
            inputs: vec![FormatSet::default(); total_inputs],
            outputs: vec![FormatSet::default(); streams],
            ties,
            label: req.instance.to_owned(),
        },
        filter: Box::new(Concat {
            segments,
            streams,
            state,
        }),
    })
}

const fn pads_limit() -> usize {
    pads::MAX
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
    use vaco_filter_core::mock::{gray_frame, gray_link, video_source_formats};
    use vaco_filter_core::{Graph, GraphStatus};

    /// Two one-second, 5-frame-per-second segments (`n=2:v=1:a=0`):
    /// segment 0's frames carry pts `0..5` and segment 1's *also* carry pts
    /// `0..5` (as they would fresh out of a second demuxed file). Concat
    /// must rebase segment 1 so its frames land at pts `5..10` — the
    /// plumbing risk this filter exists to get right.
    #[test]
    fn rebases_the_second_segment_after_the_first() {
        let req = Instantiate {
            name: "concat",
            instance: "concat",
            args: Some("n=2:v=1:a=0"),
            arguments: &[],
        };
        let instance = create(&req).unwrap();

        let mut graph = Graph::new();
        let src_a = graph.add_source(
            "seg0",
            MediaType::Video,
            video_source_formats("seg0", vaco_pixfmt::PixFmt::Gray8),
        );
        let src_b = graph.add_source(
            "seg1",
            MediaType::Video,
            video_source_formats("seg1", vaco_pixfmt::PixFmt::Gray8),
        );
        let node = graph.add(instance.desc, instance.formats, instance.filter);
        let sink = graph.add_sink(
            "out",
            MediaType::Video,
            vaco_filter_core::mock::any_video_sink("out"),
        );

        graph.connect(src_a, 0, node, 0).unwrap();
        graph.connect(src_b, 0, node, 1).unwrap();
        graph.connect(node, 0, sink, 0).unwrap();
        let tb = vaco_core::Rational::new(1, 25);
        graph.set_source_format(src_a, gray_link(4, 4, tb)).unwrap();
        graph.set_source_format(src_b, gray_link(4, 4, tb)).unwrap();
        graph.configure().unwrap();

        for i in 0..5i64 {
            graph.send(src_a, gray_frame(4, 4, i, 0)).unwrap();
        }
        graph
            .close_source(src_a, vaco_core::Timestamp::new(5))
            .unwrap();
        for i in 0..5i64 {
            graph.send(src_b, gray_frame(4, 4, i, 0)).unwrap();
        }
        graph
            .close_source(src_b, vaco_core::Timestamp::new(5))
            .unwrap();

        let mut pts = Vec::new();
        loop {
            match graph.run().unwrap() {
                GraphStatus::Eof => break,
                GraphStatus::HasOutput(_) | GraphStatus::NeedInput(_) => {}
                other => panic!("unexpected graph status: {other:?}"),
            }
            loop {
                match graph.recv(sink) {
                    Ok(frame) => pts.push(frame.pts.ticks().unwrap_or(-1)),
                    Err(vaco_core::Error::Eof | vaco_core::Error::NeedMoreInput) => break,
                    Err(e) => panic!("unexpected recv error: {e}"),
                }
            }
        }
        assert_eq!(pts, vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }
}
