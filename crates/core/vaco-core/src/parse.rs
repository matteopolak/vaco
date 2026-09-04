//! The `parseutils` family: the CLI's user-facing value grammars.
//!
//! # What it is
//!
//! One parser per CLI value syntax — `-s 1920x1080`, `-r 30000/1001`,
//! `-t 00:01:30.5`, `-fill_color red@0.5` — plus the formatter that inverts it.
//! These grammars are a compatibility contract: a command line written against
//! the reference tool has to mean the same thing here, so the accepted forms
//! follow `planning/research/05-fftools-cli.md` §5.6 rather than anyone's taste.
//!
//! # How it works
//!
//! Every parser returns `Option`, never a panic and never a partial parse:
//! trailing junk is a rejection, not something to ignore. Every parser has a
//! `format_*` counterpart, and the pair is property-tested to round-trip. Where
//! a grammar has several spellings of one value the formatter picks one
//! canonical spelling, so `format(parse(s))` normalises but
//! `parse(format(v)) == v` holds exactly.
//!
//! Nothing here goes through `f64` where it can be avoided. [`duration`] scales
//! its fractional part by repeated integer division so that `0.1s` is exactly
//! 100 000 µs rather than 99 999.
//!
//! # Grammars
//!
//! | Function | Accepted |
//! |---|---|
//! | [`image_size`] | `W<sep>H` for any single-byte `<sep>`, or one of [`image_size_names`] |
//! | [`video_rate`] | `num/den`, `num:den`, an integer, a decimal, or one of [`video_rate_names`] |
//! | [`rational`] | `num/den`, `num:den`, an integer, or a decimal (approximated, `max_den` 10^6) |
//! | [`duration`] | `[-][HH:]MM:SS[.m…]` or `[-]S+[.m…][s\|ms\|us]` |
//! | [`color`] | `#RRGGBB[AA]`, `0xRRGGBB[AA]`, a name from [`color_names`], `random`, any of those with `@alpha` |
//! | [`boolean`] | `1/true/on/yes/enable/enabled` and their negatives |
//! | [`binary`] | an even number of hex digits, either case |

use core::fmt::Write as _;

use crate::{Duration, Rational};

// ---------------------------------------------------------------- vocabulary

/// An 8-bit-per-channel colour with alpha. The `color` option base.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Rgba {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
    /// Alpha; 255 is opaque.
    pub a: u8,
}

impl Rgba {
    /// Fully transparent black.
    pub const TRANSPARENT: Self = Self::new(0, 0, 0, 0);
    /// Opaque black.
    pub const BLACK: Self = Self::new(0, 0, 0, 0xff);
    /// Opaque white.
    pub const WHITE: Self = Self::new(0xff, 0xff, 0xff, 0xff);

    /// From components.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

/// `0xRRGGBBAA`, the canonical rendering — the same text [`format_color`] emits.
impl core::fmt::Display for Rgba {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "0x{:02x}{:02x}{:02x}{:02x}",
            self.r, self.g, self.b, self.a
        )
    }
}

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

