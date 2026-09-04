#![forbid(unsafe_code)]
//! CLI front end for the differential harness. See `vaco_checkasm` (the
//! library half of this crate) for the `Kernel`/`Differential` API this binds
//! together.
//!
//! ```text
//! vaco-checkasm verify        # run every wired-in kernel, exit non-zero on any mismatch
//! vaco-checkasm list          # print the wired-in kernel names
//! vaco-checkasm bench         # measure scalar and dispatched adapters
//! ```
//!
//! There is no plugin registry here: a kernel family becomes reachable from
//! this binary by adding one `Kernel` impl under `src/kernels` and one line
//! in [`run_all`]. A crate that wants its own kernels checked without a
//! change to this binary can depend on the `vaco_checkasm` library directly
//! and call `Differential::<K>::run()` from its own tests — that is how
//! `vaco-checkasm`'s own `kernels::scale_affine` module is itself tested.

use std::path::PathBuf;
use std::time::Duration;

use vaco_checkasm::bench::{
    BenchConfig, BenchError, BenchResult, CacheState, apply_baseline, benchmark, load_baseline,
    write_jsonl,
};
use vaco_checkasm::kernels::blockdsp::AddPixelsClampedKernel;
use vaco_checkasm::kernels::fir_mc::FirMcKernel;
use vaco_checkasm::kernels::fmtconvert::{Int16ToFloatKernel, Int32ToFloatKernel};
use vaco_checkasm::kernels::intrapred::DcPredictKernel;
use vaco_checkasm::kernels::lpc::AutocorrelateKernel;
use vaco_checkasm::kernels::masked_select::MaskedSelectKernel;
use vaco_checkasm::kernels::mecmp::{SadKernel, SatdKernel, SsdKernel, VarianceKernel};
use vaco_checkasm::kernels::scale_affine::AffineRowKernel;
use vaco_checkasm::{Differential, Kernel, Report};

const USAGE: &str = "usage: vaco-checkasm [verify|list|bench] [OPTIONS]\n\
bench options:\n\
  -t, --test <GLOB>       kernel-name filter [default: *]\n\
  -f, --function <GLOB>   variant filter: scalar|vector [default: *]\n\
      --bench-cache <M>   hot|cold|both [default: both]\n\
      --min-samples <N>   minimum samples [default: 30]\n\
      --budget <MS>       per-variant budget [default: 250]\n\
      --json <PATH>       write JSONL results\n\
      --baseline <PATH>   compare like-for-like JSONL rows\n\
      --fail-under <R>    fail when baseline/current speed ratio is below R\n\
      --fail-slower-than-reference";

fn main() {
    let mut args = std::env::args().skip(1);
    let code = match args.next().as_deref() {
        None | Some("verify") => run_all(),
        Some("list") => {
            list_all();
            0
        }
        Some("bench" | "-b" | "--bench") => match BenchOptions::parse(args) {
            Ok(options) => run_bench(&options),
            Err(error) => {
                eprintln!("{error}\n{USAGE}");
                2
            }
        },
        Some("-h" | "--help") => {
            println!("{USAGE}");
            0
        }
        Some(other) => {
            eprintln!("unknown subcommand '{other}'");
            eprintln!("{USAGE}");
            2
        }
    };
    std::process::exit(code);
}

/// One entry in the wired-in kernel table: a name (for `list`) and a thunk
/// that runs the differential and prints its report (for `verify`).
///
/// A plain struct of function pointers rather than `dyn Kernel` — `Kernel`
/// is not object-safe (associated types), so the type erasure has to happen
/// one level up, at the point each kernel is already monomorphised.
struct Entry {
    name: &'static str,
    verify: fn() -> bool,
    bench: fn(&BenchConfig) -> Result<Vec<BenchResult>, BenchError>,
}

fn verify_report<K: Kernel>() -> bool
where
    K::Lane: PartialEq,
{
    let report: Report<K> = Differential::<K>::run();
    print!("{report}");
    report.is_clean()
}

fn bench_kernel<K: Kernel>(config: &BenchConfig) -> Result<Vec<BenchResult>, BenchError> {
    benchmark::<K>(config)
}

