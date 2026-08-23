//! `convolution` — apply an arbitrary small kernel per plane.
//!
//! `ffmpeg -h filter=convolution` documents four independent per-plane
//! option groups (`0`-`3` prefix): `<n>m` (matrix, string, default `"0 0 0
//! 0 1 0 0 0 0"` — the 3x3 identity), `<n>rdiv` (float, default `0`),
//! `<n>bias` (float, default `0`), `<n>mode` (`square`/`row`/`column`,
//! default `square`).
//!
//! # Measured semantics
//!
//! ```text
//! ffmpeg -f lavfi -i "color=gray:s=4x4,format=gray8,geq=lum='50'" \
//!   -vf "convolution=0m='1 1 1 1 1 1 1 1 1':0rdiv=0" -f rawvideo -pix_fmt gray8 -frames:v 1 -
//! ```
//!
//! comes back all `50`: with `rdiv=0` the reference normalises by the
//! matrix's own coefficient sum (`9`), not by `1` — `rdiv=0` is a sentinel
//! for "auto", never a literal zero divisor.
//!
//! ```text
//! ffmpeg ... -vf "convolution=0m='0 0 0 0 2 0 0 0 0':0rdiv=1:0bias=10" ...
//! ```
//!
//! comes back `110` on a `50` field: `out = round(sum(kernel*window)/rdiv) +
//! bias`, bias added *after* the division, both before the final clip.
//!
//! # Measured: a hard zero at any pixel whose kernel would read out of bounds
//!
//! ```text
//! ffmpeg -f lavfi -i "color=gray:s=5x5,format=gray8,geq=lum='10*X'" \
//!   -vf "convolution=0m='-1 0 1 -2 0 2 -1 0 1':0rdiv=1" -f rawvideo -pix_fmt gray8 -frames:v 1 -
//! ```
//!
//! gives `0 80 80 80 0` per row: the outer column is not computed with a
//! replicated or zero-padded border, it is forced to `0` outright. This is
//! the opposite of [`crate::boxblur`]'s measured border rule
//! (replicate-and-average) — two filters, two different rules, both
//! measured rather than assumed to match. [`crate::edge`] reuses this
//! module's engine for `sobel`/`prewitt`/`scharr` and inherits the same
//! zero border (confirmed by the identical `80` interior values above); see
//! that module's doc for where `roberts`/`kirsch` were measured to differ.
//!
//! # Not implemented
//!
//! `row`/`column` modes are parsed (an `n`-length 1-D kernel, centred) but
//! not separately probed against the reference; only `square` (the
//! default, and what every worked example above uses) is verified.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "convolution",
    description: "Apply convolution filter",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

