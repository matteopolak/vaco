#![forbid(unsafe_code)]
//! Command-line interface for registry-complete benchmark tracking.

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use vaco_bench::{
    BenchError, BenchmarkSandbox, ChildBatchMode, CommandTemplate, FilterBenchConfig,
    Implementation, MacroScenario, MeasurementBackend, apply_baseline, filter_cases,
    macro_json_record, regressions, run_filter_child_batch, run_filter_suite, run_macro_scenario,
    validate_macro_manifest, verify_machine_control, write_jsonl, write_report,
};
use vaco_corpus::fetch::{self, NetworkPolicy};
use vaco_corpus::{ObjectId, Store, embedded_catalogue};

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
        Some(command) if command == "machine-check" => run_machine_check(args),
        Some(command) if command == "macro" => run_macro(args),
        Some(command) if command == "__filter-batch" => run_filter_child(args),
        Some(command) if command == "filter" => run_filter(args),
        Some(command) if command == "report" => run_report(args),
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

#[derive(Debug, Default)]
struct MacroArgs {
    name: Option<String>,
    asset: Option<String>,
    expected_output: Option<ObjectId>,
    vaco: Option<PathBuf>,
    reference: Option<PathBuf>,
    vaco_args: Vec<String>,
    reference_args: Vec<String>,
    cache_dir: Option<PathBuf>,
    json: Option<PathBuf>,
    rounds: usize,
}

