//! CL-17 (#208)'s `-progress <url>`: the same run data `crate::stats`
//! reports for `-stats`, reshaped into `key=value` lines and written to a
//! file/pipe/URL instead of a human-readable line on stderr.
//!
//! # Scope
//!
//! Real `ffmpeg` writes a block every `-stats_period` (default 0.5s) while
//! encoding runs, plus a final one. [`vaco_sched::Driver`] runs a pipeline to
//! completion in one blocking call with no progress callback — the same gap
//! `crate::stats`'s own module doc names — so there is no seam here to hang a
//! periodic write on without a scheduler change outside this crate's
//! ownership. What is implemented is the **final** block only, terminated by
//! `progress=end` exactly as the reference's own last block is.
//!
//! `fps=`, `speed=` and `out_time*=` are wall-clock/duration-derived the same
//! way `crate::stats`' fields are and are not byte-comparable to the
//! reference on any run; `frame=`, `stream_N_M_q=`, `bitrate=` and
//! `total_size=` are not, and are computed honestly from
//! [`crate::nullmux::OutputTally`] rather than faked.

use vaco_core::MediaType;
use vaco_time::Instant;

use crate::exec::RunSpec;

/// The last `-progress <url>` occurrence's target, if any — the same
/// last-flag-wins convention `crate::stats::wants_stats` and every other
/// global-option pre-scan in this crate uses.
#[must_use]
pub fn target<S: AsRef<std::ffi::OsStr>>(argv: &[S]) -> Option<String> {
    let mut found = None;
    let mut iter = argv.iter();
    while let Some(a) = iter.next() {
        if a.as_ref() == std::ffi::OsStr::new("-progress") {
            found = iter.next().and_then(|v| v.as_ref().to_str()).map(str::to_owned);
        }
    }
    found
}

/// Render the one `-progress` block this build can produce: the final state,
/// computed at the moment the pipeline finished. See the module docs for why
/// there is only one.
#[must_use]
pub fn render(report: &RunSpec, started: Instant) -> String {
    let elapsed_secs = started.elapsed().as_secs_f64();

    let frames: u64 = report
        .tallies
        .iter()
        .flat_map(|t| &t.streams)
        .filter(|s| s.media == Some(MediaType::Video))
        .map(|s| s.packets)
        .sum();

    let total_bytes: Option<u64> =
        report
            .total_bytes
            .iter()
            .copied()
            .fold(None, |acc, b| match (acc, b) {
                (None, x) => x,
                (Some(a), Some(x)) => Some(a + x),
                (acc, None) => acc,
            });

    let time_secs = report.input_duration_secs.unwrap_or(0.0);
    let fps = if elapsed_secs > 0.0 {
        frames as f64 / elapsed_secs
    } else {
        0.0
    };
    let speed = if elapsed_secs > 0.0 && time_secs > 0.0 {
        time_secs / elapsed_secs
    } else {
        0.0
    };

    let bitrate = if time_secs > 0.0 {
        total_bytes.map_or_else(
            || "N/A".to_owned(),
            |b| format!("{:6.1}kbits/s", (b as f64) * 8.0 / 1000.0 / time_secs),
        )
    } else {
        "N/A".to_owned()
    };
    let total_size = total_bytes.map_or_else(|| "N/A".to_owned(), |b| b.to_string());

    let out_time_us = (time_secs * 1_000_000.0).round() as i64;

    let mut lines = vec![format!("frame={frames}"), format!("fps={fps:.2}")];
    for (oi, tally) in report.tallies.iter().enumerate() {
        for (si, _) in tally.streams.iter().enumerate() {
            lines.push(format!("stream_{oi}_{si}_q=-1.0"));
        }
    }
    lines.push(format!("bitrate={bitrate}"));
    lines.push(format!("total_size={total_size}"));
    lines.push(format!("out_time_us={out_time_us}"));
    // Measured, and reproduced rather than repaired: the reference's own
    // `out_time_ms` carries **microseconds**, the same value as
    // `out_time_us` above, not milliseconds.
    lines.push(format!("out_time_ms={out_time_us}"));
    lines.push(format!("out_time={}", clock(time_secs)));
    lines.push("dup_frames=0".to_owned());
    lines.push("drop_frames=0".to_owned());
    lines.push(format!("speed={}x", format_g3(speed)));
    lines.push("progress=end".to_owned());
    lines.join("\n") + "\n"
}

/// `HH:MM:SS.ffffff` — microsecond precision, distinct from `-stats`'
/// centisecond `clock_for_stats`.
fn clock(secs: f64) -> String {
    let total_us = (secs * 1_000_000.0).round().max(0.0) as u64;
    let us = total_us.rem_euclid(1_000_000);
    let total_s = total_us.div_euclid(1_000_000);
    let s = total_s.rem_euclid(60);
    let total_m = total_s.div_euclid(60);
    let m = total_m.rem_euclid(60);
    let h = total_m.div_euclid(60);
    format!("{h:02}:{m:02}:{s:02}.{us:06}")
}

/// C's `%.3g`: three significant digits, fixed or scientific notation
/// depending on magnitude, with trailing zeros (and a bare trailing `.`)
/// stripped — measured (`ffmpeg 8.1`, `-progress`): `speed=1.8e+03x`, not
/// `1.80e+03x`.
fn format_g3(value: f64) -> String {
    if value == 0.0 || !value.is_finite() {
        return "0".to_owned();
    }
    let exp = value.abs().log10().floor() as i32;
    if (-4..3).contains(&exp) {
        let decimals = u8::try_from((2 - exp).max(0)).unwrap_or(0) as usize;
        strip_trailing_zeros(&format!("{value:.decimals$}"))
    } else {
        let mantissa = value / 10f64.powi(exp);
        let m = strip_trailing_zeros(&format!("{mantissa:.2}"));
        let sign = if exp >= 0 { '+' } else { '-' };
        format!("{m}e{sign}{:02}", exp.abs())
    }
}

fn strip_trailing_zeros(s: &str) -> String {
    if !s.contains('.') {
        return s.to_owned();
    }
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn target_takes_the_last_occurrence() {
        assert_eq!(target::<&str>(&[]), None);
        assert_eq!(
            target(&["-progress", "a.txt", "-progress", "b.txt"]),
            Some("b.txt".to_owned())
        );
        // A dangling `-progress` with nothing after it names nothing.
        assert_eq!(target(&["-progress"]), None);
    }

    #[test]
    fn the_clock_is_microsecond_precision() {
        assert_eq!(clock(0.8), "00:00:00.800000");
        assert_eq!(clock(3661.5), "01:01:01.500000");
    }

    #[test]
    fn g3_matches_the_measured_examples() {
        assert_eq!(format_g3(1800.0), "1.8e+03");
        assert_eq!(format_g3(5410.0), "5.41e+03");
        assert_eq!(format_g3(1.5), "1.5");
        assert_eq!(format_g3(0.0), "0");
    }

    #[test]
    fn render_ends_with_progress_end_and_has_the_measured_key_order() {
        let report = RunSpec::default();
        let s = render(&report, Instant::now());
        assert!(s.starts_with("frame=0\nfps="), "{s}");
        assert!(s.ends_with("progress=end\n"), "{s}");
        assert!(s.contains("out_time_us="), "{s}");
        assert!(s.contains("out_time_ms="), "{s}");
        assert!(s.contains("dup_frames=0\ndrop_frames=0\n"), "{s}");
    }
}
