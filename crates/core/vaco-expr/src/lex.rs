//! Lexical layer: whitespace removal, number scanning, identifier matching.
//!
//! Every rule here was established by probing the reference binary; the probe
//! harness is documented in `docs/core/vaco-expr.md`.

/// `log2(10)`, the constant the reference's decibel conversion is built from.
///
/// `20dB` evaluates to `9.999999999999998`, not `10`. That is the signature of
/// `exp2(LOG2_10 * x / 20)` rather than `pow(10, x / 20)` — `pow(10, 1.0)` is
/// exactly `10.0` on every libm we can test. Verified against `20dB`, `6dB`,
/// `-20dB` and `0dB`. See [`from_decibels`].
pub(crate) const LOG2_10: f64 = core::f64::consts::LOG2_10;

/// Removes every whitespace byte from `src`.
///
/// # D17: whitespace is deleted, not skipped
///
/// Conventionally a tokeniser skips whitespace *between* tokens. The reference
/// strips it from the whole string before parsing, so it also disappears from
/// the *middle* of tokens:
///
/// - `"1 2"` parses as the single number `12`, not as two tokens.
/// - `"m a x ( 1 , 2 )"` parses as `max(1,2)` and evaluates to `2`.
/// - `"s in(0)"` parses as `sin(0)`.
///
/// Both were confirmed against the reference. This decides which command lines
/// are accepted, so it must not be "corrected" into ordinary token skipping.
/// The byte set is C's `isspace`: space, `\t`, `\n`, `\v`, `\f`, `\r`.
#[must_use]
pub fn strip_whitespace(src: &str) -> String {
    src.chars().filter(|c| !is_c_space(*c)).collect()
}

const fn is_c_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r')
}

/// True if `name` matches a prefix of `s` and the character after it cannot
/// continue an identifier.
///
/// # D17: names are matched by prefix, not by token
///
/// A conventional lexer reads a whole identifier and compares it. The reference
/// tests each candidate name against the head of the remaining input and only
/// requires that the following byte is not `[A-Za-z0-9_]`. Two observable
/// consequences, both confirmed:
///
/// - `PI(1)` is rejected with *trailing garbage* (`(1)`), not "unknown
///   function": the constant `PI` matched first and consumed two bytes.
/// - `abs.(1)` evaluates to `1`, because the collected function name `abs.`
///   still matches the builtin `abs` under this rule — while `abs_(1)` does
///   not, because `_` can continue an identifier.
#[must_use]
pub fn strmatch(s: &str, name: &str) -> bool {
    let Some(rest) = s.strip_prefix(name) else {
        return false;
    };
    !rest.starts_with(is_ident_continue)
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// A number literal, as scanned from the head of `s`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Number {
    /// The value, with the SI prefix, `B` and `dB` suffixes already applied.
    pub value: f64,
    /// How many bytes of `s` the literal consumed.
    pub len: usize,
}

/// Scans a numeric literal from the head of `s`.
///
/// Returns `None` when `s` does not begin with one. The grammar is the
/// reference's, which is *not* C's `strtod` in one respect: hexadecimal is
/// integer-only (`strtoull`), so `0x1p4` is not a hex float — it scans as
/// `0x1`, then the SI prefix `p`, leaving `4` as trailing garbage. Confirmed.
#[must_use]
pub fn scan_number(s: &str) -> Option<Number> {
    let (value, mut len) = scan_mantissa(s)?;
    let rest = s.get(len..)?;

    // `dB` is tested before the SI prefixes, because `d` is itself a prefix
    // (deci) and `B` is itself a suffix (times eight). The reference resolves
    // the ambiguity in favour of decibels: `-2dB` is 0.794..., not -1.6.
    if rest.starts_with("dB") {
        return Some(Number {
            value: from_decibels(value),
            len: len + 2,
        });
    }

    let mut value = value;
    let mut rest = rest;
    if let Some(c) = rest.chars().next()
        && let Some((dec, bin)) = si_prefix(c)
    {
        // `i` after the prefix selects the binary value; `2ki` is 2048.
        // `i` on its own is not a prefix: `2i` is rejected.
        let after = rest.get(c.len_utf8()..).unwrap_or("");
        if let Some(after_i) = after.strip_prefix('i') {
            value *= bin;
            len += c.len_utf8() + 1;
            rest = after_i;
        } else {
            value *= dec;
            len += c.len_utf8();
            rest = after;
        }
    }

    // `B` multiplies by eight and must come last: `2Bk` and `2kBB` are both
    // rejected by the reference.
    if let Some(after_b) = rest.strip_prefix('B') {
        let _ = after_b;
        value *= 8.0;
        len += 1;
    }

    Some(Number { value, len })
}