/// `"1920x1080"`, or one of the abbreviations in [`image_size_names`]
/// (matched exactly, so `VGA` is not `vga`).
///
/// Otherwise: a decimal width, **one** separator byte, a decimal height, and
/// then end of string. Both dimensions must be strictly positive.
///
/// # D17: the separator is any single byte, not `x`
///
/// The reference does not look for an `x`. It runs `strtol` for the width,
/// skips exactly one byte if any remain, runs `strtol` for the height, and
/// requires the string to be exhausted. So `320-240`, `320 240`, `320,240` and
/// `320+240` all parse as 320x240, and are accepted on real command lines.
/// `320240` is rejected — not for lacking a separator, but because the first
/// `strtol` eats the lot and the height comes out 0.
///
/// `strtol` also skips leading whitespace and takes a sign, which is why
/// `" 320x240"` is accepted but `"320x240 "` is not, and why the sign is
/// caught by the positivity check rather than by the grammar.
///
/// # D17: out-of-range dimensions wrap rather than fail
///
/// The reference stores `strtol`'s `long` into an `int`. On every target that
/// matters that is a wrapping truncation, and the *truncated* value is what
/// gets range-checked. `4294967297x240` is therefore accepted as `1x240`, and
/// `4294967296x240` is rejected for being 0 — not for being too large.
///
/// Both of these are reproduced deliberately. They decide which command lines
/// are accepted, so "fixing" them would diverge our CLI from the reference.
/// Read D17 before changing either.
#[must_use]
pub fn image_size(s: &str) -> Option<(u32, u32)> {
    if let Some(&(_, w, h)) = SIZES.iter().find(|(n, _, _)| *n == s) {
        return Some((w, h));
    }
    // Byte-wise throughout: the reference advances one *byte* past the
    // separator, which for a multi-byte character lands mid-sequence. Slicing
    // a `&str` there would panic, and the leftover continuation byte is not a
    // digit, so the height parse fails and the whole thing is rejected anyway.
    let (w, rest) = strtol10(s.as_bytes());
    let rest = rest.get(1..).unwrap_or(rest);
    let (h, rest) = strtol10(rest);
    if !rest.is_empty() {
        return None;
    }
    // The truncation is the reference's, and it happens before the check.
    let (w, h) = (w as i32, h as i32);
    (w > 0 && h > 0).then_some((w as u32, h as u32))
}

/// `strtol(s, &end, 10)`: leading ASCII whitespace, one optional sign, then
/// decimal digits, saturating at the `i64` bounds on overflow.
///
/// Returns the value and the unconsumed tail. When no digits are present the
/// value is 0 and the tail is the *whole* input, whitespace included — C leaves
/// `endptr` at the original pointer on a failed conversion, and callers that
/// step over a separator byte can see the difference.
fn strtol10(b: &[u8]) -> (i64, &[u8]) {
    let ws = b.iter().take_while(|c| c.is_ascii_whitespace()).count();
    let mut rest = b.get(ws..).unwrap_or_default();
    let neg = match rest.first() {
        Some(b'-') => {
            rest = rest.get(1..).unwrap_or_default();
            true
        }
        Some(b'+') => {
            rest = rest.get(1..).unwrap_or_default();
            false
        }
        _ => false,
    };
    let ndigits = rest.iter().take_while(|c| c.is_ascii_digit()).count();
    let (digits, tail) = rest.split_at_checked(ndigits).unwrap_or((rest, &[]));
    if digits.is_empty() {
        // No conversion: C leaves `endptr` at the original pointer, whitespace
        // and sign included. A caller stepping over a separator byte can tell.
        return (0, b);
    }
    let mag = digits.iter().fold(0_u64, |a, d| {
        a.saturating_mul(10).saturating_add(u64::from(d - b'0'))
    });
    // `strtol` clamps to LONG_MAX / LONG_MIN rather than failing.
    let v = if neg {
        if mag > i64::MIN.unsigned_abs() {
            i64::MIN
        } else {
            mag.cast_signed().wrapping_neg()
        }
    } else if mag > i64::MAX.cast_unsigned() {
        i64::MAX
    } else {
        mag.cast_signed()
    };
    (v, tail)
}

/// The abbreviation table, for help output and tests.
pub fn image_size_names() -> impl Iterator<Item = &'static str> {
    SIZES.iter().map(|(n, _, _)| *n)
}

/// The canonical rendering: `WxH`.
#[must_use]
pub fn format_image_size(w: u32, h: u32) -> String {
    format!("{w}x{h}")
}

// ---------------------------------------------------------------- video rate

/// Standard frame-rate abbreviations, per research 05 §5.6.
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
///
/// The abbreviations are checked first, so `film` is 24/1 and never a
/// numeric parse.
///
/// **Only a strictly positive, finite rate is accepted.** `rational` parses
/// zero, negative and infinite ratios and documents that filtering them is the
/// caller's job, because some options legitimately accept them — and this is
/// that caller. A frame rate of zero or infinity is not a rate.
///
/// Verified against the reference, which rejects every one of `0`, `0/0`,
/// `0/5`, `-25` and `1/0` with "Unable to parse … as video rate". A fuzz target
/// found us returning `Some(0/0)` for `"00:0"`, which is indistinguishable from
/// our "unknown" sentinel and would have propagated as a silently undefined
/// frame rate.
#[must_use]
pub fn video_rate(s: &str) -> Option<Rational> {
    if let Some(&(_, n, d)) = RATES.iter().find(|(n, _, _)| *n == s) {
        return Some(Rational::new(n, d));
    }
    let r = rational(s)?;
    (r.num > 0 && r.den > 0).then_some(r)
}

