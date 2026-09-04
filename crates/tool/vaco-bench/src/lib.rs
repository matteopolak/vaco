#![forbid(unsafe_code)]
//! Registry-complete benchmark measurement and comparison.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::File;
use std::hint::black_box;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use vaco_filter_graph::registry::{FilterRegistry, Instantiate};

const TRAILING_BASELINES: usize = 7;
static SANDBOX_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static SANDBOX_LOCK: Mutex<()> = Mutex::new(());

/// Temporary working directory that contains relative files created by filters.
///
/// Some filter constructors open their default output when they are instantiated.
/// Keep this guard alive for every benchmark invocation so those files cannot
/// escape into the checkout. Dropping the guard restores the original working
/// directory and removes the owned temporary tree.
#[derive(Debug)]
pub struct BenchmarkSandbox {
    _lock: MutexGuard<'static, ()>,
    previous: PathBuf,
    scratch: PathBuf,
}

impl BenchmarkSandbox {
    /// Enter a fresh process-local benchmark working directory.
    ///
    /// # Errors
    ///
    /// Returns [`BenchError::Io`] if the current directory cannot be read or the
    /// temporary directory cannot be created or entered.
    pub fn enter() -> Result<Self, BenchError> {
        let lock = SANDBOX_LOCK
            .lock()
            .map_err(|_| BenchError::SandboxPoisoned)?;
        let previous = std::env::current_dir().map_err(BenchError::Io)?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| BenchError::Clock(error.to_string()))?
            .as_nanos();
        let sequence = SANDBOX_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let scratch = std::env::temp_dir().join(format!(
            "vaco-bench-{}-{timestamp}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&scratch).map_err(BenchError::Io)?;
        if let Err(error) = std::env::set_current_dir(&scratch) {
            let _ = std::fs::remove_dir(&scratch);
            return Err(BenchError::Io(error));
        }
        Ok(Self {
            _lock: lock,
            previous,
            scratch,
        })
    }
}

impl Drop for BenchmarkSandbox {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.previous);
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}

/// Controls the registry-complete filter benchmark.
#[derive(Debug, Clone, Copy)]
pub struct FilterBenchConfig {
    /// Untimed constructions before calibration.
    pub warmup_calls: usize,
    /// Independently timed batches per filter.
    pub samples: usize,
    /// Minimum duration used to amortise timer resolution.
    pub target_sample_ns: u64,
    /// Hard cap on constructions in one batch.
    pub max_iterations: usize,
}

impl Default for FilterBenchConfig {
    fn default() -> Self {
        Self {
            warmup_calls: 8,
            samples: 11,
            target_sample_ns: 100_000,
            max_iterations: 1 << 20,
        }
    }
}

impl FilterBenchConfig {
    fn validate(self) -> Result<Self, BenchError> {
        if self.samples == 0 {
            return Err(BenchError::InvalidConfig("samples must be positive"));
        }
        if self.max_iterations == 0 {
            return Err(BenchError::InvalidConfig("max iterations must be positive"));
        }
        Ok(self)
    }
}

/// One generated argument to the per-filter Divan suite.
#[derive(Clone, Copy)]
pub struct FilterCase {
    name: &'static str,
}

impl FilterCase {
    /// The production registry name used as the benchmark id.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }
}

impl fmt::Debug for FilterCase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name)
    }
}

/// Generate exactly one case from every enabled filter descriptor.
pub fn filter_cases() -> impl Iterator<Item = FilterCase> {
    vaco_registry::filters()
        .iter()
        .map(|descriptor| FilterCase {
            name: descriptor.name,
        })
}

/// Construct one filter through the production generated registry.
///
/// `rejected` is a valid stable outcome: filters with mandatory arguments are
/// expected to reject the deliberately empty default request.
#[must_use]
pub fn instantiate_filter(case: &FilterCase) -> &'static str {
    let arguments = [];
    let request = Instantiate {
        name: case.name,
        instance: case.name,
        args: None,
        arguments: &arguments,
    };
    match vaco_registry::Filters.create(&request) {
        Ok(instance) => {
            black_box(instance);
            "created"
        }
        Err(error) => {
            black_box(error);
            "rejected"
        }
    }
}

/// Distribution summary in nanoseconds per construction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Statistics {
    /// Median sample.
    pub median: f64,
    /// Median absolute deviation from [`Statistics::median`].
    pub mad: f64,
    /// Least-noisy observed sample.
    pub min: f64,
    /// Nearest-rank 95th percentile.
    pub p95: f64,
}

