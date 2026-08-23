//! `maskedclamp` — clamp `base` between `min(dark,bright)-undershoot` and
//! `max(dark,bright)+overshoot`, per pixel.
//!
//! `ffmpeg -h filter=maskedclamp` documents three inputs (`base`, `dark`,
//! `bright`) and three options: `undershoot`/`overshoot` (`0..=65535`,
//! default `0`), `planes` (default `15`). No neighbourhood, so none of the
//! border questions the rest of this crate spent time on apply here —
//! every output sample is a pure function of the three co-located input
//! samples.
//!
//! Three equal-weight inputs, all of which must have started before a
//! pixel can be computed at all: this is
//! [`vaco_filter_framesync::FsInput::uniform`], the same role
//! `vaco-filter-framesync`'s own docs name `maskedmerge` as using.
//!
//! Not measured directly against the reference (this crate's probing time
//! went to the neighbourhood filters, where the border rules are the part
//! that cannot be derived from the option table); the clamp formula itself
//! is exactly what the option help states, with no free parameter to get
//! wrong the way a border convention can be.

use vaco_core::{MediaType, Result};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::FrameData;
use vaco_opts::OptionsExt as _;

use vaco_filter_framesync::{FrameSyncEvent, FrameSyncFilter, FrameSyncOpts, FsInput, Synced};
use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

const THREE_VIDEO_PADS: &[Pad] = &[
    Pad {
        name: "base",
        media_type: MediaType::Video,
    },
    Pad {
        name: "dark",
        media_type: MediaType::Video,
    },
    Pad {
        name: "bright",
        media_type: MediaType::Video,
    },
];

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "maskedclamp",
    description: "Clamp first stream with second stream and third stream",
    inputs: THREE_VIDEO_PADS,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "maskedclamp", help = "Clamp first stream with second stream and third stream")]
pub(crate) struct Opts {
    #[opt(name = "undershoot", help = "set undershoot", default = 0, range = 0..=65535, flags(video, filtering))]
    pub undershoot: i32,
    #[opt(name = "overshoot", help = "set overshoot", default = 0, range = 0..=65535, flags(video, filtering))]
    pub overshoot: i32,
    #[opt(name = "planes", help = "set planes", default = 15, range = 0..=15, flags(video, filtering))]
    pub planes: i64,
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

#[derive(Debug)]
pub(crate) struct Filter {
    opts: Opts,
}

impl FrameSyncFilter for Filter {
    fn inputs(&self, n: usize) -> Vec<FsInput> {
        FsInput::uniform(n)
    }

    fn opts(&self) -> FrameSyncOpts {
        FrameSyncOpts::default()
    }

    fn on_event(
        &mut self,
        _ctx: &mut FilterContext<'_>,
        event: &mut FrameSyncEvent<'_>,
    ) -> Result<vaco_filter_core::adapt::FrameOut> {
        use vaco_filter_core::adapt::FrameOut;

        let (Some(dark), Some(bright)) = (event.get(1).cloned(), event.get(2).cloned()) else {
            return match event.take(0) {
                Some(base) => Ok(FrameOut::One(base)),
                None => Ok(FrameOut::None),
            };
        };
        let Some(mut base) = event.take(0) else {
            return Ok(FrameOut::None);
        };
        let FrameData::Video { format, .. } = base.data else {
            return Ok(FrameOut::One(base));
        };
        common::ensure_8bit_addressable(format)?;

        let plane_count = format.plane_count();
        for p in 0..plane_count {
            let p8 = p as u8;
            if !common::plane_selected(self.opts.planes, p8) {
                continue;
            }
            let (Some(dark_plane), Some(bright_plane)) = (dark.plane(p), bright.plane(p)) else {
                continue;
            };
            let height = base.plane(p).map_or(0, |pl| pl.rows());
            let Some(mut base_plane) = base.plane_mut(p) else {
                continue;
            };
            for y in 0..height {
                let Some(dark_row) = dark_plane.row(y) else {
                    continue;
                };
                let Some(bright_row) = bright_plane.row(y) else {
                    continue;
                };
                let Some(base_row) = base_plane.row_mut(y) else {
                    continue;
                };
                let n = base_row.len().min(dark_row.len()).min(bright_row.len());
                for x in 0..n {
                    let (Some(b), Some(d), Some(br)) =
                        (base_row.get_mut(x), dark_row.get(x), bright_row.get(x))
                    else {
                        continue;
                    };
                    let d = i32::from(*d);
                    let br = i32::from(*br);
                    let lo = d.min(br) - self.opts.undershoot;
                    let hi = d.max(br) + self.opts.overshoot;
                    let clamped = i32::from(*b).clamp(lo, hi).clamp(0, 255);
                    *b = u8::try_from(clamped).unwrap_or(*b);
                }
            }
        }
        base.pts = event.timestamp();
        base.time_base = event.time_base();
        Ok(FrameOut::One(base))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(3, 1, MediaType::Video, req.instance),
        filter: Box::new(Synced::new(Filter { opts })),
    })
}
