//! External Linux `perf stat` counter parsing and process execution.

#![cfg_attr(
    not(any(test, target_os = "linux")),
    allow(
        dead_code,
        reason = "the parser is exercised on non-Linux only by unit tests"
    )
)]

use std::fmt;

use crate::ChildBatchMode;

#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicU64, Ordering};

const MINIMUM_RUNNING_PERCENT: f64 = 99.0;
#[cfg(target_os = "linux")]
static COUNTER_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "off-Linux tests construct the unsupported request"
    )
)]
pub(crate) struct BatchCommand<'a> {
    pub(crate) mode: ChildBatchMode,
    pub(crate) name: &'a str,
    pub(crate) iterations: usize,
    pub(crate) outcome: &'a str,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PerfStatError {
    MissingCycles,
    Unavailable(String),
    MalformedCount(String),
    MalformedRunningPercent(String),
    Multiplexed(f64),
    CountOverflow,
    #[cfg(not(target_os = "linux"))]
    UnsupportedPlatform,
    #[cfg(target_os = "linux")]
    Launch(String),
    #[cfg(target_os = "linux")]
    CommandFailed(String),
    #[cfg(target_os = "linux")]
    Read(String),
}

impl fmt::Display for PerfStatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingCycles => formatter.write_str("perf stat reported no CPU-cycle event"),
            Self::Unavailable(value) => {
                write!(formatter, "CPU-cycle event is unavailable: {value}")
            }
            Self::MalformedCount(value) => {
                write!(
                    formatter,
                    "perf stat returned an invalid cycle count: {value}"
                )
            }
            Self::MalformedRunningPercent(value) => write!(
                formatter,
                "perf stat returned an invalid counter-running percentage: {value}"
            ),
            Self::Multiplexed(percent) => write!(
                formatter,
                "CPU-cycle event ran for only {percent:.2}% of the measurement"
            ),
            Self::CountOverflow => formatter.write_str("summed CPU-cycle count exceeds u64"),
            #[cfg(not(target_os = "linux"))]
            Self::UnsupportedPlatform => formatter.write_str("perf-stat CPU cycles require Linux"),
            #[cfg(target_os = "linux")]
            Self::Launch(detail) => write!(formatter, "could not launch perf stat: {detail}"),
            #[cfg(target_os = "linux")]
            Self::CommandFailed(detail) => write!(formatter, "perf stat failed: {detail}"),
            #[cfg(target_os = "linux")]
            Self::Read(detail) => write!(formatter, "could not read perf stat output: {detail}"),
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn measure_cycles(_batch: &BatchCommand<'_>) -> Result<u64, PerfStatError> {
    Err(PerfStatError::UnsupportedPlatform)
}

#[cfg(target_os = "linux")]
pub(crate) fn measure_cycles(batch: &BatchCommand<'_>) -> Result<u64, PerfStatError> {
    let sequence = COUNTER_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::current_dir()
        .map_err(|error| PerfStatError::Read(error.to_string()))?
        .join(format!(
            ".vaco-perf-stat-{}-{sequence}.csv",
            std::process::id()
        ));
    let _counter_file = CounterFile(path.clone());
    let executable =
        std::env::current_exe().map_err(|error| PerfStatError::Launch(error.to_string()))?;
    let perf = std::env::var_os("VACO_BENCH_PERF").unwrap_or_else(|| "perf".into());
    let mode = match batch.mode {
        ChildBatchMode::Work => "work",
        ChildBatchMode::Control => "control",
    };
    let output = Command::new(perf)
        .args(["stat", "--no-big-num", "-x", ";", "-e", "cycles:u", "-o"])
        .arg(&path)
        .arg("--")
        .arg(executable)
        .arg("__filter-batch")
        .arg(mode)
        .arg(batch.name)
        .arg(batch.iterations.to_string())
        .arg(batch.outcome)
        .env("LC_ALL", "C")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| PerfStatError::Launch(error.to_string()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(PerfStatError::CommandFailed(stderr.trim().to_owned()));
    }
    let captured =
        std::fs::read_to_string(&path).map_err(|error| PerfStatError::Read(error.to_string()))?;
    parse_cycles(&captured)
}