/// Host and toolchain fields that must agree before results are comparable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MachineFingerprint {
    /// Stable runner class, optionally set through `VACO_BENCH_MACHINE`.
    pub machine: String,
    /// Rust target operating system.
    pub os: String,
    /// Rust target architecture.
    pub arch: String,
    /// Host CPU model when the platform exposes it.
    pub cpu: String,
    /// Full `rustc --version` line.
    pub rustc: String,
    /// `debug` or `release`, derived from the built binary.
    pub profile: String,
}

impl MachineFingerprint {
    /// Read a comparison fingerprint from the current host and binary.
    #[must_use]
    pub fn detect() -> Self {
        let os = std::env::consts::OS.to_owned();
        let arch = std::env::consts::ARCH.to_owned();
        let cpu = cpu_model().unwrap_or_else(|| "unknown".to_owned());
        let machine = std::env::var("VACO_BENCH_MACHINE")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| format!("{os}-{arch}-{}", slug(&cpu)));
        let rustc =
            command_line("rustc", &["--version"]).unwrap_or_else(|| "rustc unknown".to_owned());
        let profile = if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        };
        Self {
            machine,
            os,
            arch,
            cpu,
            rustc,
            profile: profile.to_owned(),
        }
    }
}

/// One filter measurement and its complete comparison identity.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchResult {
    /// Registered filter name.
    pub benchmark: String,
    /// Work included in the timed closure.
    pub scope: &'static str,
    /// Whether the empty request constructs or deliberately rejects.
    pub outcome: &'static str,
    /// Timer implementation.
    pub backend: &'static str,
    /// Timer unit.
    pub unit: &'static str,
    /// Independently timed batches.
    pub samples: usize,
    /// Constructions inside each batch.
    pub iterations: usize,
    /// Per-construction timing distribution.
    pub stats: Statistics,
    /// Host and compiler identity.
    pub fingerprint: MachineFingerprint,
    /// Commit measured, or `unknown` outside a Git checkout.
    pub git_sha: String,
    /// Wall-clock timestamp used to select the trailing baseline window.
    pub measured_unix_ms: i64,
    /// One-minute load average, recorded as context rather than identity.
    pub load_average_1m: Option<f64>,
    /// Trailing baseline median divided by this result's median.
    pub baseline_ratio: Option<f64>,
}

/// Summarize a non-empty sample set.
#[must_use]
pub fn summarize(samples: &[f64]) -> Option<Statistics> {
    let mut ordered = samples.to_vec();
    ordered.sort_by(f64::total_cmp);
    let median = sample_median(&ordered)?;
    let mut deviations: Vec<_> = ordered
        .iter()
        .map(|sample| (sample - median).abs())
        .collect();
    deviations.sort_by(f64::total_cmp);
    let mad = sample_median(&deviations)?;
    let min = *ordered.first()?;
    let rank = ordered
        .len()
        .saturating_mul(95)
        .div_ceil(100)
        .saturating_sub(1);
    let p95 = *ordered.get(rank).or_else(|| ordered.last())?;
    Some(Statistics {
        median,
        mad,
        min,
        p95,
    })
}

fn sample_median(ordered: &[f64]) -> Option<f64> {
    let middle = 0usize.midpoint(ordered.len());
    let upper = *ordered.get(middle)?;
    if ordered.len() % 2 == 1 {
        Some(upper)
    } else {
        let lower = *ordered.get(middle.saturating_sub(1))?;
        Some((lower + upper) / 2.0)
    }
}

/// Measure one default instantiation for every registered filter.
///
/// # Errors
///
/// Returns an error for invalid sampling configuration, duplicate registry
/// names, a changing construction outcome, or an unavailable system clock.
pub fn run_filter_suite(config: &FilterBenchConfig) -> Result<Vec<BenchResult>, BenchError> {
    let config = config.validate()?;
    let cases: Vec<_> = filter_cases().collect();
    if cases.is_empty() {
        return Err(BenchError::EmptyRegistry);
    }
    let mut names = BTreeSet::new();
    for case in &cases {
        if !names.insert(case.name) {
            return Err(BenchError::DuplicateFilter(case.name));
        }
    }

    let fingerprint = MachineFingerprint::detect();
    let git_sha =
        command_line("git", &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_owned());
    let measured_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| BenchError::Clock(error.to_string()))?
        .as_millis()
        .try_into()
        .map_err(|_| BenchError::Clock("timestamp exceeds i64".to_owned()))?;
    let load_average_1m = load_average_1m();
    let _sandbox = BenchmarkSandbox::enter()?;
    let mut results = Vec::new();
    for case in &cases {
        let (iterations, outcome, stats) = benchmark_case(case, config)?;
        results.push(BenchResult {
            benchmark: case.name.to_owned(),
            scope: "instantiate",
            outcome,
            backend: "instant",
            unit: "ns",
            samples: config.samples,
            iterations,
            stats,
            fingerprint: fingerprint.clone(),
            git_sha: git_sha.clone(),
            measured_unix_ms,
            load_average_1m,
            baseline_ratio: None,
        });
    }
    Ok(results)
}

