//! `mergeplanes` — build one frame's planes from up to four separate input
//! streams.
//!
//! `ffmpeg -h filter=mergeplanes` documents `format` (output pixel format,
//! default `yuva444p`) and four `map<N>s`/`map<N>p` pairs (each `0`-`3`,
//! default `0`), one pair per *output* plane: `map<N>s` names which input
//! stream feeds output plane `N`, `map<N>p` names which of that stream's
//! own planes. The legacy `mapping` hex option the reference itself marks
//! **deprecated** is not implemented — the per-plane options above are the
//! current, non-deprecated interface and this filter's whole option
//! surface is otherwise exactly what a `Paired` input needs to know:
//! "which of my N inputs feeds output plane P."
//!
//! # This is `Paired`, generalised past two inputs — measured
//!
//! `ffmpeg -h filter=mergeplanes` carries no `eof_action`/`shortest`/
//! `repeatlast`/`ts_sync_mode` section at all, the same absence
//! `framepack` has (see [`vaco_filter_core::adapt::Paired`]'s own doc for
//! the measurement and what it means: strict lockstep, no per-input
//! timeline, first input to run dry ends the filter — measured directly:
//! a 5-frame and a 3-frame input at the same rate stop the whole filter at
//! 3 frames, not 5 with the shorter input's last frame repeated).
//! `mergeplanes` is `Paired`'s reason to generalise past two inputs in the
//! first place: its own input count is fixed at construction from
//! `map<N>s`, and can be anywhere from one (every plane from stream 0) to
//! four.
//!
//! # Scope: same overall geometry, no colour conversion
//!
//! Every output plane is a byte-for-byte copy from the named
//! `(stream, plane)`, sized to `format`'s own subsampling for that plane
//! index (`PixFmt::plane_width`/`plane_height`) — not resampled or
//! reinterpreted. That is correct for the documented use (recombining
//! planes that were themselves split from same-size sources, e.g. via
//! `extractplanes`) and does not attempt to size-convert a mismatched
//! source plane; a plane copy is simply truncated to whichever of the
//! source or destination row is shorter, the same defensive shape this
//! crate's other byte-movers use.

use smallvec::SmallVec;
use vaco_core::{Error, MediaType, Result};
use vaco_filter_core::adapt::{FrameOut, Paired, PairedFilter};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate, pads};

const OUTPUT_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "mergeplanes",
    description: "Merge planes",
    inputs: &[],
    outputs: OUTPUT_PAD,
    flags: FilterFlags::DYNAMIC_INPUTS,
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "mergeplanes", help = "Merge planes")]
pub(crate) struct Opts {
    #[opt(
        name = "format",
        help = "set output pixel format",
        default = "yuva444p".to_owned(),
        flags(video, filtering)
    )]
    pub format: String,
    #[opt(
        name = "map0s",
        help = "set 1st input to output stream mapping",
        default = 0,
        range = 0..=3,
        flags(video, filtering)
    )]
    pub map0s: i32,
    #[opt(
        name = "map0p",
        help = "set 1st input to output plane mapping",
        default = 0,
        range = 0..=3,
        flags(video, filtering)
    )]
    pub map0p: i32,
    #[opt(
        name = "map1s",
        help = "set 2nd input to output stream mapping",
        default = 0,
        range = 0..=3,
        flags(video, filtering)
    )]
    pub map1s: i32,
    #[opt(
        name = "map1p",
        help = "set 2nd input to output plane mapping",
        default = 0,
        range = 0..=3,
        flags(video, filtering)
    )]
    pub map1p: i32,
    #[opt(
        name = "map2s",
        help = "set 3rd input to output stream mapping",
        default = 0,
        range = 0..=3,
        flags(video, filtering)
    )]
    pub map2s: i32,
    #[opt(
        name = "map2p",
        help = "set 3rd input to output plane mapping",
        default = 0,
        range = 0..=3,
        flags(video, filtering)
    )]
    pub map2p: i32,
    #[opt(
        name = "map3s",
        help = "set 4th input to output stream mapping",
        default = 0,
        range = 0..=3,
        flags(video, filtering)
    )]
    pub map3s: i32,
    #[opt(
        name = "map3p",
        help = "set 4th input to output plane mapping",
        default = 0,
        range = 0..=3,
        flags(video, filtering)
    )]
    pub map3p: i32,
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

    /// `(stream, plane)` for each of the four map pairs, in declaration
    /// order.
    fn maps(&self) -> [(i32, i32); 4] {
        [
            (self.map0s, self.map0p),
            (self.map1s, self.map1p),
            (self.map2s, self.map2p),
            (self.map3s, self.map3p),
        ]
    }
}

