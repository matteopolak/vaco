//! Per-format timestamp grammars.
//!
//! Every function here is named for the one format it parses or prints, on
//! purpose (`planning/AGENT-CONSTRAINTS.md`'s "Detection and demuxing ask
//! different questions" applies just as much to punctuation: a parser that
//! quietly accepts the wrong separator for a format will happily mis-time a
//! whole file). None of these functions guess at a caller's intent from the
//! string shape — the caller picks the function that matches the format it is
//! reading, the same way it picks the module.
//!
//! # Measured, not assumed (D17)
//!
//! Several of these grammars are not what their on-disk shape suggests.
//! Measured against `ffmpeg 8.1` by round-tripping known inputs through
//! `ffprobe -show_packets` and reading `pts_time`/`duration_time`:
//!
//! | Format | Field syntax | What it actually counts |
//! |---|---|---|
//! | `SubRip` | `HH:MM:SS,mmm` | milliseconds, comma mandatory |
//! | `WebVTT` | `HH:MM:SS.mmm` or `MM:SS.mmm` | milliseconds, period mandatory, hour field optional |
//! | ASS/SSA | `H:MM:SS.cc` | **centi**seconds, one property despite two script versions — the same demuxer reports `codec_name=ass` for a `ScriptType: v4.00` (SSA) script and a `v4.00+` (ASS) one alike |
//! | `JACOsub` | `H:MM:SS.hh` | centiseconds — same shape as ASS, a different format |
//! | `MicroDVD` | `{start}{end}` | **frame numbers**, and the default rate absent a `{1}{1}<fps>` header line is **23.976 (24000/1001)**, not 25 |
//! | `SubViewer` | `HH:MM:SS.mmm,HH:MM:SS.mmm` | milliseconds |
//! | `SubViewer` 1.0 | `[HH:MM:SS]` | whole seconds, start-only |
//! | MPL2 | `[n][n]` | **tenths of a second**, not frames and not hundredths |
//! | PJS | `n,n,"text"` | tenths of a second |
//! | `VPlayer` | `HH:MM:SS:` | whole seconds, start-only |
//! | LRC | `[mm:ss.xx]` | hundredths, start-only, `mm` unbounded |
//! | Spruce STL | `HH:MM:SS:hh` | **hundredths**, despite the field being where a frame count would go in an editing timecode |
//! | SAMI | `Start=n` | milliseconds, start-only |
//! | `RealText` | `HH:MM:SS` | whole seconds; `end=`/`dur=` optional, and **a cue with neither defaults to a 60-second duration** rather than borrowing the next cue's start |
//! | `MPsub` `FORMAT=TIME` | `gap duration` | seconds, and **both fields are relative to the previous cue's end**, not absolute — the second data line in a two-cue file measured `pts_time=4.0` from two identical `"1.0 2.0"` lines, which only holds if the first cue's *end* (3.0) is what the second line's gap is added to |
//!
//! See `docs/format/vaco-subtitle-text.md` for the full table and the probe
//! transcripts each row came from.

#![allow(
    clippy::integer_division,
    reason = "every division here is exact by construction: fixed bases (60, 3600, 1_000_000 and its divisors 10/100/1000) that this module's own callers chose, never a caller-supplied denominator. Rewriting through f64 would introduce the precision loss this lint exists to catch, not avoid it."
)]

use vaco_core::Duration;

const US_PER_SEC: i64 = 1_000_000;

// --------------------------------------------------------------- primitives

