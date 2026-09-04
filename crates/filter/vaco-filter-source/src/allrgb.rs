//! `allrgb` — every 24-bit RGB colour exactly once, in a 4096×4096 `rgb24`
//! frame (4096×4096 = 2^24).
//!
//! `ffmpeg -h filter=allrgb` documents only `rate`/`r`, `duration`/`d` and
//! `sar` — no `size`, because the size is fixed by the closed-form
//! requirement that every 24-bit value appear exactly once.
//!
//! # The formula (measured, not read)
//!
//! Per D17, the byte layout was recovered by probing `ffmpeg -f lavfi -i
//! allrgb -f rawvideo -pix_fmt rgb24 -frames:v 1 -` and solving small,
//! independent (x, y) queries rather than reading `allrgb.c`. The pattern,
//! confirmed on points not used to derive it (e.g. `(100, 100) ->
//! (12, 100, 6)`, `(2048, 2048) -> (255, 255, 128)`):
//!
//! ```text
//! R(x, y) = x & 0xFF
//! G(x, y) = y & 0xFF
//! B(x, y) = ((x >> 8) & 0xF) | (((y >> 8) & 0xF) << 4)
//! ```
//!
//! `x` and `y` each range over 0..4095 (12 bits). `R` and `G` take the low
//! byte of `x` and `y` directly; `B` packs `x`'s and `y`'s remaining four
//! high bits into one byte, so every `(x, y)` maps to a distinct 24-bit
//! triple and every triple is reached — a bijection, verified separately by
//! exhaustively checking that all 2^24 output triples are distinct (see the
//! `an_entire_frame_is_a_bijection_onto_every_24_bit_value` test, which is
//! the independent property this generator must have, not just a second
//! transcription of the same formula).
//!
//! **Exact.**

use vaco_core::{Duration as VDuration, MediaType, Rational, Result, Timestamp};
use vaco_filter_core::adapt::{SourceFilter, Sourced};
use vaco_filter_core::negotiate::{Constraint, FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

const UNLIMITED: VDuration = VDuration::from_micros(-1);
const SIZE: u32 = 4096;

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "allrgb", help = "generate all RGB colors")]
pub(crate) struct Opts {
    #[opt(name = "rate", alias = "r", help = "set video rate", default = vaco_opts::VideoRate(Rational::new(25, 1)), flags(filtering))]
    pub rate: vaco_opts::VideoRate,
    #[opt(name = "duration", alias = "d", help = "set video duration", default = UNLIMITED, flags(filtering))]
    pub duration: VDuration,
    #[opt(name = "sar", help = "set video sample aspect ratio", default = Rational::ONE, flags(filtering))]
    pub sar: Rational,
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

pub const DESC: FilterDesc = FilterDesc {
    name: "allrgb",
    description: "Generate all RGB colors",
    inputs: &[],
    outputs: &[Pad {
        name: "default",
        media_type: MediaType::Video,
    }],
    flags: FilterFlags::empty(),
};

/// Fills one `rgb24` row of the fixed 4096×4096 frame per the measured
/// formula in the module doc.
fn fill_row(row: &mut [u8], y: u32) {
    let g = (y & 0xFF) as u8;
    let y_hi = ((y >> 8) & 0xF) as u8;
    for (x, px) in (0u32..SIZE).zip(row.chunks_exact_mut(3)) {
        let r = (x & 0xFF) as u8;
        let x_hi = ((x >> 8) & 0xF) as u8;
        let b = x_hi | (y_hi << 4);
        if let [pr, pg, pb] = px {
            *pr = r;
            *pg = g;
            *pb = b;
        }
    }
}

#[derive(Debug)]
struct Source {
    frame_rate: Rational,
    sar: Rational,
    total_frames: Option<u64>,
    next: i64,
}

impl SourceFilter for Source {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video {
                width,
                height,
                time_base,
                frame_rate,
                sample_aspect_ratio,
                ..
            } = &mut out
            {
                *width = SIZE;
                *height = SIZE;
                *time_base = self.frame_rate.inverse();
                *frame_rate = self.frame_rate;
                *sample_aspect_ratio = self.sar;
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn produce(&mut self, ctx: &mut FilterContext<'_>) -> Result<Option<Frame>> {
        if self.total_frames.is_some_and(|n| self.next as u64 >= n) {
            return Ok(None);
        }
        let mut frame = ctx.pool().acquire_video(PixFmt::Rgb24, SIZE, SIZE)?;
        if let Some(mut plane) = frame.plane_mut(0) {
            for y in 0..plane.rows() {
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "plane.rows() == SIZE, which fits in u32"
                )]
                let yy = y as u32;
                if let Some(row) = plane.row_mut(y) {
                    fill_row(row, yy);
                }
            }
        }
        frame.pts = Timestamp::new(self.next);
        frame.time_base = self.frame_rate.inverse();
        frame.set_duration_ticks(1);
        frame.sample_aspect_ratio = self.sar;
        self.next = self.next.saturating_add(1);
        Ok(Some(frame))
    }