const ENTRIES: &[Entry] = &[
    Entry {
        name: AffineRowKernel::NAME,
        verify: verify_report::<AffineRowKernel>,
        bench: bench_kernel::<AffineRowKernel>,
    },
    Entry {
        name: MaskedSelectKernel::NAME,
        verify: verify_report::<MaskedSelectKernel>,
        bench: bench_kernel::<MaskedSelectKernel>,
    },
    Entry {
        name: FirMcKernel::NAME,
        verify: verify_report::<FirMcKernel>,
        bench: bench_kernel::<FirMcKernel>,
    },
    Entry {
        name: SadKernel::NAME,
        verify: verify_report::<SadKernel>,
        bench: bench_kernel::<SadKernel>,
    },
    Entry {
        name: SsdKernel::NAME,
        verify: verify_report::<SsdKernel>,
        bench: bench_kernel::<SsdKernel>,
    },
    Entry {
        name: VarianceKernel::NAME,
        verify: verify_report::<VarianceKernel>,
        bench: bench_kernel::<VarianceKernel>,
    },
    Entry {
        name: SatdKernel::NAME,
        verify: verify_report::<SatdKernel>,
        bench: bench_kernel::<SatdKernel>,
    },
    Entry {
        name: Int16ToFloatKernel::NAME,
        verify: verify_report::<Int16ToFloatKernel>,
        bench: bench_kernel::<Int16ToFloatKernel>,
    },
    Entry {
        name: Int32ToFloatKernel::NAME,
        verify: verify_report::<Int32ToFloatKernel>,
        bench: bench_kernel::<Int32ToFloatKernel>,
    },
    Entry {
        name: AutocorrelateKernel::NAME,
        verify: verify_report::<AutocorrelateKernel>,
        bench: bench_kernel::<AutocorrelateKernel>,
    },
    Entry {
        name: AddPixelsClampedKernel::NAME,
        verify: verify_report::<AddPixelsClampedKernel>,
        bench: bench_kernel::<AddPixelsClampedKernel>,
    },
    Entry {
        name: DcPredictKernel::NAME,
        verify: verify_report::<DcPredictKernel>,
        bench: bench_kernel::<DcPredictKernel>,
    },
];

fn run_all() -> i32 {
    if ENTRIES.is_empty() {
        eprintln!("no kernels wired in");
        return 1;
    }
    let mut ok = true;
    for entry in ENTRIES {
        ok &= (entry.verify)();
    }
    i32::from(!ok)
}

fn list_all() {
    for entry in ENTRIES {
        println!("{}", entry.name);
    }
}

#[derive(Debug)]
struct BenchOptions {
    config: BenchConfig,
    test_pattern: String,
    function_pattern: String,
    json: Option<PathBuf>,
    baseline: Option<PathBuf>,
    fail_under: Option<f64>,
    fail_slow_reference: bool,
}

impl BenchOptions {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut options = Self {
            config: BenchConfig::default(),
            test_pattern: "*".to_owned(),
            function_pattern: "*".to_owned(),
            json: None,
            baseline: None,
            fail_under: None,
            fail_slow_reference: false,
        };
        let mut args = args;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-t" | "--test" => {
                    options.test_pattern = next_value(&mut args, &arg)?;
                }
                "-f" | "--function" => {
                    options.function_pattern = next_value(&mut args, &arg)?;
                }
                "--bench-cache" => {
                    let value = next_value(&mut args, &arg)?;
                    options.config.cache_states = match value.as_str() {
                        "hot" => vec![CacheState::Hot],
                        "cold" => vec![CacheState::Cold],
                        "both" => vec![CacheState::Hot, CacheState::Cold],
                        _ => return Err(format!("invalid --bench-cache '{value}'")),
                    };
                }
                "--min-samples" => {
                    options.config.min_samples =
                        parse_positive(&next_value(&mut args, &arg)?, &arg)?;
                }
                "--budget" => {
                    let millis = parse_positive(&next_value(&mut args, &arg)?, &arg)?;
                    options.config.budget = Duration::from_millis(millis as u64);
                }
                "--json" => options.json = Some(PathBuf::from(next_value(&mut args, &arg)?)),
                "--baseline" => {
                    options.baseline = Some(PathBuf::from(next_value(&mut args, &arg)?));
                }
                "--fail-under" => {
                    let value = next_value(&mut args, &arg)?;
                    let ratio = value
                        .parse::<f64>()
                        .map_err(|_| format!("invalid --fail-under '{value}'"))?;
                    if !ratio.is_finite() || ratio <= 0.0 {
                        return Err("--fail-under must be finite and positive".to_owned());
                    }
                    options.fail_under = Some(ratio);
                }
                "--fail-slower-than-reference" => options.fail_slow_reference = true,
                "-h" | "--help" => return Err(String::new()),
                _ => return Err(format!("unknown bench option '{arg}'")),
            }
        }
        Ok(options)
    }
}

