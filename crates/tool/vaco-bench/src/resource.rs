//! Parse resource observations emitted by the platform `time` utility.
//!
//! Vaco's macOS baseline uses `/usr/bin/time -l`; this parser is deliberately
//! separate from process launch so recorded fixtures test every unit without
//! relaxing the macro runner's controlled-Linux preflight.

/// CPU time and peak resident set size for one completed child process.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResourceObservation {
    /// User plus system CPU time, in seconds.
    pub cpu_seconds: f64,
    /// Maximum resident set size, in bytes.
    pub peak_rss_bytes: u64,
}

/// Parse the relevant macOS `/usr/bin/time -l` lines.
///
/// # Errors
///
/// Returns an error when either required value is absent or malformed.
pub fn parse_macos_time_l(output: &str) -> Result<ResourceObservation, String> {
    let user = number_before(output, " user")?;
    let system = number_before(output, " system")?;
    let bytes = integer_before(output, " maximum resident set size")?;
    Ok(ResourceObservation {
        cpu_seconds: user + system,
        peak_rss_bytes: bytes,
    })
}

fn number_before(output: &str, suffix: &str) -> Result<f64, String> {
    output
        .lines()
        .find_map(|line| line.trim().strip_suffix(suffix))
        .map(str::trim)
        .ok_or_else(|| format!("missing{suffix}"))?
        .parse()
        .map_err(|_| format!("invalid{suffix}"))
}

fn integer_before(output: &str, suffix: &str) -> Result<u64, String> {
    output
        .lines()
        .find_map(|line| line.trim().strip_suffix(suffix))
        .map(str::trim)
        .ok_or_else(|| format!("missing{suffix}"))?
        .parse()
        .map_err(|_| format!("invalid{suffix}"))
}

#[cfg(test)]
mod tests {
    use super::{ResourceObservation, parse_macos_time_l};
    #[test]
    fn parses_cpu_and_peak_rss() {
        assert_eq!(
            parse_macos_time_l("0.12 user\n0.03 system\n12345 maximum resident set size\n"),
            Ok(ResourceObservation {
                cpu_seconds: 0.15,
                peak_rss_bytes: 12345
            })
        );
    }
    #[test]
    fn rejects_missing_fields() {
        assert!(parse_macos_time_l("0.1 user\n").is_err());
    }
}
