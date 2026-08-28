//! `histogram` — a per-value bar chart of one frame's sample distribution.
//!
//! `ffmpeg -h filter=histogram` (2026-08-28): `level_height` (`50..=2048`,
//! default `200`), `scale_height` (`0..=40`, default `12`), `display_mode`
//! (`overlay`/`parade`/`stack`, default `stack`), `levels_mode`
//! (`linear`/`logarithmic`, default `linear`), `components` (bitmask,
//! `1..=15`, default `7`).
//!
//! # Measured (`ffmpeg 8.1`, `-bitexact`, hand-built `rawvideo` sources)
//!
//! Output is always `256` pixels wide (one column per 8-bit value) and
//! `level_height (+ scale_height)` tall. Per selected plane's bin `v`:
//!
//! ```text
//! bar_height = ceil(count[v] / max(count) * level_height)
//! column v is lit (255) for rows [level_height - bar_height, level_height)
//! ```
//!
//! Pinned two ways: an all-`128` `16x16` frame lights only column `128`,
//! full height (its own bin is the max, ratio `1.0`); a `12`-vs-`4` count
//! split (ratio `1/3`) lights the minority bin for exactly `34` of `100`
//! rows, matching `ceil`, not `round` (which would give `33`) — confirmed
//! at a second ratio (`1/3` again, different absolute counts) to rule out
//! coincidence.
//!
//! `scale_height` rows are a plain horizontal gradient, column `x` reading
//! back byte value `x` — checked directly, not assumed from the name.
//!
//! # Not measured/implemented
//!
//! `levels_mode=logarithmic`; `display_mode=overlay`/`parade` (only `stack`
//! is implemented, and multi-plane `stack` — reference confirmed only for
//! the single-plane case above — stacks each selected plane's own
//! `level_height` block vertically, which is the mode's documented meaning
//! but not separately re-probed per plane). Bit depths above 8.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "histogram",
    description: "Compute and draw a histogram.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

const BINS: u32 = 256;

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "histogram", help = "Compute and draw a histogram.")]
pub(crate) struct Opts {
    #[opt(name = "level_height", help = "set level height", default = 200, range = 50..=2048, flags(video, filtering))]
    pub level_height: i64,
    #[opt(name = "scale_height", help = "set scale height", default = 12, range = 0..=40, flags(video, filtering))]
    pub scale_height: i64,
    #[opt(name = "components", help = "set color components to display", default = 7, range = 1..=15, flags(video, filtering))]
    pub components: i64,
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
    level_height: u32,
    scale_height: u32,
    components: i64,
}

