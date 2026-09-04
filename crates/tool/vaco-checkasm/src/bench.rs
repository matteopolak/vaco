//! Microbenchmark support for [`crate::Kernel`] adapters.
//!
//! The portable backend measures nanoseconds with [`std::time::Instant`]. It
//! deliberately reports `unit = "ns"`; it never turns elapsed time into a
//! synthetic cycle count. Each result includes a separately measured no-op
//! baseline, raw and corrected statistics, hot/cold cache state, and enough
//! identity fields for JSONL baseline comparison.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::hint::black_box;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::Kernel;

const MAX_SAMPLES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Metric {
    backend: &'static str,
    unit: &'static str,
}

impl Metric {
    const INSTANT_NANOS: Self = Self {
        backend: "instant",
        unit: "ns",
    };
    const PERF_EVENT_CYCLES: Self = Self {
        backend: "perf-event",
        unit: "cycles",
    };
}

#[derive(Debug)]
struct MeasurementError(String);

impl fmt::Display for MeasurementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

trait Measurement {
    fn metric(&self) -> Metric;
    fn measure(&mut self, work: &mut dyn FnMut()) -> Result<f64, MeasurementError>;
}

struct InstantMeasurement;

impl Measurement for InstantMeasurement {
    fn metric(&self) -> Metric {
        Metric::INSTANT_NANOS
    }

    fn measure(&mut self, work: &mut dyn FnMut()) -> Result<f64, MeasurementError> {
        let start = Instant::now();
        work();
        Ok(duration_ns(start.elapsed()))
    }
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
struct PerfEventMeasurement(vaco_hw_perf_event::CpuCycles);

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
impl Measurement for PerfEventMeasurement {
    fn metric(&self) -> Metric {
        Metric::PERF_EVENT_CYCLES
    }

    fn measure(&mut self, work: &mut dyn FnMut()) -> Result<f64, MeasurementError> {
        self.0
            .measure(work)
            .map(|(_, cycles)| cycles as f64)
            .map_err(|error| MeasurementError(error.to_string()))
    }
}

/// Cache condition applied before each benchmarked call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CacheState {
    /// Reuse the same input without cache eviction.
    Hot,
    /// Sweep an eviction buffer before each independently timed sample.
    Cold,
}

impl CacheState {
    /// Stable spelling used by the CLI and JSONL records.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hot => "hot",
            Self::Cold => "cold",
        }
    }
}

/// Controls one kernel benchmark.
#[derive(Debug, Clone)]
pub struct BenchConfig {
    /// Minimum independently timed samples for each variant/cache pair.
    pub min_samples: usize,
    /// Maximum sampling time after the minimum has been collected.
    pub budget: Duration,
    /// Cache states to measure.
    pub cache_states: Vec<CacheState>,
    /// Untimed calls used to fault code and data in before sampling.
    pub warmup_calls: usize,
    /// Bytes swept before each cold sample.
    pub cold_bytes: usize,
    /// Minimum batch duration used to amortise timer resolution.
    pub target_sample_time: Duration,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            min_samples: 30,
            budget: Duration::from_millis(250),
            cache_states: vec![CacheState::Hot, CacheState::Cold],
            warmup_calls: 64,
            cold_bytes: 64 * 1024 * 1024,
            target_sample_time: Duration::from_micros(20),
        }
    }
}

/// Distribution summary in the result's declared unit, per kernel call.
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

/// Host fields that must agree before benchmark rows are comparable.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MachineFingerprint {
    machine: String,
    os: String,
    arch: String,
}

impl MachineFingerprint {
    fn detect() -> Self {
        let os = std::env::consts::OS.to_owned();
        let arch = std::env::consts::ARCH.to_owned();
        let machine = std::env::var("VACO_CHECKASM_MACHINE")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| format!("{os}-{arch}"));
        Self { machine, os, arch }
    }
}

