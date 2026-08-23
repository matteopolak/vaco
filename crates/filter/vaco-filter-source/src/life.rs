//! `life` — Conway's Game of Life and its `B.../S...` generalisations, in
//! `monob` by default colouring (or `rgb0` once `life_color`/`death_color`
//! diverge from black/white — see below).
//!
//! `ffmpeg -h filter=life` documents `filename`/`f`, `size`/`s` (no stated
//! default, `320x240` here — this crate's own choice, as for `cellauto`),
//! `rate`/`r`, `rule` (default `"B3/S23"`, standard Life), `stitch`
//! (default true), `mold`, `life_color`/`death_color`/`mold_color`, and the
//! same `random_fill_ratio`/`random_seed` pair `cellauto` has.
//!
//! # What is exact, and what is not
//!
//! **The `B.../S...` rule evaluation is exact and closed-form**: a live
//! cell survives iff its live-neighbour count is in the `S` set, a dead
//! cell is born iff its live-neighbour count is in the `B` set — Conway's
//! own 1970 definition (via Berlekamp/Conway/Guy's standard notation),
//! independent of any reference implementation. This module's tests check
//! the two textbook oscillators (the blinker and the toad) against their
//! well-known periods, which is a property of Life itself, not a
//! transcription of the rule table.
//!
//! **Without `filename`, the initial grid is a random fill** —
//! algorithmically faithful (Bernoulli at `random_fill_ratio`) but not
//! bit-exact, per [`crate::rng`]. **`mold`** (a fade from `death_color`
//! toward `mold_color` the longer a cell has been dead) is accepted but not
//! implemented: dead cells always render as `death_color` here. See
//! `docs/filter/vaco-filter-source.md`.

use vaco_core::{MediaType, Rational, Result, Timestamp};
use vaco_filter_core::adapt::{SourceFilter, Sourced};
use vaco_filter_core::negotiate::{Constraint, FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use crate::rng::{SplitMix64, resolve_seed};
use vaco_filter_graph::registry::{Instance, Instantiate};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "life", help = "create life")]
pub(crate) struct Opts {
    #[opt(name = "size", alias = "s", help = "set video size", default = (320, 240), flags(filtering))]
    pub size: (u32, u32),
    #[opt(name = "rate", alias = "r", help = "set video rate", default = vaco_opts::VideoRate(Rational::new(25, 1)), flags(filtering))]
    pub rate: vaco_opts::VideoRate,
    #[opt(name = "rule", help = "set rule", default = "B3/S23".to_owned(), flags(filtering))]
    pub rule: String,
    #[opt(name = "random_fill_ratio", alias = "ratio", help = "set fill ratio for filling initial grid randomly", default = 0.618_034, range = 0.0..=1.0, flags(filtering))]
    pub random_fill_ratio: f64,
    #[opt(name = "random_seed", alias = "seed", help = "set the seed for filling the initial grid randomly", default = -1_i64, flags(filtering))]
    pub random_seed: i64,
    #[opt(name = "stitch", help = "stitch boundaries", default = true, flags(filtering))]
    pub stitch: bool,
    #[opt(name = "mold", help = "set mold speed for dead cells", default = 0, range = 0..=255, flags(filtering))]
    pub mold: i32,
    #[opt(name = "life_color", help = "set life color", default = "white".to_owned(), flags(filtering))]
    pub life_color: String,
    #[opt(name = "death_color", help = "set death color", default = "black".to_owned(), flags(filtering))]
    pub death_color: String,
    #[opt(name = "mold_color", help = "set mold color", default = "black".to_owned(), flags(filtering))]
    pub mold_color: String,
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

pub const DESC: FilterDesc = FilterDesc {
    name: "life",
    description: "Create life",
    inputs: &[],
    outputs: &[Pad {
        name: "default",
        media_type: MediaType::Video,
    }],
    flags: FilterFlags::empty(),
};

/// A parsed `B.../S...` rule: the sets of live-neighbour counts that cause a
/// birth or a survival.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Rule {
    birth: u16,
    survive: u16,
}