fn run_macro(args: impl Iterator<Item = OsString>) -> Result<ExitCode, CliError> {
    let parsed = parse_macro_args(args)?;
    let report = verify_machine_control();
    if !report.is_ready() {
        eprintln!("machine control: not ready; {}", report.failure_summary());
        return Ok(ExitCode::from(2));
    }
    let scenario = MacroScenario {
        name: parsed
            .name
            .ok_or_else(|| CliError::Usage("--scenario is required".to_owned()))?,
        asset: parsed
            .asset
            .ok_or_else(|| CliError::Usage("--asset is required".to_owned()))?,
        vaco: CommandTemplate {
            program: parsed
                .vaco
                .ok_or_else(|| CliError::Usage("--vaco is required".to_owned()))?,
            args: parsed.vaco_args,
        },
        reference: CommandTemplate {
            program: parsed
                .reference
                .ok_or_else(|| CliError::Usage("--reference is required".to_owned()))?,
            args: parsed.reference_args,
        },
        expected_output: parsed
            .expected_output
            .ok_or_else(|| CliError::Usage("--expected-output-sha256 is required".to_owned()))?,
    };
    validate_macro_manifest(std::slice::from_ref(&scenario))?;
    let catalogue = embedded_catalogue();
    let entry = catalogue
        .find(&scenario.asset)
        .ok_or_else(|| CliError::Usage(format!("unknown corpus asset {:?}", scenario.asset)))?;
    let store = parsed.cache_dir.map_or_else(Store::open_default, Store::at);
    let bytes = fetch::fetch_asset(entry, &store, NetworkPolicy::from_env())
        .map_err(|error| BenchError::Macro(format!("{}: {error}", scenario.asset)))?;
    let _sandbox = BenchmarkSandbox::enter()?;
    let sandbox = std::env::current_dir().map_err(BenchError::Io)?;
    let input = sandbox.join("input.bin");
    fs::write(&input, bytes).map_err(BenchError::Io)?;
    let samples = run_macro_scenario(&scenario, &input, parsed.rounds, &sandbox)?;
    if let Some(path) = parsed.json {
        let mut rows = samples
            .iter()
            .map(macro_json_record)
            .collect::<Vec<_>>()
            .join("\n");
        rows.push('\n');
        fs::write(path, rows).map_err(BenchError::Io)?;
    }
    for implementation in [Implementation::Vaco, Implementation::Reference] {
        let times: Vec<_> = samples
            .iter()
            .filter(|sample| sample.implementation == implementation)
            .map(|sample| sample.wall_ns)
            .collect();
        let stats = vaco_bench::summarize(&times).ok_or(BenchError::NoSamples)?;
        println!(
            "{} {} samples={} median={:.3}ns mad={:.3}ns min={:.3}ns p95={:.3}ns",
            scenario.name,
            implementation.name(),
            times.len(),
            stats.median,
            stats.mad,
            stats.min,
            stats.p95,
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn parse_macro_args(args: impl Iterator<Item = OsString>) -> Result<MacroArgs, CliError> {
    let mut parsed = MacroArgs {
        rounds: 11,
        ..MacroArgs::default()
    };
    let mut args = args;
    while let Some(flag) = args.next() {
        match flag.to_str() {
            Some("--scenario") => parsed.name = Some(text_value(&mut args, "--scenario")?),
            Some("--asset") => parsed.asset = Some(text_value(&mut args, "--asset")?),
            Some("--expected-output-sha256") => {
                let hash = text_value(&mut args, "--expected-output-sha256")?;
                parsed.expected_output = Some(ObjectId::parse(&hash).ok_or_else(|| {
                    CliError::Usage(
                        "--expected-output-sha256 requires 64 hexadecimal characters".to_owned(),
                    )
                })?);
            }
            Some("--vaco") => parsed.vaco = Some(path_value(&mut args, "--vaco")?),
            Some("--reference") => parsed.reference = Some(path_value(&mut args, "--reference")?),
            Some("--vaco-arg") => parsed.vaco_args.push(text_value(&mut args, "--vaco-arg")?),
            Some("--reference-arg") => parsed
                .reference_args
                .push(text_value(&mut args, "--reference-arg")?),
            Some("--cache-dir") => parsed.cache_dir = Some(path_value(&mut args, "--cache-dir")?),
            Some("--json") => parsed.json = Some(path_value(&mut args, "--json")?),
            Some("--rounds") => {
                parsed.rounds = usize_value(&mut args, "--rounds")?;
                if parsed.rounds < 11 {
                    return Err(CliError::Usage("--rounds must be at least 11".to_owned()));
                }
            }
            Some("--help" | "-h") => return Err(CliError::Usage(macro_usage().to_owned())),
            _ => {
                return Err(CliError::Usage(format!(
                    "unknown macro option {}\n\n{}",
                    PathBuf::from(flag).display(),
                    macro_usage()
                )));
            }
        }
    }
    Ok(parsed)
}

fn run_machine_check(args: impl Iterator<Item = OsString>) -> Result<ExitCode, CliError> {
    reject_extra(args)?;
    let report = verify_machine_control();
    for check in report.checks() {
        let status = if check.passed { "ok" } else { "fail" };
        let requirement = if check.required {
            "required"
        } else {
            "recorded"
        };
        println!("{status} {requirement} {}: {}", check.name, check.detail);
    }
    if report.is_ready() {
        println!("machine control: ready for gating results");
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("machine control: not ready; {}", report.failure_summary());
        Ok(ExitCode::from(2))
    }
}

#[derive(Debug)]
struct ReportArgs {
    input: PathBuf,
    output: PathBuf,
    generated_unix_ms: Option<i64>,
    fail_under: f64,
}

fn run_report(args: impl Iterator<Item = OsString>) -> Result<ExitCode, CliError> {
    let parsed = parse_report_args(args)?;
    let generated_unix_ms = match parsed.generated_unix_ms {
        Some(timestamp) => timestamp,
        None => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| BenchError::Clock(error.to_string()))?
            .as_millis()
            .try_into()
            .map_err(|_| BenchError::Clock("timestamp exceeds i64".to_owned()))?,
    };
    write_report(
        &parsed.input,
        &parsed.output,
        generated_unix_ms,
        parsed.fail_under,
    )?;
    println!("wrote benchmark report {}", parsed.output.display());
    Ok(ExitCode::SUCCESS)
}

fn parse_report_args(args: impl Iterator<Item = OsString>) -> Result<ReportArgs, CliError> {
    let mut input = None;
    let mut output = None;
    let mut generated_unix_ms = None;
    let mut fail_under = 0.95;
    let mut args = args;
    while let Some(flag) = args.next() {
        match flag.to_str() {
            Some("--input") => input = Some(path_value(&mut args, "--input")?),
            Some("--output") => output = Some(path_value(&mut args, "--output")?),
            Some("--generated-unix-ms") => {
                generated_unix_ms = Some(i64_value(&mut args, "--generated-unix-ms")?);
            }
            Some("--fail-under") => {
                fail_under = f64_value(&mut args, "--fail-under")?;
                if !fail_under.is_finite() || fail_under <= 0.0 {
                    return Err(CliError::Usage(
                        "--fail-under must be a finite positive ratio".to_owned(),
                    ));
                }
            }
            Some("--help" | "-h") => return Err(CliError::Usage(report_usage().to_owned())),
            _ => {
                return Err(CliError::Usage(format!(
                    "unknown report option {}\n\n{}",
                    PathBuf::from(flag).display(),
                    report_usage()
                )));
            }
        }
    }
    Ok(ReportArgs {
        input: input.ok_or_else(|| CliError::Usage("--input is required".to_owned()))?,
        output: output.ok_or_else(|| CliError::Usage("--output is required".to_owned()))?,
        generated_unix_ms,
        fail_under,
    })
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

fn i64_value(
    args: &mut impl Iterator<Item = OsString>,
    flag: &'static str,
) -> Result<i64, CliError> {
    let value = next_value(args, flag)?;
    value
        .to_str()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| CliError::Usage(format!("{flag} requires an integer")))
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
    "usage: vaco-bench list\n       vaco-bench machine-check\n       vaco-bench macro [options]\n       vaco-bench filter [options]\n       vaco-bench report [options]"
}

fn macro_usage() -> &'static str {
    "usage: vaco-bench macro --scenario S1..S10/CONFIG --asset CORPUS_ENTRY --vaco PATH --reference PATH --expected-output-sha256 SHA256 [--vaco-arg ARG]... [--reference-arg ARG]... [--cache-dir DIR] [--json PATH] [--rounds N]\n\nEach command must contain whole-argument {input} and {output} placeholders."
}

fn filter_usage() -> &'static str {
    "usage: vaco-bench filter [--backend instant|auto|perf-stat] [--warmup N] [--samples N] [--target-sample-ns N] [--max-iterations N] [--json PATH] [--baseline PATH] [--fail-under R]"
}

fn report_usage() -> &'static str {
    "usage: vaco-bench report --input JSONL --output HTML [--generated-unix-ms N] [--fail-under R]"
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