/// The frame-rate abbreviation table, for help output and tests.
pub fn video_rate_names() -> impl Iterator<Item = &'static str> {
    RATES.iter().map(|(n, _, _)| *n)
}

/// `"num:den"`, or **any expression**, evaluated and then approximated.
///
/// Negative values parse; filtering those is the caller's job, since some
/// options accept them.
///
/// `1/0` does **not** parse, contrary to research 05 §5.6, which recorded an
/// infinite ratio as valid. Probing says otherwise — `-aspect 1/0` gives
/// "Invalid aspect ratio" and `-r 1/0` gives "Invalid framerate value". It is
/// not a special case here either: `/` is division, so `1/0` is an infinity,
/// and an infinity has no rational approximation. If an option is ever found
/// that *does* accept an infinite ratio, this is the note to revisit — both
/// sites reachable today reject it.
///
/// # `/` is division, not a separator
///
/// The reference's ratio grammar has two branches, and the second is the whole
/// expression language — so `/` means division and binds like division. Probed
/// with cases chosen to tell the two readings apart:
///
/// | input | evaluates to | as a ratio |
/// |---|---|---|
/// | `1/2/2` | 0.25 | `1:4` |
/// | `4/2*3` | 6 | `6:1` |
/// | `8/2/2` | 2 | `2:1` |
/// | `2*3/4` | 1.5 | `3:2` |
/// | `sqrt(4)` | 2 | `2:1` |
///
/// A split-on-the-first-`/` reading predicts `2:3` for `4/2*3`; the reference
/// gives `6:1`. Anything that treated `/` as a separator would be wrong on every
/// row but the last.
///
/// The `:` form is the *first* branch and is not expression-backed: `3:2` and
/// `16:9` work, while `1:2/2` falls through to the evaluator and is rejected
/// with `Invalid chars ':2/2'`. So a colon is matched only when it spans the
/// whole string as `int:int`.
///
/// Exactness survives the round trip where it matters — `30000/1001` evaluates
/// to 29.97002997… and approximates back to exactly `30000/1001`, because that
/// is the best rational with denominator ≤ 10⁶.
#[must_use]
pub fn rational(s: &str) -> Option<Rational> {
    // Branch one: `int:int`, whole string. Not expression-backed.
    if let Some((n, d)) = s.split_once(':')
        && let (Ok(n), Ok(d)) = (n.trim().parse::<i32>(), d.trim().parse::<i32>())
    {
        return Some(Rational::new(n, d));
    }
    // An integer is exact, and going through f64 would lose precision past 2^53
    // for no reason. `i32::MAX` is well inside f64, but the exact path is also
    // the fast one and keeps `5` meaning exactly `5/1`.
    if let Ok(n) = s.trim().parse::<i32>() {
        return Some(Rational::new(n, 1));
    }
    // Branch two: the whole string is an expression, with no variables bound —
    // the ratio grammar has no `w`/`n`/`t` in scope, unlike a filter argument.
    let expr = vaco_expr::Expr::parse(s, &vaco_expr::Bindings::new(&[])).ok()?;
    approximate(expr.eval(&[]), 1_000_000)
}

/// Best rational approximation of `value` with a bounded denominator.
///
/// A thin `Option`-returning wrapper over [`Rational::approximate`]: `None`
/// only for a non-finite `value`, where a ratio would be a lie rather than an
/// approximation.
#[must_use]
pub fn approximate(value: f64, max_den: i64) -> Option<Rational> {
    if !value.is_finite() {
        return None;
    }
    let max_den = i32::try_from(max_den.clamp(1, i64::from(i32::MAX))).ok()?;
    Some(Rational::approximate(value, max_den))
}