fn next_value(args: &mut impl Iterator<Item = String>, option: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("missing value for {option}"))
}

fn parse_positive(value: &str, option: &str) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| format!("invalid {option} '{value}'"))?;
    if parsed == 0 {
        Err(format!("{option} must be positive"))
    } else {
        Ok(parsed)
    }
}

fn run_bench(options: &BenchOptions) -> i32 {
    let mut results = Vec::new();
    for entry in ENTRIES
        .iter()
        .filter(|entry| glob_matches(&options.test_pattern, entry.name))
    {
        match (entry.bench)(&options.config) {
            Ok(mut rows) => results.append(&mut rows),
            Err(error) => {
                eprintln!("{}: {error}", entry.name);
                return 1;
            }
        }
    }
    results.retain(|row| glob_matches(&options.function_pattern, row.variant));
    if results.is_empty() {
        eprintln!("no benchmark rows matched the filters");
        return 1;
    }

    if let Some(path) = &options.baseline {
        match load_baseline(path) {
            Ok(baseline) => apply_baseline(&mut results, &baseline),
            Err(error) => {
                eprintln!("{error}");
                return 1;
            }
        }
    }

    let mut failed = false;
    for row in &results {
        println!(
            "{} {} {} backend={} unit={} min={:.3} median={:.3} mad={:.3} p95={:.3} nop={:.3} iterations={} nop_iterations={} samples={} reference_ratio={} baseline_ratio={}",
            row.kernel,
            row.variant,
            row.cache_state.as_str(),
            row.backend,
            row.unit,
            row.corrected.min,
            row.corrected.median,
            row.corrected.mad,
            row.corrected.p95,
            row.nop.median,
            row.iterations,
            row.nop_iterations,
            row.samples,
            display_ratio(row.reference_ratio),
            display_ratio(row.baseline_ratio),
        );
        if options
            .fail_under
            .is_some_and(|threshold| row.baseline_ratio.is_some_and(|ratio| ratio < threshold))
        {
            failed = true;
        }
        if options.fail_slow_reference
            && row.variant == "vector"
            && row.reference_ratio.is_some_and(|ratio| ratio < 1.0)
        {
            failed = true;
        }
    }

    if let Some(path) = &options.json
        && let Err(error) = write_jsonl(path, &results)
    {
        eprintln!("{error}");
        return 1;
    }
    i32::from(failed)
}

fn display_ratio(value: Option<f64>) -> String {
    value.map_or_else(|| "n/a".to_owned(), |ratio| format!("{ratio:.3}"))
}

fn glob_matches(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == value;
    }
    let starts_anchored = !pattern.starts_with('*');
    let ends_anchored = !pattern.ends_with('*');
    let mut remainder = value;
    let mut saw_part = false;
    for part in pattern.split('*').filter(|part| !part.is_empty()) {
        if !saw_part && starts_anchored {
            let Some(after) = remainder.strip_prefix(part) else {
                return false;
            };
            remainder = after;
        } else {
            let Some((_, after)) = remainder.split_once(part) else {
                return false;
            };
            remainder = after;
        }
        saw_part = true;
    }
    !ends_anchored || remainder.is_empty()
}