fn benchmark_case(
    case: &FilterCase,
    config: FilterBenchConfig,
) -> Result<(usize, &'static str, Statistics), BenchError> {
    for _ in 0..config.warmup_calls {
        black_box(instantiate_filter(case));
    }

    let target = Duration::from_nanos(config.target_sample_ns);
    let mut iterations = 1usize;
    let outcome = loop {
        let (elapsed, observed) = measure_batch(case, iterations, None)?;
        if elapsed >= target || iterations >= config.max_iterations {
            break observed;
        }
        iterations = iterations.saturating_mul(2).min(config.max_iterations);
    };

    let mut samples = Vec::new();
    for _ in 0..config.samples {
        let (elapsed, _) = measure_batch(case, iterations, Some(outcome))?;
        samples.push(duration_ns(elapsed) / iterations as f64);
    }
    let stats = summarize(&samples).ok_or(BenchError::NoSamples)?;
    Ok((iterations, outcome, stats))
}

fn measure_batch(
    case: &FilterCase,
    iterations: usize,
    expected: Option<&'static str>,
) -> Result<(Duration, &'static str), BenchError> {
    let start = Instant::now();
    let mut outcome = None;
    for _ in 0..iterations {
        let observed = instantiate_filter(case);
        if let Some(want) = expected.or(outcome)
            && observed != want
        {
            return Err(BenchError::InconsistentOutcome(case.name));
        }
        outcome = Some(observed);
        black_box(observed);
    }
    let elapsed = start.elapsed();
    outcome
        .map(|observed| (elapsed, observed))
        .ok_or(BenchError::NoSamples)
}

fn duration_ns(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000_000_000.0
}

/// Write one valid JSON object per result line.
///
/// # Errors
///
/// Returns [`BenchError::Io`] if the destination cannot be written.
pub fn write_jsonl(path: &Path, results: &[BenchResult]) -> Result<(), BenchError> {
    let file = File::create(path).map_err(BenchError::Io)?;
    let mut writer = BufWriter::new(file);
    for row in results {
        writeln!(writer, "{}", json_line(row)).map_err(BenchError::Io)?;
    }
    writer.flush().map_err(BenchError::Io)
}

fn json_line(row: &BenchResult) -> String {
    format!(
        concat!(
            "{{\"schema\":1,\"suite\":\"filter\",\"benchmark\":{},",
            "\"scope\":{},\"outcome\":{},\"backend\":{},\"unit\":{},",
            "\"samples\":{},\"iterations\":{},\"median\":{},\"mad\":{},",
            "\"min\":{},\"p95\":{},\"baseline_ratio\":{},",
            "\"machine\":{},\"os\":{},\"arch\":{},\"cpu\":{},",
            "\"rustc\":{},\"profile\":{},\"git_sha\":{},",
            "\"measured_unix_ms\":{},\"load_average_1m\":{}}}"
        ),
        json_string(&row.benchmark),
        json_string(row.scope),
        json_string(row.outcome),
        json_string(row.backend),
        json_string(row.unit),
        row.samples,
        row.iterations,
        row.stats.median,
        row.stats.mad,
        row.stats.min,
        row.stats.p95,
        option_number(row.baseline_ratio),
        json_string(&row.fingerprint.machine),
        json_string(&row.fingerprint.os),
        json_string(&row.fingerprint.arch),
        json_string(&row.fingerprint.cpu),
        json_string(&row.fingerprint.rustc),
        json_string(&row.fingerprint.profile),
        json_string(&row.git_sha),
        row.measured_unix_ms,
        option_number(row.load_average_1m),
    )
}

fn json_string(value: &str) -> String {
    let mut output = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            other => output.push(other),
        }
    }
    output.push('"');
    output
}

