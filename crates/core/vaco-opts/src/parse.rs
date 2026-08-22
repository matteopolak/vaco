//! Value parsers for the string-shaped option bases, plus the small vocabulary
//! types they produce.
//!
//! # Where this belongs
//!
//! Plan 11 §4.2 places `parse::{image_size, video_rate, duration, color}` and
//! `Rgba` in `vaco-core`. That crate is a stub without them. They live here
//! until it catches up; the signatures are deliberately the same.

use core::fmt::Write as _;

use vaco_core::{Duration, Rational};

// ---------------------------------------------------------------- vocabulary

/// An 8-bit-per-channel colour with alpha. The `color` option base.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

/// A frame rate.
///
/// A newtype over [`Rational`] rather than a bare `Rational`, because
/// `OptBase::Rational` and `OptBase::VideoRate` are different option types with
/// different grammars and a type can carry only one `OptValue` impl.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VideoRate(pub Rational);

/// Opaque bytes, hex-encoded on the wire. The `binary` option base.
///
/// A newtype rather than a bare `Vec<u8>` so it does not collide with the
/// blanket array impl for `Vec<T>`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Binary(pub Vec<u8>);

// ---------------------------------------------------------------- image size

/// Standard frame-size abbreviations. Sizes are industry-standard interface
/// facts (D9), not authored data.
const SIZES: &[(&str, u32, u32)] = &[
    ("ntsc", 720, 480),
    ("pal", 720, 576),
    ("qntsc", 352, 240),
    ("qpal", 352, 288),
    ("sntsc", 640, 480),
    ("spal", 768, 576),
    ("film", 352, 240),
    ("ntsc-film", 352, 240),
    ("sqcif", 128, 96),
    ("qcif", 176, 144),
    ("cif", 352, 288),
    ("4cif", 704, 576),
    ("16cif", 1408, 1152),
    ("qqvga", 160, 120),
    ("qvga", 320, 240),
    ("vga", 640, 480),
    ("svga", 800, 600),
    ("xga", 1024, 768),
    ("uxga", 1600, 1200),
    ("qxga", 2048, 1536),
    ("sxga", 1280, 1024),
    ("qsxga", 2560, 2048),
    ("hsxga", 5120, 4096),
    ("wvga", 852, 480),
    ("wxga", 1366, 768),
    ("wsxga", 1600, 1024),
    ("wuxga", 1920, 1200),
    ("woxga", 2560, 1600),
    ("wqsxga", 3200, 2048),
    ("wquxga", 3840, 2400),
    ("whsxga", 6400, 4096),
    ("whuxga", 7680, 4800),
    ("cga", 320, 200),
    ("ega", 640, 350),
    ("hd480", 852, 480),
    ("hd720", 1280, 720),
    ("hd1080", 1920, 1080),
    ("2k", 2048, 1080),
    ("2kflat", 1998, 1080),
    ("2kscope", 2048, 858),
    ("4k", 4096, 2160),
    ("4kflat", 3996, 2160),
    ("4kscope", 4096, 1716),
    ("nhd", 640, 360),
    ("hqvga", 240, 160),
    ("wqvga", 400, 240),
    ("fwqvga", 432, 240),
    ("hvga", 480, 320),
    ("qhd", 960, 540),
    ("2kdci", 2048, 1080),
    ("4kdci", 4096, 2160),
    ("uhd2160", 3840, 2160),
    ("uhd4320", 7680, 4320),
];

/// `"1920x1080"`, or one of the abbreviations in [`SIZES`].
#[must_use]
pub fn image_size(s: &str) -> Option<(u32, u32)> {
    if let Some(&(_, w, h)) = SIZES.iter().find(|(n, _, _)| *n == s) {
        return Some((w, h));
    }
    let (w, h) = s.split_once('x').or_else(|| s.split_once('X'))?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// The abbreviation table, for help output and tests.
pub fn image_size_names() -> impl Iterator<Item = &'static str> {
    SIZES.iter().map(|(n, _, _)| *n)
}

// ---------------------------------------------------------------- video rate

const RATES: &[(&str, i32, i32)] = &[
    ("ntsc", 30000, 1001),
    ("pal", 25, 1),
    ("qntsc", 30000, 1001),
    ("qpal", 25, 1),
    ("sntsc", 30000, 1001),
    ("spal", 25, 1),
    ("film", 24, 1),
    ("ntsc-film", 24000, 1001),
];

/// `"25"`, `"30000/1001"`, `"29.97"`, `"ntsc"`, …
#[must_use]
pub fn video_rate(s: &str) -> Option<Rational> {
    if let Some(&(_, n, d)) = RATES.iter().find(|(n, _, _)| *n == s) {
        return Some(Rational::new(n, d));
    }
    rational(s)
}

/// `"num/den"`, `"25"`, or a decimal, which is approximated.
#[must_use]
pub fn rational(s: &str) -> Option<Rational> {
    if let Some((n, d)) = s.split_once('/').or_else(|| s.split_once(':')) {
        return Some(Rational::new(
            n.trim().parse().ok()?,
            d.trim().parse().ok()?,
        ));
    }
    if let Ok(n) = s.parse::<i32>() {
        return Some(Rational::new(n, 1));
    }
    let f: f64 = s.parse().ok()?;
    approximate(f, 1_000_000)
}

