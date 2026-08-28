//! CL-17 (#208)'s `-report`/`FFREPORT`: mirror everything this run writes to
//! stderr into a log file, plus the header line the reference always writes
//! first.
//!
//! # What is reproduced, and how it was measured
//!
//! `ffmpeg -report` (ffmpeg 8.1) opens `PROGRAM-YYYYMMDD-HHMMSS.log` in the
//! current directory, writes:
//!
//! ```text
//! ffmpeg started on 2026-08-28 at 16:32:52
//! Report written to "ffmpeg-20260828-163252.log"
//! Log level: 48
//! Command line:
//! ffmpeg -report -i in.mp4 -c copy -f null -
//! ```
//!
//! then duplicates everything it would otherwise print — the banner, the
//! per-option parse trace, every `Input #0`/`Output #0` block, `-stats` — into
//! the same file, forced to debug level in the file sink regardless of
//! `-loglevel` on the console. `FFREPORT` (`key=value` pairs joined by `:`,
//! keys `file` and `level`) triggers the same thing without `-report` on the
//! command line, and names its own filename/level.
//!
//! `PROGRAM` becomes `vaco` here (D9: this binary does not claim to be
//! `ffmpeg`), the header shape and the command-line echo are reproduced, and
//! everything this build actually writes to stderr is duplicated into the
//! file. What is not reproduced: the per-option parse trace and the
//! library-version block, because D9 already puts the reference's own
//! internal tracing and version strings outside what this project claims to
//! be — the same reasoning `listing::banner` already applies.
//!
//! # The timestamp is UTC, not local
//!
//! Resolving the system's local timezone needs a platform call this crate's
//! zero-FFI, `#![forbid(unsafe_code)]` surface has no way to make. The
//! filename and header timestamp are computed from UTC instead — a
//! documented, honest substitute, and not a byte-identity target either way:
//! `-stats`' `elapsed=`/`speed=` are the same class of run-dependent field.

use std::io::{self, Write};

use vaco_time::unix_nanos;

/// What `-report`/`FFREPORT` asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReportRequest {
    pub file: Option<String>,
    pub level: Option<i64>,
}

/// Whether argv or `FFREPORT` asks for a report.
///
/// `-report` on the command line wins over `FFREPORT` when both are given —
/// the reference's own "an explicit flag beats the environment" rule, applied
/// here the same way [`crate::stats::wants_stats`] applies "last flag wins"
/// to `-stats`/`-nostats`.
#[must_use]
pub fn wants_report<S: AsRef<std::ffi::OsStr>>(
    argv: &[S],
    ffreport_env: Option<&str>,
) -> Option<ReportRequest> {
    if argv.iter().any(|a| a.as_ref() == std::ffi::OsStr::new("-report")) {
        return Some(ReportRequest::default());
    }
    ffreport_env.map(|spec| parse_ffreport(spec))
}

fn parse_ffreport(spec: &str) -> ReportRequest {
    let mut req = ReportRequest::default();
    for kv in spec.split(':') {
        if let Some((k, v)) = kv.split_once('=') {
            match k {
                "file" => req.file = Some(v.to_owned()),
                "level" => req.level = v.parse().ok(),
                _ => {}
            }
        }
    }
    req
}

/// The reference's default filename shape, `PROGRAM-YYYYMMDD-HHMMSS.log`,
/// with `vaco` substituted for the reference's own name (D9). See the module
/// docs for why this is UTC rather than local time.
#[must_use]
pub fn default_filename(unix_secs: i64) -> String {
    let (y, mo, d, h, mi, s) = civil_from_unix(unix_secs);
    format!("vaco-{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}.log")
}

/// Howard Hinnant's `civil_from_days` (a well-known, public-domain proleptic-
/// Gregorian calendar algorithm — no external date/time crate, no OS
/// timezone call), extended with a time-of-day split. Returns
/// `(year, month, day, hour, minute, second)`.
#[allow(
    clippy::many_single_char_names,
    reason = "the algorithm's own variable names (era/doe/yoe/doy/mp), kept recognisable"
)]
fn civil_from_unix(unix_secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = unix_secs.div_euclid(86_400);
    let secs_of_day = unix_secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1); // [1, 31]
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1); // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    let h = u32::try_from(secs_of_day / 3600).unwrap_or(0);
    let mi = u32::try_from((secs_of_day % 3600) / 60).unwrap_or(0);
    let s = u32::try_from(secs_of_day % 60).unwrap_or(0);
    (y, m, d, h, mi, s)
}