const IDENTITY: &str = "0 0 0 0 1 0 0 0 0";

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "convolution", help = "Apply convolution filter")]
pub(crate) struct Opts {
    #[opt(name = "0m", help = "set matrix for 1st plane", default = IDENTITY.to_owned(), flags(video, filtering))]
    pub m0: String,
    #[opt(name = "1m", help = "set matrix for 2nd plane", default = IDENTITY.to_owned(), flags(video, filtering))]
    pub m1: String,
    #[opt(name = "2m", help = "set matrix for 3rd plane", default = IDENTITY.to_owned(), flags(video, filtering))]
    pub m2: String,
    #[opt(name = "3m", help = "set matrix for 4th plane", default = IDENTITY.to_owned(), flags(video, filtering))]
    pub m3: String,
    #[opt(name = "0rdiv", help = "set rdiv for 1st plane", default = 0.0, flags(video, filtering))]
    pub rdiv0: f64,
    #[opt(name = "1rdiv", help = "set rdiv for 2nd plane", default = 0.0, flags(video, filtering))]
    pub rdiv1: f64,
    #[opt(name = "2rdiv", help = "set rdiv for 3rd plane", default = 0.0, flags(video, filtering))]
    pub rdiv2: f64,
    #[opt(name = "3rdiv", help = "set rdiv for 4th plane", default = 0.0, flags(video, filtering))]
    pub rdiv3: f64,
    #[opt(name = "0bias", help = "set bias for 1st plane", default = 0.0, flags(video, filtering))]
    pub bias0: f64,
    #[opt(name = "1bias", help = "set bias for 2nd plane", default = 0.0, flags(video, filtering))]
    pub bias1: f64,
    #[opt(name = "2bias", help = "set bias for 3rd plane", default = 0.0, flags(video, filtering))]
    pub bias2: f64,
    #[opt(name = "3bias", help = "set bias for 4th plane", default = 0.0, flags(video, filtering))]
    pub bias3: f64,
    #[opt(name = "0mode", help = "set matrix mode for 1st plane", default = "square".to_owned(), flags(video, filtering))]
    pub mode0: String,
    #[opt(name = "1mode", help = "set matrix mode for 2nd plane", default = "square".to_owned(), flags(video, filtering))]
    pub mode1: String,
    #[opt(name = "2mode", help = "set matrix mode for 3rd plane", default = "square".to_owned(), flags(video, filtering))]
    pub mode2: String,
    #[opt(name = "3mode", help = "set matrix mode for 4th plane", default = "square".to_owned(), flags(video, filtering))]
    pub mode3: String,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Square,
    Row,
    Column,
}

impl Mode {
    fn parse(s: &str) -> std::result::Result<Self, String> {
        match s {
            "0" | "square" => Ok(Self::Square),
            "1" | "row" => Ok(Self::Row),
            "2" | "column" => Ok(Self::Column),
            other => Err(format!("convolution: bad mode `{other}`")),
        }
    }
}

/// One plane's resolved kernel: taps as `(dx, dy, weight)`, plus the
/// half-extent in each axis (for the zero-border test).
#[derive(Debug, Clone)]
pub(crate) struct Kernel {
    taps: Vec<(i32, i32, f64)>,
    rx: i32,
    ry: i32,
    rdiv: f64,
    bias: f64,
}

