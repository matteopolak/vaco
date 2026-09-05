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
    /// Complete version/configuration record emitted by the executable.
    pub version: String,
}

/// Render one stable JSON object without accepting unescaped command text.
#[must_use]
pub fn json_record(observation: ResourceObservation, provenance: &CommandProvenance) -> String {
    let argv = provenance
        .argv
        .iter()
        .map(|arg| json_string(arg))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"schema\":1,\"cpu_seconds\":{},\"peak_rss_bytes\":{},\"argv\":[{}],\"version\":{}}}",
        observation.cpu_seconds,
        observation.peak_rss_bytes,
        argv,
        json_string(&provenance.version)
    )
}

/// Escape one string for the stable JSONL schema used by macro benchmarks.
#[must_use]
pub(crate) fn json_string(value: &str) -> String {
    let mut rendered = String::with_capacity(value.len() + 2);
    rendered.push('"');
    for character in value.chars() {
        match character {
            '"' => rendered.push_str("\\\""),
            '\\' => rendered.push_str("\\\\"),
            '\n' => rendered.push_str("\\n"),
            '\r' => rendered.push_str("\\r"),
            '\t' => rendered.push_str("\\t"),
            character if character.is_control() => {
                let code = character as u32;
                rendered.push_str("\\u");
                for shift in [12, 8, 4, 0] {
                    let nibble = ((code >> shift) & 0x0f) as usize;
                    rendered.push(char::from_digit(nibble as u32, 16).unwrap_or('0'));
                }
            }
            character => rendered.push(character),
        }
    }
    rendered.push('"');
    rendered
}

/// Parse the relevant macOS `/usr/bin/time -l` lines.
///
/// # Errors
///
/// Returns an error when either required value is absent or malformed.
pub fn parse_macos_time_l(output: &str) -> Result<ResourceObservation, String> {
    let user = number_before(output, " user")?;
    let system = number_before(output, " sys")?;
    let bytes = integer_before(output, " maximum resident set size")?;
    Ok(ResourceObservation {
        cpu_seconds: user + system,
        peak_rss_bytes: bytes,
    })
}

fn number_before(output: &str, suffix: &str) -> Result<f64, String> {
    output
        .lines()
        .find_map(|line| value_before(line, suffix))
        .ok_or_else(|| format!("missing{suffix}"))?
        .parse()
        .map_err(|_| format!("invalid{suffix}"))
}

fn integer_before(output: &str, suffix: &str) -> Result<u64, String> {
    output
        .lines()
        .find_map(|line| value_before(line, suffix))
        .ok_or_else(|| format!("missing{suffix}"))?
        .parse()
        .map_err(|_| format!("invalid{suffix}"))
}

fn value_before<'a>(line: &'a str, suffix: &str) -> Option<&'a str> {
    let prefix = line.get(..line.find(suffix)?)?.trim_end();
    prefix.split_ascii_whitespace().next_back()
}

/// Parse the relevant GNU `/usr/bin/time -v` fields.
///
/// GNU time reports peak RSS in KiB, unlike macOS's byte count.
///
/// # Errors
///
/// Returns an error when either required value is absent, malformed, or cannot
/// be converted from KiB to bytes.
pub fn parse_gnu_time_v(output: &str) -> Result<ResourceObservation, String> {
    let user = value_after(output, "User time (seconds):")?
        .parse::<f64>()
        .map_err(|_| "invalid User time (seconds)".to_owned())?;
    let system = value_after(output, "System time (seconds):")?
        .parse::<f64>()
        .map_err(|_| "invalid System time (seconds)".to_owned())?;
    let kib = value_after(output, "Maximum resident set size (kbytes):")?
        .parse::<u64>()
        .map_err(|_| "invalid Maximum resident set size (kbytes)".to_owned())?;
    let peak_rss_bytes = kib
        .checked_mul(1024)
        .ok_or_else(|| "Maximum resident set size overflows bytes".to_owned())?;
    Ok(ResourceObservation {
        cpu_seconds: user + system,
        peak_rss_bytes,
    })
}

fn value_after<'a>(output: &'a str, label: &str) -> Result<&'a str, String> {
    output
        .lines()
        .find_map(|line| line.trim_start().strip_prefix(label))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("missing {label}"))
}

#[cfg(test)]
mod tests {
    use super::{
        CommandProvenance, ResourceObservation, json_record, parse_gnu_time_v, parse_macos_time_l,
    };
    #[test]
    fn parses_cpu_and_peak_rss() {
        assert_eq!(
            parse_macos_time_l(
                "0.15 real         0.12 user         0.03 sys\n12345  maximum resident set size\n",
            ),
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
    fn parses_gnu_cpu_and_peak_rss_kib() {
        assert_eq!(
            parse_gnu_time_v(
                "\tUser time (seconds): 0.12\n\tSystem time (seconds): 0.03\n\tMaximum resident set size (kbytes): 12\n",
            ),
            Ok(ResourceObservation {
                cpu_seconds: 0.15,
                peak_rss_bytes: 12 * 1024,
            })
        );
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