/// One variant and cache-state measurement.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchResult {
    /// Stable [`Kernel::NAME`].
    pub kernel: &'static str,
    /// `scalar` or `vector`.
    pub variant: &'static str,
    /// Cache state for this row.
    pub cache_state: CacheState,
    /// Stable host class, optionally set with `VACO_CHECKASM_MACHINE`.
    pub machine: String,
    /// Target operating system.
    pub os: String,
    /// Target architecture.
    pub arch: String,
    /// Counter implementation. Currently `instant` on the portable path.
    pub backend: &'static str,
    /// `ns` for [`Instant`], reserved as `cycles` for a real PMU backend.
    pub unit: &'static str,
    /// Calls made inside each timed sample.
    pub iterations: usize,
    /// No-op calls made inside each independently timed control sample.
    pub nop_iterations: usize,
    /// Number of independently timed samples.
    pub samples: usize,
    /// Raw adapter-inclusive measurement before no-op subtraction.
    pub raw: Statistics,
    /// No-op control-flow/cache-eviction baseline.
    pub nop: Statistics,
    /// Raw samples corrected by the no-op median.
    pub corrected: Statistics,
    /// Scalar median divided by this variant's median for the same cache state.
    pub reference_ratio: Option<f64>,
    /// Stored-baseline median divided by this run's median, matched by unit.
    pub baseline_ratio: Option<f64>,
}

/// Stored benchmark rows keyed by kernel, variant, cache state, host and metric.
#[derive(Debug, Clone, Default)]
pub struct Baseline {
    medians: BTreeMap<
        (
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            String,
        ),
        f64,
    >,
}