/// Parse an ASCII decimal integer with exactly the digits in `s`, no sign, no
/// leading/trailing junk. `None` on anything else, including an empty string —
/// a malformed timestamp field is a parse failure, not a zero.
fn parse_uint(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// A fractional-seconds field of `digits`, scaled to microseconds.
///
/// Handles any digit width by scaling explicitly rather than assuming a fixed
/// count: `"5"` after a `.` is 500ms in SRT-shaped input, not 5ms, and a
/// literal reading of "pad to 3 and parse" gets that backwards. Truncates
/// (does not round) past 6 digits, which none of these formats use anyway.
fn frac_micros(digits: &str) -> Option<i64> {
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value: i64 = digits
        .get(..digits.len().min(6))
        .unwrap_or(digits)
        .parse()
        .ok()?;
    let width = digits.len().min(6) as u32;
    let scale = 10i64.checked_pow(6 - width)?;
    Some(value.saturating_mul(scale))
}

/// `h:m:s` plus a already-scaled fractional microsecond count, combined and
/// checked. `None` if `m` or `s` is out of the 0..60 range some formats state
/// contractually — this is the one place that range is enforced, so every
/// caller gets it for free.
fn combine_hms(h: i64, m: i64, s: i64, frac_us: i64) -> Option<Duration> {
    if !(0..60).contains(&m) || !(0..60).contains(&s) || h < 0 {
        return None;
    }
    let total = h
        .checked_mul(3600)?
        .checked_add(m.checked_mul(60)?)?
        .checked_add(s)?
        .checked_mul(US_PER_SEC)?
        .checked_add(frac_us)?;
    Some(Duration::from_micros(total))
}

/// Split `d` into non-negative `(h, m, s, frac_of(scale))` for printing.
/// `scale` is the fractional field's own denominator (1000 for milliseconds,
/// 100 for centiseconds, 10 for deciseconds).
fn split_hms(d: Duration, scale: i64) -> (i64, i64, i64, i64) {
    let total_us = d.as_micros().max(0);
    let total_secs = total_us / US_PER_SEC;
    let frac_us = total_us % US_PER_SEC;
    let h = total_secs / 3600;
    let m = (total_secs / 60) % 60;
    let s = total_secs % 60;
    // `scale` divides `US_PER_SEC` exactly for every caller in this module.
    let frac = frac_us / (US_PER_SEC / scale);
    (h, m, s, frac)
}

// -------------------------------------------------------------------- SRT

/// `HH:MM:SS,mmm`. The comma is load-bearing: a period here is `WebVTT`'s
/// grammar, not a looser spelling of this one.
#[must_use]
pub fn parse_srt_time(s: &str) -> Option<Duration> {
    let (h, rest) = s.split_once(':')?;
    let (m, rest) = rest.split_once(':')?;
    let (sec, ms) = rest.split_once(',')?;
    combine_hms(
        parse_uint(h)?,
        parse_uint(m)?,
        parse_uint(sec)?,
        frac_micros(ms)?,
    )
}

/// The inverse of [`parse_srt_time`]. Always `HH:MM:SS,mmm`, hours unbounded.
#[must_use]
pub fn format_srt_time(d: Duration) -> String {
    let (h, m, s, ms) = split_hms(d, 1000);
    format!("{h:02}:{m:02}:{s:02},{ms:03}")
}

/// `SS,mmm --> SS,mmm`, returning `(start, end)`.
#[must_use]
pub fn parse_srt_timing_line(line: &str) -> Option<(Duration, Duration)> {
    let (a, b) = line.split_once("-->")?;
    Some((parse_srt_time(a.trim())?, parse_srt_time(b.trim())?))
}

// ------------------------------------------------------------------ WebVTT

/// `HH:MM:SS.mmm` or the short `MM:SS.mmm` form the spec permits when the
/// hour component is zero. The period is load-bearing the other way round
/// from SRT: a comma here is not `WebVTT`.
#[must_use]
pub fn parse_vtt_time(s: &str) -> Option<Duration> {
    let (main, ms) = s.split_once('.')?;
    let fields: Vec<&str> = main.split(':').collect();
    let (h, m, sec) = match fields.as_slice() {
        [h, m, sec] => (parse_uint(h)?, parse_uint(m)?, parse_uint(sec)?),
        [m, sec] => (0, parse_uint(m)?, parse_uint(sec)?),
        _ => return None,
    };
    combine_hms(h, m, sec, frac_micros(ms)?)
}

/// The inverse of [`parse_vtt_time`]. Always prints the full `HH:MM:SS.mmm`
/// form — the short form is a reading convenience the spec permits, not a
/// canonical output shape.
#[must_use]
pub fn format_vtt_time(d: Duration) -> String {
    let (h, m, s, ms) = split_hms(d, 1000);
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

/// `SS.mmm --> SS.mmm`, with `WebVTT`'s optional trailing cue settings ignored.
#[must_use]
pub fn parse_vtt_timing_line(line: &str) -> Option<(Duration, Duration)> {
    let rest = line.trim_start();
    let (a, rest) = rest.split_once("-->")?;
    let rest = rest.trim_start();
    // The end timestamp is the first whitespace-delimited token; anything
    // after it is cue settings (`align:`, `position:`, ...), which is markup
    // this crate does not interpret.
    let b = rest.split_whitespace().next()?;
    Some((parse_vtt_time(a.trim())?, parse_vtt_time(b)?))
}

// --------------------------------------------------------------- ASS / SSA

/// `H:MM:SS.cc` — **centi**seconds, and the hour field is conventionally one
/// digit but parsed at any width.
#[must_use]
pub fn parse_ass_time(s: &str) -> Option<Duration> {
    let (h, rest) = s.split_once(':')?;
    let (m, rest) = rest.split_once(':')?;
    let (sec, cs) = rest.split_once('.')?;
    if cs.len() != 2 {
        return None;
    }
    combine_hms(
        parse_uint(h)?,
        parse_uint(m)?,
        parse_uint(sec)?,
        frac_micros(cs)?,
    )
}

/// The inverse of [`parse_ass_time`]. The hour field is printed unpadded,
/// matching every ASS script the reference emits (`0:00:01.00`, not
/// `00:00:01.00`).
#[must_use]
pub fn format_ass_time(d: Duration) -> String {
    let (h, m, s, cs) = split_hms(d, 100);
    format!("{h}:{m:02}:{s:02}.{cs:02}")
}

// ------------------------------------------------------------------ JACOsub

/// `H:MM:SS.hh` — centiseconds. The same shape as ASS's clock, kept as a
/// separate function because the two are different formats that happen to
/// agree on punctuation; a future change to one must not silently reach the
/// other.
#[must_use]
pub fn parse_jacosub_time(s: &str) -> Option<Duration> {
    parse_ass_time(s)
}

/// The inverse of [`parse_jacosub_time`].
#[must_use]
pub fn format_jacosub_time(d: Duration) -> String {
    format_ass_time(d)
}

// --------------------------------------------------------------- SubViewer

/// `HH:MM:SS.mmm` — milliseconds, unlike ASS/JACOsub's period-separated
/// field of the same width class.
#[must_use]
pub fn parse_subviewer_time(s: &str) -> Option<Duration> {
    let (h, rest) = s.split_once(':')?;
    let (m, rest) = rest.split_once(':')?;
    let (sec, ms) = rest.split_once('.')?;
    combine_hms(
        parse_uint(h)?,
        parse_uint(m)?,
        parse_uint(sec)?,
        frac_micros(ms)?,
    )
}

/// The inverse of [`parse_subviewer_time`].
#[must_use]
pub fn format_subviewer_time(d: Duration) -> String {
    let (h, m, s, ms) = split_hms(d, 1000);
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

/// `HH:MM:SS,HH:MM:SS.mmm` timing line: two [`parse_subviewer_time`] fields
/// separated by a comma.
#[must_use]
pub fn parse_subviewer_timing_line(line: &str) -> Option<(Duration, Duration)> {
    let (a, b) = line.split_once(',')?;
    Some((
        parse_subviewer_time(a.trim())?,
        parse_subviewer_time(b.trim())?,
    ))
}

/// `SubViewer` 1.0's bracketed, start-only `[HH:MM:SS]` — whole seconds, no
/// fractional field at all.
#[must_use]
pub fn parse_subviewer1_time(s: &str) -> Option<Duration> {
    let (h, rest) = s.split_once(':')?;
    let (m, sec) = rest.split_once(':')?;
    combine_hms(parse_uint(h)?, parse_uint(m)?, parse_uint(sec)?, 0)
}

// ---------------------------------------------------------------- VPlayer

/// `HH:MM:SS:` — whole seconds, start-only, colon-terminated rather than
/// bracketed.
#[must_use]
pub fn parse_vplayer_time(s: &str) -> Option<Duration> {
    let (h, rest) = s.split_once(':')?;
    let (m, sec) = rest.split_once(':')?;
    combine_hms(parse_uint(h)?, parse_uint(m)?, parse_uint(sec)?, 0)
}

// -------------------------------------------------------------------- LRC

/// `mm:ss.xx` — hundredths, start-only. `mm` is not clamped to 59: an LRC
/// timestamp is minutes-since-start-of-track and commonly exceeds an hour.
#[must_use]
pub fn parse_lrc_time(s: &str) -> Option<Duration> {
    let (m, rest) = s.split_once(':')?;
    let (sec, frac) = rest.split_once('.')?;
    let minutes = parse_uint(m)?;
    let seconds = parse_uint(sec)?;
    if !(0..60).contains(&seconds) {
        return None;
    }
    let base = minutes
        .checked_mul(60)?
        .checked_add(seconds)?
        .checked_mul(US_PER_SEC)?;
    Some(Duration::from_micros(base.checked_add(frac_micros(frac)?)?))
}

/// The inverse of [`parse_lrc_time`]. `mm` grows past 59 rather than
/// wrapping into an hours field, matching the format's own convention.
#[must_use]
pub fn format_lrc_time(d: Duration) -> String {
    let total_us = d.as_micros().max(0);
    let total_secs = total_us / US_PER_SEC;
    let frac_us = total_us % US_PER_SEC;
    let m = total_secs / 60;
    let s = total_secs % 60;
    let cs = frac_us / (US_PER_SEC / 100);
    format!("{m:02}:{s:02}.{cs:02}")
}

// --------------------------------------------------------------- Spruce STL

/// `HH:MM:SS:hh`. Measured: the fourth field is **hundredths of a second**,
/// not a frame count at some assumed rate — `00:00:01:12` demuxes to exactly
/// `1.12s` regardless of any frame rate. Do not be misled by the field's
/// resemblance to an editing timecode's frame slot.
#[must_use]
pub fn parse_stl_time(s: &str) -> Option<Duration> {
    let (h, rest) = s.split_once(':')?;
    let (m, rest) = rest.split_once(':')?;
    let (sec, hh) = rest.rsplit_once(':')?;
    if hh.len() != 2 {
        return None;
    }
    combine_hms(
        parse_uint(h)?,
        parse_uint(m)?,
        parse_uint(sec)?,
        frac_micros(hh)?,
    )
}

/// The inverse of [`parse_stl_time`].
#[must_use]
pub fn format_stl_time(d: Duration) -> String {
    let (h, m, s, hh) = split_hms(d, 100);
    format!("{h:02}:{m:02}:{s:02}:{hh:02}")
}

// -------------------------------------------------------------------- SAMI

/// SAMI's `Start=` attribute value: plain milliseconds, no separators.
#[must_use]
pub fn parse_sami_millis(s: &str) -> Option<Duration> {
    Some(Duration::from_micros(parse_uint(s)?.checked_mul(1000)?))
}

// ---------------------------------------------------------------- RealText

/// `RealText`'s `HH:MM:SS` clock attribute value — whole seconds. Measured: a
/// bare `begin="1"` (no colons) is also accepted by the reference as a raw
/// second count, so a single field with no colon is handled here too.
#[must_use]
pub fn parse_realtext_time(s: &str) -> Option<Duration> {
    let parts: Vec<&str> = s.split(':').collect();
    match parts.as_slice() {
        [only] => Some(Duration::from_micros(
            parse_uint(only)?.checked_mul(US_PER_SEC)?,
        )),
        [h, m, sec] => combine_hms(parse_uint(h)?, parse_uint(m)?, parse_uint(sec)?, 0),
        _ => None,
    }
}

/// The 60-second default `RealText` applies to a `<time>` tag with neither an
/// `end=` nor a `dur=` attribute. Measured against the reference twice
/// (`ffprobe -f realtext -show_packets`), not assumed.
pub const REALTEXT_DEFAULT_DURATION: Duration = Duration::from_micros(60 * US_PER_SEC);

// ------------------------------------------------------------ tenths (PJS, MPL2)

/// PJS and MPL2 both count **tenths of a second** in a plain integer field —
/// measured: `10` and `50` demux to `1.0s` and `5.0s`, a factor of 100 in
/// microseconds, i.e. a factor of 10 in seconds. Shared here because the unit
/// genuinely is the same, unlike the HH:MM:SS families above where sharing
/// code across formats would blur two different grammars.
#[must_use]
pub fn parse_deciseconds(s: &str) -> Option<Duration> {
    Some(Duration::from_micros(
        parse_uint(s)?.checked_mul(US_PER_SEC / 10)?,
    ))
}

/// The inverse of [`parse_deciseconds`], truncating toward zero.
#[must_use]
pub fn format_deciseconds(d: Duration) -> i64 {
    d.as_micros().max(0) / (US_PER_SEC / 10)
}

// ----------------------------------------------------------------- seconds

/// A plain decimal-seconds field (`"1.0"`, `"12"`), as `MPsub`'s `FORMAT=TIME`
/// lines use. Not a full float grammar — no exponents, no sign — because no
/// sample of the format needs one and a narrower parser rejects more garbage.
#[must_use]
pub fn parse_seconds(s: &str) -> Option<Duration> {
    let s = s.trim();
    match s.split_once('.') {
        Some((whole, frac)) => {
            let sign = if whole.starts_with('-') { -1 } else { 1 };
            let whole = whole.trim_start_matches('-');
            let whole = if whole.is_empty() {
                0
            } else {
                parse_uint(whole)?
            };
            let frac_us = frac_micros(frac)?;
            Some(Duration::from_micros(
                sign * (whole.checked_mul(US_PER_SEC)?.checked_add(frac_us)?),
            ))
        }
        None => Some(Duration::from_micros(
            parse_uint(s)?.checked_mul(US_PER_SEC)?,
        )),
    }
}

/// The inverse of [`parse_seconds`], always with one decimal digit — the
/// shape every `MPsub` sample this crate was checked against uses.
#[must_use]
pub fn format_seconds(d: Duration) -> String {
    let us = d.as_micros().max(0);
    format!("{}.{}", us / US_PER_SEC, (us % US_PER_SEC) / 100_000)
}

// -------------------------------------------------------------- MicroDVD

/// `MicroDVD`'s default frame rate absent an explicit `{1}{1}<fps>` header
/// line. Measured: 25 frames demuxed to `1.042709s`, which is `25 /
/// (24000/1001)`, not `25/25`.
pub const MICRODVD_DEFAULT_FPS: f64 = 24000.0 / 1001.0;

/// Convert a `MicroDVD` frame number to a [`Duration`] at `fps`.
///
/// `fps` non-positive or non-finite falls back to
/// [`MICRODVD_DEFAULT_FPS`] rather than dividing by zero or producing `NaN`.
#[must_use]
pub fn microdvd_frame_to_duration(frame: i64, fps: f64) -> Duration {
    let fps = if fps.is_finite() && fps > 0.0 {
        fps
    } else {
        MICRODVD_DEFAULT_FPS
    };
    let secs = frame as f64 / fps;
    Duration::from_micros((secs * US_PER_SEC as f64).round() as i64)
}

/// The inverse of [`microdvd_frame_to_duration`], rounding to the nearest
/// frame.
#[must_use]
pub fn microdvd_duration_to_frame(d: Duration, fps: f64) -> i64 {
    let fps = if fps.is_finite() && fps > 0.0 {
        fps
    } else {
        MICRODVD_DEFAULT_FPS
    };
    (d.as_secs_f64() * fps).round() as i64
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn srt_round_trips_and_rejects_the_wrong_punctuation() {
        assert_eq!(
            parse_srt_time("00:00:01,000"),
            Some(Duration::from_micros(1_000_000))
        );
        assert_eq!(
            parse_srt_time("00:00:01.000"),
            None,
            "a period is WebVTT's separator, not SRT's"
        );
        assert_eq!(
            format_srt_time(Duration::from_micros(3_723_456_000)),
            "01:02:03,456"
        );
    }

    #[test]
    fn vtt_accepts_both_field_widths_and_rejects_the_comma() {
        assert_eq!(
            parse_vtt_time("00:00:01.000"),
            Some(Duration::from_micros(1_000_000))
        );
        assert_eq!(
            parse_vtt_time("00:01.000"),
            Some(Duration::from_micros(1_000_000))
        );
        assert_eq!(
            parse_vtt_time("00:00:01,000"),
            None,
            "a comma is SRT's separator, not WebVTT's"
        );
    }

    #[test]
    fn vtt_timing_line_ignores_trailing_cue_settings() {
        let (a, b) =
            parse_vtt_timing_line("00:00:01.000 --> 00:00:02.000 align:start position:10%")
                .unwrap();
        assert_eq!(a, Duration::from_micros(1_000_000));
        assert_eq!(b, Duration::from_micros(2_000_000));
    }

    #[test]
    fn ass_time_is_centiseconds_with_an_unpadded_hour() {
        assert_eq!(
            parse_ass_time("0:00:01.50"),
            Some(Duration::from_micros(1_500_000))
        );
        assert_eq!(
            format_ass_time(Duration::from_micros(1_500_000)),
            "0:00:01.50"
        );
        assert_eq!(
            parse_ass_time("0:00:01.5"),
            None,
            "centiseconds is exactly two digits"
        );
    }

    #[test]
    fn stl_fourth_field_is_hundredths_not_frames() {
        assert_eq!(
            parse_stl_time("00:00:01:12"),
            Some(Duration::from_micros(1_120_000))
        );
    }

    #[test]
    fn deciseconds_matches_the_measured_pjs_and_mpl2_unit() {
        assert_eq!(
            parse_deciseconds("10"),
            Some(Duration::from_micros(1_000_000))
        );
        assert_eq!(
            parse_deciseconds("50"),
            Some(Duration::from_micros(5_000_000))
        );
    }

    #[test]
    fn microdvd_default_fps_matches_measurement() {
        let d = microdvd_frame_to_duration(25, MICRODVD_DEFAULT_FPS);
        // Measured 1.042709s; allow the last microsecond of float rounding.
        assert!((d.as_micros() - 1_042_709).abs() <= 1);
    }

    #[test]
    fn lrc_minutes_are_not_clamped_to_59() {
        assert_eq!(
            parse_lrc_time("75:30.00"),
            Some(Duration::from_micros((75 * 60 + 30) * US_PER_SEC))
        );
    }

    #[test]
    fn out_of_range_seconds_are_rejected_not_wrapped() {
        assert_eq!(parse_srt_time("00:00:60,000"), None);
        assert_eq!(parse_srt_time("00:60:00,000"), None);
    }

    proptest! {
        #[test]
        fn srt_round_trip(h in 0i64..99, m in 0i64..60, s in 0i64..60, ms in 0i64..1000) {
            let d = Duration::from_micros(((h * 3600 + m * 60 + s) * 1000 + ms) * 1000);
            let text = format_srt_time(d);
            prop_assert_eq!(parse_srt_time(&text), Some(d));
        }

        #[test]
        fn vtt_round_trip(h in 0i64..99, m in 0i64..60, s in 0i64..60, ms in 0i64..1000) {
            let d = Duration::from_micros(((h * 3600 + m * 60 + s) * 1000 + ms) * 1000);
            let text = format_vtt_time(d);
            prop_assert_eq!(parse_vtt_time(&text), Some(d));
        }

        #[test]
        fn ass_round_trip(h in 0i64..24, m in 0i64..60, s in 0i64..60, cs in 0i64..100) {
            let d = Duration::from_micros(((h * 3600 + m * 60 + s) * 100 + cs) * 10_000);
            let text = format_ass_time(d);
            prop_assert_eq!(parse_ass_time(&text), Some(d));
        }

        #[test]
        fn stl_round_trip(h in 0i64..24, m in 0i64..60, s in 0i64..60, hh in 0i64..100) {
            let d = Duration::from_micros(((h * 3600 + m * 60 + s) * 100 + hh) * 10_000);
            let text = format_stl_time(d);
            prop_assert_eq!(parse_stl_time(&text), Some(d));
        }

        #[test]
        fn lrc_round_trip(m in 0i64..600, s in 0i64..60, cs in 0i64..100) {
            let d = Duration::from_micros((m * 60 + s) * US_PER_SEC + cs * 10_000);
            let text = format_lrc_time(d);
            prop_assert_eq!(parse_lrc_time(&text), Some(d));
        }

        #[test]
        fn deciseconds_round_trip(n in 0i64..1_000_000) {
            let d = parse_deciseconds(&n.to_string()).unwrap();
            prop_assert_eq!(format_deciseconds(d), n);
        }

        #[test]
        fn microdvd_frame_round_trip(frame in 0i64..1_000_000) {
            let d = microdvd_frame_to_duration(frame, MICRODVD_DEFAULT_FPS);
            let back = microdvd_duration_to_frame(d, MICRODVD_DEFAULT_FPS);
            prop_assert_eq!(back, frame);
        }

        #[test]
        fn no_time_parser_panics_on_arbitrary_text(s in ".{0,64}") {
            let _ = parse_srt_time(&s);
            let _ = parse_vtt_time(&s);
            let _ = parse_ass_time(&s);
            let _ = parse_subviewer_time(&s);
            let _ = parse_subviewer1_time(&s);
            let _ = parse_vplayer_time(&s);
            let _ = parse_lrc_time(&s);
            let _ = parse_stl_time(&s);
            let _ = parse_sami_millis(&s);
            let _ = parse_realtext_time(&s);
            let _ = parse_deciseconds(&s);
            let _ = parse_seconds(&s);
        }
    }
}