/// Converts a decibel value to a linear gain.
///
/// `x` dB is `exp2(log2(10) * x / 20)`. It is **not** `pow(10, x/20)`: the
/// literal `20dB` comes back from the reference as `9.999999999999998`, and
/// `pow(10, 1.0)` is exactly `10.0` on every libm available to test. `exp2` of
/// a scaled `log2(10)` is the only form that reproduces it, and it also matches
/// `6dB` (1.9952623149688795), `-20dB` (0.1) and `0dB` (1.0) bit for bit.
#[must_use]
pub fn from_decibels(x: f64) -> f64 {
    // The parenthesisation is load-bearing: `LOG2_10 * (x/20)` and
    // `(LOG2_10 * x)/20` disagree in the last ULP for most inputs, and it is
    // the first that reproduces the reference. `100dB` is the sharpest of the
    // probes -- 0x40f86a0000000002 one way, 0x40f869fffffffff1 the other.
    (LOG2_10 * (x / 20.0)).exp2()
}

/// The International System prefixes, as `(decimal, binary)` pairs.
///
/// The binary column is `2^(10*e/3)` where `e` is the decimal exponent, which
/// is why `c`, `d` and `h` have binary values that are not powers of two
/// (`2hi` is 203.187..., not 204.8). Every value in this table was read back
/// from the reference as raw f64 bits, one probe per prefix.
const fn si_prefix(c: char) -> Option<(f64, f64)> {
    Some(match c {
        'y' => (1e-24, 8.271_806_125_530_277e-25),
        'z' => (1e-21, 8.470_329_472_543_003e-22),
        'a' => (1e-18, 8.673_617_379_884_035e-19),
        'f' => (1e-15, 8.881_784_197_001_252e-16),
        'p' => (1e-12, 9.094_947_017_729_282e-13),
        'n' => (1e-9, 9.313_225_746_154_785e-10),
        'u' => (1e-6, 9.536_743_164_062_5e-7),
        'm' => (1e-3, 0.000_976_562_5),
        'c' => (1e-2, 0.009_843_133_202_303_695),
        'd' => (1e-1, 0.099_212_565_748_012_46),
        'h' => (1e2, 101.593_667_325_964_79),
        'k' | 'K' => (1e3, 1024.0),
        'M' => (1e6, 1_048_576.0),
        'G' => (1e9, 1_073_741_824.0),
        'T' => (1e12, 1_099_511_627_776.0),
        'P' => (1e15, 1_125_899_906_842_624.0),
        'E' => (1e18, 1.152_921_504_606_847e18),
        'Z' => (1e21, 1.180_591_620_717_411_3e21),
        'Y' => (1e24, 1.208_925_819_614_629_2e24),
        _ => return None,
    })
}