fn option_number(value: Option<f64>) -> String {
    value.map_or_else(|| "null".to_owned(), |number| number.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Identity {
    benchmark: String,
    scope: String,
    outcome: String,
    backend: String,
    unit: String,
    machine: String,
    os: String,
    arch: String,
    cpu: String,
    rustc: String,
    profile: String,
}

impl Identity {
    fn from_result(row: &BenchResult) -> Self {
        Self {
            benchmark: row.benchmark.clone(),
            scope: row.scope.to_owned(),
            outcome: row.outcome.to_owned(),
            backend: row.backend.to_owned(),
            unit: row.unit.to_owned(),
            machine: row.fingerprint.machine.clone(),
            os: row.fingerprint.os.clone(),
            arch: row.fingerprint.arch.clone(),
            cpu: row.fingerprint.cpu.clone(),
            rustc: row.fingerprint.rustc.clone(),
            profile: row.fingerprint.profile.clone(),
        }
    }
}

#[derive(Debug, Default)]
struct Baseline {
    measurements: BTreeMap<Identity, Vec<(i64, f64)>>,
}

/// Attach trailing-median ratios from like-for-like JSONL history.
///
/// A missing path is a first run and attaches zero rows rather than failing.
///
/// # Errors
///
/// Returns [`BenchError::Io`] for unreadable existing files and
/// [`BenchError::InvalidBaseline`] for malformed or foreign-schema rows.
pub fn apply_baseline(results: &mut [BenchResult], path: &Path) -> Result<usize, BenchError> {
    let Some(mut baseline) = load_baseline(path)? else {
        return Ok(0);
    };
    let mut matched = 0usize;
    for row in results {
        let key = Identity::from_result(row);
        let Some(history) = baseline.measurements.get_mut(&key) else {
            continue;
        };
        history.sort_by_key(|(timestamp, _)| *timestamp);
        let mut trailing: Vec<_> = history
            .iter()
            .rev()
            .take(TRAILING_BASELINES)
            .map(|(_, median)| *median)
            .collect();
        trailing.sort_by(f64::total_cmp);
        let Some(previous) = sample_median(&trailing) else {
            continue;
        };
        if row.stats.median > 0.0 {
            row.baseline_ratio = Some(previous / row.stats.median);
            matched = matched.saturating_add(1);
        }
    }
    Ok(matched)
}

fn load_baseline(path: &Path) -> Result<Option<Baseline>, BenchError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(BenchError::Io(error)),
    };
    let mut baseline = Baseline::default();
    for line in BufReader::new(file).lines() {
        let line = line.map_err(BenchError::Io)?;
        if line.trim().is_empty() {
            continue;
        }
        let row = baseline_row(&line).ok_or_else(|| BenchError::InvalidBaseline(line.clone()))?;
        baseline.measurements.entry(row.0).or_default().push(row.1);
    }
    Ok(Some(baseline))
}

fn baseline_row(line: &str) -> Option<(Identity, (i64, f64))> {
    if integer_field(line, "schema")? != 1 || string_field(line, "suite")? != "filter" {
        return None;
    }
    let identity = Identity {
        benchmark: string_field(line, "benchmark")?,
        scope: string_field(line, "scope")?,
        outcome: string_field(line, "outcome")?,
        backend: string_field(line, "backend")?,
        unit: string_field(line, "unit")?,
        machine: string_field(line, "machine")?,
        os: string_field(line, "os")?,
        arch: string_field(line, "arch")?,
        cpu: string_field(line, "cpu")?,
        rustc: string_field(line, "rustc")?,
        profile: string_field(line, "profile")?,
    };
    let timestamp = integer_field(line, "measured_unix_ms")?;
    let median = number_field(line, "median")?;
    Some((identity, (timestamp, median)))
}

fn string_field(line: &str, name: &str) -> Option<String> {
    let marker = format!("\"{name}\":\"");
    let rest = line.split_once(&marker)?.1;
    let mut output = String::new();
    let mut escaped = false;
    for character in rest.chars() {
        if escaped {
            match character {
                'n' => output.push('\n'),
                'r' => output.push('\r'),
                't' => output.push('\t'),
                other => output.push(other),
            }
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '"' {
            return Some(output);
        } else {
            output.push(character);
        }
    }
    None
}

fn number_field(line: &str, name: &str) -> Option<f64> {
    raw_number_field(line, name)?.parse().ok()
}

fn integer_field(line: &str, name: &str) -> Option<i64> {
    raw_number_field(line, name)?.parse().ok()
}

fn raw_number_field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let marker = format!("\"{name}\":");
    let rest = line.split_once(&marker)?.1;
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    rest.get(..end)
}