impl Kernel {
    #[allow(
        clippy::integer_division,
        reason = "decomposing a flat matrix index into 2D (row, col) coordinates; \
                  both operands are small (matrix length), not a value where \
                  precision loss matters"
    )]
    pub(crate) fn parse(matrix: &str, mode: Mode, rdiv: f64, bias: f64) -> std::result::Result<Self, String> {
        let values: Vec<f64> = matrix
            .split_whitespace()
            .map(|s| {
                s.parse::<f64>()
                    .map_err(|_| format!("convolution: bad matrix value `{s}`"))
            })
            .collect::<std::result::Result<_, _>>()?;
        if values.is_empty() {
            return Err("convolution: empty matrix".to_owned());
        }
        let (taps, rx, ry) = match mode {
            Mode::Square => {
                let n = (values.len() as f64).sqrt().round() as usize;
                if n * n != values.len() || n.is_multiple_of(2) {
                    return Err(format!(
                        "convolution: matrix of length {} is not an odd square",
                        values.len()
                    ));
                }
                let r = common::to_i32(n >> 1);
                let mut taps = Vec::new();
                for (i, &w) in values.iter().enumerate() {
                    let dy = common::to_i32(i / n) - r;
                    let dx = common::to_i32(i % n) - r;
                    taps.push((dx, dy, w));
                }
                (taps, r, r)
            }
            Mode::Row => {
                let n = values.len();
                if n.is_multiple_of(2) {
                    return Err("convolution: row matrix must have odd length".to_owned());
                }
                let r = common::to_i32(n >> 1);
                let taps = values
                    .iter()
                    .enumerate()
                    .map(|(i, &w)| (common::to_i32(i) - r, 0, w))
                    .collect();
                (taps, r, 0)
            }
            Mode::Column => {
                let n = values.len();
                if n.is_multiple_of(2) {
                    return Err("convolution: column matrix must have odd length".to_owned());
                }
                let r = common::to_i32(n >> 1);
                let taps = values
                    .iter()
                    .enumerate()
                    .map(|(i, &w)| (0, common::to_i32(i) - r, w))
                    .collect();
                (taps, 0, r)
            }
        };
        let sum: f64 = values.iter().sum();
        let effective_rdiv = if rdiv == 0.0 {
            if sum == 0.0 { 1.0 } else { sum }
        } else {
            rdiv
        };
        Ok(Self {
            taps,
            rx,
            ry,
            rdiv: effective_rdiv,
            bias,
        })
    }

    /// The raw `sum(kernel*window)/rdiv` at `(x, y)`, or `None` if any tap
    /// would read outside `[0, w) x [0, h)` — the measured zero-border
    /// rule shared by `convolution` and, per [`crate::edge`]'s doc, by
    /// `sobel`/`prewitt`/`scharr`.
    ///
    /// Exposed (not just [`Self::apply`]) because [`crate::edge`] needs the
    /// un-rounded, un-biased value for two kernels (`Gx`, `Gy`) before
    /// combining them into one magnitude.
    pub(crate) fn value_at(&self, rows: &[&[u8]], x: i32, y: i32, w: i32, h: i32) -> Option<f64> {
        if x - self.rx < 0 || x + self.rx >= w || y - self.ry < 0 || y + self.ry >= h {
            return None;
        }
        let mut acc = 0.0f64;
        for &(dx, dy, weight) in &self.taps {
            let v = common::sample_clamped(rows, x + dx, y + dy, w, h);
            acc += weight * f64::from(v);
        }
        Some(acc / self.rdiv)
    }

    /// Apply this kernel at `(x, y)`, or `None` if any tap would read
    /// outside `[0, w) x [0, h)` — the measured zero-border rule.
    fn apply(&self, rows: &[&[u8]], x: i32, y: i32, w: i32, h: i32) -> Option<u8> {
        let result = self.value_at(rows, x, y, w, h)?.round() + self.bias;
        Some(clamp_u8(result))
    }
}

pub(crate) fn clamp_u8(value: f64) -> u8 {
    if !value.is_finite() {
        return 0;
    }
    if value <= 0.0 {
        0
    } else if value >= 255.0 {
        255
    } else {
        value as u8
    }
}

/// Apply `kernel` to a whole plane, writing `0` at any pixel its window
/// would read out of bounds.
pub(crate) fn apply_plane(rows: &[&[u8]], w: i32, h: i32, kernel: &Kernel) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for y in 0..h {
        let mut row = Vec::new();
        for x in 0..w {
            row.push(kernel.apply(rows, x, y, w, h).unwrap_or(0));
        }
        out.push(row);
    }
    out
}

#[derive(Debug)]
pub(crate) struct Filter {
    kernels: [Kernel; 4],
}