/// `num/den`, the canonical rendering — as stored, not reduced.
#[must_use]
pub fn format_rational(r: Rational) -> String {
    format!("{}/{}", r.num, r.den)
}

// ---------------------------------------------------------------- duration

const US_PER_S: i128 = 1_000_000;

/// `"12:34:56.789"`, `"-1:02.5"`, `"1234.5"`, `"5ms"`, `"2s"`. Microseconds.
///
/// Two shapes, per research 05 §5.6: `[-][HH:]MM:SS[.m…]` — at most three
/// colon-separated columns, only the last of which may carry a fraction — or
/// `[-]S+[.m…][s|ms|us]`, where a bare number means seconds.
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
    i64::try_from(us).ok().map(Duration::from_micros)
}

/// Parse a bare seconds column (`"56.789"`) into microseconds.
fn seconds_us(s: &str) -> Option<i128> {
    scaled_us(s, US_PER_S)
}

/// Parse a decimal into `unit`-scaled microseconds without going through `f64`,
/// so the result is exact for every representable input.
#[allow(
    clippy::integer_division,
    reason = "the divisor is the literal 10; this is decimal place-value, not a computed quotient"
)]
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
            scale /= 10;
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
#[allow(
    clippy::integer_division,
    reason = "div_euclid/rem_euclid against the constant 10^6, splitting seconds from microseconds"
)]
pub fn format_duration(d: Duration) -> String {
    let n = i128::from(d.as_micros());
    let neg = n < 0;
    let a = n.abs();
    let secs = a.div_euclid(US_PER_S);
    let frac = a.rem_euclid(US_PER_S);
    format!("{}{secs}.{frac:06}", if neg { "-" } else { "" })
}

/// `HH:MM:SS.ffffff`, the clock rendering. Not the round-trip form: hours are
/// unbounded here, and `format_duration` is what [`duration`] inverts exactly.
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "sexagesimal decomposition against compile-time constants"
)]
#[allow(
    clippy::many_single_char_names,
    reason = "h, m and s are hours, minutes and seconds"
)]
pub fn format_duration_clock(d: Duration) -> String {
    let n = i128::from(d.as_micros());
    let neg = n < 0;
    let a = n.abs();
    let total_secs = a.div_euclid(US_PER_S);
    let frac = a.rem_euclid(US_PER_S);
    let (h, m, s) = (
        total_secs.div_euclid(3600),
        total_secs.div_euclid(60).rem_euclid(60),
        total_secs.rem_euclid(60),
    );
    format!(
        "{}{h:02}:{m:02}:{s:02}.{frac:06}",
        if neg { "-" } else { "" }
    )
}

// ---------------------------------------------------------------- colour