#[cfg(target_os = "linux")]
struct CounterFile(std::path::PathBuf);

#[cfg(target_os = "linux")]
impl Drop for CounterFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub(crate) fn parse_cycles(output: &str) -> Result<u64, PerfStatError> {
    let mut total = None;
    for line in output.lines() {
        let mut fields = line.split(';').map(str::trim);
        let Some(raw_count) = fields.next() else {
            continue;
        };
        let _unit = fields.next();
        let Some(event) = fields.next() else {
            continue;
        };
        if !is_cycle_event(event) {
            continue;
        }

        if raw_count.starts_with('<') {
            return Err(PerfStatError::Unavailable(raw_count.to_owned()));
        }
        let count = raw_count
            .parse::<u64>()
            .map_err(|_| PerfStatError::MalformedCount(raw_count.to_owned()))?;
        let _runtime = fields.next();
        if let Some(raw_percent) = fields.next().filter(|value| !value.is_empty()) {
            let raw_percent = raw_percent.trim_end_matches('%');
            let percent = raw_percent
                .parse::<f64>()
                .map_err(|_| PerfStatError::MalformedRunningPercent(raw_percent.to_owned()))?;
            if percent < MINIMUM_RUNNING_PERCENT {
                return Err(PerfStatError::Multiplexed(percent));
            }
        }
        total = Some(
            total
                .unwrap_or(0_u64)
                .checked_add(count)
                .ok_or(PerfStatError::CountOverflow)?,
        );
    }
    total.ok_or(PerfStatError::MissingCycles)
}

fn is_cycle_event(event: &str) -> bool {
    let base = event.split_once(':').map_or(event, |(base, _)| base);
    base == "cycles" || base.split('/').any(|part| part == "cycles")
}

#[cfg(test)]
mod tests {
    use super::{BatchCommand, PerfStatError, measure_cycles, parse_cycles};
    use crate::ChildBatchMode;

    #[test]
    fn parses_a_direct_unmultiplexed_cycle_count() {
        let captured = "1250000;;cycles:u;500000;100.00;;\n";

        assert_eq!(parse_cycles(captured), Ok(1_250_000));
    }

    #[test]
    fn sums_direct_hybrid_pmu_cycle_rows() {
        let captured = concat!(
            "750000;;cpu_core/cycles/u;500000;100.00;;\n",
            "250000;;cpu_atom/cycles/u;500000;100.00;;\n",
        );

        assert_eq!(parse_cycles(captured), Ok(1_000_000));
    }

    #[test]
    fn rejects_a_multiplexed_cycle_count() {
        let captured = "1250000;;cycles:u;500000;97.50;;\n";

        assert_eq!(
            parse_cycles(captured),
            Err(PerfStatError::Multiplexed(97.5))
        );
    }

    #[test]
    fn rejects_not_counted_and_not_supported_events() {
        for unavailable in ["<not counted>", "<not supported>"] {
            let captured = format!("{unavailable};;cycles:u;0;0.00;;\n");
            assert_eq!(
                parse_cycles(&captured),
                Err(PerfStatError::Unavailable(unavailable.to_owned()))
            );
        }
    }

    #[test]
    fn rejects_a_malformed_cycle_count() {
        let captured = "twelve;;cycles:u;500000;100.00;;\n";

        assert_eq!(
            parse_cycles(captured),
            Err(PerfStatError::MalformedCount("twelve".to_owned()))
        );
    }

    #[test]
    fn rejects_output_without_a_cycle_event() {
        let captured = "0.000123;;seconds time elapsed;;;;\n";

        assert_eq!(parse_cycles(captured), Err(PerfStatError::MissingCycles));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn external_counter_is_explicitly_unavailable_off_linux() {
        let batch = BatchCommand {
            mode: ChildBatchMode::Control,
            name: "null",
            iterations: 1,
            outcome: "created",
        };

        assert!(matches!(
            measure_cycles(&batch),
            Err(PerfStatError::UnsupportedPlatform)
        ));
    }
}