/// Summarize a non-empty sample set.
#[must_use]
pub fn summarize(samples: &[f64]) -> Option<Statistics> {
    let mut ordered = samples.to_vec();
    ordered.sort_by(f64::total_cmp);
    let median = sample_median(&ordered)?;
    let mut deviations: Vec<f64> = ordered.iter().map(|value| (value - median).abs()).collect();
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

/// Benchmark the scalar and dispatched adapters for one kernel.
///
/// The largest corpus case is selected by scalar output length. Measurements
/// include the adapter's output allocation because the current [`Kernel`] API
/// returns an owned `Vec`; JSONL labels the rows as adapter-inclusive in the
/// schema version.
///
/// # Errors
///
/// Returns [`BenchError::EmptyCorpus`] if the kernel has no benchmark case, or
/// [`BenchError::NoSamples`] if the configured measurement produces no sample.
pub fn benchmark<K>(config: &BenchConfig) -> Result<Vec<BenchResult>, BenchError>
where
    K: Kernel,
{
    let mut measurement = preferred_measurement();
    match benchmark_with_measurement::<K>(config, measurement.as_mut()) {
        Ok(results) => Ok(results),
        Err(BenchError::Measurement(error))
            if measurement.metric() == Metric::PERF_EVENT_CYCLES =>
        {
            eprintln!(
                "vaco-checkasm: perf-event measurement failed ({error}); falling back to instant/ns"
            );
            let mut fallback = InstantMeasurement;
            benchmark_with_measurement::<K>(config, &mut fallback)
        }
        Err(error) => Err(error),
    }
}

fn preferred_measurement() -> Box<dyn Measurement> {
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    match vaco_hw_perf_event::CpuCycles::open_for_current_thread() {
        Ok(counter) => return Box::new(PerfEventMeasurement(counter)),
        Err(error) => {
            eprintln!("vaco-checkasm: perf-event unavailable ({error}); falling back to instant/ns")
        }
    }

    Box::new(InstantMeasurement)
}

fn benchmark_with_measurement<K>(
    config: &BenchConfig,
    measurement: &mut dyn Measurement,
) -> Result<Vec<BenchResult>, BenchError>
where
    K: Kernel,
{
    let case = K::benchmark_case().ok_or(BenchError::EmptyCorpus(K::NAME))?;
    let machine = MachineFingerprint::detect();
    let mut results = Vec::new();
    for &cache_state in &config.cache_states {
        results.push(measure_variant::<K>(
            &case,
            config,
            cache_state,
            "scalar",
            K::scalar,
            &machine,
            measurement,
        )?);
        results.push(measure_variant::<K>(
            &case,
            config,
            cache_state,
            "vector",
            K::vector,
            &machine,
            measurement,
        )?);
    }
    add_reference_ratios(&mut results);
    Ok(results)
}

fn measure_variant<K>(
    case: &K::Case,
    config: &BenchConfig,
    cache_state: CacheState,
    variant: &'static str,
    call: fn(&K::Case) -> Vec<K::Lane>,
    machine: &MachineFingerprint,
    measurement: &mut dyn Measurement,
) -> Result<BenchResult, BenchError>
where
    K: Kernel,
{
    for _ in 0..config.warmup_calls {
        black_box(call(black_box(case)));
    }

    let mut cold = vec![0u8; config.cold_bytes];
    let iterations = calibrate(
        case,
        call,
        cache_state,
        &mut cold,
        config.target_sample_time,
    );
    let nop_iterations =
        calibrate_nop::<K::Case, K::Lane>(case, cache_state, &mut cold, config.target_sample_time);
    let nop_samples = collect_nop_samples::<K::Case, K::Lane>(
        case,
        nop_iterations,
        cache_state,
        &mut cold,
        config.min_samples,
        measurement,
    );
    let nop_samples = nop_samples?;
    let nop = summarize(&nop_samples).ok_or(BenchError::NoSamples)?;
    let raw_samples = collect_samples(
        case,
        call,
        iterations,
        cache_state,
        &mut cold,
        config,
        measurement,
    )?;
    let raw = summarize(&raw_samples).ok_or(BenchError::NoSamples)?;
    let corrected_samples: Vec<f64> = raw_samples
        .iter()
        .map(|sample| (sample - nop.median).max(0.0))
        .collect();
    let corrected = summarize(&corrected_samples).ok_or(BenchError::NoSamples)?;

    Ok(BenchResult {
        kernel: K::NAME,
        variant,
        cache_state,
        machine: machine.machine.clone(),
        os: machine.os.clone(),
        arch: machine.arch.clone(),
        backend: measurement.metric().backend,
        unit: measurement.metric().unit,
        iterations,
        nop_iterations,
        samples: raw_samples.len(),
        raw,
        nop,
        corrected,
        reference_ratio: None,
        baseline_ratio: None,
    })
}

fn calibrate_nop<C, L>(
    case: &C,
    cache_state: CacheState,
    cold: &mut [u8],
    target: Duration,
) -> usize {
    let mut iterations = 1usize;
    loop {
        let elapsed = measure_batch(case, nop::<C, L>, iterations, cache_state, cold);
        if elapsed >= target || iterations >= (1usize << 20) {
            return iterations;
        }
        iterations = iterations.saturating_mul(2).max(1);
    }
}

fn calibrate<C, L>(
    case: &C,
    call: fn(&C) -> Vec<L>,
    cache_state: CacheState,
    cold: &mut [u8],
    target: Duration,
) -> usize {
    let mut iterations = 1usize;
    loop {
        let elapsed = measure_batch(case, call, iterations, cache_state, cold);
        if elapsed >= target || iterations >= (1usize << 20) {
            return iterations;
        }
        iterations = iterations.saturating_mul(2).max(1);
    }
}

fn collect_nop_samples<C, L>(
    case: &C,
    iterations: usize,
    cache_state: CacheState,
    cold: &mut [u8],
    count: usize,
    measurement: &mut dyn Measurement,
) -> Result<Vec<f64>, BenchError> {
    let mut samples = Vec::new();
    for _ in 0..count.max(1) {
        let value = measure_batch_with_measurement(
            case,
            nop::<C, L>,
            iterations,
            cache_state,
            cold,
            measurement,
        )?;
        samples.push(value / iterations as f64);
    }
    Ok(samples)
}

fn nop<C, L>(case: &C) -> Vec<L> {
    black_box(case);
    Vec::new()
}

fn collect_samples<C, L>(
    case: &C,
    call: fn(&C) -> Vec<L>,
    iterations: usize,
    cache_state: CacheState,
    cold: &mut [u8],
    config: &BenchConfig,
    measurement: &mut dyn Measurement,
) -> Result<Vec<f64>, BenchError> {
    let start = Instant::now();
    let mut samples = Vec::new();
    loop {
        let value =
            measure_batch_with_measurement(case, call, iterations, cache_state, cold, measurement)?;
        samples.push(value / iterations as f64);
        if samples.len() < config.min_samples.max(1) {
            continue;
        }
        let stable = summarize(&samples)
            .is_some_and(|stats| stats.median > 0.0 && stats.mad / stats.median <= 0.01);
        if stable || start.elapsed() >= config.budget || samples.len() >= MAX_SAMPLES {
            return Ok(samples);
        }
    }
}

fn measure_batch_with_measurement<C, L>(
    case: &C,
    call: fn(&C) -> Vec<L>,
    iterations: usize,
    cache_state: CacheState,
    cold: &mut [u8],
    measurement: &mut dyn Measurement,
) -> Result<f64, BenchError> {
    prepare_cache(cache_state, cold);
    let mut work = || {
        for _ in 0..iterations {
            black_box(call(black_box(case)));
        }
    };
    measurement
        .measure(&mut work)
        .map_err(|error| BenchError::Measurement(error.to_string()))
}

fn measure_batch<C, L>(
    case: &C,
    call: fn(&C) -> Vec<L>,
    iterations: usize,
    cache_state: CacheState,
    cold: &mut [u8],
) -> Duration {
    prepare_cache(cache_state, cold);
    let start = Instant::now();
    for _ in 0..iterations {
        black_box(call(black_box(case)));
    }
    start.elapsed()
}

fn prepare_cache(cache_state: CacheState, cold: &mut [u8]) {
    if cache_state == CacheState::Cold {
        for byte in cold.iter_mut().step_by(64) {
            *byte = byte.wrapping_add(1);
        }
        black_box(cold.as_ptr());
    }
}

fn duration_ns(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000_000_000.0
}

fn add_reference_ratios(results: &mut [BenchResult]) {
    for cache_state in [CacheState::Hot, CacheState::Cold] {
        let reference = results
            .iter()
            .find(|row| row.cache_state == cache_state && row.variant == "scalar")
            .map(|row| row.corrected.median);
        if let Some(reference) = reference {
            for row in results
                .iter_mut()
                .filter(|row| row.cache_state == cache_state)
            {
                if row.corrected.median > 0.0 {
                    row.reference_ratio = Some(reference / row.corrected.median);
                }
            }
        }
    }
}

/// Write one valid JSON object per result line.
///
/// # Errors
///
/// Returns [`BenchError::Io`] if the output cannot be created or written.
pub fn write_jsonl(path: &Path, results: &[BenchResult]) -> Result<(), BenchError> {
    let file = File::create(path).map_err(BenchError::Io)?;
    let mut writer = BufWriter::new(file);
    for row in results {
        writeln!(writer, "{}", json_line(row)).map_err(BenchError::Io)?;
    }
    writer.flush().map_err(BenchError::Io)
}

fn json_line(row: &BenchResult) -> String {
    let reference = option_number(row.reference_ratio);
    let baseline = option_number(row.baseline_ratio);
    format!(
        concat!(
            "{{\"schema\":2,\"scope\":\"adapter-inclusive\",",
            "\"kernel\":{},\"variant\":{},\"cache\":{},",
            "\"machine\":{},\"os\":{},\"arch\":{},",
            "\"backend\":{},\"unit\":{},\"iterations\":{},\"nop_iterations\":{},",
            "\"samples\":{},",
            "\"nop_median\":{},\"raw_median\":{},\"median\":{},",
            "\"mad\":{},\"min\":{},\"p95\":{},",
            "\"reference_ratio\":{},\"baseline_ratio\":{}}}"
        ),
        json_string(row.kernel),
        json_string(row.variant),
        json_string(row.cache_state.as_str()),
        json_string(&row.machine),
        json_string(&row.os),
        json_string(&row.arch),
        json_string(row.backend),
        json_string(row.unit),
        row.iterations,
        row.nop_iterations,
        row.samples,
        row.nop.median,
        row.raw.median,
        row.corrected.median,
        row.corrected.mad,
        row.corrected.min,
        row.corrected.p95,
        reference,
        baseline,
    )
}

fn json_string(value: &str) -> String {
    let mut output = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            _ => output.push(ch),
        }
    }
    output.push('"');
    output
}

