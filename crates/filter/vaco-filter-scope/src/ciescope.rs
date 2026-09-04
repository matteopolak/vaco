//! `ciescope` — plot input RGB pixels in a CIE chromaticity diagram.
//!
//! The filter consumes packed 8-bit RGB and emits a square packed 16-bit
//! RGBA canvas. RGB is converted through the selected colour system's
//! published primary chromaticities into XYZ, then projected as CIE 1931
//! xyY, CIE 1960 UCS, or CIE 1976 u'v'. The spectral-locus outline is
//! generated analytically from Wyman, Sloan & Shirley's 2013 CIE 1931
//! matching-function fit rather than from a copied table.
//!
//! Black-box measurements against `ffmpeg 9.0.1` pinned the observable
//! raster rules used here: coordinates use `floor(c * (size - 1))`; a cell's
//! value is `count * floor(65535 * intensity)`, saturated to 16 bits; and a
//! touched cell is fully opaque. At `size=256`, BT.709 red/green/blue land at
//! `(163,170)`, `(76,102)`, and `(38,239)` in xyY mode. The same primaries
//! land at `(114,166)`, `(31,159)`, `(44,228)` in UCS and `(114,121)`,
//! `(31,111)`, `(44,214)` in u'v'.
//!
//! The reference's exact spectral-locus antialiasing and interior colour
//! rendering are authorial rasterisation choices, not properties of the CIE
//! standards. This implementation is therefore pixel-exact for measured data
//! cell placement/intensity but only structural for the diagram background.

use vaco_color::{Chromaticity, ColorPrimaries};
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_filter_graph::registry::{Instance, Instantiate};
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "ciescope",
    description: "Video CIE scope.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "ciescope", help = "Video CIE scope.")]