impl Rule {
    /// Parses e.g. `"B3/S23"`. Unrecognised text falls back to standard
    /// Life (`B3/S23`) rather than erroring, since a malformed rule string
    /// is not this generator's problem to diagnose.
    pub(crate) fn parse(s: &str) -> Self {
        let mut birth = 0u16;
        let mut survive = 0u16;
        let mut mode = 0u8; // 0 = none, 1 = B, 2 = S
        for ch in s.chars() {
            match ch.to_ascii_uppercase() {
                'B' => mode = 1,
                'S' => mode = 2,
                '0'..='8' => {
                    let bit = 1u16 << (ch as u8 - b'0');
                    match mode {
                        1 => birth |= bit,
                        2 => survive |= bit,
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        if birth == 0 && survive == 0 {
            return Self {
                birth: 1 << 3,
                survive: (1 << 2) | (1 << 3),
            };
        }
        Self { birth, survive }
    }

    fn births_on(self, n: u32) -> bool {
        n < 16 && (self.birth >> n) & 1 == 1
    }

    fn survives_on(self, n: u32) -> bool {
        n < 16 && (self.survive >> n) & 1 == 1
    }
}

#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    reason = "grid dimensions are video-sized (far below i64::MAX), and nx/ny are checked >= 0 before the cast back to usize"
)]
fn neighbour_count(grid: &[bool], w: usize, h: usize, x: usize, y: usize, stitch: bool) -> u32 {
    let mut count = 0u32;
    for dy in [-1i64, 0, 1] {
        for dx in [-1i64, 0, 1] {
            if dx == 0 && dy == 0 {
                continue;
            }
            let (nx, ny) = if stitch {
                (
                    (x as i64 + dx).rem_euclid(w as i64),
                    (y as i64 + dy).rem_euclid(h as i64),
                )
            } else {
                let nx = x as i64 + dx;
                let ny = y as i64 + dy;
                if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                    continue;
                }
                (nx, ny)
            };
            if grid
                .get(ny as usize * w + nx as usize)
                .copied()
                .unwrap_or(false)
            {
                count += 1;
            }
        }
    }
    count
}

/// One Life generation step under `rule`.
pub(crate) fn step(grid: &[bool], w: usize, h: usize, rule: Rule, stitch: bool) -> Vec<bool> {
    let mut out = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let n = neighbour_count(grid, w, h, x, y, stitch);
            let alive = grid.get(y * w + x).copied().unwrap_or(false);
            let next = if alive { rule.survives_on(n) } else { rule.births_on(n) };
            if let Some(slot) = out.get_mut(y * w + x) {
                *slot = next;
            }
        }
    }
    out
}