/// Rows whose attached baseline ratio is below `threshold`.
#[must_use]
pub fn regressions(results: &[BenchResult], threshold: f64) -> Vec<&BenchResult> {
    results
        .iter()
        .filter(|row| row.baseline_ratio.is_some_and(|ratio| ratio < threshold))
        .collect()
}

fn cpu_model() -> Option<String> {
    if let Ok(contents) = std::fs::read_to_string("/proc/cpuinfo") {
        for line in contents.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            if matches!(key.trim(), "model name" | "Hardware") {
                return Some(value.trim().to_owned());
            }
        }
    }
    command_line("sysctl", &["-n", "machdep.cpu.brand_string"])
        .or_else(|| command_line("sysctl", &["-n", "hw.model"]))
        .or_else(|| std::env::var("PROCESSOR_IDENTIFIER").ok())
}

fn load_average_1m() -> Option<f64> {
    if let Ok(contents) = std::fs::read_to_string("/proc/loadavg") {
        return contents.split_whitespace().next()?.parse().ok();
    }
    if let Some(output) = command_line("sysctl", &["-n", "vm.loadavg"])
        && let Some(value) = output
            .trim_matches(|character: char| character == '{' || character == '}')
            .split_whitespace()
            .next()
            .and_then(|number| number.parse().ok())
    {
        return Some(value);
    }
    command_line("uptime", &[]).and_then(|output| parse_uptime_load_average(&output))
}

fn parse_uptime_load_average(output: &str) -> Option<f64> {
    let values = output
        .split_once("load averages:")
        .or_else(|| output.split_once("load average:"))?
        .1;
    values
        .split_whitespace()
        .next()?
        .trim_end_matches(',')
        .parse()
        .ok()
}

fn command_line(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn slug(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Benchmark setup, measurement, or baseline error.
#[derive(Debug)]
pub enum BenchError {
    /// Sampling configuration cannot produce a measurement.
    InvalidConfig(&'static str),
    /// The selected build contains no filters.
    EmptyRegistry,
    /// The generated registry exposed the same filter name twice.
    DuplicateFilter(&'static str),
    /// An operation changed between calibration and sampling.
    InconsistentOutcome(&'static str),
    /// A timing path unexpectedly produced no sample.
    NoSamples,
    /// A constructor panicked while holding the process-wide CWD guard.
    SandboxPoisoned,
    /// The system clock could not identify the measurement.
    Clock(String),
    /// JSONL I/O failed.
    Io(std::io::Error),
    /// A baseline row lacked the complete schema identity.
    InvalidBaseline(String),
}

impl fmt::Display for BenchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(detail) => write!(formatter, "invalid benchmark config: {detail}"),
            Self::EmptyRegistry => formatter.write_str("the enabled filter registry is empty"),
            Self::DuplicateFilter(name) => {
                write!(formatter, "filter benchmark id is duplicated: {name}")
            }
            Self::InconsistentOutcome(name) => {
                write!(
                    formatter,
                    "filter {name} changed construction outcome while measuring"
                )
            }
            Self::NoSamples => formatter.write_str("benchmark produced no samples"),
            Self::SandboxPoisoned => {
                formatter.write_str("benchmark working-directory lock is poisoned")
            }
            Self::Clock(detail) => write!(formatter, "benchmark clock failed: {detail}"),
            Self::Io(error) => write!(formatter, "benchmark I/O failed: {error}"),
            Self::InvalidBaseline(line) => {
                write!(formatter, "invalid benchmark JSONL line: {line}")
            }
        }
    }
}

impl std::error::Error for BenchError {}

#[cfg(test)]
mod tests {
    use super::parse_uptime_load_average;

    #[test]
    fn uptime_load_average_accepts_darwin_and_linux_labels() {
        assert_eq!(
            parse_uptime_load_average("14:10  up 3 days, 2 users, load averages: 2.54 2.82 3.02"),
            Some(2.54)
        );
        assert_eq!(
            parse_uptime_load_average(
                " 14:10:00 up 3 days, 2 users, load average: 0.42, 0.51, 0.60"
            ),
            Some(0.42)
        );
        assert_eq!(parse_uptime_load_average("unexpected output"), None);
    }
}
