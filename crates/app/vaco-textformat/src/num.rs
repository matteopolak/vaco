//! The only module allowed to format a number for output.
//!
//! Byte identity lives and dies here: `start_time=0.000000` and
//! `start_time=0.0` are the same number and different files. Everything that
//! reaches a writer as a string has passed through one of these functions.
//!
//! All rules were confirmed against ffprobe 8.1 under `LC_ALL=C`.

use std::fmt::Write as _;

use vaco_core::Rational;

/// The token every writer prints for an unavailable value.
pub const NA: &str = "N/A";

/// The unit a scalar carries when `-unit` is in force.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unit {
    /// No unit; `-unit` prints nothing extra.
    None,
    /// `byte`.
    Byte,
    /// `bit/s`.
    BitPerSecond,
    /// `s`. Seconds are special: they never collapse to a bare integer.
    Second,
}

impl Unit {
    const fn suffix(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Byte => "byte",
            Self::BitPerSecond => "bit/s",
            Self::Second => "s",
        }
    }
}

/// `-unit` / `-prefix` / `-sexagesimal`, the three switches that change how a
/// number is spelled. `-pretty` sets all three (plus the no-op
/// `-byte_binary_prefix`).
// Four independent switches, and each one is an ffprobe CLI flag by that exact
// name. Folding them into two-variant enums (what `struct_excessive_bools`
// suggests) would put a layer of translation between the option table and the
// formatting rules, which is precisely where a byte divergence would hide.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Pretty {
    /// `-unit`: append the unit suffix.
    pub unit: bool,
    /// `-prefix`: scale by the SI ladder and insert the prefix letter.
    pub prefix: bool,
    /// `-byte_binary_prefix`: **a no-op in 8.1**. Kept so the option parses and
    /// so `-pretty` can set it, but it changes no output byte. Verified by
    /// sweeping byte sizes with and without it.
    pub byte_binary_prefix: bool,
    /// `-sexagesimal`: `H:MM:SS.microseconds` for time values.
    pub sexagesimal: bool,
}

/// Format an integer field. Plain `{}`; nothing else is ever right.
#[must_use]
pub fn int(v: i64) -> String {
    v.to_string()
}

/// Six decimal places — the spelling of every `*_time`, `start_time` and
/// `duration` field when `-sexagesimal` is off.
#[must_use]
pub fn seconds(v: f64) -> String {
    format!("{v:.6}")
}

/// `H:MM:SS.microseconds`, the `-sexagesimal` spelling.
///
/// The hours field is **not** zero-padded, and a negative value does *not* get
/// a leading sign on the whole clock: the sign stays on whichever component is
/// non-zero, so −0.02322 s prints `0:00:-0.023220`. That falls out of
/// truncating division and `%09.6f`, and the reference binary does exactly it.
#[must_use]
pub fn sexagesimal(v: f64) -> String {
    let hours = (v / 3600.0).trunc();
    let rem = v - hours * 3600.0;
    let mins = (rem / 60.0).trunc();
    let secs = rem - mins * 60.0;
    format!("{}:{:02}:{:09.6}", hours as i64, mins as i64, secs)
}

/// A time in seconds, honouring `-sexagesimal` and `-unit`/`-prefix`.
#[must_use]
pub fn time(v: f64, p: Pretty) -> String {
    if p.sexagesimal {
        sexagesimal(v)
    } else {
        value(v, Unit::Second, p)
    }
}

/// A `num/den` rational, as `r_frame_rate` and `time_base` are printed.
#[must_use]
pub fn rational(r: Rational) -> String {
    format!("{}/{}", r.num, r.den)
}

/// A `num:den` rational, as `sample_aspect_ratio` and `display_aspect_ratio`
/// are printed.
#[must_use]
pub fn ratio(r: Rational) -> String {
    format!("{}:{}", r.num, r.den)
}

/// `codec_tag`: exactly eight lowercase hex digits behind `0x`.
#[must_use]
pub fn codec_tag(v: u32) -> String {
    format!("0x{v:08x}")
}

/// `id`: minimal lowercase hex behind `0x`.
#[must_use]
pub fn id(v: i64) -> String {
    if v < 0 {
        format!("-0x{:x}", v.unsigned_abs())
    } else {
        format!("0x{v:x}")
    }
}

/// The SI prefix letters, upward only. The reference never scales below 1.
const PREFIXES: [char; 8] = ['K', 'M', 'G', 'T', 'P', 'E', 'Z', 'Y'];

