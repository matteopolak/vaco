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

/// Reproducibility fields attached to one macro child result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandProvenance {
    /// Exact executable and arguments after placeholder substitution.
    pub argv: Vec<String>,
    /// First complete version/configuration record emitted by the executable.
    pub version: String,
}

/// Render one stable JSON object without accepting unescaped command text.
#[must_use]
pub fn json_record(observation: ResourceObservation, provenance: &CommandProvenance) -> String {
    let quote = |value: &str| {
        format!(
            "\"{}\"",
            value
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
        )
    };
    let argv = provenance
        .argv
        .iter()
        .map(|arg| quote(arg))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"schema\":1,\"cpu_seconds\":{},\"peak_rss_bytes\":{},\"argv\":[{}],\"version\":{}}}",
        observation.cpu_seconds,
        observation.peak_rss_bytes,
        argv,
        quote(&provenance.version)
    )
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
    use super::{CommandProvenance, ResourceObservation, json_record, parse_macos_time_l};
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
    #[test]
    fn record_escapes_exact_argv_and_version() {
        let row = json_record(
            ResourceObservation {
                cpu_seconds: 0.1,
                peak_rss_bytes: 2,
            },
            &CommandProvenance {
                argv: vec!["vaco\"x".into()],
                version: "vaco\nconfig".into(),
            },
        );
        assert_eq!(
            row,
            "{\"schema\":1,\"cpu_seconds\":0.1,\"peak_rss_bytes\":2,\"argv\":[\"vaco\\\"x\"],\"version\":\"vaco\\nconfig\"}"
        );
    }
}