fn option_number(value: Option<f64>) -> String {
    value.map_or_else(|| "null".to_owned(), |number| number.to_string())
}

/// Read baseline identities and medians from this harness's JSONL schema.
///
/// # Errors
///
/// Returns [`BenchError::Io`] when the file cannot be read, or
/// [`BenchError::InvalidBaseline`] when a row lacks a required identity or
/// median field.
pub fn load_baseline(path: &Path) -> Result<Baseline, BenchError> {
    let file = File::open(path).map_err(BenchError::Io)?;
    let mut baseline = Baseline::default();
    for line in BufReader::new(file).lines() {
        let line = line.map_err(BenchError::Io)?;
        let kernel = string_field(&line, "kernel");
        let variant = string_field(&line, "variant");
        let cache = string_field(&line, "cache");
        let machine = string_field(&line, "machine");
        let os = string_field(&line, "os");
        let arch = string_field(&line, "arch");
        let backend = string_field(&line, "backend");
        let unit = string_field(&line, "unit");
        let median = number_field(&line, "median");
        match (
            kernel, variant, cache, machine, os, arch, backend, unit, median,
        ) {
            (
                Some(kernel),
                Some(variant),
                Some(cache),
                Some(machine),
                Some(os),
                Some(arch),
                Some(backend),
                Some(unit),
                Some(median),
            ) => {
                baseline.medians.insert(
                    (kernel, variant, cache, machine, os, arch, backend, unit),
                    median,
                );
            }
            _ => return Err(BenchError::InvalidBaseline(line)),
        }
    }
    Ok(baseline)
}