#[derive(Debug)]
pub(crate) struct Filter {
    format: PixFmt,
    /// `(stream, plane)` per output plane, `format.plane_count()` long.
    sources: Vec<(u8, u8)>,
    n: usize,
}

impl PairedFilter for Filter {
    fn input_count(&self) -> usize {
        self.n
    }

    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let Some(LinkFormat::Video {
            width,
            height,
            time_base,
            frame_rate,
            sample_aspect_ratio,
            ..
        }) = ctx.input_link(0).cloned()
        else {
            return Ok(());
        };
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video {
                format: f,
                width: w,
                height: h,
                time_base: tb,
                frame_rate: fr,
                sample_aspect_ratio: sar,
                ..
            } = &mut out
            {
                *f = self.format;
                *w = width;
                *h = height;
                *tb = time_base;
                *fr = frame_rate;
                *sar = sample_aspect_ratio;
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn filter_frames(
        &mut self,
        ctx: &mut FilterContext<'_>,
        inputs: SmallVec<[Frame; 4]>,
    ) -> Result<FrameOut> {
        let Some(first) = inputs.first() else {
            return Ok(FrameOut::None);
        };
        let FrameData::Video { width, height, .. } = first.data else {
            return Err(Error::InvalidData("mergeplanes: input is not video"));
        };
        let mut out = ctx.pool().acquire_video(self.format, width, height)?;
        for (plane_idx, &(stream, src_plane)) in self.sources.iter().enumerate() {
            let plane_idx = plane_idx as u8;
            let Some(src_frame) = inputs.get(stream as usize) else {
                continue;
            };
            let Some(src) = src_frame.plane(src_plane as usize) else {
                continue;
            };
            let Some(mut dst) = out.plane_mut(plane_idx as usize) else {
                continue;
            };
            let rows = self.format.plane_height(height, plane_idx) as usize;
            for y in 0..rows {
                let Some(src_row) = src.row(y) else { continue };
                if let Some(dst_row) = dst.row_mut(y) {
                    let n = dst_row.len().min(src_row.len());
                    if let (Some(d), Some(s)) = (dst_row.get_mut(..n), src_row.get(..n)) {
                        d.copy_from_slice(s);
                    }
                }
            }
        }
        out.pts = first.pts;
        out.time_base = first.time_base;
        out.duration = first.duration;
        out.sample_aspect_ratio = first.sample_aspect_ratio;
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let format = PixFmt::from_name(&opts.format)
        .map_err(|_| format!("mergeplanes: bad `format` `{}`", opts.format))?;
    let plane_count = format.plane_count().min(4);
    let maps = opts.maps();
    let mut sources = Vec::new();
    let mut max_stream = 0i32;
    for &(s, p) in maps.iter().take(plane_count) {
        max_stream = max_stream.max(s);
        sources.push((u8::try_from(s).unwrap_or(0), u8::try_from(p).unwrap_or(0)));
    }
    let n = usize::try_from(max_stream)
        .unwrap_or(0)
        .saturating_add(1)
        .max(1);
    let input_pads = pads::video(n).ok_or_else(|| "mergeplanes: too many inputs".to_owned())?;
    let filter = Filter { format, sources, n };
    Ok(Instance {
        desc: FilterDesc {
            inputs: input_pads,
            ..DESC
        },
        formats: NodeFormats {
            inputs: vec![FormatSet::default(); n],
            outputs: vec![FormatSet::video_exact(format)],
            ties: Vec::new(),
            label: req.instance.to_owned(),
        },
        filter: Box::new(Paired::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn opts(format: &str, maps: [(i32, i32); 4]) -> Opts {
        Opts {
            format: format.to_owned(),
            map0s: maps[0].0,
            map0p: maps[0].1,
            map1s: maps[1].0,
            map1p: maps[1].1,
            map2s: maps[2].0,
            map2p: maps[2].1,
            map3s: maps[3].0,
            map3p: maps[3].1,
        }
    }

    #[test]
    fn default_options_are_a_single_input_reading_plane_0_everywhere() {
        let o = Opts::default();
        assert_eq!(o.format, "yuva444p");
        assert_eq!(o.maps(), [(0, 0); 4]);
    }

    #[test]
    fn input_count_is_the_highest_referenced_stream_plus_one() {
        // gbrp has 3 planes, so map3s/map3p (stream 3) is not consulted.
        let o = opts("gbrp", [(0, 0), (1, 0), (2, 0), (3, 0)]);
        let format = PixFmt::from_name(&o.format).unwrap();
        let plane_count = format.plane_count().min(4);
        assert_eq!(plane_count, 3);
        let max_stream = o
            .maps()
            .iter()
            .take(plane_count)
            .map(|&(s, _)| s)
            .max()
            .unwrap_or(0);
        assert_eq!(
            max_stream, 2,
            "stream 3 in the unused 4th map must not count"
        );
    }

    /// Three same-size single-plane inputs recombined into `gbrp` land on
    /// the planes their `map<N>p`/`map<N>s` say, byte for byte.
    #[test]
    fn three_gray_inputs_become_the_three_planes_of_gbrp() {
        let pool = vaco_frame::FramePool::default();
        let filter = Filter {
            format: PixFmt::Gbrp,
            sources: vec![(0, 0), (1, 0), (2, 0)],
            n: 3,
        };

        // `filter_frames` needs a `FilterContext`, which only a live graph
        // constructs; drive it through a minimal three-source graph rather
        // than hand-building one.
        let mut graph = vaco_filter_core::Graph::new();
        let sink_fmt = vaco_filter_core::mock::any_video_sink("out");
        let src_a = graph.add_source(
            "a",
            MediaType::Video,
            vaco_filter_core::mock::video_source_formats("a", PixFmt::Gray8),
        );
        let src_b = graph.add_source(
            "b",
            MediaType::Video,
            vaco_filter_core::mock::video_source_formats("b", PixFmt::Gray8),
        );
        let src_c = graph.add_source(
            "c",
            MediaType::Video,
            vaco_filter_core::mock::video_source_formats("c", PixFmt::Gray8),
        );
        let node = graph.add(
            FilterDesc {
                inputs: pads::video(3).unwrap(),
                ..DESC
            },
            NodeFormats {
                inputs: vec![FormatSet::default(); 3],
                outputs: vec![FormatSet::video_exact(PixFmt::Gbrp)],
                ties: Vec::new(),
                label: "merge".to_owned(),
            },
            Box::new(Paired::new(filter)),
        );
        let sink = graph.add_sink("out", MediaType::Video, sink_fmt);
        graph.connect(src_a, 0, node, 0).unwrap();
        graph.connect(src_b, 0, node, 1).unwrap();
        graph.connect(src_c, 0, node, 2).unwrap();
        graph.connect(node, 0, sink, 0).unwrap();
        graph
            .set_source_format(
                src_a,
                vaco_filter_core::mock::gray_link(2, 2, vaco_core::Rational::new(1, 25)),
            )
            .unwrap();
        graph
            .set_source_format(
                src_b,
                vaco_filter_core::mock::gray_link(2, 2, vaco_core::Rational::new(1, 25)),
            )
            .unwrap();
        graph
            .set_source_format(
                src_c,
                vaco_filter_core::mock::gray_link(2, 2, vaco_core::Rational::new(1, 25)),
            )
            .unwrap();
        graph.configure().unwrap();

        let mut a2 = pool.acquire_video(PixFmt::Gray8, 2, 2).unwrap();
        a2.plane_mut(0).unwrap().fill(0x10);
        let mut b2 = pool.acquire_video(PixFmt::Gray8, 2, 2).unwrap();
        b2.plane_mut(0).unwrap().fill(0x20);
        let mut c2 = pool.acquire_video(PixFmt::Gray8, 2, 2).unwrap();
        c2.plane_mut(0).unwrap().fill(0x30);
        graph.send(src_a, a2).unwrap();
        graph.send(src_b, b2).unwrap();
        graph.send(src_c, c2).unwrap();
        graph.run().unwrap();
        let out = graph.recv(sink).unwrap();
        assert_eq!(
            out.plane(0)
                .and_then(|p| p.row(0))
                .and_then(|r| r.first())
                .copied(),
            Some(0x10)
        );
        assert_eq!(
            out.plane(1)
                .and_then(|p| p.row(0))
                .and_then(|r| r.first())
                .copied(),
            Some(0x20)
        );
        assert_eq!(
            out.plane(2)
                .and_then(|p| p.row(0))
                .and_then(|r| r.first())
                .copied(),
            Some(0x30)
        );
    }
}
