//! `-v` / `-loglevel` values, and whether to print the banner.
//!
//! The banner decision is a pre-scan rather than a parsed option because the
//! reference prints it *before* validating the command line: `ffprobe -v
//! nonsense` prints the banner, then the error, and exits 1.
//!
//! Levels are `quiet panic fatal error warning info verbose debug trace` and
//! their numeric equivalents; `warn` is not one. `repeat` and `level` are
//! formatting flags stripped before the level is read. Informational output
//! appears at `info` (32) and above.

use std::ffi::OsStr;

/// `AV_LOG_INFO`. The banner prints at this level and above.
pub const INFO: i32 = 32;

/// The nine level names the reference accepts. Exact, not generous: it
/// rejects `warn`.
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

/// The effective log level argv asks for, defaulting to [`INFO`]. Last
/// occurrence wins. An unparseable value leaves the level alone, which is how
/// the reference behaves before rejecting it.
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
/// Two independent conditions: `-hide_banner` suppresses the banner only,
/// while a level below [`INFO`] suppresses every informational line.
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
        assert!(wants_banner(&["-i", "error"]));
    }
}
