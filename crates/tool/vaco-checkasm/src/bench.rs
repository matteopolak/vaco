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

/// One variant and cache-state measurement.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchResult {
    /// Stable [`Kernel::NAME`].
    pub kernel: &'static str,
    /// `scalar` or `vector`.
    pub variant: &'static str,
    /// Cache state for this row.
    pub cache_state: CacheState,
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

/// Stored benchmark rows keyed by kernel, variant, cache state, backend and unit.
#[derive(Debug, Clone, Default)]
pub struct Baseline {
    medians: BTreeMap<(String, String, String, String, String), f64>,
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
    let case = K::benchmark_case().ok_or(BenchError::EmptyCorpus(K::NAME))?;
    let mut results = Vec::new();
    for &cache_state in &config.cache_states {
        results.push(measure_variant::<K>(
            &case,
            config,
            cache_state,
            "scalar",
            K::scalar,
        )?);
        results.push(measure_variant::<K>(
            &case,
            config,
            cache_state,
            "vector",
            K::vector,
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
    );
    let nop = summarize(&nop_samples).ok_or(BenchError::NoSamples)?;
    let raw_samples = collect_samples(case, call, iterations, cache_state, &mut cold, config);
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
        backend: "instant",
        unit: "ns",
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
) -> Vec<f64> {
    let mut samples = Vec::new();
    for _ in 0..count.max(1) {
        let elapsed = measure_batch(case, nop::<C, L>, iterations, cache_state, cold);
        samples.push(duration_ns(elapsed) / iterations as f64);
    }
    samples
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
) -> Vec<f64> {
    let start = Instant::now();
    let mut samples = Vec::new();
    loop {
        let elapsed = measure_batch(case, call, iterations, cache_state, cold);
        samples.push(duration_ns(elapsed) / iterations as f64);
        if samples.len() < config.min_samples.max(1) {
            continue;
        }
        let stable = summarize(&samples)
            .is_some_and(|stats| stats.median > 0.0 && stats.mad / stats.median <= 0.01);
        if stable || start.elapsed() >= config.budget || samples.len() >= MAX_SAMPLES {
            break;
        }
    }
    samples
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
            "{{\"schema\":1,\"scope\":\"adapter-inclusive\",",
            "\"kernel\":{},\"variant\":{},\"cache\":{},",
            "\"backend\":{},\"unit\":{},\"iterations\":{},\"nop_iterations\":{},",
            "\"samples\":{},",
            "\"nop_median\":{},\"raw_median\":{},\"median\":{},",
            "\"mad\":{},\"min\":{},\"p95\":{},",
            "\"reference_ratio\":{},\"baseline_ratio\":{}}}"
        ),
        json_string(row.kernel),
        json_string(row.variant),
        json_string(row.cache_state.as_str()),
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
        let backend = string_field(&line, "backend");
        let unit = string_field(&line, "unit");
        let median = number_field(&line, "median");
        match (kernel, variant, cache, backend, unit, median) {
            (Some(kernel), Some(variant), Some(cache), Some(backend), Some(unit), Some(median)) => {
                baseline
                    .medians
                    .insert((kernel, variant, cache, backend, unit), median);
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
            Self::Io(error) => write!(formatter, "benchmark I/O failed: {error}"),
            Self::InvalidBaseline(line) => {
                write!(formatter, "invalid benchmark JSONL line: {line}")
            }
        }
    }
}

impl std::error::Error for BenchError {}