/// Open the report file and write its measured header. `argv` is the whole
/// command line, joined with spaces and prefixed `vaco` — the same
/// substitution [`default_filename`] makes for the program name.
///
/// # Errors
/// The file's own I/O error — a caller should treat this as "no report this
/// run", not a fatal one; the reference itself does not abort a run over a
/// report file it could not open.
pub fn open<S: AsRef<std::ffi::OsStr>>(
    req: &ReportRequest,
    argv: &[S],
) -> io::Result<(std::fs::File, String)> {
    let now = unix_nanos().map_or(0, |n| (n / 1_000_000_000) as i64);
    let name = req.file.clone().unwrap_or_else(|| default_filename(now));
    let mut file = std::fs::File::create(&name)?;
    let (y, mo, d, h, mi, s) = civil_from_unix(now);
    let level = req.level.unwrap_or(48);
    let mut cmd = String::from("vaco");
    for a in argv {
        cmd.push(' ');
        cmd.push_str(&a.as_ref().to_string_lossy());
    }
    writeln!(file, "vaco started on {y:04}-{mo:02}-{d:02} at {h:02}:{mi:02}:{s:02} UTC")?;
    writeln!(file, "Report written to \"{name}\"")?;
    writeln!(file, "Log level: {level}")?;
    writeln!(file, "Command line:")?;
    writeln!(file, "{cmd}")?;
    Ok((file, name))
}

/// Duplicates every write onto a second sink, best-effort: a failure to write
/// the report file must not interrupt or fail the run, since the report is a
/// side channel and the reference's own behaviour is the same (a report file
/// that cannot be opened at all is diagnosed once by [`open`]'s caller and
/// the run proceeds without one; a report file that stops accepting writes
/// partway through is not treated as a run failure either).
pub struct Tee<'a, A: Write> {
    primary: &'a mut A,
    secondary: std::fs::File,
}

impl<A: Write> core::fmt::Debug for Tee<'_, A> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Tee").field("secondary", &self.secondary).finish_non_exhaustive()
    }
}

impl<'a, A: Write> Tee<'a, A> {
    pub fn new(primary: &'a mut A, secondary: std::fs::File) -> Self {
        Self { primary, secondary }
    }
}

impl<A: Write> Write for Tee<'_, A> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.primary.write(buf)?;
        let _ = self.secondary.write_all(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.primary.flush()?;
        let _ = self.secondary.flush();
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn wants_report_prefers_argv_over_the_environment() {
        assert_eq!(wants_report::<&str>(&[], None), None);
        assert!(wants_report(&["-report"], None).is_some());
        assert_eq!(
            wants_report::<&str>(&[], Some("file=x.log:level=32")),
            Some(ReportRequest {
                file: Some("x.log".to_owned()),
                level: Some(32),
            })
        );
        // Both given: argv wins, and its request carries no file/level of its
        // own — matching the reference's own "-report alone uses the
        // default name and level 48" behaviour.
        assert_eq!(
            wants_report(&["-report"], Some("file=x.log")),
            Some(ReportRequest::default())
        );
    }

    #[test]
    fn the_default_filename_matches_the_measured_shape() {
        // 2026-08-28 16:32:52 UTC (cross-checked against Python's
        // `datetime.utcfromtimestamp`).
        let unix = 1_787_934_772;
        assert_eq!(default_filename(unix), "vaco-20260828-163252.log");
    }

    #[test]
    fn civil_from_unix_matches_a_handful_of_known_instants() {
        assert_eq!(civil_from_unix(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil_from_unix(86_400), (1970, 1, 2, 0, 0, 0));
        // 2000 is a leap year (divisible by 400): 951_868_800 is exactly
        // 2000-03-01 00:00:00 UTC, cross-checked against Python.
        assert_eq!(civil_from_unix(951_868_800), (2000, 3, 1, 0, 0, 0));
    }

    #[test]
    fn tee_forwards_everything_to_the_primary_and_best_effort_to_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.log");
        let file = std::fs::File::create(&path).unwrap();
        let mut primary = Vec::new();
        {
            let mut tee = Tee::new(&mut primary, file);
            tee.write_all(b"hello\n").unwrap();
        }
        assert_eq!(primary, b"hello\n");
        assert_eq!(std::fs::read(&path).unwrap(), b"hello\n");
    }
}