pub(crate) struct Opts {
    #[opt(name = "system", help = "set color system", default = "hdtv".to_string(), flags(video, filtering))]
    system: String,
    #[opt(name = "cie", help = "set cie system", default = "xyy".to_string(), flags(video, filtering))]
    cie: String,
    #[opt(name = "gamuts", help = "set what gamuts to draw", default = "0".to_string(), flags(video, filtering))]
    gamuts: String,
    #[opt(name = "size", alias = "s", help = "set ciescope size", default = 512, range = 256..=8192, flags(video, filtering))]
    size: i64,
    #[opt(name = "intensity", alias = "i", help = "set ciescope intensity", default = 0.001, range = 0.0..=1.0, flags(video, filtering))]
    intensity: f64,
    #[opt(name = "contrast", help = "set diagram contrast", default = 0.75, range = 0.0..=1.0, flags(video, filtering))]
    contrast: f64,
    #[opt(
        name = "corrgamma",
        help = "correct input gamma",
        default = true,
        flags(video, filtering)
    )]
    corrgamma: bool,
    #[opt(
        name = "showwhite",
        help = "show reference white",
        default = false,
        flags(video, filtering)
    )]
    showwhite: bool,
    #[opt(name = "gamma", help = "set diagram gamma", default = 2.6, range = 0.1..=6.0, flags(video, filtering))]
    gamma: f64,
    #[opt(
        name = "fill",
        help = "fill with CIE colors",
        default = true,
        flags(video, filtering)
    )]
    fill: bool,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut opts = Self::default();
        if let Some(text) = args {
            opts.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        Ok(opts)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CieSpace {
    Xyy,
    Ucs,
    Luv,
}

impl CieSpace {
    fn parse(value: &str) -> std::result::Result<Self, String> {
        match value {
            "xyy" | "0" => Ok(Self::Xyy),
            "ucs" | "1" => Ok(Self::Ucs),
            "luv" | "2" => Ok(Self::Luv),
            other => Err(format!("ciescope: unknown cie system `{other}`")),
        }
    }

    fn project(self, xyz: [f64; 3]) -> Option<(f64, f64)> {
        let [x, y, z] = xyz;
        match self {
            Self::Xyy => {
                let sum = x + y + z;
                (sum > 0.0).then_some((x / sum, y / sum))
            }
            Self::Ucs | Self::Luv => {
                let d = x + 15.0 * y + 3.0 * z;
                if d <= 0.0 {
                    return None;
                }
                let v_scale = if self == Self::Ucs { 6.0 } else { 9.0 };
                Some((4.0 * x / d, v_scale * y / d))
            }
        }
    }

    fn unproject(self, u: f64, v: f64) -> Option<[f64; 3]> {
        if v <= 0.0 {
            return None;
        }
        match self {
            Self::Xyy => Some([u / v, 1.0, (1.0 - u - v) / v]),
            Self::Ucs | Self::Luv => {
                let v_scale = if self == Self::Ucs { 6.0 } else { 9.0 };
                let d = v_scale / v;
                let x = u * d / 4.0;
                Some([x, 1.0, (d - x - 15.0) / 3.0])
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum System {
    Ntsc,
    Ebu,
    Smpte,
    Smpte240m,
    Apple,
    WideRgb,
    Cie1931,
    Hdtv,
    Uhdtv,
    DciP3,
}

impl System {
    fn parse(value: &str) -> std::result::Result<Self, String> {
        match value {
            "ntsc" | "470m" | "0" => Ok(Self::Ntsc),
            "ebu" | "470bg" | "1" => Ok(Self::Ebu),
            "smpte" | "2" => Ok(Self::Smpte),
            "240m" | "3" => Ok(Self::Smpte240m),
            "apple" | "4" => Ok(Self::Apple),
            "widergb" | "5" => Ok(Self::WideRgb),
            "cie1931" | "6" => Ok(Self::Cie1931),
            "hdtv" | "rec709" | "7" => Ok(Self::Hdtv),
            "uhdtv" | "rec2020" | "8" => Ok(Self::Uhdtv),
            "dcip3" | "9" => Ok(Self::DciP3),
            other => Err(format!("ciescope: unknown color system `{other}`")),
        }
    }

    #[allow(
        clippy::expect_used,
        reason = "the internal System enum maps only to ColorPrimaries variants with defined chromaticities"
    )]
    fn chromaticity(self) -> Chromaticity {
        let standard = match self {
            Self::Ntsc => Some(ColorPrimaries::Bt470m),
            Self::Ebu => Some(ColorPrimaries::Bt470bg),
            Self::Smpte => Some(ColorPrimaries::Smpte170m),
            Self::Smpte240m => Some(ColorPrimaries::Smpte240m),
            Self::Cie1931 => Some(ColorPrimaries::Smpte428),
            Self::Hdtv => Some(ColorPrimaries::Bt709),
            Self::Uhdtv => Some(ColorPrimaries::Bt2020),
            Self::DciP3 => Some(ColorPrimaries::Smpte431),
            Self::Apple | Self::WideRgb => None,
        };
        if let Some(primaries) = standard {
            return primaries
                .chromaticity()
                .expect("every selected standard has chromaticities");
        }
        match self {
            Self::Apple => Chromaticity {
                red: (0.625, 0.340),
                green: (0.280, 0.595),
                blue: (0.115, 0.070),
                white: (0.3127, 0.3290),
            },
            Self::WideRgb => Chromaticity {
                red: (0.7347, 0.2653),
                green: (0.1152, 0.8264),
                blue: (0.1566, 0.0177),
                white: (0.3457, 0.3585),
            },
            _ => unreachable!("standard systems returned above"),
        }
    }

    fn rgb_to_xyz(self) -> Option<[[f64; 3]; 3]> {
        match self {
            Self::Ntsc => ColorPrimaries::Bt470m.rgb_to_xyz(),
            Self::Ebu => ColorPrimaries::Bt470bg.rgb_to_xyz(),
            Self::Smpte => ColorPrimaries::Smpte170m.rgb_to_xyz(),
            Self::Smpte240m => ColorPrimaries::Smpte240m.rgb_to_xyz(),
            Self::Cie1931 => ColorPrimaries::Smpte428.rgb_to_xyz(),
            Self::Hdtv => ColorPrimaries::Bt709.rgb_to_xyz(),
            Self::Uhdtv => ColorPrimaries::Bt2020.rgb_to_xyz(),
            Self::DciP3 => ColorPrimaries::Smpte431.rgb_to_xyz(),
            Self::Apple | Self::WideRgb => self.chromaticity().rgb_to_xyz(),
        }
    }

    fn project_rgb(self, cie: CieSpace, rgb: [f64; 3], size: u32) -> Option<(i32, i32)> {
        let xyz = if rgb.iter().all(|channel| channel.abs() <= f64::EPSILON) {
            let (x, y) = self.chromaticity().white;
            [x / y, 1.0, (1.0 - x - y) / y]
        } else {
            mul3v(self.rgb_to_xyz()?, rgb)
        };
        cie.project(xyz).map(|point| canvas_point(point, size))
    }
}

#[derive(Debug)]
pub(crate) struct Filter {
    size: u32,
    system: System,
    cie: CieSpace,
    gamut_systems: Vec<System>,
    intensity: f64,
    contrast: f64,
    corrgamma: bool,
    showwhite: bool,
    gamma: f64,
    fill: bool,
    base: Option<Vec<u8>>,
}

fn mul3v(matrix: [[f64; 3]; 3], vector: [f64; 3]) -> [f64; 3] {
    matrix.map(|row| row[0] * vector[0] + row[1] * vector[1] + row[2] * vector[2])
}

fn invert3(matrix: [[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let [[a, b, c], [d, e, f], [g, h, i]] = matrix;
    let aa = e * i - f * h;
    let ab = c * h - b * i;
    let ac = b * f - c * e;
    let ba = f * g - d * i;
    let bb = a * i - c * g;
    let bc = c * d - a * f;
    let ca = d * h - e * g;
    let cb = b * g - a * h;
    let cc = a * e - b * d;
    let determinant = a * aa + b * ba + c * ca;
    if !determinant.is_finite() || determinant.abs() < f64::EPSILON {
        return None;
    }
    let r = determinant.recip();
    Some([
        [aa * r, ab * r, ac * r],
        [ba * r, bb * r, bc * r],
        [ca * r, cb * r, cc * r],
    ])
}

fn cie_xyz(wavelength: f64) -> [f64; 3] {
    let gaussian = |centre: f64, left: f64, right: f64| {
        let scale = if wavelength < centre { left } else { right };
        let t = (wavelength - centre) * scale;
        (-0.5 * t * t).exp()
    };
    [
        0.362 * gaussian(442.0, 0.0624, 0.0374) + 1.056 * gaussian(599.8, 0.0264, 0.0323)
            - 0.065 * gaussian(501.1, 0.0490, 0.0382),
        0.821 * gaussian(568.8, 0.0213, 0.0247) + 0.286 * gaussian(530.9, 0.0613, 0.0322),
        1.217 * gaussian(437.0, 0.0845, 0.0278) + 0.681 * gaussian(459.0, 0.0385, 0.0725),
    ]
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "coordinates are clamped to the output canvas before conversion"
)]
fn canvas_point(point: (f64, f64), size: u32) -> (i32, i32) {
    let edge = f64::from(size.saturating_sub(1));
    let x = (point.0.clamp(0.0, 1.0) * edge).floor() as i32;
    let y = ((1.0 - point.1.clamp(0.0, 1.0)) * edge).floor() as i32;
    (x, y)
}

fn rgba_offset(size: u32, x: i32, y: i32) -> Option<usize> {
    if x < 0 || y < 0 || x >= i32::try_from(size).ok()? || y >= i32::try_from(size).ok()? {
        return None;
    }
    let pos = usize::try_from(y).ok()? * usize::try_from(size).ok()? + usize::try_from(x).ok()?;
    pos.checked_mul(8)
}

fn set_rgba(canvas: &mut [u8], size: u32, x: i32, y: i32, rgba: [u16; 4]) {
    let Some(offset) = rgba_offset(size, x, y) else {
        return;
    };
    let Some(pixel) = canvas.get_mut(offset..offset + 8) else {
        return;
    };
    for (dst, value) in pixel.chunks_exact_mut(2).zip(rgba) {
        dst.copy_from_slice(&value.to_le_bytes());
    }
}

fn draw_line(canvas: &mut [u8], size: u32, from: (i32, i32), to: (i32, i32), rgba: [u16; 4]) {
    let (mut x, mut y) = from;
    let dx = (to.0 - x).abs();
    let sx = if x < to.0 { 1 } else { -1 };
    let dy = -(to.1 - y).abs();
    let sy = if y < to.1 { 1 } else { -1 };
    let mut error = dx + dy;
    loop {
        set_rgba(canvas, size, x, y, rgba);
        if (x, y) == to {
            break;
        }
        let twice = 2 * error;
        if twice >= dy {
            error += dy;
            x += sx;
        }
        if twice <= dx {
            error += dx;
            y += sy;
        }
    }
}

fn locus(cie: CieSpace, size: u32) -> Vec<(i32, i32)> {
    (380..=780)
        .step_by(5)
        .filter_map(|wavelength| cie.project(cie_xyz(f64::from(wavelength))))
        .map(|point| canvas_point(point, size))
        .collect()
}

fn point_in_polygon(point: (i32, i32), polygon: &[(i32, i32)]) -> bool {
    let (x, y) = (f64::from(point.0), f64::from(point.1));
    let mut inside = false;
    for (&a, &b) in polygon
        .iter()
        .zip(polygon.iter().cycle().skip(1))
        .take(polygon.len())
    {
        let (ax, ay) = (f64::from(a.0), f64::from(a.1));
        let (bx, by) = (f64::from(b.0), f64::from(b.1));
        if (ay > y) != (by > y) && x < (bx - ax) * (y - ay) / (by - ay) + ax {
            inside = !inside;
        }
    }
    inside
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "unit value is clamped to 0..=1 before conversion"
)]
fn unit_u16(value: f64) -> u16 {
    (value.clamp(0.0, 1.0) * 65_535.0).round() as u16
}

impl Filter {
    fn build_base(&self) -> Result<Vec<u8>> {
        let side = usize::try_from(self.size).unwrap_or(0);
        let pixels = side
            .checked_mul(side)
            .ok_or(vaco_core::Error::LimitExceeded {
                limit: "ciescope_pixels",
                requested: u64::MAX,
                cap: usize::MAX as u64,
            })?;
        let bytes = pixels
            .checked_mul(8)
            .ok_or(vaco_core::Error::LimitExceeded {
                limit: "ciescope_canvas_bytes",
                requested: u64::MAX,
                cap: usize::MAX as u64,
            })?;
        let mut budget = Budget::new(Limits::permissive());
        let mut canvas = budget.alloc::<u8>(bytes)?;
        let locus = locus(self.cie, self.size);
        let chromaticity = self.system.chromaticity();
        let inverse = self.system.rgb_to_xyz().and_then(invert3);

        if self.fill
            && let Some(xyz_to_rgb) = inverse
        {
            for y in 0..self.size {
                for x in 0..self.size {
                    let (ix, iy) = (
                        i32::try_from(x).unwrap_or(i32::MAX),
                        i32::try_from(y).unwrap_or(i32::MAX),
                    );
                    if !point_in_polygon((ix, iy), &locus) {
                        continue;
                    }
                    let edge = f64::from(self.size.saturating_sub(1)).max(1.0);
                    let coords = (f64::from(x) / edge, 1.0 - f64::from(y) / edge);
                    let Some(xyz) = self.cie.unproject(coords.0, coords.1) else {
                        continue;
                    };
                    let mut rgb = mul3v(xyz_to_rgb, xyz);
                    let peak = rgb.iter().copied().fold(0.0f64, f64::max);
                    if peak > 1.0 {
                        for channel in &mut rgb {
                            *channel /= peak;
                        }
                    }
                    let encode = |channel: f64| {
                        unit_u16(channel.max(0.0).powf(self.gamma.recip()) * self.contrast)
                    };
                    set_rgba(
                        &mut canvas,
                        self.size,
                        ix,
                        iy,
                        [encode(rgb[0]), encode(rgb[1]), encode(rgb[2]), 65_535],
                    );
                }
            }
        }

        let white = [65_535; 4];
        for pair in locus.windows(2) {
            if let [from, to] = pair {
                draw_line(&mut canvas, self.size, *from, *to, white);
            }
        }
        if let (Some(&first), Some(&last)) = (locus.first(), locus.last()) {
            draw_line(&mut canvas, self.size, last, first, white);
        }

        for system in &self.gamut_systems {
            let c = system.chromaticity();
            let points = if *system == System::Cie1931 {
                [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
                    .map(|xyz| canvas_point(self.cie.project(xyz).unwrap_or((0.0, 0.0)), self.size))
            } else {
                [c.red, c.green, c.blue].map(|xy| {
                    let xyz = [xy.0 / xy.1, 1.0, (1.0 - xy.0 - xy.1) / xy.1];
                    canvas_point(self.cie.project(xyz).unwrap_or(xy), self.size)
                })
            };
            draw_line(&mut canvas, self.size, points[0], points[1], white);
            draw_line(&mut canvas, self.size, points[1], points[2], white);
            draw_line(&mut canvas, self.size, points[2], points[0], white);
        }

        if self.showwhite {
            let (x, y) = canvas_point(chromaticity.white, self.size);
            draw_line(&mut canvas, self.size, (x - 3, y), (x + 3, y), white);
            draw_line(&mut canvas, self.size, (x, y - 3), (x, y + 3), white);
        }
        Ok(canvas)
    }

    fn add_input_histogram(&self, input: &Frame, output: &mut Frame) {
        let Some(plane) = input.plane(0) else {
            return;
        };
        let Some(mut output_plane) = output.plane_mut(0) else {
            return;
        };
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "intensity is constrained to 0..=1"
        )]
        let per_hit = (65_535.0 * self.intensity).floor() as u16;
        for row in plane.rows_iter() {
            for pixel in row.chunks_exact(3) {
                let [red, green, blue] = pixel else {
                    continue;
                };
                let channel = |sample: u8| {
                    let value = f64::from(sample) / 255.0;
                    if self.corrgamma {
                        value.powf(self.gamma)
                    } else {
                        value
                    }
                };
                let rgb = [channel(*red), channel(*green), channel(*blue)];
                let Some((x, y)) = self.system.project_rgb(self.cie, rgb, self.size) else {
                    continue;
                };
                let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
                    continue;
                };
                let Some(row) = output_plane.row_mut(y) else {
                    continue;
                };
                let Some(offset) = x.checked_mul(8) else {
                    continue;
                };
                let Some(pixel) = row.get_mut(offset..offset + 8) else {
                    continue;
                };
                let Some(rgb) = pixel.get_mut(..6) else {
                    continue;
                };
                for channel in rgb.chunks_exact_mut(2) {
                    let [low, high] = channel else { continue };
                    let old = u16::from_le_bytes([*low, *high]);
                    let [next_low, next_high] = old.saturating_add(per_hit).to_le_bytes();
                    *low = next_low;
                    *high = next_high;
                }
                if let Some(alpha) = pixel.get_mut(6..8) {
                    alpha.copy_from_slice(&65_535u16.to_le_bytes());
                }
            }
        }
    }
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(mut output) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video { width, height, .. } = &mut output {
                *width = self.size;
                *height = self.size;
            }
            ctx.set_output_link(0, output);
        }
        Ok(())
    }

    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { format, .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        if format != PixFmt::Rgb24 {
            return Ok(FrameOut::One(input));
        }
        let mut output = ctx
            .pool()
            .acquire_video(PixFmt::Rgba64le, self.size, self.size)?;
        if self.base.is_none() {
            self.base = Some(self.build_base()?);
        }
        if let Some(mut plane) = output.plane_mut(0) {
            let row_bytes = usize::try_from(self.size).unwrap_or(0).saturating_mul(8);
            for (row, source) in plane.rows_mut().zip(
                self.base
                    .as_deref()
                    .unwrap_or_default()
                    .chunks_exact(row_bytes),
            ) {
                if let Some(dst) = row.get_mut(..row_bytes) {
                    dst.copy_from_slice(source);
                }
            }
        }
        self.add_input_histogram(&input, &mut output);
        output.pts = input.pts;
        output.time_base = input.time_base;
        output.duration = input.duration;
        Ok(FrameOut::One(output))
    }
}

