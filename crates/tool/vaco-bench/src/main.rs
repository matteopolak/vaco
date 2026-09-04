#![forbid(unsafe_code)]
//! Command-line interface for registry-complete benchmark tracking.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use vaco_bench::{
    BenchError, ChildBatchMode, FilterBenchConfig, MeasurementBackend, apply_baseline,
    filter_cases, regressions, run_filter_child_batch, run_filter_suite, write_jsonl,
};

fn main() -> ExitCode {
    match run(std::env::args_os().skip(1)) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("vaco-bench: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(args: impl Iterator<Item = OsString>) -> Result<ExitCode, CliError> {
    let mut args = args;
    match args.next().as_deref() {
        Some(command) if command == "list" => {
            reject_extra(args)?;
            for case in filter_cases() {
                println!("{}", case.name());
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(command) if command == "__filter-batch" => run_filter_child(args),
        Some(command) if command == "filter" => run_filter(args),
        Some(command) if command == "--help" || command == "-h" => {
            reject_extra(args)?;
            print_usage();
            Ok(ExitCode::SUCCESS)
        }
        Some(command) => Err(CliError::Usage(format!(
            "unknown command {}\n\n{}",
            PathBuf::from(command).display(),
            usage()
        ))),
        None => Err(CliError::Usage(usage().to_owned())),
    }
}

fn run_filter_child(mut args: impl Iterator<Item = OsString>) -> Result<ExitCode, CliError> {
    let mode = match text_value(&mut args, "child batch mode")?.as_str() {
        "work" => ChildBatchMode::Work,
        "control" => ChildBatchMode::Control,
        value => {
            return Err(CliError::Usage(format!(
                "child batch mode must be work or control, got {value}"
            )));
        }
    };
    let name = text_value(&mut args, "child filter name")?;
    let iterations = usize_value(&mut args, "child iterations")?;
    let outcome = text_value(&mut args, "child expected outcome")?;
    reject_extra(args)?;
    run_filter_child_batch(mode, &name, iterations, &outcome)?;
    Ok(ExitCode::SUCCESS)
}

#[derive(Debug, Default)]
struct FilterArgs {
    config: FilterBenchConfig,
    json: Option<PathBuf>,
    baseline: Option<PathBuf>,
    fail_under: Option<f64>,
}

fn run_filter(args: impl Iterator<Item = OsString>) -> Result<ExitCode, CliError> {
    let parsed = parse_filter_args(args)?;
    if parsed.fail_under.is_some() && parsed.baseline.is_none() {
        return Err(CliError::Usage(
            "--fail-under requires --baseline so there is something to compare".to_owned(),
        ));
    }

    let mut rows = run_filter_suite(&parsed.config)?;
    let matched = if let Some(path) = parsed.baseline.as_deref() {
        apply_baseline(&mut rows, path)?
    } else {
        0
    };
    if let Some(path) = parsed.json.as_deref() {
        write_jsonl(path, &rows)?;
    }

    for row in &rows {
        let comparison = row.baseline_ratio.map_or_else(
            || "incomparable".to_owned(),
            |ratio| format!("baseline/current={ratio:.4}"),
        );
        println!(
            "{} outcome={} median={:.3}{} mad={:.3}{} min={:.3}{} p95={:.3}{} iterations={} samples={} backend={}/{} {}",
            row.benchmark,
            row.outcome,
            row.stats.median,
            row.unit,
            row.stats.mad,
            row.unit,
            row.stats.min,
            row.unit,
            row.stats.p95,
            row.unit,
            row.iterations,
            row.samples,
            row.backend,
            row.unit,
            comparison,
        );
    }
    println!(
        "measured={} matched={} incomparable={}",
        rows.len(),
        matched,
        rows.len().saturating_sub(matched)
    );

    let Some(threshold) = parsed.fail_under else {
        return Ok(ExitCode::SUCCESS);
    };
    let slower = regressions(&rows, threshold);
    if slower.is_empty() {
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!(
            "{} matched benchmark(s) fell below baseline/current {threshold:.4}",
            slower.len()
        );
        for row in slower {
            if let Some(ratio) = row.baseline_ratio {
                eprintln!("  {}: {ratio:.4}", row.benchmark);
            }
        }
        Ok(ExitCode::FAILURE)
    }
}

fn parse_filter_args(args: impl Iterator<Item = OsString>) -> Result<FilterArgs, CliError> {
    let mut parsed = FilterArgs::default();
    let mut args = args;
    while let Some(flag) = args.next() {
        match flag.to_str() {
            Some("--backend") => parsed.config.backend = backend_value(&mut args)?,
            Some("--warmup") => parsed.config.warmup_calls = usize_value(&mut args, "--warmup")?,
            Some("--samples") => parsed.config.samples = usize_value(&mut args, "--samples")?,
            Some("--target-sample-ns") => {
                parsed.config.target_sample_ns = u64_value(&mut args, "--target-sample-ns")?;
            }
            Some("--max-iterations") => {
                parsed.config.max_iterations = usize_value(&mut args, "--max-iterations")?;
            }
            Some("--json") => parsed.json = Some(path_value(&mut args, "--json")?),
            Some("--baseline") => {
                parsed.baseline = Some(path_value(&mut args, "--baseline")?);
            }
            Some("--fail-under") => {
                let threshold = f64_value(&mut args, "--fail-under")?;
                if !threshold.is_finite() || threshold <= 0.0 {
                    return Err(CliError::Usage(
                        "--fail-under must be a finite positive ratio".to_owned(),
                    ));
                }
                parsed.fail_under = Some(threshold);
            }
            Some("--help" | "-h") => return Err(CliError::Usage(filter_usage().to_owned())),
            _ => {
                return Err(CliError::Usage(format!(
                    "unknown filter option {}\n\n{}",
                    PathBuf::from(flag).display(),
                    filter_usage()
                )));
            }
        }
    }
    Ok(parsed)
}

fn reject_extra(mut args: impl Iterator<Item = OsString>) -> Result<(), CliError> {
    if let Some(extra) = args.next() {
        Err(CliError::Usage(format!(
            "unexpected argument {}",
            PathBuf::from(extra).display()
        )))
    } else {
        Ok(())
    }
}

fn next_value(
    args: &mut impl Iterator<Item = OsString>,
    flag: &'static str,
) -> Result<OsString, CliError> {
    args.next()
        .ok_or_else(|| CliError::Usage(format!("{flag} requires a value")))
}

fn text_value(
    args: &mut impl Iterator<Item = OsString>,
    label: &'static str,
) -> Result<String, CliError> {
    next_value(args, label)?
        .into_string()
        .map_err(|_| CliError::Usage(format!("{label} must be valid UTF-8")))
}

fn usize_value(
    args: &mut impl Iterator<Item = OsString>,
    flag: &'static str,
) -> Result<usize, CliError> {
    let value = next_value(args, flag)?;
    value
        .to_str()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| CliError::Usage(format!("{flag} requires a non-negative integer")))
}

fn u64_value(
    args: &mut impl Iterator<Item = OsString>,
    flag: &'static str,
) -> Result<u64, CliError> {
    let value = next_value(args, flag)?;
    value
        .to_str()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| CliError::Usage(format!("{flag} requires a non-negative integer")))
}

fn f64_value(
    args: &mut impl Iterator<Item = OsString>,
    flag: &'static str,
) -> Result<f64, CliError> {
    let value = next_value(args, flag)?;
    value
        .to_str()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| CliError::Usage(format!("{flag} requires a number")))
}