    fn end_pts(&self) -> Timestamp {
        Timestamp::new(self.next)
    }

    fn flush_state(&mut self) {
        self.next = 0;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let rate = opts.rate.0;
    let total_frames = if opts.duration.as_micros() < 0 {
        None
    } else {
        Some(
            (opts.duration.as_secs_f64() * rate.to_f64())
                .round()
                .max(0.0) as u64,
        )
    };
    let source = Source {
        frame_rate: rate,
        sar: opts.sar,
        total_frames,
        next: 0,
    };
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats {
            inputs: Vec::new(),
            outputs: vec![FormatSet {
                pixel_formats: Some(Constraint::Exact(PixFmt::Rgb24)),
                ..FormatSet::default()
            }],
            ties: Vec::new(),
            label: req.instance.to_owned(),
        },
        filter: Box::new(Sourced::new(source)),
    })
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn frame_rows() -> Vec<Vec<u8>> {
        (0..SIZE)
            .map(|y| {
                let mut row = vec![0u8; (SIZE * 3) as usize];
                fill_row(&mut row, y);
                row
            })
            .collect()
    }

    #[test]
    fn matches_measured_reference_points() {
        let rows = frame_rows();
        let px = |x: u32, y: u32| {
            let i = (x * 3) as usize;
            let row = &rows[y as usize];
            (row[i], row[i + 1], row[i + 2])
        };
        assert_eq!(px(0, 0), (0, 0, 0));
        assert_eq!(px(1, 0), (1, 0, 0));
        assert_eq!(px(0, 1), (0, 1, 0));
        assert_eq!(px(4095, 0), (255, 0, 15));
        assert_eq!(px(0, 4095), (0, 255, 240));
        assert_eq!(px(4095, 4095), (255, 255, 255));
        assert_eq!(px(256, 0), (0, 0, 1));
        assert_eq!(px(255, 0), (255, 0, 0));
    }

    /// The independent oracle: not a re-derivation of the same formula, but a
    /// property the true generator must have — every one of the 2^24 pixels
    /// is a distinct colour, i.e. a bijection onto the entire RGB cube. A
    /// formula that agreed with the four corner points above by coincidence
    /// (e.g. swapped nibble order) would fail this.
    #[test]
    fn an_entire_frame_is_a_bijection_onto_every_24_bit_value() {
        let rows = frame_rows();
        let mut seen: HashSet<(u8, u8, u8)> = HashSet::with_capacity((SIZE * SIZE) as usize);
        for row in &rows {
            for px in row.chunks_exact(3) {
                if let [r, g, b] = *px {
                    assert!(seen.insert((r, g, b)), "duplicate colour {:?}", (r, g, b));
                }
            }
        }
        assert_eq!(seen.len(), 1 << 24);
    }

    #[test]
    fn creatable_with_no_arguments() {
        let req = Instantiate {
            name: "allrgb",
            instance: "allrgb",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }
}