impl Filter {
    fn new(opts: &Opts) -> std::result::Result<Self, String> {
        let kernels = [
            Kernel::parse(&opts.m0, Mode::parse(&opts.mode0)?, opts.rdiv0, opts.bias0)?,
            Kernel::parse(&opts.m1, Mode::parse(&opts.mode1)?, opts.rdiv1, opts.bias1)?,
            Kernel::parse(&opts.m2, Mode::parse(&opts.mode2)?, opts.rdiv2, opts.bias2)?,
            Kernel::parse(&opts.m3, Mode::parse(&opts.mode3)?, opts.rdiv3, opts.bias3)?,
        ];
        Ok(Self { kernels })
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { format, .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        common::ensure_8bit_addressable(format)?;
        let Some(LinkFormat::Video { width, height, .. }) = ctx.input_link(0).cloned() else {
            return Ok(FrameOut::One(input));
        };
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        for p in 0..format.plane_count() {
            let p8 = p as u8;
            let pw = common::to_i32(format.plane_width(width, p8));
            let ph = common::to_i32(format.plane_height(height, p8));
            let Some(kernel) = self.kernels.get(p) else {
                continue;
            };
            let Some(src_plane) = input.plane(p) else {
                continue;
            };
            let rows = common::collect_rows(src_plane, ph.max(0) as usize);
            let filtered = apply_plane(&rows, pw, ph, kernel);
            let Some(mut dst_plane) = out.plane_mut(p) else {
                continue;
            };
            for (y, row) in filtered.iter().enumerate() {
                if let Some(dst_row) = dst_plane.row_mut(y) {
                    let n = dst_row.len().min(row.len());
                    if let (Some(d), Some(s)) = (dst_row.get_mut(..n), row.get(..n)) {
                        d.copy_from_slice(s);
                    }
                }
            }
        }
        common::copy_frame_meta(&mut out, &input);
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter::new(&opts)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    /// Pinned against the reference probe in this module's doc: `rdiv=0`
    /// auto-normalises by the matrix sum.
    #[test]
    fn rdiv_zero_normalizes_by_the_matrix_sum() {
        let kernel = Kernel::parse("1 1 1 1 1 1 1 1 1", Mode::Square, 0.0, 0.0).unwrap();
        let rows_owned = vec![vec![50u8; 4]; 4];
        let rows: Vec<&[u8]> = rows_owned.iter().map(Vec::as_slice).collect();
        let out = apply_plane(&rows, 4, 4, &kernel);
        assert_eq!(out[1][1], 50);
        assert_eq!(out[2][2], 50);
    }

    /// Pinned against the reference probe in this module's doc: bias is
    /// added after the rdiv division.
    #[test]
    fn bias_is_added_after_division() {
        let kernel = Kernel::parse("0 0 0 0 2 0 0 0 0", Mode::Square, 1.0, 10.0).unwrap();
        let rows_owned = vec![vec![50u8; 4]; 4];
        let rows: Vec<&[u8]> = rows_owned.iter().map(Vec::as_slice).collect();
        let out = apply_plane(&rows, 4, 4, &kernel);
        assert_eq!(out[1][1], 110);
    }

    /// Pinned against the reference probe in this module's doc: any pixel
    /// whose 3x3 window would read out of bounds comes back exactly `0`.
    #[test]
    fn border_pixels_are_forced_to_zero() {
        let kernel = Kernel::parse("-1 0 1 -2 0 2 -1 0 1", Mode::Square, 1.0, 0.0).unwrap();
        let rows_owned: Vec<Vec<u8>> = (0..5).map(|_| vec![0u8, 10, 20, 30, 40]).collect();
        let rows: Vec<&[u8]> = rows_owned.iter().map(Vec::as_slice).collect();
        let out = apply_plane(&rows, 5, 5, &kernel);
        assert_eq!(out[2][0], 0);
        assert_eq!(out[2][4], 0);
        assert_eq!(out[2][1], 80);
        assert_eq!(out[2][2], 80);
        assert_eq!(out[2][3], 80);
    }

    /// Independent oracle: the identity matrix must be a true identity —
    /// interior pixels unchanged — for any input, which is a property of
    /// "identity kernel" rather than a re-derivation of the convolution
    /// arithmetic.
    #[test]
    fn identity_matrix_is_identity_away_from_the_border() {
        let kernel = Kernel::parse(IDENTITY, Mode::Square, 0.0, 0.0).unwrap();
        let rows_owned: Vec<Vec<u8>> = (0..5).map(|y| (0..5).map(|x| (x * 7 + y * 3) as u8).collect()).collect();
        let rows: Vec<&[u8]> = rows_owned.iter().map(Vec::as_slice).collect();
        let out = apply_plane(&rows, 5, 5, &kernel);
        for y in 1..4 {
            for x in 1..4 {
                assert_eq!(out[y][x], rows_owned[y][x]);
            }
        }
    }
}