/// The X11/SVG named-colour set: 147 entries, the union of the `gray` and
/// `grey` spellings.
///
/// Interface facts (D9), not authored data — a command line that says
/// `-fill_color papayawhip` has to mean the same thing here as it does to the
/// reference tool. Sorted by name; matched case-insensitively.
const COLORS: &[(&str, u8, u8, u8)] = &[
    ("aliceblue", 0xf0, 0xf8, 0xff),
    ("antiquewhite", 0xfa, 0xeb, 0xd7),
    ("aqua", 0x00, 0xff, 0xff),
    ("aquamarine", 0x7f, 0xff, 0xd4),
    ("azure", 0xf0, 0xff, 0xff),
    ("beige", 0xf5, 0xf5, 0xdc),
    ("bisque", 0xff, 0xe4, 0xc4),
    ("black", 0x00, 0x00, 0x00),
    ("blanchedalmond", 0xff, 0xeb, 0xcd),
    ("blue", 0x00, 0x00, 0xff),
    ("blueviolet", 0x8a, 0x2b, 0xe2),
    ("brown", 0xa5, 0x2a, 0x2a),
    ("burlywood", 0xde, 0xb8, 0x87),
    ("cadetblue", 0x5f, 0x9e, 0xa0),
    ("chartreuse", 0x7f, 0xff, 0x00),
    ("chocolate", 0xd2, 0x69, 0x1e),
    ("coral", 0xff, 0x7f, 0x50),
    ("cornflowerblue", 0x64, 0x95, 0xed),
    ("cornsilk", 0xff, 0xf8, 0xdc),
    ("crimson", 0xdc, 0x14, 0x3c),
    ("cyan", 0x00, 0xff, 0xff),
    ("darkblue", 0x00, 0x00, 0x8b),
    ("darkcyan", 0x00, 0x8b, 0x8b),
    ("darkgoldenrod", 0xb8, 0x86, 0x0b),
    ("darkgray", 0xa9, 0xa9, 0xa9),
    ("darkgreen", 0x00, 0x64, 0x00),
    ("darkgrey", 0xa9, 0xa9, 0xa9),
    ("darkkhaki", 0xbd, 0xb7, 0x6b),
    ("darkmagenta", 0x8b, 0x00, 0x8b),
    ("darkolivegreen", 0x55, 0x6b, 0x2f),
    ("darkorange", 0xff, 0x8c, 0x00),
    ("darkorchid", 0x99, 0x32, 0xcc),
    ("darkred", 0x8b, 0x00, 0x00),
    ("darksalmon", 0xe9, 0x96, 0x7a),
    ("darkseagreen", 0x8f, 0xbc, 0x8f),
    ("darkslateblue", 0x48, 0x3d, 0x8b),
    ("darkslategray", 0x2f, 0x4f, 0x4f),
    ("darkslategrey", 0x2f, 0x4f, 0x4f),
    ("darkturquoise", 0x00, 0xce, 0xd1),
    ("darkviolet", 0x94, 0x00, 0xd3),
    ("deeppink", 0xff, 0x14, 0x93),
    ("deepskyblue", 0x00, 0xbf, 0xff),
    ("dimgray", 0x69, 0x69, 0x69),
    ("dimgrey", 0x69, 0x69, 0x69),
    ("dodgerblue", 0x1e, 0x90, 0xff),
    ("firebrick", 0xb2, 0x22, 0x22),
    ("floralwhite", 0xff, 0xfa, 0xf0),
    ("forestgreen", 0x22, 0x8b, 0x22),
    ("fuchsia", 0xff, 0x00, 0xff),
    ("gainsboro", 0xdc, 0xdc, 0xdc),
    ("ghostwhite", 0xf8, 0xf8, 0xff),
    ("gold", 0xff, 0xd7, 0x00),
    ("goldenrod", 0xda, 0xa5, 0x20),
    ("gray", 0x80, 0x80, 0x80),
    ("green", 0x00, 0x80, 0x00),
    ("greenyellow", 0xad, 0xff, 0x2f),
    ("grey", 0x80, 0x80, 0x80),
    ("honeydew", 0xf0, 0xff, 0xf0),
    ("hotpink", 0xff, 0x69, 0xb4),
    ("indianred", 0xcd, 0x5c, 0x5c),
    ("indigo", 0x4b, 0x00, 0x82),
    ("ivory", 0xff, 0xff, 0xf0),
    ("khaki", 0xf0, 0xe6, 0x8c),
    ("lavender", 0xe6, 0xe6, 0xfa),
    ("lavenderblush", 0xff, 0xf0, 0xf5),
    ("lawngreen", 0x7c, 0xfc, 0x00),
    ("lemonchiffon", 0xff, 0xfa, 0xcd),
    ("lightblue", 0xad, 0xd8, 0xe6),
    ("lightcoral", 0xf0, 0x80, 0x80),
    ("lightcyan", 0xe0, 0xff, 0xff),
    ("lightgoldenrodyellow", 0xfa, 0xfa, 0xd2),
    ("lightgray", 0xd3, 0xd3, 0xd3),
    ("lightgreen", 0x90, 0xee, 0x90),
    ("lightgrey", 0xd3, 0xd3, 0xd3),
    ("lightpink", 0xff, 0xb6, 0xc1),
    ("lightsalmon", 0xff, 0xa0, 0x7a),
    ("lightseagreen", 0x20, 0xb2, 0xaa),
    ("lightskyblue", 0x87, 0xce, 0xfa),
    ("lightslategray", 0x77, 0x88, 0x99),
    ("lightslategrey", 0x77, 0x88, 0x99),
    ("lightsteelblue", 0xb0, 0xc4, 0xde),
    ("lightyellow", 0xff, 0xff, 0xe0),
    ("lime", 0x00, 0xff, 0x00),
    ("limegreen", 0x32, 0xcd, 0x32),
    ("linen", 0xfa, 0xf0, 0xe6),
    ("magenta", 0xff, 0x00, 0xff),
    ("maroon", 0x80, 0x00, 0x00),
    ("mediumaquamarine", 0x66, 0xcd, 0xaa),
    ("mediumblue", 0x00, 0x00, 0xcd),
    ("mediumorchid", 0xba, 0x55, 0xd3),
    // D17: SVG 1.1 / CSS Color 3 define #9370DB. FFmpeg 8.1 emits #9370D8 — a
    // `db`->`d8` transposition. We reproduce the reference's value so that
    // `-fill_color mediumpurple` paints identical pixels in both programs (D6).
    // Do NOT "correct" this to the standard's value without reading D17.
    ("mediumpurple", 0x93, 0x70, 0xd8),
    ("mediumseagreen", 0x3c, 0xb3, 0x71),
    ("mediumslateblue", 0x7b, 0x68, 0xee),
    ("mediumspringgreen", 0x00, 0xfa, 0x9a),
    ("mediumturquoise", 0x48, 0xd1, 0xcc),
    ("mediumvioletred", 0xc7, 0x15, 0x85),
    ("midnightblue", 0x19, 0x19, 0x70),
    ("mintcream", 0xf5, 0xff, 0xfa),
    ("mistyrose", 0xff, 0xe4, 0xe1),
    ("moccasin", 0xff, 0xe4, 0xb5),
    ("navajowhite", 0xff, 0xde, 0xad),
    ("navy", 0x00, 0x00, 0x80),
    ("oldlace", 0xfd, 0xf5, 0xe6),
    ("olive", 0x80, 0x80, 0x00),
    ("olivedrab", 0x6b, 0x8e, 0x23),
    ("orange", 0xff, 0xa5, 0x00),
    ("orangered", 0xff, 0x45, 0x00),
    ("orchid", 0xda, 0x70, 0xd6),
    ("palegoldenrod", 0xee, 0xe8, 0xaa),
    ("palegreen", 0x98, 0xfb, 0x98),
    ("paleturquoise", 0xaf, 0xee, 0xee),
    // D17: SVG 1.1 / CSS Color 3 define #DB7093. FFmpeg 8.1 emits #D87093 — the
    // same transposition as `mediumpurple`. Reference value reproduced for D6.
    ("palevioletred", 0xd8, 0x70, 0x93),
    ("papayawhip", 0xff, 0xef, 0xd5),
    ("peachpuff", 0xff, 0xda, 0xb9),
    ("peru", 0xcd, 0x85, 0x3f),
    ("pink", 0xff, 0xc0, 0xcb),
    ("plum", 0xdd, 0xa0, 0xdd),
    ("powderblue", 0xb0, 0xe0, 0xe6),
    ("purple", 0x80, 0x00, 0x80),
    ("red", 0xff, 0x00, 0x00),
    ("rosybrown", 0xbc, 0x8f, 0x8f),
    ("royalblue", 0x41, 0x69, 0xe1),
    ("saddlebrown", 0x8b, 0x45, 0x13),
    ("salmon", 0xfa, 0x80, 0x72),
    ("sandybrown", 0xf4, 0xa4, 0x60),
    ("seagreen", 0x2e, 0x8b, 0x57),
    ("seashell", 0xff, 0xf5, 0xee),
    ("sienna", 0xa0, 0x52, 0x2d),
    ("silver", 0xc0, 0xc0, 0xc0),
    ("skyblue", 0x87, 0xce, 0xeb),
    ("slateblue", 0x6a, 0x5a, 0xcd),
    ("slategray", 0x70, 0x80, 0x90),
    ("slategrey", 0x70, 0x80, 0x90),
    ("snow", 0xff, 0xfa, 0xfa),
    ("springgreen", 0x00, 0xff, 0x7f),
    ("steelblue", 0x46, 0x82, 0xb4),
    ("tan", 0xd2, 0xb4, 0x8c),
    ("teal", 0x00, 0x80, 0x80),
    ("thistle", 0xd8, 0xbf, 0xd8),
    ("tomato", 0xff, 0x63, 0x47),
    ("turquoise", 0x40, 0xe0, 0xd0),
    ("violet", 0xee, 0x82, 0xee),
    ("wheat", 0xf5, 0xde, 0xb3),
    ("white", 0xff, 0xff, 0xff),
    ("whitesmoke", 0xf5, 0xf5, 0xf5),
    ("yellow", 0xff, 0xff, 0x00),
    ("yellowgreen", 0x9a, 0xcd, 0x32),
];

