//! The exact integer scanner the reference's specifier grammar is built on.
//!
//! Every numeric field in a stream specifier — program IDs, group indices,
//! stream IDs, stream indices — is read by C's `strtol` with base `0`. That is
//! not `str::parse`: it skips leading whitespace, accepts a sign, understands
//! `0x` hex and `0` octal, stops at the first character it cannot use, and
//! **saturates instead of failing** on overflow. Reproducing it exactly is the
//! difference between `-c:v:010` selecting video stream 8 and selecting video
//! stream 10.
//!
//! Probed against ffmpeg 8.1 (see `docs/app/vaco-cli-core.md` §Method):
//!
//! ```text
//! -c:010                    accepted, selects stream 8   (octal)
//! -c:0x10                   accepted, selects stream 16  (hex)
//! -c:0b1                    rejected: "Trailing garbage … b1"  (strtol stops at 'b')
//! -c:99999999999999999999   accepted, no error           (saturates to LONG_MAX)
//! -c:p: 1                   accepted                     (strtol skips whitespace)
//! ```

/// Result of one `strtol`-shaped scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scanned<'a> {
    /// The value, saturated to `i64` bounds exactly as `strtol` saturates to
    /// `LONG_MAX`/`LONG_MIN` on a 64-bit target.
    pub value: i64,
    /// Everything after the digits consumed.
    pub rest: &'a str,
    /// How many bytes were consumed. Zero means "no subject sequence", which is
    /// the reference's `endptr == nptr` error condition.
    pub consumed: usize,
}

/// C `isspace` for the "C" locale: the six characters `strtol` skips.
const fn is_c_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

const fn digit_value(b: u8) -> Option<u32> {
    match b {
        b'0'..=b'9' => Some((b - b'0') as u32),
        b'a'..=b'z' => Some((b - b'a') as u32 + 10),
        b'A'..=b'Z' => Some((b - b'A') as u32 + 10),
        _ => None,
    }
}

/// `strtol(s, &end, 0)`.
///
/// Returns `consumed == 0` when there is no subject sequence — the caller then
/// reports the reference's "Expected … got: {rest}" message with `rest` being
/// the *original* string, not the post-whitespace one, which is what the
/// reference prints.
#[must_use]
pub fn strtol_base0(s: &str) -> Scanned<'_> {
    let bytes = s.as_bytes();
    let mut i = 0usize;

    while bytes.get(i).copied().is_some_and(is_c_space) {
        i += 1;
    }

    let negative = match bytes.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };

    // Base detection, exactly as strtol(base = 0) performs it. `0x` with no hex
    // digit after it is NOT an error: the longest valid subject sequence is the
    // single `0`, and `x…` becomes the remainder.
    let (radix, digits_start) = match (bytes.get(i), bytes.get(i + 1)) {
        (Some(b'0'), Some(b'x' | b'X')) if bytes.get(i + 2).is_some_and(|&b| is_hex(b)) => {
            (16, i + 2)
        }
        (Some(b'0'), _) => (8, i),
        _ => (10, i),
    };

    let mut j = digits_start;
    let mut acc: i128 = 0;
    let mut saturated = false;
    while let Some(v) = bytes.get(j).copied().and_then(digit_value) {
        if v >= radix {
            break;
        }
        j += 1;
        if !saturated {
            acc = acc * i128::from(radix) + i128::from(v);
            if acc > i128::from(i64::MAX) + 1 {
                saturated = true;
            }
        }
    }

    if j == digits_start {
        // No digits at all. For radix 8 the leading `0` itself is the subject
        // sequence, so this only fires for radix 10/16 — and radix 16 is only
        // chosen when a hex digit follows, so in practice: radix 10, no digits.
        return Scanned {
            value: 0,
            rest: s,
            consumed: 0,
        };
    }

    let value = if negative {
        i64::try_from(-acc).unwrap_or(i64::MIN)
    } else {
        i64::try_from(acc).unwrap_or(i64::MAX)
    };

    let Some(rest) = s.get(j..) else {
        // Unreachable: every byte consumed above is ASCII, so `j` is always on a
        // character boundary. Degrade to "consumed nothing" rather than panic.
        return Scanned {
            value: 0,
            rest: s,
            consumed: 0,
        };
    };

    Scanned {
        value,
        rest,
        consumed: j,
    }
}

const fn is_hex(b: u8) -> bool {
    b.is_ascii_hexdigit()
}

/// The *index* form: `strtol` base 0, but the first character must be an ASCII
/// digit.
///
/// This asymmetry is real and observable. `-c:v:+0` is rejected with "Trailing
/// garbage" (nothing consumed) while `-c:p:+1` is accepted — the top-level
/// dispatch tests `isdigit` before entering the index branch, whereas the `p:`
/// branch calls `strtol` unconditionally. Both behaviours are reproduced.
#[must_use]
pub fn strtol_index(s: &str) -> Option<Scanned<'_>> {
    if !s.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        return None;
    }
    let sc = strtol_base0(s);
    (sc.consumed != 0).then_some(sc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> (i64, &str) {
        let sc = strtol_base0(s);
        (sc.value, sc.rest)
    }

    #[test]
    fn decimal() {
        assert_eq!(v("123"), (123, ""));
        assert_eq!(v("123abc"), (123, "abc"));
        assert_eq!(v("+7"), (7, ""));
        assert_eq!(v("-7"), (-7, ""));
    }

    #[test]
    fn octal_and_hex() {
        assert_eq!(v("010"), (8, ""));
        assert_eq!(v("0x10"), (16, ""));
        assert_eq!(v("0X1f"), (31, ""));
        // `0b1`: base 0 has no binary prefix, so `0` is the number and `b1` the rest.
        assert_eq!(v("0b1"), (0, "b1"));
        // `0x` with nothing usable after it: the subject sequence is just `0`.
        assert_eq!(v("0x"), (0, "x"));
        assert_eq!(v("0xg"), (0, "xg"));
        // Octal stops at the first digit outside the radix.
        assert_eq!(v("09"), (0, "9"));
    }

    #[test]
    fn leading_whitespace_is_skipped() {
        assert_eq!(v(" 1"), (1, ""));
        assert_eq!(v("\t\n 42x"), (42, "x"));
    }

    #[test]
    fn saturates_rather_than_failing() {
        assert_eq!(v("99999999999999999999"), (i64::MAX, ""));
        assert_eq!(v("-99999999999999999999"), (i64::MIN, ""));
        assert_eq!(v("9223372036854775807"), (i64::MAX, ""));
        assert_eq!(v("-9223372036854775808"), (i64::MIN, ""));
    }

    #[test]
    fn no_subject_sequence() {
        assert_eq!(strtol_base0("").consumed, 0);
        assert_eq!(strtol_base0("abc").consumed, 0);
        assert_eq!(strtol_base0("+").consumed, 0);
        assert_eq!(strtol_base0("  ").consumed, 0);
    }

    #[test]
    fn index_form_requires_a_leading_digit() {
        assert!(strtol_index("+0").is_none());
        assert!(strtol_index("-1").is_none());
        assert!(strtol_index(" 1").is_none());
        assert_eq!(strtol_index("010").map(|s| s.value), Some(8));
        assert_eq!(strtol_index("0x2").map(|s| s.value), Some(2));
    }

    #[test]
    fn never_panics_on_arbitrary_input() {
        for s in [
            "\u{1f600}",
            "0\u{1f600}",
            "-",
            "0x\u{ff}",
            "\u{0}",
            "0\u{0}9",
        ] {
            let _ = strtol_base0(s);
            let _ = strtol_index(s);
        }
    }
}