fn backend_value(
    args: &mut impl Iterator<Item = OsString>,
) -> Result<MeasurementBackend, CliError> {
    match next_value(args, "--backend")?.to_str() {
        Some("instant") => Ok(MeasurementBackend::Instant),
        Some("auto") => Ok(MeasurementBackend::Auto),
        Some("perf-stat") => Ok(MeasurementBackend::PerfStat),
        _ => Err(CliError::Usage(
            "--backend requires instant, auto, or perf-stat".to_owned(),
        )),
    }
}

fn path_value(
    args: &mut impl Iterator<Item = OsString>,
    flag: &'static str,
) -> Result<PathBuf, CliError> {
    Ok(PathBuf::from(next_value(args, flag)?))
}

fn print_usage() {
    println!("{}", usage());
}

fn usage() -> &'static str {
    "usage: vaco-bench list\n       vaco-bench filter [options]"
}

fn filter_usage() -> &'static str {
    "usage: vaco-bench filter [--backend instant|auto|perf-stat] [--warmup N] [--samples N] [--target-sample-ns N] [--max-iterations N] [--json PATH] [--baseline PATH] [--fail-under R]"
}

#[derive(Debug)]
enum CliError {
    Usage(String),
    Bench(BenchError),
}

impl From<BenchError> for CliError {
    fn from(error: BenchError) -> Self {
        Self::Bench(error)
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage(message) => formatter.write_str(message),
            Self::Bench(error) => error.fmt(formatter),
        }
    }
}