/// `"#rrggbb[aa]"`, `"0xRRGGBB[AA]"`, a colour name, `"name@0.5"`, or
/// `"random"`.
///
/// The optional `@alpha` suffix is either a float in `0.0..=1.0` or a
/// `0x`-prefixed byte. `random` draws fresh RGB on every call with alpha 255,
/// so it is the one input that does not round-trip through [`format_color`].
#[must_use]
pub fn color(s: &str) -> Option<Rgba> {
    let (body, alpha) = match s.split_once('@') {
        Some((b, a)) => (b, Some(a)),
        None => (s, None),
    };
    let body = body.trim();
    let mut rgba = if body.eq_ignore_ascii_case("random") {
        random_rgb()
    } else if let Some(hex) = body
        .strip_prefix('#')
        .or_else(|| body.strip_prefix("0x"))
        .or_else(|| body.strip_prefix("0X"))
    {
        parse_hex_color(hex)?
    } else {
        let &(_, r, g, b) = COLORS.iter().find(|(n, ..)| n.eq_ignore_ascii_case(body))?;
        Rgba::new(r, g, b, 0xff)
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

/// The colour-name table, for help output and tests. Does not include
/// `random`, which is a generator rather than a name.
pub fn color_names() -> impl Iterator<Item = &'static str> {
    COLORS.iter().map(|(n, ..)| *n)
}

/// Look a name up without the rest of the [`color`] grammar.
#[must_use]
pub fn color_by_name(name: &str) -> Option<Rgba> {
    COLORS
        .iter()
        .find(|(n, ..)| n.eq_ignore_ascii_case(name))
        .map(|&(_, r, g, b)| Rgba::new(r, g, b, 0xff))
}

/// A fresh opaque colour, for `color=random`.
///
/// `SplitMix64` over an atomic counter seeded from the wall clock. It is a
/// *decorative* generator — a placeholder fill, a debug overlay — and is
/// deliberately not offered as a general random source, so no cryptographic or
/// statistical claim attaches to it.
fn random_rgb() -> Rgba {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: OnceLock<AtomicU64> = OnceLock::new();
    const GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

    let counter = SEQ.get_or_init(|| {
        // Via `vaco-time` so this compiles for wasm, where there may be no
        // wall clock at all. The fallback constant is then the seed for the
        // whole process — acceptable only because this function is documented
        // as carrying no statistical claim.
        let seed = vaco_time::unix_nanos().map_or(0x1234_5678_9ABC_DEF0, |n| n as u64);
        AtomicU64::new(seed)
    });
    let mut z = counter
        .fetch_add(GAMMA, Ordering::Relaxed)
        .wrapping_add(GAMMA);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    Rgba::new(z as u8, (z >> 8) as u8, (z >> 16) as u8, 0xff)
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

/// The canonical rendering: `true` / `false`.
#[must_use]
pub const fn format_boolean(b: bool) -> &'static str {
    if b { "true" } else { "false" }
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
    for &[hi_b, lo_b] in bytes.as_chunks::<2>().0 {
        let hi = char::from(hi_b).to_digit(16)?;
        let lo = char::from(lo_b).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}
