//! CL-17 (#208)'s `frame= fps= … speed= elapsed=` line, gated by `-stats`.
//!
//! # Scope
//!
//! The reference prints this line **repeatedly**, once every half second or
//! so while encoding runs, plus a final one. [`vaco_sched::Driver`] runs a
//! pipeline to completion in one blocking call with no progress callback, so
//! there is no seam here to hang a periodic print on without a scheduler
//! change outside this crate's ownership. What is implemented is the *final*
//! line only — everything the reference's last `-stats` line reports is
//! still true, just not the intermediate ones. `-progress` (the same data as
//! `key=value` pairs to a file or pipe) is not implemented at all: it is a
//! genuinely separate output sink, not a formatting variant of this line, and
//! is deferred to a follow-up (see the CL-17 issue for the split).
//!
//! # What cannot be byte-compared, and is not faked
//!
//! `speed=` and `elapsed=` are derived from this process's own wall-clock
//! runtime end to end. Two runs of the identical command produce different
//! numbers by construction, so nothing here is measured against the
//! reference for those two fields — there is no reference value to match, on
//! any run. `fps=` is the same kind of number for the same reason. All three
//! are computed honestly from [`vaco_time::Instant`] (routed through
//! `vaco-time` rather than `std::time`, per `cargo xtask time-gate`) rather
//! than hardcoded or omitted.
//!
//! `frame=`, `Lsize=`, `bitrate=` and `time=` are not wall-clock-derived and
//! *would* be comparable in principle, but `time=` here is approximated from
//! the input's own stated duration rather than the muxed presentation-time
//! range, because [`crate::nullmux::StreamTally`] carries packet counts and
//! byte counts and not timestamps. Documented rather than silently
//! approximated.

use vaco_core::MediaType;
use vaco_time::Instant;

use crate::exec::RunSpec;

/// Whether argv asks for `-stats`.
///
/// On by default (matching the reference); `-nostats` turns it off, and — as
/// with every other global option — the last occurrence of either spelling
/// wins. This scans argv directly rather than going through `vaco-cli-core`'s
/// option table, the same pattern `vaco_cli_core::loglevel` uses for
/// `-v`/`-loglevel`, because both are read before (and independently of) the
/// full parse that can fail.
#[must_use]
pub fn wants_stats<S: AsRef<std::ffi::OsStr>>(argv: &[S]) -> bool {
    let mut on = true;
    for a in argv {
        let a = a.as_ref();
        if a == std::ffi::OsStr::new("-stats") {
            on = true;
        } else if a == std::ffi::OsStr::new("-nostats") {
            on = false;
        }
    }
    on
}

/// Render the one `-stats` line this build can produce: the final state,
/// computed at the moment the pipeline finished.
///
/// `started` is when the run began (`Instant::now()`, captured by the caller
/// before the pipeline ran) — the only wall-clock reading this function
/// needs, and the only one it takes, so that every other field stays a pure
/// function of `report`.
#[must_use]
pub fn render(report: &RunSpec, started: Instant) -> String {
    let elapsed = started.elapsed();
    let elapsed_secs = elapsed.as_secs_f64();

    let frames: u64 = report
        .tallies
        .iter()
        .flat_map(|t| &t.streams)
        .filter(|s| s.media == Some(MediaType::Video))
        .map(|s| s.packets)
        .sum();

    // `Lsize`: the real muxed byte count when this run tracked one (every
    // seekable-file and real-pipe output does; a `NOFILE` container like
    // `null` does not, and reports `N/A` the same way the summary line's
    // `muxing overhead: unknown` does for the same reason).
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

    let kib = |b: u64| (b as f64 / 1024.0).round_ties_even() as u64;
    let lsize = total_bytes.map_or_else(|| "N/A".to_owned(), |b| format!("{}KiB", kib(b)));

    // `time=`: approximated from the input's stated duration (see module
    // docs) rather than a muxed presentation-time range this build does not
    // track.
    let time_secs = report.input_duration_secs.unwrap_or(0.0);
    let time = crate::dump::clock_for_stats(time_secs);

    let bitrate = if time_secs > 0.0 {
        total_bytes.map_or_else(
            || "N/A".to_owned(),
            |b| format!("{:.1}kbits/s", (b as f64) * 8.0 / 1000.0 / time_secs),
        )
    } else {
        "N/A".to_owned()
    };

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

    format!(
        "frame={frames:5} fps={fps:.1} q=-1.0 Lsize={lsize:>10} time={time} bitrate={bitrate:>10} speed={speed:.3}x elapsed={}",
        crate::dump::elapsed_for_stats(elapsed_secs)
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn stats_defaults_on_and_the_last_flag_wins() {
        assert!(wants_stats::<&str>(&[]));
        assert!(!wants_stats(&["-nostats"]));
        assert!(wants_stats(&["-nostats", "-stats"]));
        assert!(!wants_stats(&["-stats", "-nostats"]));
    }
}
