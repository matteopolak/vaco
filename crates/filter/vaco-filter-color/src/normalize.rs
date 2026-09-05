//! `normalize` — remap an RGB frame's observed range to chosen endpoints.
//!
//! `ffmpeg -h filter=normalize` documents `blackpt`, `whitept`,
//! `smoothing`, `independence`, and `strength`.  The first, third, and
//! fourth are not interchangeable: each component first gets its own
//! observed minimum and maximum, then `independence` blends those bounds with
//! the whole-frame RGB bounds.  `strength` blends the resulting pixel with
//! the original pixel.  This is deliberately a frame-global filter, rather
//! than three accidental per-row stretches.
//!
//! ## Measured arithmetic
//!
//! A two-by-two RGB fixture with component ranges R=32..128, G=64..144 and
//! B=96..160 produced the following `ffmpeg 9.0.1` raw `rgb24` output:
//!
//! ```text
//! normalize=independence=0.5:
//!   00 27 55  db ec ff  25 4f 80  49 76 ab
//! normalize=strength=0.5:
//!   10 20 30  c0 c8 d0  2d 42 58  4b 63 80
//! ```
//!
//! Those distinguish interpolation of the *input bounds* from blending
//! already-normalized component values, and distinguish rounded output from
//! truncation.  `blackpt=0x102030:whitept=0xa0b0c0` on the same fixture
//! similarly produced `10 20 30 a0 b0 c0 28 3d 54 40 5a 78`.
//!
//! `smoothing` retains a history of prior frames.  It is refused when
//! non-zero until that stateful behavior is measured; accepting it and
//! treating it as zero would make a registered filter silently wrong.

use vaco_core::{MediaType, Result, Rgba};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::sample;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "normalize",
    description: "Normalize RGB video",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "normalize", help = "Normalize RGB video")]
pub(crate) struct Opts {
    #[opt(name = "blackpt", help = "set the output color for the darkest input color", default = "black".to_owned(), flags(video, filtering))]
    pub blackpt: String,
    #[opt(name = "whitept", help = "set the output color for the brightest input color", default = "white".to_owned(), flags(video, filtering))]
    pub whitept: String,
    #[opt(name = "smoothing", help = "set temporal input-range smoothing", default = 0, range = 0..=268_435_455, flags(video, filtering))]
    pub smoothing: i32,
    #[opt(name = "independence", help = "set independent-channel normalization proportion", default = 1.0, range = 0.0..=1.0, flags(video, filtering))]
    pub independence: f64,
    #[opt(name = "strength", help = "set normalization strength", default = 1.0, range = 0.0..=1.0, flags(video, filtering))]
    pub strength: f64,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let o: Self = common::parse(args)?;
        if o.smoothing != 0 {
            return Err(
                "normalize: `smoothing` is stateful and not yet measured; refusing it rather than treating it as zero".to_owned(),
            );
        }
        Ok(o)
    }
}

#[derive(Debug)]
pub(crate) struct Filter {
    black: [f64; 3],
    white: [f64; 3],
    independence: f64,
    strength: f64,
}

impl Filter {
    fn new(o: &Opts) -> std::result::Result<Self, String> {
        let parse = |name: &str, text: &str| {
            vaco_core::parse::color(text)
                .ok_or_else(|| format!("normalize: invalid {name} color `{text}`"))
        };
        let black = parse("blackpt", &o.blackpt)?;
        let white = parse("whitept", &o.whitept)?;
        Ok(Self {
            black: normalized(black),
            white: normalized(white),
            independence: o.independence,
            strength: o.strength,
        })
    }

