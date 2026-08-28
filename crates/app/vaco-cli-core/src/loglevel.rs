//! `-v` / `-loglevel` values, and the one decision that depends on them
//! before argv has been parsed: whether to print the banner.
//!
//! # Why this is a pre-scan rather than a parsed option
//!
//! The reference prints the banner *before* it validates the command line —
//! `ffprobe -v nonsense` prints the banner and then the error, and exits 1.
//! So the banner decision cannot wait for a successful parse, and neither can
//! reading the level it depends on.
//!
//! # Measured
//!
//! `ffprobe -v <level> long.mp4`, counting `ffprobe version` in the output:
//!
//! ```text
//! quiet panic fatal error warning   16 24 31   no banner
//! info verbose debug trace          32 33 40   banner
//! warn                                         invalid: exit 1, banner
//! level+error  repeat+level+16  +error         no banner
//! ```
//!
//! So the rule is exactly `effective level >= 32`, the flag words are stripped
//! before the level is read, numeric levels are accepted, and anything that
//! does not parse leaves the banner on — which falls out of the pre-scan
//! ordering above rather than being a separate rule.

use std::ffi::OsStr;

/// `AV_LOG_INFO`. The banner prints at this level and above.
pub const INFO: i32 = 32;

/// The nine level names the reference accepts, with their numeric values.
///
/// Measured from its own error text: `Invalid loglevel "warn"` — the
/// abbreviation is *not* accepted, so this list is exact rather than
/// generous.
const LEVELS: &[(&str, i32)] = &[
    ("quiet", -8),
    ("panic", 0),
    ("fatal", 8),
    ("error", 16),
    ("warning", 24),
    ("info", 32),
    ("verbose", 40),
    ("debug", 48),
    ("trace", 56),
];

/// Parse a `-loglevel` value: `warning`, `32`, `repeat+level+error`, `+error`.
///
/// Returns `None` for anything the reference rejects, which is the caller's
/// signal to behave as though no level were given.
#[must_use]
pub fn parse(spec: &str) -> Option<i32> {
    // `repeat` and `level` are formatting flags that may precede the level in
    // any combination, including with a leading `+` and no flags at all.
    let mut level = None;
    for part in spec.split('+') {
        match part {
            "" | "repeat" | "level" => {}
            other => {
                let value = LEVELS
                    .iter()
                    .find(|(n, _)| *n == other)
                    .map(|(_, v)| *v)
                    .or_else(|| other.parse::<i32>().ok())?;
                level = Some(value);
            }
        }
    }
    level
}

/// The effective log level argv asks for, defaulting to [`INFO`].
///
/// Later occurrences win, matching the reference's last-wins handling of
/// global options. An unparseable value leaves the level alone rather than
/// failing here — the reference prints its informational output *before*
/// rejecting the value, so behaving as though it were absent reproduces that
/// without a rule of its own.
#[must_use]
pub fn level<S: AsRef<OsStr>>(argv: &[S]) -> i32 {
    let mut level = INFO;
    let mut args = argv.iter();
    while let Some(arg) = args.next() {
        let arg = arg.as_ref();
        if arg == OsStr::new("-v") || arg == OsStr::new("-loglevel") {
            if let Some(spec) = args.next().and_then(|v| v.as_ref().to_str())
                && let Some(parsed) = parse(spec)
            {
                level = parsed;
            }
        }
    }
    level
}

/// Whether argv asks for informational output — the banner, the `Stream
/// mapping:` block, the per-output `muxing overhead` line.
#[must_use]
pub fn prints_info<S: AsRef<OsStr>>(argv: &[S]) -> bool {
    level(argv) >= INFO
}

/// Whether argv asks for the banner to be printed.
///
/// Two independent conditions, and it is worth keeping them apart:
/// `-hide_banner` suppresses the banner **only**, while a level below [`INFO`]
/// suppresses every informational line. Measured, counting each in
/// `ffmpeg … -c copy -f mpegts`'s stderr:
///
/// ```text
///                banner   Stream mapping:   muxing overhead
/// -hide_banner      no          yes               yes
/// -v warning        no           no                no
/// -v info          yes          yes               yes
/// ```
#[must_use]
pub fn wants_banner<S: AsRef<OsStr>>(argv: &[S]) -> bool {
    !argv
        .iter()
        .any(|a| a.as_ref() == OsStr::new("-hide_banner"))
        && prints_info(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_measured_level_name_lands_on_the_right_side_of_the_threshold() {
        for (name, quiet) in [
            ("quiet", true),
            ("panic", true),
            ("fatal", true),
            ("error", true),
            ("warning", true),
            ("info", false),
            ("verbose", false),
            ("debug", false),
            ("trace", false),
        ] {
            assert_eq!(
                !wants_banner(&["-v", name, "x.mp4"]),
                quiet,
                "-v {name} put the banner on the wrong side"
            );
        }
    }

    #[test]
    fn the_threshold_is_thirty_two_exactly() {
        assert!(!wants_banner(&["-v", "31"]));
        assert!(wants_banner(&["-v", "32"]));
        assert!(wants_banner(&["-v", "33"]));
    }

    #[test]
    fn formatting_flags_are_stripped_before_the_level_is_read() {
        assert!(!wants_banner(&["-v", "level+error"]));
        assert!(!wants_banner(&["-v", "repeat+level+16"]));
        assert!(!wants_banner(&["-v", "+error"]));
        assert!(wants_banner(&["-v", "repeat+info"]));
    }

    #[test]
    fn an_invalid_level_leaves_the_banner_on() {
        // Measured: `ffprobe -v warn` prints the banner, then the error, and
        // exits 1. The abbreviation is not a level name.
        assert_eq!(parse("warn"), None);
        assert!(wants_banner(&["-v", "warn"]));
        assert!(wants_banner(&["-v", "nonsense"]));
    }

    #[test]
    fn loglevel_is_the_same_option_as_v_and_the_last_one_wins() {
        assert!(!wants_banner(&["-loglevel", "warning"]));
        assert!(wants_banner(&["-v", "error", "-v", "info"]));
        assert!(!wants_banner(&["-v", "info", "-v", "error"]));
    }

    #[test]
    fn hide_banner_suppresses_the_banner_but_not_the_informational_output() {
        // Measured: `ffmpeg -hide_banner … -f mpegts` still prints
        // `Stream mapping:` and the `muxing overhead` line; `-v warning`
        // suppresses all three.
        assert!(!wants_banner(&["-hide_banner"]));
        assert!(prints_info(&["-hide_banner"]));
        assert!(!prints_info(&["-v", "warning"]));
        assert!(prints_info(&["-v", "info"]));
    }

    #[test]
    fn hide_banner_still_wins_on_its_own() {
        assert!(!wants_banner(&["-hide_banner"]));
        assert!(!wants_banner(&["-v", "info", "-hide_banner"]));
        assert!(wants_banner(&["-i", "x.mp4"]));
    }

    #[test]
    fn a_level_shaped_filename_is_not_read_as_a_level() {
        // `-v` takes its value from the next argv element, so a file called
        // `error` is only a level when it follows `-v`.
        assert!(wants_banner(&["-i", "error"]));
    }
}
