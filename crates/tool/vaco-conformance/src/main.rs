#![forbid(unsafe_code)]
//! The `vaco-conformance` command line.
//!
//! ```text
//! vaco-conformance tables [--deep] [--strict]   differential checks on our static tables
//! vaco-conformance refbin                       what reference is installed, and does it gate
//! vaco-conformance run [--suite S] [--tier T] [--case ID]   run declared suites
//! vaco-conformance divergences                  the allowlist and its health
//! vaco-conformance explore -- <argv…>           run both binaries and diff, writing nothing
//! ```
//!
//! Exit codes: `0` clean, `1` unexplained findings, `2` a usage or load error.
//! An absent reference is `0` with a skip message — a contributor without
//! `FFmpeg` must still be able to run everything (plan 13 §1.5.4).

use std::process::ExitCode;

use vaco_conformance::case::Tier;
use vaco_conformance::divergence::Allowlist;
use vaco_conformance::extract::{self, Depth};
use vaco_conformance::refbin::{self, Discovery, RefSpec};
use vaco_conformance::report;
use vaco_conformance::runner::{Runner, Tally};
use vaco_conformance::{manifest, suite_roots};

const USAGE: &str = "\
vaco-conformance — differential harness against the pinned reference binary

USAGE:
    vaco-conformance tables [--deep] [--strict]
    vaco-conformance refbin
    vaco-conformance run [--suite <name>] [--tier <tier>] [--case <id>]
    vaco-conformance divergences
    vaco-conformance explore -- <argv…>