    fn apply_frame(&self, input: &mut Frame) {
        let FrameData::Video { format, .. } = input.data else {
            return;
        };
        if !format.is_rgb() || !sample::is_addressable(format) {
            return;
        }
        let big_endian = format.is_big_endian();
        let Some(stats) = Stats::for_frame(input, format, big_endian) else {
            return;
        };
        let all_min = stats.min.into_iter().fold(f64::INFINITY, f64::min);
        let all_max = stats.max.into_iter().fold(f64::NEG_INFINITY, f64::max);

        for (ch, (((min, max), black), white)) in stats
            .min
            .into_iter()
            .zip(stats.max)
            .zip(self.black)
            .zip(self.white)
            .enumerate()
        {
            let Some(comp) = sample::component(format, ch) else {
                continue;
            };
            let max_value = f64::from(sample::max_value(comp));
            let lower = self
                .independence
                .mul_add(min, (1.0 - self.independence) * all_min);
            let upper = self
                .independence
                .mul_add(max, (1.0 - self.independence) * all_max);
            let span = upper - lower;
            let Some(mut plane) = input.plane_mut(comp.plane as usize) else {
                continue;
            };
            let width = plane
                .row_bytes()
                .checked_div(usize::from(comp.step.max(1)))
                .unwrap_or(0);
            for y in 0..plane.rows() {
                let Some(row) = plane.row_mut(y) else {
                    continue;
                };
                for x in 0..width {
                    let raw = f64::from(sample::read(row, x, comp, big_endian));
                    let source = raw / max_value;
                    let normalized = if span > 0.0 {
                        // The reference scales an in-range fraction by the
                        // number of representable samples, then saturates to
                        // the greatest sample.  `max` instead of `max + 1`
                        // misses several interior values in the measured
                        // half-independence fixture.
                        ((raw - lower) * (max_value + 1.0) / span).clamp(0.0, max_value)
                    } else {
                        0.0
                    };
                    let target = black + (normalized / max_value) * (white - black);
                    let out = source.mul_add(1.0 - self.strength, target * self.strength);
                    sample::write(row, x, comp, big_endian, to_sample(out, max_value));
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Stats {
    min: [f64; 3],
    max: [f64; 3],
}

impl Stats {
    fn for_frame(input: &Frame, format: vaco_pixfmt::PixFmt, big_endian: bool) -> Option<Self> {
        let mut stats = Self {
            min: [f64::INFINITY; 3],
            max: [f64::NEG_INFINITY; 3],
        };
        for (ch, (min, max)) in stats.min.iter_mut().zip(stats.max.iter_mut()).enumerate() {
            let comp = sample::component(format, ch)?;
            let plane = input.plane(comp.plane as usize)?;
            let width = plane
                .row_bytes()
                .checked_div(usize::from(comp.step.max(1)))
                .unwrap_or(0);
            for y in 0..plane.rows() {
                let Some(row) = plane.row(y) else {
                    continue;
                };
                for x in 0..width {
                    let value = f64::from(sample::read(row, x, comp, big_endian));
                    *min = min.min(value);
                    *max = max.max(value);
                }
            }
        }
        stats.min.iter().all(|v| v.is_finite()).then_some(stats)
    }
}

fn normalized(c: Rgba) -> [f64; 3] {
    [
        f64::from(c.r) / 255.0,
        f64::from(c.g) / 255.0,
        f64::from(c.b) / 255.0,
    ]
}

fn to_sample(value: f64, max: f64) -> u16 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "value is clamped to a u16 component range before rounding"
    )]
    {
        (value.clamp(0.0, 1.0) * max).round() as u16
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, mut input: Frame) -> Result<FrameOut> {
        input.make_writable();
        self.apply_frame(&mut input);
        Ok(FrameOut::One(input))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter::new(&opts)?;
    let set = FormatSet::video_list(common::formats_where(|f| {
        f.is_rgb() && sample::is_addressable(f)
    }));
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::uniform(1, 1, MediaType::Video, &set, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};
    use vaco_pixfmt::PixFmt;

    fn frame(values: [[u8; 3]; 4]) -> Frame {
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgb24, 2, 2).unwrap();
        for (row, pair) in values.chunks_exact(2).enumerate() {
            let mut plane = frame.plane_mut(0).unwrap();
            let line = plane.row_mut(row).unwrap();
            for (col, pixel) in pair.iter().enumerate() {
                let dst = &mut line[col * 3..col * 3 + 3];
                dst.copy_from_slice(pixel);
            }
        }
        frame
    }

    fn pixels(frame: &Frame) -> Vec<u8> {
        (0..2)
            .flat_map(|y| frame.plane(0).unwrap().row(y).unwrap()[..6].iter().copied())
            .collect()
    }

    fn measured_fixture() -> Frame {
        frame([
            [0x20, 0x40, 0x60],
            [0x80, 0x90, 0xa0],
            [0x30, 0x50, 0x70],
            [0x40, 0x60, 0x80],
        ])
    }

    #[test]
    fn independent_normalization_matches_the_reference_fixture() {
        let mut frame = measured_fixture();
        Filter::new(&Opts::default())
            .unwrap()
            .apply_frame(&mut frame);
        assert_eq!(
            pixels(&frame),
            [0, 0, 0, 255, 255, 255, 43, 51, 64, 85, 102, 128]
        );
    }

    #[test]
    fn linked_channel_half_independence_matches_the_reference_fixture() {
        let mut frame = measured_fixture();
        let o = Opts {
            independence: 0.5,
            ..Opts::default()
        };
        Filter::new(&o).unwrap().apply_frame(&mut frame);
        assert_eq!(
            pixels(&frame),
            [0, 39, 85, 219, 236, 255, 37, 79, 128, 73, 118, 171]
        );
    }

    #[test]
    fn strength_blends_the_normalized_pixel_with_the_original() {
        let mut frame = measured_fixture();
        let o = Opts {
            strength: 0.5,
            ..Opts::default()
        };
        Filter::new(&o).unwrap().apply_frame(&mut frame);
        assert_eq!(
            pixels(&frame),
            [16, 32, 48, 192, 200, 208, 45, 66, 88, 75, 99, 128]
        );
    }

    #[test]
    fn endpoint_colors_are_applied_per_rgb_component() {
        let mut frame = measured_fixture();
        let o = Opts {
            blackpt: "0x102030".to_owned(),
            whitept: "0xa0b0c0".to_owned(),
            ..Opts::default()
        };
        Filter::new(&o).unwrap().apply_frame(&mut frame);
        assert_eq!(
            pixels(&frame),
            [16, 32, 48, 160, 176, 192, 40, 61, 84, 64, 90, 120]
        );
    }

    #[test]
    fn stateful_smoothing_is_refused_instead_of_ignored() {
        assert!(Opts::parse(Some("smoothing=1")).is_err());
    }
}