/// Best rational approximation of `value` with a bounded denominator.
///
/// `vaco_core::Rational::approximate` is still a `todo!()`, so this crate
/// carries its own continued-fraction routine. Delete it when that lands.
#[must_use]
pub fn approximate(value: f64, max_den: i64) -> Option<Rational> {
    if !value.is_finite() {
        return None;
    }
    let neg = value < 0.0;
    let mut x = value.abs();
    // Continued fraction expansion, tracking the two most recent convergents.
    let (mut num0, mut den0, mut num1, mut den1) = (0i64, 1i64, 1i64, 0i64);
    for _ in 0..64 {
        let a = x.floor();
        if !a.is_finite() || a > 1e18 {
            break;
        }
        let a = a as i64;
        let num2 = a.checked_mul(num1)?.checked_add(num0)?;
        let den2 = a.checked_mul(den1)?.checked_add(den0)?;
        if den2 > max_den {
            break;
        }
        num0 = num1;
        den0 = den1;
        num1 = num2;
        den1 = den2;
        let frac = x - a as f64;
        if frac.abs() < 1e-12 {
            break;
        }
        x = 1.0 / frac;
    }
    if den1 == 0 {
        return None;
    }
    let n = i32::try_from(num1).ok()?;
    let d = i32::try_from(den1).ok()?;
    Some(Rational::new(if neg { -n } else { n }, d))
}

// ---------------------------------------------------------------- duration

const US_PER_S: i128 = 1_000_000;

/// `"12:34:56.789"`, `"-1:02.5"`, `"1234.5"`, `"5ms"`, `"2s"`. Microseconds.
#[must_use]
pub fn duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (neg, rest) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    if rest.is_empty() {
        return None;
    }

    let us: i128 = if rest.contains(':') {
        let parts: Vec<&str> = rest.split(':').collect();
        if parts.len() > 3 || parts.iter().any(|p| p.is_empty()) {
            return None;
        }
        let last = parts.len() - 1;
        // Hour and minute columns accumulate as whole seconds; only the last
        // column may carry a fraction.
        let mut whole: i128 = 0;
        for p in parts.get(..last)? {
            let v: i128 = p.parse::<i64>().ok()?.into();
            if v < 0 {
                return None;
            }
            whole = whole.checked_mul(60)?.checked_add(v)?;
        }
        whole
            .checked_mul(60)?
            .checked_mul(US_PER_S)?
            .checked_add(seconds_us(parts.get(last)?)?)?
    } else {
        let (body, scale) = if let Some(b) = rest.strip_suffix("ms") {
            (b, 1_000i128)
        } else if let Some(b) = rest.strip_suffix("us") {
            (b, 1i128)
        } else if let Some(b) = rest.strip_suffix('s') {
            (b, US_PER_S)
        } else {
            (rest, US_PER_S)
        };
        scaled_us(body, scale)?
    };

    let us = if neg { -us } else { us };
    i64::try_from(us).ok().map(Duration)
}

/// Parse a bare seconds column (`"56.789"`) into microseconds.
fn seconds_us(s: &str) -> Option<i128> {
    scaled_us(s, US_PER_S)
}

/// Parse a decimal into `unit`-scaled microseconds without going through `f64`,
/// so the result is exact for every representable input.
fn scaled_us(s: &str, unit_us: i128) -> Option<i128> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    let (int_part, frac_part) = s.split_once('.').unwrap_or((s, ""));
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    let whole: i128 = if int_part.is_empty() {
        0
    } else {
        int_part.parse().ok()?
    };
    let mut total = whole.checked_mul(unit_us)?;
    if !frac_part.is_empty() {
        // Scale the fraction by `unit_us` exactly: value = frac / 10^len * unit.
        let mut scale = unit_us;
        for c in frac_part.chars() {
            let d = i128::from(c.to_digit(10)?);
            scale = scale.checked_div(10)?;
            total = total.checked_add(d.checked_mul(scale)?)?;
            if scale == 0 {
                break;
            }
        }
    }
    Some(total)
}

/// The canonical rendering: signed seconds with exactly six decimals.
///
/// Chosen over `HH:MM:SS.ffffff` because it round-trips for the whole `i64`
/// range without an hour column that would overflow on the way back in.
#[must_use]
pub fn format_duration(d: Duration) -> String {
    let n = i128::from(d.0);
    let neg = n < 0;
    let a = n.abs();
    let secs = a.div_euclid(US_PER_S);
    let frac = a.rem_euclid(US_PER_S);
    format!("{}{secs}.{frac:06}", if neg { "-" } else { "" })
}

// ---------------------------------------------------------------- colour