THE BRIGHT-LINE RULE
    You may run the reference binary as often as you like. You may not read its
    source. When our output differs and you cannot explain why, you escalate —
    you do not go looking in the source for the answer.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(first) = args.first().map(String::as_str) else {
        print!("{USAGE}");
        return ExitCode::from(2);
    };
    // `just conformance <suite>` expands to `-- --suite <suite>` with no
    // subcommand, so a leading option means `run`. Keeping the Justfile working
    // matters more than insisting on the subcommand.
    let command = if first.starts_with('-') && !matches!(first, "-h" | "--help") {
        "run"
    } else {
        first
    };
    let flag = |name: &str| args.iter().any(|a| a == name);
    let value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };

    let spec = match RefSpec::load() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let allow = match Allowlist::load() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let discovery = refbin::discover(&spec);

    match command {
        "tables" => cmd_tables(&discovery, &allow, flag("--deep"), flag("--strict")),
        "refbin" => cmd_refbin(&spec, &discovery),
        "run" => cmd_run(
            &discovery,
            &allow,
            value("--suite").as_deref(),
            value("--tier").as_deref(),
            value("--case").as_deref(),
        ),
        "divergences" => cmd_divergences(&allow),
        "explore" => cmd_explore(&discovery, &args),
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("unknown command `{other}`\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn cmd_tables(discovery: &Discovery, allow: &Allowlist, deep: bool, strict_flag: bool) -> ExitCode {
    let Some(reference) = discovery.reference() else {
        println!(
            "{}",
            report::render_reference(None, discovery.skip_reason().unwrap_or_default())
        );
        println!("table checks skipped.");
        return ExitCode::SUCCESS;
    };
    print!("{}", report::render_reference(Some(reference), ""));
    let strict = strict_flag || refbin::strict();
    if strict && !reference.gates() {
        eprintln!("strict mode: the installed reference is not the pinned gating version");
        return ExitCode::from(1);
    }
    let depth = if deep { Depth::Deep } else { Depth::Listings };
    if !deep {
        println!("(listings only; --deep adds the per-format and per-abbreviation probes)\n");
    }
    let reports = extract::run_all(reference, allow, depth);
    print!("{}", report::render_tables(&reports));
    let findings: usize = reports
        .iter()
        .map(extract::TableReport::finding_count)
        .sum();
    let errored = reports.iter().any(|r| r.error.is_some());
    if findings > 0 || errored {
        // Advisory when the reference is not the gating pin: a divergence
        // against an unpinned oracle is information, not a verdict.
        if reference.gates() {
            return ExitCode::from(1);
        }
        println!("(advisory: the installed reference is not the gating pin)");
    }
    ExitCode::SUCCESS
}

fn cmd_refbin(spec: &RefSpec, discovery: &Discovery) -> ExitCode {
    println!("pins:");
    for pin in spec.pins.values() {
        println!(
            "  {:<9} {:<6} {}  {}",
            pin.channel.as_str(),
            pin.version,
            if pin.gates { "GATES" } else { "advisory" },
            if pin.sha256.is_empty() {
                "(tarball hash not yet recorded)"
            } else {
                "hash recorded"
            }
        );
    }
    if !spec.drift.is_empty() {
        println!("\ntriaged behaviour drift between pins:");
        for d in &spec.drift {
            println!("  {} -> {}  [{}] {}", d.from, d.to, d.bucket, d.subject);
        }
    }
    println!();
    print!(
        "{}",
        report::render_reference(
            discovery.reference(),
            discovery.skip_reason().unwrap_or_default()
        )
    );
    if let Some(r) = discovery.reference() {
        let drift = spec.drift_touching(&r.version);
        if !drift.is_empty() {
            println!("behaviours known to differ around this version:");
            for d in drift {
                println!("  [{}] {} — {}", d.bucket, d.subject, d.note);
            }
        }
    }
    ExitCode::SUCCESS
}

fn cmd_run(
    discovery: &Discovery,
    allow: &Allowlist,
    suite_filter: Option<&str>,
    tier_name: Option<&str>,
    case_filter: Option<&str>,
) -> ExitCode {
    let tier = tier_name.and_then(Tier::parse).unwrap_or(Tier::Smoke);
    let mut cases = Vec::new();
    let mut load_errors = 0_usize;
    for root in suite_roots() {
        let found = match manifest::discover(&root) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("{e}");
                load_errors += 1;
                continue;
            }
        };
        for suite in found {
            match suite {
                Ok(s) => {
                    if suite_filter.is_none_or(|f| s.name == f) {
                        cases.extend(s.expand());
                    }
                }
                Err(e) => {
                    eprintln!("suite failed to load: {e}");
                    load_errors += 1;
                }
            }
        }
        if !cases.is_empty() {
            break;
        }
    }
    if load_errors > 0 {
        return ExitCode::from(2);
    }

    // `--case <id>` reproduces exactly one case, by the id printed with every
    // failure (§1.5.2) — this is what `Case::reproduction`'s
    // `just conformance-run '<id>'` line actually invokes. It bypasses the
    // tier filter entirely rather than requiring `--tier exhaustive` too: a
    // case declared `tier = "full"` or even `"manual"` must still be
    // reproducible by pasting the one line a failure report gave you, and
    // `Tier::included_by` deliberately never includes `manual` through tier
    // selection at all (§1.8) — this is the other door.
    if let Some(id) = case_filter {
        let Some(case) = cases.iter().find(|c| c.id.as_str() == id) else {
            eprintln!(
                "no case with id `{id}` among the {} declared case(s) — case ids are \
                 `suite/media/axis=value,...`; check the suite name and try `run --suite \
                 <suite>` to list what it declares",
                cases.len()
            );
            return ExitCode::from(2);
        };
        print!(
            "{}",
            report::render_reference(
                discovery.reference(),
                discovery.skip_reason().unwrap_or_default()
            )
        );
        println!("case: {id} (tier gating bypassed)\n");
        let mut runner = Runner::new(discovery.reference(), allow);
        discovery
            .skip_reason()
            .unwrap_or_default()
            .clone_into(&mut runner.absent_reason);
        let outcome = runner.run_case(case);
        let mut tally = Tally::default();
        tally.record(&outcome.verdict);
        print!(
            "{}",
            report::render_run(std::slice::from_ref(&outcome), tally, allow)
        );
        return if tally.is_failing()
            && discovery
                .reference()
                .is_some_and(vaco_conformance::refbin::Reference::gates)
        {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        };
    }

    print!(
        "{}",
        report::render_reference(
            discovery.reference(),
            discovery.skip_reason().unwrap_or_default()
        )
    );
    println!("tier: {tier}, {} case(s) declared\n", cases.len());

    let mut runner = Runner::new(discovery.reference(), allow);
    discovery
        .skip_reason()
        .unwrap_or_default()
        .clone_into(&mut runner.absent_reason);
    let (outcomes, tally) = runner.run_all(&cases, tier);
    print!("{}", report::render_run(&outcomes, tally, allow));

    if tally.is_failing()
        && discovery
            .reference()
            .is_some_and(vaco_conformance::refbin::Reference::gates)
    {
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn cmd_divergences(allow: &Allowlist) -> ExitCode {
    let counts = allow.live_counts();
    if allow.entries().is_empty() {
        println!("the divergence register is empty.");
        println!(
            "That is the healthy state. Every entry is a place the harness has been \
             told to stop proving something."
        );
        return ExitCode::SUCCESS;
    }
    for (cat, n) in &counts {
        println!("{cat}: {n}");
    }
    println!();
    for e in allow.entries() {
        println!("{} [{}] {}", e.id, e.category, e.title);
        println!("  scope:     {:?}", e.scope);
        println!(
            "  owner:     {} (approved by {})",
            e.owner,
            e.approved_by.join(", ")
        );
        println!("  opened:    {}   review_by: {}", e.opened, e.review_by);
        println!("  issue:     {}", e.issue);
        for line in e.justification.lines() {
            println!("  | {line}");
        }
        println!();
    }
    ExitCode::SUCCESS
}

fn cmd_explore(discovery: &Discovery, args: &[String]) -> ExitCode {
    let Some(sep) = args.iter().position(|a| a == "--") else {
        eprintln!("explore needs `--` followed by the argument vector\n\n{USAGE}");
        return ExitCode::from(2);
    };
    let argv: Vec<String> = args.iter().skip(sep + 1).cloned().collect();
    if argv.is_empty() {
        eprintln!("explore needs an argument vector after `--`");
        return ExitCode::from(2);
    }
    let Some(reference) = discovery.reference() else {
        println!("{}", discovery.skip_reason().unwrap_or_default());
        return ExitCode::SUCCESS;
    };
    // Interrogating the oracle is unlimited and is exactly what it is for
    // (§1.7.3 step 3). This writes nothing to the repository: the author copies
    // any manifest stanza across by hand, which keeps a human in the loop on
    // every case that lands (§1.5.3).
    let inv = vaco_conformance::run::Invocation::new(&reference.ffprobe, argv.clone());
    println!("$ {}", inv.command_line());
    match vaco_conformance::run::run(&inv) {
        Ok(obs) => {
            println!(
                "exit: {:?}  ({} bytes of stdout)",
                obs.exit,
                obs.stdout.len()
            );
            print!("{}", obs.stdout_text());
            if !obs.stderr.is_empty() {
                eprint!("{}", obs.stderr_text());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}