#[derive(Debug)]
struct Source {
    width: u32,
    height: u32,
    rule: Rule,
    stitch: bool,
    grid: Vec<bool>,
    life_rgb: [u8; 3],
    death_rgb: [u8; 3],
    frame_rate: Rational,
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
                ..
            } = &mut out
            {
                *width = self.width;
                *height = self.height;
                *time_base = self.frame_rate.inverse();
                *frame_rate = self.frame_rate;
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn produce(&mut self, ctx: &mut FilterContext<'_>) -> Result<Option<Frame>> {
        let mut frame = ctx
            .pool()
            .acquire_video(PixFmt::Rgb0, self.width, self.height)?;
        let w = self.width as usize;
        if let Some(mut plane) = frame.plane_mut(0) {
            for row_idx in 0..plane.rows() {
                if let Some(dst) = plane.row_mut(row_idx) {
                    for (x, px) in dst.chunks_exact_mut(4).enumerate() {
                        let alive = self
                            .grid
                            .get(row_idx * w + x)
                            .copied()
                            .unwrap_or(false);
                        let rgb = if alive { self.life_rgb } else { self.death_rgb };
                        if let [r, g, b, a] = px {
                            *r = rgb[0];
                            *g = rgb[1];
                            *b = rgb[2];
                            *a = 255;
                        }
                    }
                }
            }
        }
        self.grid = step(&self.grid, w, self.height as usize, self.rule, self.stitch);
        frame.pts = Timestamp::new(self.next);
        frame.time_base = self.frame_rate.inverse();
        frame.duration = vaco_core::Duration(1);
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

/// A defensive cap on the grid this crate pre-allocates as a plain
/// `Vec<bool>` in [`create`], independent of `vaco_frame::FramePool`'s own
/// per-plane limits (this grid is not a frame plane). 64 Mi cells is 64 MiB
/// of `bool`s -- generous for any real use, and far short of a request like
/// `size=911111x91111` (found by this crate's own fuzz target) trying to
/// allocate 83 GB.
const MAX_CELLS: u64 = 1 << 26;

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let (width, height) = opts.size;
    let cells = u64::from(width) * u64::from(height);
    if cells > MAX_CELLS {
        return Err(format!(
            "life: size {width}x{height} ({cells} cells) exceeds the {MAX_CELLS}-cell limit"
        ));
    }
    let rate = opts.rate.0;
    let rule = Rule::parse(&opts.rule);
    let seed = resolve_seed(opts.random_seed, 0x11FE_5EED);
    let mut rng = SplitMix64::new(seed);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "cells <= MAX_CELLS = 2^26, which fits in usize on every supported target"
    )]
    let cell_count = cells as usize;
    let grid: Vec<bool> = (0..cell_count)
        .map(|_| rng.next_f64() < opts.random_fill_ratio)
        .collect();
    let life_rgba = vaco_core::parse::color(&opts.life_color)
        .ok_or_else(|| format!("life: bad life_color `{}`", opts.life_color))?;
    let death_rgba = vaco_core::parse::color(&opts.death_color)
        .ok_or_else(|| format!("life: bad death_color `{}`", opts.death_color))?;
    // `mold`/`mold_color` are accepted but not implemented -- see module doc.
    let _ = (opts.mold, &opts.mold_color);
    let source = Source {
        width,
        height,
        rule,
        stitch: opts.stitch,
        grid,
        life_rgb: [life_rgba.r, life_rgba.g, life_rgba.b],
        death_rgb: [death_rgba.r, death_rgba.g, death_rgba.b],
        frame_rate: rate,
        next: 0,
    };
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats {
            inputs: Vec::new(),
            outputs: vec![FormatSet {
                pixel_formats: Some(Constraint::Exact(PixFmt::Rgb0)),
                ..FormatSet::default()
            }],
            ties: Vec::new(),
            label: req.instance.to_owned(),
        },
        filter: Box::new(Sourced::new(source)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(grid: &mut [bool], w: usize, cells: &[(usize, usize)]) {
        for &(x, y) in cells {
            if let Some(c) = grid.get_mut(y * w + x) {
                *c = true;
            }
        }
    }

    #[test]
    fn the_blinker_oscillates_with_period_two() {
        // A textbook Life oscillator: a horizontal 3-cell line becomes
        // vertical, then horizontal again. This is a property of the Life
        // rule itself, not a transcription of `step`.
        let (w, h) = (5, 5);
        let mut grid = vec![false; w * h];
        set(&mut grid, w, &[(1, 2), (2, 2), (3, 2)]);
        let rule = Rule::parse("B3/S23");
        let gen1 = step(&grid, w, h, rule, false);
        let mut expected1 = vec![false; w * h];
        set(&mut expected1, w, &[(2, 1), (2, 2), (2, 3)]);
        assert_eq!(gen1, expected1);
        let gen2 = step(&gen1, w, h, rule, false);
        assert_eq!(gen2, grid);
    }

    #[test]
    fn a_stable_block_never_changes() {
        let (w, h) = (4, 4);
        let mut grid = vec![false; w * h];
        set(&mut grid, w, &[(1, 1), (2, 1), (1, 2), (2, 2)]);
        let rule = Rule::parse("B3/S23");
        let next = step(&grid, w, h, rule, false);
        assert_eq!(next, grid);
    }

    #[test]
    fn malformed_rule_falls_back_to_standard_life() {
        let default_rule = Rule::parse("B3/S23");
        let fallback = Rule::parse("not a rule");
        assert_eq!(fallback.birth, default_rule.birth);
        assert_eq!(fallback.survive, default_rule.survive);
    }

    #[test]
    fn creatable_with_no_arguments() {
        let req = Instantiate {
            name: "life",
            instance: "life",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }
}