/// A subset of the X11/SVG colour names. These are interface facts; the full
/// ~140-entry table belongs in `vaco_core::parse::color`.
const COLORS: &[(&str, u32)] = &[
    ("aliceblue", 0x00f0_f8ff),
    ("aqua", 0x0000_ffff),
    ("aquamarine", 0x007f_ffd4),
    ("beige", 0x00f5_f5dc),
    ("black", 0x0000_0000),
    ("blue", 0x0000_00ff),
    ("brown", 0x00a5_2a2a),
    ("chartreuse", 0x007f_ff00),
    ("chocolate", 0x00d2_691e),
    ("coral", 0x00ff_7f50),
    ("crimson", 0x00dc_143c),
    ("cyan", 0x0000_ffff),
    ("darkblue", 0x0000_008b),
    ("darkgreen", 0x0000_6400),
    ("darkgray", 0x00a9_a9a9),
    ("darkred", 0x008b_0000),
    ("fuchsia", 0x00ff_00ff),
    ("gold", 0x00ff_d700),
    ("gray", 0x0080_8080),
    ("green", 0x0000_8000),
    ("indigo", 0x004b_0082),
    ("ivory", 0x00ff_fff0),
    ("khaki", 0x00f0_e68c),
    ("lavender", 0x00e6_e6fa),
    ("lime", 0x0000_ff00),
    ("magenta", 0x00ff_00ff),
    ("maroon", 0x0080_0000),
    ("navy", 0x0000_0080),
    ("olive", 0x0080_8000),
    ("orange", 0x00ff_a500),
    ("orangered", 0x00ff_4500),
    ("orchid", 0x00da_70d6),
    ("pink", 0x00ff_c0cb),
    ("purple", 0x0080_0080),
    ("red", 0x00ff_0000),
    ("salmon", 0x00fa_8072),
    ("silver", 0x00c0_c0c0),
    ("skyblue", 0x0087_ceeb),
    ("teal", 0x0000_8080),
    ("tomato", 0x00ff_6347),
    ("turquoise", 0x0040_e0d0),
    ("violet", 0x00ee_82ee),
    ("wheat", 0x00f5_deb3),
    ("white", 0x00ff_ffff),
    ("yellow", 0x00ff_ff00),
];

/// `"#rrggbb[aa]"`, `"0xRRGGBB[AA]"`, a colour name, or `"name@0.5"`.
///
/// `"random"` is not supported: it needs an RNG, which this crate has no
/// business owning. See the docs.
#[must_use]
pub fn color(s: &str) -> Option<Rgba> {
    let (body, alpha) = match s.split_once('@') {
        Some((b, a)) => (b, Some(a)),
        None => (s, None),
    };
    let body = body.trim();
    let mut rgba = if let Some(hex) = body
        .strip_prefix('#')
        .or_else(|| body.strip_prefix("0x"))
        .or_else(|| body.strip_prefix("0X"))
    {
        parse_hex_color(hex)?
    } else {
        let v = COLORS
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(body))
            .map(|(_, v)| *v)?;
        Rgba::new(
            ((v >> 16) & 0xff) as u8,
            ((v >> 8) & 0xff) as u8,
            (v & 0xff) as u8,
            0xff,
        )
    };
    if let Some(a) = alpha {
        rgba.a = parse_alpha(a.trim())?;
    }
    Some(rgba)
}

fn parse_hex_color(hex: &str) -> Option<Rgba> {
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    match hex.len() {
        6 => Some(Rgba::new(
            ((v >> 16) & 0xff) as u8,
            ((v >> 8) & 0xff) as u8,
            (v & 0xff) as u8,
            0xff,
        )),
        8 => Some(Rgba::new(
            ((v >> 24) & 0xff) as u8,
            ((v >> 16) & 0xff) as u8,
            ((v >> 8) & 0xff) as u8,
            (v & 0xff) as u8,
        )),
        _ => None,
    }
}

fn parse_alpha(s: &str) -> Option<u8> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u8::from_str_radix(hex, 16).ok();
    }
    let f: f64 = s.parse().ok()?;
    if !(0.0..=1.0).contains(&f) {
        return None;
    }
    Some((f * 255.0).round() as u8)
}

/// The canonical rendering: `0xRRGGBBAA`.
#[must_use]
pub fn format_color(c: Rgba) -> String {
    format!("0x{:02x}{:02x}{:02x}{:02x}", c.r, c.g, c.b, c.a)
}

/// The colour-name table, for help output and tests.
pub fn color_names() -> impl Iterator<Item = &'static str> {
    COLORS.iter().map(|(n, _)| *n)
}

// ---------------------------------------------------------------- booleans

/// The boolean spellings the CLI accepts.
#[must_use]
pub fn boolean(s: &str) -> Option<bool> {
    match s {
        "1" | "true" | "on" | "yes" | "enable" | "enabled" => Some(true),
        "0" | "false" | "off" | "no" | "disable" | "disabled" => Some(false),
        _ => None,
    }
}

// ---------------------------------------------------------------- binary

/// Lower-case hex, no separators.
#[must_use]
pub fn format_binary(bytes: &[u8]) -> String {
    let mut out = String::new();
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Hex in either case; an odd digit count is an error.
#[must_use]
pub fn binary(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    for pair in bytes.chunks_exact(2) {
        let hi = char::from(*pair.first()?).to_digit(16)?;
        let lo = char::from(*pair.get(1)?).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}