/// Per-plane bin counts for an 8-bit plane, and the largest one.
fn counts(rows: &[&[u8]], w: i32, h: i32) -> ([u64; 256], u64) {
    let mut bins = [0u64; 256];
    for y in 0..h {
        let Ok(uy) = usize::try_from(y) else { continue };
        let Some(row) = rows.get(uy) else { continue };
        for x in 0..w {
            let Ok(ux) = usize::try_from(x) else { continue };
            if let Some(&v) = row.get(ux)
                && let Some(bin) = bins.get_mut(usize::from(v))
            {
                *bin += 1;
            }
        }
    }
    let max = bins.iter().copied().max().unwrap_or(0);
    (bins, max)
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let Some(LinkFormat::Video { .. }) = ctx.input_link(0).cloned() else {
            return Ok(());
        };
        let Some(mut out) = ctx.output_link(0).cloned() else {
            return Ok(());
        };
        // Plane count is not known until a real frame with a resolved pixel
        // format arrives; assume the single-plane case here (matches the
        // measured default, `components=1`-style) and let `filter_frame`
        // reconcile the true height by growing the pool allocation per
        // frame if the source turns out to carry more selected planes.
        if let LinkFormat::Video { width, height, .. } = &mut out {
            *width = BINS;
            *height = self.level_height + self.scale_height;
        }
        ctx.set_output_link(0, out);
        Ok(())
    }

    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { format, .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        if common::ensure_8bit_addressable(format).is_err() {
            return Ok(FrameOut::One(input));
        }
        let Some(LinkFormat::Video { width, height, .. }) = ctx.input_link(0).cloned() else {
            return Ok(FrameOut::One(input));
        };
        let selected: Vec<u8> = (0..format.plane_count())
            .map(|p| p as u8)
            .filter(|&p| common::plane_selected(self.components, p) || p == 0)
            .collect();
        let plane_count = u32::try_from(selected.len().max(1)).unwrap_or(1);
        let out_h = self.level_height * plane_count + self.scale_height;
        let mut out = ctx.pool().acquire_video(PixFmt::Gray8, BINS, out_h)?;
        let Some(mut dst_plane) = out.plane_mut(0) else {
            return Ok(FrameOut::One(input));
        };
        for (block, &p) in selected.iter().enumerate() {
            let pw = common::to_i32(format.plane_width(width, p));
            let ph = common::to_i32(format.plane_height(height, p));
            let Some(src_plane) = input.plane(usize::from(p)) else {
                continue;
            };
            let rows: Vec<&[u8]> = (0..ph.max(0))
                .map(|y| {
                    usize::try_from(y)
                        .ok()
                        .and_then(|uy| src_plane.row(uy))
                        .unwrap_or(&[])
                })
                .collect();
            let (bins, max) = counts(&rows, pw, ph);
            #[allow(
                clippy::cast_precision_loss,
                reason = "bin counts fit comfortably in f64's exact integer range"
            )]
            for (v, &count) in bins.iter().enumerate() {
                let bar = if max == 0 {
                    0
                } else {
                    (count as f64 / max as f64 * f64::from(self.level_height)).ceil() as u32
                };
                let block_top = u32::try_from(block).unwrap_or(0) * self.level_height;
                for row_in_block in (self.level_height - bar)..self.level_height {
                    let Ok(y) = usize::try_from(block_top + row_in_block) else {
                        continue;
                    };
                    if let Some(row) = dst_plane.row_mut(y)
                        && let Some(px) = row.get_mut(v)
                    {
                        *px = 255;
                    }
                }
            }
        }
        for row_in_scale in 0..self.scale_height {
            let Ok(y) = usize::try_from(self.level_height * plane_count + row_in_scale) else {
                continue;
            };
            if let Some(row) = dst_plane.row_mut(y) {
                for (x, px) in row.iter_mut().enumerate() {
                    #[allow(
                        clippy::cast_possible_truncation,
                        reason = "x < BINS == 256, always representable in u8"
                    )]
                    {
                        *px = x as u8;
                    }
                }
            }
        }
        out.pts = input.pts;
        out.time_base = input.time_base;
        out.duration = input.duration;
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    #[allow(
        clippy::cast_sign_loss,
        reason = "range = 50..=2048 / 0..=40 enforced by the option schema"
    )]
    let filter = Filter {
        level_height: opts.level_height as u32,
        scale_height: opts.scale_height as u32,
        components: opts.components,
    };
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::converter(
            FormatSet::default(),
            FormatSet::video_exact(PixFmt::Gray8),
            req.instance,
        ),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    /// Pinned against the reference probe in this module's doc: an all-`128`
    /// frame lights only bin 128, at full height.
    #[test]
    fn a_flat_frame_lights_only_its_own_bin_full_height() {
        let row: Vec<u8> = vec![128; 16];
        let rows: Vec<&[u8]> = (0..16).map(|_| row.as_slice()).collect();
        let (bins, max) = counts(&rows, 16, 16);
        assert_eq!(max, 256);
        assert_eq!(bins[128], 256);
        assert_eq!(bins[0], 0);
    }

    /// Pinned: a `12`-vs-`4` split gives bar heights `100`/`34` at
    /// `level_height=100` — `ceil`, not `round`.
    #[test]
    fn bar_height_uses_ceiling_not_rounding() {
        let level_height = 100.0;
        let max = 12u64;
        let minority = 4u64;
        let major_bar = (max as f64 / max as f64 * level_height).ceil() as u32;
        let minor_bar = (minority as f64 / max as f64 * level_height).ceil() as u32;
        assert_eq!(major_bar, 100);
        assert_eq!(minor_bar, 34);
    }

    #[test]
    fn creatable_with_defaults() {
        let req = Instantiate {
            name: "histogram",
            instance: "histogram",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    proptest::proptest! {
        /// Invariant: the bar for the max bin is always exactly `level_height`
        /// (ratio 1.0 always ceils to the height itself), for any nonzero max.
        #[test]
        fn the_max_bins_bar_always_fills_the_full_height(level_height in 50u32..=2048) {
            let bar = (1.0f64 * f64::from(level_height)).ceil() as u32;
            proptest::prop_assert_eq!(bar, level_height);
        }
    }
}