fn parse_gamuts(value: &str) -> std::result::Result<Vec<System>, String> {
    if value.is_empty() || value == "0" {
        return Ok(Vec::new());
    }
    if let Ok(mask) = value.parse::<u16>() {
        return (0..10)
            .filter(|bit| mask & (1 << bit) != 0)
            .map(|bit| System::parse(&bit.to_string()))
            .collect();
    }
    value
        .split(['+', '|'])
        .filter(|name| !name.is_empty())
        .map(System::parse)
        .collect()
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let system = System::parse(&opts.system)?;
    let cie = CieSpace::parse(&opts.cie)?;
    let gamut_systems = parse_gamuts(&opts.gamuts)?;
    let size = u32::try_from(opts.size).map_err(|_| "ciescope: invalid size".to_string())?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::converter(
            FormatSet::video_exact(PixFmt::Rgb24),
            FormatSet::video_exact(PixFmt::Rgba64le),
            req.instance,
        ),
        filter: Box::new(Simple::new(Filter {
            size,
            system,
            cie,
            gamut_systems,
            intensity: opts.intensity,
            contrast: opts.contrast,
            corrgamma: opts.corrgamma,
            showwhite: opts.showwhite,
            gamma: opts.gamma,
            fill: opts.fill,
            base: None,
        })),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn measured_bt709_primary_locations_match_all_coordinate_systems() {
        let matrix = System::Hdtv.rgb_to_xyz().unwrap();
        let xyz = |rgb| mul3v(matrix, rgb);
        let position = |space: CieSpace, rgb| canvas_point(space.project(xyz(rgb)).unwrap(), 256);
        assert_eq!(position(CieSpace::Xyy, [1.0, 0.0, 0.0]), (163, 170));
        assert_eq!(position(CieSpace::Xyy, [0.0, 1.0, 0.0]), (76, 102));
        assert_eq!(position(CieSpace::Xyy, [0.0, 0.0, 1.0]), (38, 239));
        assert_eq!(position(CieSpace::Ucs, [1.0, 0.0, 0.0]), (114, 166));
        assert_eq!(position(CieSpace::Luv, [0.0, 0.0, 1.0]), (44, 214));
    }

    #[test]
    fn measured_black_maps_to_the_selected_system_white_point() {
        assert_eq!(
            System::Hdtv.project_rgb(CieSpace::Xyy, [0.0, 0.0, 0.0], 256),
            Some((79, 171))
        );
    }

    #[test]
    fn measured_apple_blue_primary_location_matches_reference() {
        assert_eq!(
            System::Apple.project_rgb(CieSpace::Xyy, [0.0, 0.0, 1.0], 256),
            Some((29, 237))
        );
    }

    #[test]
    fn every_documented_alias_parses() {
        for name in [
            "ntsc", "470m", "ebu", "470bg", "smpte", "240m", "apple", "widergb", "cie1931", "hdtv",
            "rec709", "uhdtv", "rec2020", "dcip3",
        ] {
            assert!(System::parse(name).is_ok(), "{name}");
        }
        assert!(parse_gamuts("rec709+dcip3").is_ok());
        assert_eq!(CieSpace::parse("0"), Ok(CieSpace::Xyy));
        assert_eq!(CieSpace::parse("1"), Ok(CieSpace::Ucs));
        assert_eq!(CieSpace::parse("2"), Ok(CieSpace::Luv));
    }
}