/// Attach like-for-like stored-baseline speed ratios to result rows.
pub fn apply_baseline(results: &mut [BenchResult], baseline: &Baseline) {
    for row in results {
        let key = (
            row.kernel.to_owned(),
            row.variant.to_owned(),
            row.cache_state.as_str().to_owned(),
            row.machine.clone(),
            row.os.clone(),
            row.arch.clone(),
            row.backend.to_owned(),
            row.unit.to_owned(),
        );
        if let Some(previous) = baseline.medians.get(&key)
            && row.corrected.median > 0.0
        {
            row.baseline_ratio = Some(previous / row.corrected.median);
        }
    }
}

fn string_field(line: &str, name: &str) -> Option<String> {
    let marker = format!("\"{name}\":\"");
    let rest = line.split_once(&marker)?.1;
    let mut output = String::new();
    let mut escaped = false;
    for ch in rest.chars() {
        if escaped {
            match ch {
                'n' => output.push('\n'),
                'r' => output.push('\r'),
                't' => output.push('\t'),
                other => output.push(other),
            }
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Some(output);
        } else {
            output.push(ch);
        }
    }
    None
}

fn number_field(line: &str, name: &str) -> Option<f64> {
    let marker = format!("\"{name}\":");
    let rest = line.split_once(&marker)?.1;
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    rest.get(..end)?.parse().ok()
}

/// Benchmark setup, measurement, or baseline error.
#[derive(Debug)]
pub enum BenchError {
    /// The kernel registered no deterministic cases.
    EmptyCorpus(&'static str),
    /// A timing path unexpectedly produced no samples.
    NoSamples,
    /// The selected measurement backend could not produce a direct sample.
    Measurement(String),
    /// JSONL I/O failed.
    Io(std::io::Error),
    /// A baseline line did not carry the required identity and median fields.
    InvalidBaseline(String),
}

impl fmt::Display for BenchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCorpus(kernel) => {
                write!(formatter, "kernel {kernel} has no benchmark cases")
            }
            Self::NoSamples => formatter.write_str("benchmark produced no samples"),
            Self::Measurement(error) => write!(formatter, "benchmark measurement failed: {error}"),
            Self::Io(error) => write!(formatter, "benchmark I/O failed: {error}"),
            Self::InvalidBaseline(line) => {
                write!(formatter, "invalid benchmark JSONL line: {line}")
            }
        }
    }
}