/// Format a scalar under `-unit`/`-prefix`.
///
/// Two rules, both derived by sweeping the reference:
///
/// * With `-prefix`, divide by 1000 while the magnitude is at least 1000,
///   at most eight times. Never scales upward from below 1.
/// * The scaled number prints **bare when it is an exact integer** and at six
///   decimals otherwise — `1000` bytes is `1 Kbyte`, `1500` is
///   `1.500000 Kbyte`. [`Unit::Second`] is exempt and always prints six
///   decimals, so a 4000-second file is `4.000000 Ks`, not `4 Ks`.
///
/// The prefix letter is separated from the unit by nothing (`Kbyte`, `Ks`), and
/// from the number by one space. With `-prefix` but no `-unit` the suffix
/// disappears and the bare prefix letter remains (`17.540000 K`).
#[must_use]
pub fn value(v: f64, unit: Unit, p: Pretty) -> String {
    if !p.unit && !p.prefix {
        return if unit == Unit::Second {
            seconds(v)
        } else if v.fract() == 0.0 && v.is_finite() {
            format!("{}", v as i64)
        } else {
            seconds(v)
        };
    }

    let mut scaled = v;
    let mut prefix = None;
    if p.prefix {
        for letter in PREFIXES {
            if scaled.abs() < 1000.0 {
                break;
            }
            scaled /= 1000.0;
            prefix = Some(letter);
        }
    }

    let mut out = if unit == Unit::Second || scaled.fract() != 0.0 || !scaled.is_finite() {
        format!("{scaled:.6}")
    } else {
        format!("{}", scaled as i64)
    };

    if prefix.is_some() || p.unit {
        out.push(' ');
        if let Some(letter) = prefix {
            out.push(letter);
        }
        if p.unit {
            out.push_str(unit.suffix());
        }
    }
    out
}

/// The three-character `flags` field of a packet: `K`/`_`, `D`/`_`, `C`/`_`.
#[must_use]
pub fn packet_flags(key: bool, discard: bool, corrupt: bool) -> String {
    let mut s = String::with_capacity(3);
    s.push(if key { 'K' } else { '_' });
    s.push(if discard { 'D' } else { '_' });
    s.push(if corrupt { 'C' } else { '_' });
    s
}

/// A hexdump line in ffprobe's `xxd` shape, used by `-show_data`.
#[must_use]
pub fn hex_byte(b: u8) -> String {
    let mut s = String::with_capacity(2);
    let _ = write!(s, "{b:02x}");
    s
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    const PLAIN: Pretty = Pretty {
        unit: false,
        prefix: false,
        byte_binary_prefix: false,
        sexagesimal: false,
    };
    const UP: Pretty = Pretty {
        unit: true,
        prefix: true,
        byte_binary_prefix: false,
        sexagesimal: false,
    };

    #[test]
    fn observed_unit_prefix_sweep() {
        // `ffprobe -unit -prefix -show_entries format=size`, swept.
        for (bytes, want) in [
            (1.0, "1 byte"),
            (999.0, "999 byte"),
            (1000.0, "1 Kbyte"),
            (1500.0, "1.500000 Kbyte"),
            (999_999.0, "999.999000 Kbyte"),
            (1_000_000.0, "1 Mbyte"),
            (2_097_152.0, "2.097152 Mbyte"),
            (1_000_000_000.0, "1 Gbyte"),
        ] {
            assert_eq!(value(bytes, Unit::Byte, UP), want, "{bytes}");
        }
    }

    #[test]
    fn observed_unit_without_prefix_is_bare() {
        let u = Pretty {
            unit: true,
            ..PLAIN
        };
        assert_eq!(value(2_097_152.0, Unit::Byte, u), "2097152 byte");
        assert_eq!(value(80224.0, Unit::BitPerSecond, u), "80224 bit/s");
    }

    #[test]
    fn observed_prefix_without_unit_drops_the_suffix() {
        let p = Pretty {
            prefix: true,
            ..PLAIN
        };
        assert_eq!(value(2_097_152.0, Unit::Byte, p), "2.097152 M");
    }

    #[test]
    fn observed_seconds_never_collapse() {
        let u = Pretty {
            unit: true,
            ..PLAIN
        };
        assert_eq!(value(1.0, Unit::Second, u), "1.000000 s");
        assert_eq!(value(0.523_222, Unit::Second, u), "0.523222 s");
        assert_eq!(value(4000.0, Unit::Second, UP), "4.000000 Ks");
    }

    #[test]
    fn observed_byte_binary_prefix_is_a_no_op() {
        let with = Pretty {
            byte_binary_prefix: true,
            ..UP
        };
        assert_eq!(value(2_097_152.0, Unit::Byte, with), "2.097152 Mbyte");
        assert_eq!(value(2_097_152.0, Unit::Byte, UP), "2.097152 Mbyte");
    }

    #[test]
    fn observed_sexagesimal() {
        assert_eq!(sexagesimal(0.0), "0:00:00.000000");
        assert_eq!(sexagesimal(1.0), "0:00:01.000000");
        assert_eq!(sexagesimal(0.523_222), "0:00:00.523222");
        // The reference really does print this. Not a typo.
        assert_eq!(sexagesimal(-0.023_220), "0:00:-0.023220");
    }

    #[test]
    fn observed_hex_fields() {
        assert_eq!(codec_tag(0x6134_706d), "0x6134706d");
        assert_eq!(codec_tag(0), "0x00000000");
        assert_eq!(id(1), "0x1");
    }

    #[test]
    fn rationals() {
        assert_eq!(rational(Rational::new(0, 0)), "0/0");
        assert_eq!(rational(Rational::new(1, 44100)), "1/44100");
        assert_eq!(ratio(Rational::new(16, 9)), "16:9");
    }

    #[test]
    fn seconds_is_six_decimals() {
        assert_eq!(seconds(0.0), "0.000000");
        assert_eq!(seconds(-0.023_220), "-0.023220");
        assert_eq!(seconds(-0.0), "-0.000000");
    }
}