/// Scans the numeric part: sign, then `inf`/`nan`/hex/decimal.
fn scan_mantissa(s: &str) -> Option<(f64, usize)> {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    let negative = match bytes.first() {
        Some(b'-') => {
            i = 1;
            true
        }
        Some(b'+') => {
            i = 1;
            false
        }
        _ => false,
    };
    let rest = s.get(i..)?;

    if let Some(len) = match_ci(rest, "infinity").or_else(|| match_ci(rest, "inf")) {
        let v = if negative {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
        return Some((v, i + len));
    }
    if let Some(nan_len) = match_ci(rest, "nan") {
        let (bits, len) = scan_nan_payload(rest.get(nan_len..).unwrap_or(""), nan_len);
        let signed = if negative { bits | (1u64 << 63) } else { bits };
        return Some((f64::from_bits(signed), i + len));
    }
    if let Some((v, len)) = scan_hex(rest) {
        let v = if negative { -v } else { v };
        return Some((v, i + len));
    }

    let len = scan_decimal(rest)?;
    let text = rest.get(..len)?;
    let v: f64 = text.parse().ok()?;
    let v = if negative { -v } else { v };
    Some((v, i + len))
}

/// Matches `needle` case-insensitively against the head of `s`.
fn match_ci(s: &str, needle: &str) -> Option<usize> {
    let head = s.get(..needle.len())?;
    head.eq_ignore_ascii_case(needle).then_some(needle.len())
}

/// Handles the `nan(<chars>)` form that C's `strtod` accepts.
///
/// `nan(1)` really does come back with payload 1 from the reference, so the
/// syntax is consumed and a decimal payload is honoured. An unterminated or
/// non-numeric sequence leaves a plain quiet NaN and consumes only `nan`.
fn scan_nan_payload(after: &str, base_len: usize) -> (u64, usize) {
    const QUIET: u64 = 0x7ff8_0000_0000_0000;
    let Some(inner) = after.strip_prefix('(') else {
        return (QUIET, base_len);
    };
    let Some(close) = inner.find(')') else {
        return (QUIET, base_len);
    };
    let Some(text) = inner.get(..close) else {
        return (QUIET, base_len);
    };
    if !text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return (QUIET, base_len);
    }
    let consumed = base_len + 1 + close + 1;
    let payload = text.parse::<u64>().unwrap_or(0) & 0x0007_ffff_ffff_ffff;
    (QUIET | payload, consumed)
}

/// `0x` / `0X` followed by at least one hex digit, accumulated the way
/// `strtoull` does: saturating at `u64::MAX` rather than wrapping.
///
/// `0x20000000000000001` comes back as 1.8446744073709552e19 — that is
/// `(double)UINT64_MAX`, so the accumulator saturates. Confirmed.
fn scan_hex(s: &str) -> Option<(f64, usize)> {
    let rest = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
    let digits: usize = rest.bytes().take_while(u8::is_ascii_hexdigit).count();
    if digits == 0 {
        return None;
    }
    let mut acc: u64 = 0;
    let mut saturated = false;
    for b in rest.bytes().take(digits) {
        if saturated {
            continue;
        }
        let d = u64::from(hex_val(b));
        if let Some(next) = acc.checked_mul(16).and_then(|a| a.checked_add(d)) {
            acc = next;
        } else {
            acc = u64::MAX;
            saturated = true;
        }
    }
    // `2 + digits` counts the `0x`.
    Some((acc as f64, 2 + digits))
}

const fn hex_val(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

/// Length of the longest decimal float prefix of `s`, or `None`.
///
/// Deliberately the same shape as `strtod`'s decimal grammar and as Rust's own
/// `f64::from_str`, so the substring found here can be handed straight to
/// `parse()` and get `strtod`'s correctly-rounded result.
fn scan_decimal(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut end = 0usize;
    let mut digits = 0usize;
    while matches!(bytes.get(end), Some(ch) if ch.is_ascii_digit()) {
        end += 1;
        digits += 1;
    }
    if matches!(bytes.get(end), Some(b'.')) {
        end += 1;
        while matches!(bytes.get(end), Some(ch) if ch.is_ascii_digit()) {
            end += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return None;
    }
    // The exponent is only consumed when it actually has digits: `1e` scans as
    // `1` with `e` left over, which the reference then rejects as garbage.
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        let mut probe = end + 1;
        if matches!(bytes.get(probe), Some(b'+' | b'-')) {
            probe += 1;
        }
        let mut exp_digits = 0usize;
        while matches!(bytes.get(probe), Some(ch) if ch.is_ascii_digit()) {
            probe += 1;
            exp_digits += 1;
        }
        if exp_digits > 0 {
            end = probe;
        }
    }
    Some(end)
}