impl std::error::Error for BenchError {}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::float_cmp,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "a failing assertion in a test is a failing test"
)]
mod tests {
    use std::time::Duration;

    use super::{
        BenchConfig, CacheState, Measurement, MeasurementError, Metric, apply_baseline,
        benchmark_with_measurement,
    };
    use crate::Kernel;

    #[derive(Debug, Clone, Copy)]
    struct TestKernel;

    impl Kernel for TestKernel {
        const NAME: &'static str = "bench::test";
        type Case = u8;
        type Lane = u8;

        fn cases() -> Vec<Self::Case> {
            vec![1]
        }

        fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
            vec![*case]
        }

        fn vector(case: &Self::Case) -> Vec<Self::Lane> {
            vec![*case]
        }
    }

    struct ScriptedMeasurement {
        metric: Metric,
        samples: Vec<f64>,
    }

    impl Measurement for ScriptedMeasurement {
        fn metric(&self) -> Metric {
            self.metric
        }

        fn measure(&mut self, work: &mut dyn FnMut()) -> Result<f64, MeasurementError> {
            work();
            Ok(self.samples.remove(0))
        }
    }

    #[test]
    fn injected_cycles_backend_keeps_kernel_and_nop_identity_together() {
        let config = BenchConfig {
            min_samples: 1,
            budget: Duration::ZERO,
            cache_states: vec![CacheState::Hot],
            warmup_calls: 0,
            cold_bytes: 0,
            target_sample_time: Duration::ZERO,
        };
        let mut measurement = ScriptedMeasurement {
            metric: Metric::PERF_EVENT_CYCLES,
            samples: vec![10.0, 20.0, 10.0, 20.0],
        };

        let results = benchmark_with_measurement::<TestKernel>(&config, &mut measurement)
            .expect("scripted measurement succeeds");

        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|row| row.backend == "perf-event"));
        assert!(results.iter().all(|row| row.unit == "cycles"));
        assert!(results.iter().all(|row| row.nop.median == 10.0));
        assert!(results.iter().all(|row| row.raw.median == 20.0));
    }

    #[test]
    fn baseline_requires_the_complete_backend_and_unit_identity() {
        let config = BenchConfig {
            min_samples: 1,
            budget: Duration::ZERO,
            cache_states: vec![CacheState::Hot],
            warmup_calls: 0,
            cold_bytes: 0,
            target_sample_time: Duration::ZERO,
        };
        let mut measurement = ScriptedMeasurement {
            metric: Metric::PERF_EVENT_CYCLES,
            samples: vec![10.0, 20.0, 10.0, 20.0],
        };
        let mut results = benchmark_with_measurement::<TestKernel>(&config, &mut measurement)
            .expect("scripted measurement succeeds");
        let path = std::env::temp_dir().join(format!(
            "vaco-checkasm-metric-identity-{}.jsonl",
            std::process::id()
        ));
        super::write_jsonl(&path, &results).expect("write baseline");

        let mismatched = std::fs::read_to_string(&path)
            .expect("read baseline")
            .replace("\"backend\":\"perf-event\"", "\"backend\":\"instant\"")
            .replace("\"unit\":\"cycles\"", "\"unit\":\"ns\"");
        std::fs::write(&path, mismatched).expect("write mismatched baseline");
        let baseline = super::load_baseline(&path).expect("read mismatched baseline");
        std::fs::remove_file(path).expect("remove baseline");

        apply_baseline(&mut results, &baseline);
        assert!(results.iter().all(|row| row.baseline_ratio.is_none()));
    }
}
