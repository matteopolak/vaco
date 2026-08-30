//! The `ffmpeg`-equivalent: argv in, a run executed, a correct exit code out.
//!
//! # What this is
//!
//! `vaco` is the transcoding binary. This crate is its **spine**: option
//! binding, opening inputs through the protocol and format registries, stream
//! selection, building a [`vaco_sched::PipelineSpec`], driving it, and
//! reporting. The twenty work packages above that spine — metadata mapping,
//! `-progress`/`-stats`/`-report`, filtergraph binding, `-force_key_frames`,
//! the timestamp matrix, `[dec:N]`, `-stream_group`, presets, hardware devices —
//! are deliberately not here. See `docs/app/vaco-cli.md` for what is deferred
//! and which issue owns each piece.
//!
//! ```text
//! argv ─▶ [cli]      split, validate, bind          (vaco-cli-core, cli.rs)
//!      ─▶ [listing]  -version/-formats/… and exit   (listing.rs)
//!      ─▶ [input]    protocol → probe → demux       (vaco-io, vaco-format-core)
//!      ─▶ [select]   -map, or the auto rules        (select.rs)
//!      ─▶ [exec]     a PipelineSpec, driven          (vaco-sched)
//!      ─▶ [exit]     stderr text and a status code   (exit.rs)
//! ```
//!
//! # There are muxers now; there are still no encoders
//!
//! Until the container wave, D5 scoped v0.1 to demuxing: `crates/format/` held
//! three `vaco-demux-*` crates and no `vaco-mux-*`, and this section said so at
//! length. It now holds a `vaco-mux-*` crate per container — 63 registered
//! muxers — and [`exec::muxer_for`]/[`exec::run_pipeline`] reach every one of
//! them through `vaco_registry`, not just a local `null` stand-in. There are
//! still no decoders and no encoders, so:
//!
//! * `vaco -i in.mkv -c copy -f matroska out.mkv` **writes a real file**:
//!   protocol → probe → demux → discovery → selection → `vaco-sched` →
//!   the registry's `matroska` muxer, opened on a real sink through the
//!   protocol layer. Streamcopy end to end, which is the one thing a build
//!   with no encoders can do — and enough to remux.
//! * `vaco -i in.mkv -f null -` still works exactly as before: `null` is one
//!   registered muxer among 63 now, not a special case, and it is still the
//!   workhorse of this crate's own tests (`-f null -` needs no container
//!   knowledge and makes every stage observable through packet counts alone).
//! * `vaco -i in.mkv out.zzz` and `-f nosuchformat` still fail with a message
//!   naming the real reason, and a stream with no `-c copy` still fails on the
//!   reference's own missing-encoder path. Neither pretends.
//!
//! The acceptance criterion for a real output is now the same as the
//! reference's own: the bytes on disk, read back. For `-f null -` and for any
//! output whose bytes are not yet worth comparing, it remains what it always
//! was — the same stream selection, the same stderr text, the same exit code,
//! and the same packet counts through the pipeline.
//! [`nullmux::OutputTally`] is what makes the last of those observable, real
//! muxer or not — see [`nullmux::TallyingMuxer`], which wraps whichever the
//! registry returned.
//!
//! # A library plus a thin binary
//!
//! Same reason as `vaco-probe`: `cargo fuzz` links a *library*, and D6 makes a
//! fuzz target mandatory for a crate whose input is a user's command line — the
//! least trusted input in the project. The binary target keeps only argv, stdio
//! and the exit code. Having a lib target also brings the crate inside
//! `cargo xtask wasm-check` (D18), which it passes.
//!
//! # Configuration
//!
//! No environment variables and no config files. Everything is an option, and
//! every option is in `vaco_cli_core::table::ffmpeg()`.

#![forbid(unsafe_code)]

pub mod cli;
pub mod complexgraph;
pub mod dump;
pub mod exec;
pub mod exit;
pub mod filtergraph;
pub mod filterreg;
pub mod force_key_frames;
pub mod help;
pub mod input;
pub mod listing;
pub mod nullmux;
pub mod output;
pub mod progress;
pub mod report;
pub mod select;
pub mod stats;

use std::ffi::OsStr;
use std::io::Write;


pub use exit::{AvError, Diagnostic, ExitCode};
pub use listing::VERSION;

/// Run one invocation.
///
/// `argv` must not include the program name. Everything the program would print
/// goes to `out` or `err`; nothing reaches the real stdio, which is what makes
/// the whole binary testable and fuzzable without spawning a process.
pub fn run<S, O, E>(argv: &[S], out: &mut O, err: &mut E) -> ExitCode
where
    S: AsRef<OsStr>,
    O: Write,
    E: Write,
{
    // CL-17: `-report`/`FFREPORT` mirrors everything this run writes to
    // stderr — the banner included — into a log file. Opened here, before
    // the banner, so both share one sink for the whole run; see
    // `report`'s module docs for the header this writes and why a failure to
    // open it is not fatal.
    let ffreport_env = std::env::var("FFREPORT").ok();
    let report_req = report::wants_report(argv, ffreport_env.as_deref());
    match report_req.as_ref().and_then(|r| report::open(r, argv).ok()) {
        Some((file, _name)) => run_banner_and_execute(argv, out, &mut report::Tee::new(err, file)),
        None => run_banner_and_execute(argv, out, err),
    }
}

fn run_banner_and_execute<S, O, E>(argv: &[S], out: &mut O, err: &mut E) -> ExitCode
where
    S: AsRef<OsStr>,
    O: Write,
    E: Write,
{
    // The banner goes out before anything is parsed: `ffmpeg -qwerty 3` prints
    // it and *then* the error, so it cannot wait for a successful parse.
    if cli::wants_banner(argv) {
        let _ = listing::banner(err);
    }
    match execute(argv, out, err) {
        Ok(code) => code,
        Err(d) => {
            let _ = err.write_all(d.render().as_bytes());
            d.exit
        }
    }
}

fn execute<S, O, E>(argv: &[S], out: &mut O, err: &mut E) -> Result<ExitCode, Diagnostic>
where
    S: AsRef<OsStr>,
    O: Write,
    E: Write,
{
    if argv.is_empty() {
        // OBSERVED: bare `ffmpeg` exits 1 after printing the banner and a usage
        // block. The usage prose is ours (D9); the status is behaviour.
        return Err(Diagnostic::usage(usage()));
    }

    let cli = cli::parse(argv)?;

    if let Some(name) = cli.listing {
        if name == "h" {
            help::render(out, cli.listing_value.as_deref()).map_err(listing::io_diagnostic)?;
        } else {
            listing::render(out, name, cli.listing_value.as_deref())?;
        }
        return Ok(ExitCode::OK);
    }

    // Both this dump and the informational blocks below follow the log
    // level — and note that is a *different* condition from the banner's.
    // Measured: `-hide_banner` drops the banner and keeps these; `-v warning`
    // drops all of them. `prints_info` is the one predicate both the banner
    // check above and every block below need, and it is already used for
    // `Stream mapping:`/the muxing-overhead line, so this reuses rather than
    // re-derives it.
    let show_info = vaco_cli_core::loglevel::prints_info(argv);

    let mut inputs = Vec::new();
    for spec in &cli.inputs {
        let white: Option<Vec<&str>> = spec
            .whitelist
            .as_ref()
            .map(|v| v.iter().map(String::as_str).collect());
        let black: Option<Vec<&str>> = spec
            .blacklist
            .as_ref()
            .map(|v| v.iter().map(String::as_str).collect());
        let req = input::OpenRequest {
            force_format: spec.format.as_deref(),
            whitelist: white.as_deref(),
            blacklist: black.as_deref(),
            format_opts: Some(&spec.format_opts),
        };
        let opened = input::open(spec.index, &spec.url, &req).map_err(|e| {
            let av = AvError::of(&e);
            Diagnostic::opening(
                av,
                vec![format!(
                    "[in#{}] Error opening input: {}",
                    spec.index, av.text
                )],
                "input",
                &spec.url,
            )
        })?;
        // #641: `Input #0, …` printed as soon as the input opens, exactly
        // like the reference — which is *before* the "no output" check
        // below, not after it. `ffmpeg -i in.mp4` with no output prints the
        // whole dump and then that error; this used to check for a missing
        // output before opening anything, so the dump had no path that could
        // ever reach it in that case. See `dump::render_input`.
        if show_info {
            for line in dump::render_input(
                spec.index,
                &spec.url,
                &opened.desc,
                opened.demuxer.as_ref(),
                opened.size,
            ) {
                let _ = writeln!(err, "{line}");
            }
        }
        inputs.push(opened);
    }

    if cli.outputs.is_empty() {
        // OBSERVED: `ffmpeg -i in.mkv` exits 1 with exactly this line.
        return Err(Diagnostic::usage(vec![
            "At least one output file must be specified".to_owned(),
        ]));
    }

    let files: Vec<select::InputStreams> = inputs.iter().map(exec::describe).collect();

    // CL-25: parse every `-filter_complex`/`-lavfi` occurrence far enough to
    // list its labelled output pads, before any real decode happens — see
    // `complexgraph::catalog`'s own docs for why that is safe and why the
    // same texts are parsed again for real inside `exec::run_pipeline`.
    let complex_catalog = complexgraph::catalog(&cli.complex_filters).map_err(|e| {
        Diagnostic::new(
            AvError::EINVAL,
            vec![format!("Error configuring filter graph: {e}")],
        )
    })?;
    let mut used_complex = std::collections::HashSet::new();

    let mut outputs = Vec::new();
    for spec in &cli.outputs {
        outputs.push(exec::resolve_output(
            &cli,
            spec,
            &files,
            &complex_catalog,
            &mut used_complex,
        )?);
    }

    // Rule 4: every labelled complex-graph output must be consumed by
    // exactly one `-map [label]`. `select::resolve` already refuses a second
    // use of the same label; this is the other half — zero uses.
    for (i, pad) in complex_catalog.iter().enumerate() {
        if !used_complex.contains(&i) {
            return Err(Diagnostic::new(
                AvError::EINVAL,
                vec![format!(
                    "Filter graph output '{}' is not connected to any output stream",
                    pad.label
                )],
            ));
        }
    }

    // CL-17/#208: `Output #0, …` and `Press [q] to stop, [?] for help`. The
    // reference prints both of these, and `Stream mapping:`, before it starts
    // writing any packet; `exec::run_pipeline` below does the mapping and the
    // writing in one blocking call with no earlier hook to print from, so
    // `Stream mapping:`/the summary line stay where they already were (after
    // the call, unchanged) while this block prints *before* it — an ordering
    // this crate's own `-i F` (no output) diff loop does not exercise. See
    // `dump`'s module docs for what the `Output` block does not attempt to
    // reproduce (the muxer's own `tbr`/`tbn`, `-map_metadata`'s copied tags,
    // `q=…`).
    let any_output = outputs.iter().any(|o| !o.dropped);
    if show_info {
        for out in outputs.iter().filter(|o| !o.dropped) {
            for line in dump::render_output(out, &inputs) {
                let _ = writeln!(err, "{line}");
            }
        }
        if any_output {
            let _ = writeln!(err, "Press [q] to stop, [?] for help");
        }
    }

    let started = vaco_time::Instant::now();
    let auto_conversion_filters = !cli
        .line
        .last_global("auto_conversion_filters")
        .is_some_and(|o| o.negated);
    let report = exec::run_pipeline(
        inputs,
        &outputs,
        &files,
        &cli.complex_filters,
        auto_conversion_filters,
        cli.thread_count(),
    )?;

    // `Stream mapping:` is the reference's own wording and layout, and it is
    // the most useful single line of evidence that selection agreed.
    if show_info {
        if !report.mapping.is_empty() {
            let _ = writeln!(err, "Stream mapping:");
            for line in &report.mapping {
                let _ = writeln!(err, "{line}");
            }
        }
        for line in &report.summary {
            let _ = writeln!(err, "{line}");
        }
        if any_output && stats::wants_stats(argv) {
            let _ = writeln!(err, "{}", stats::render(&report, started));
        }
    }
    // CL-17: `-progress <url>` — a separate output channel from the
    // informational blocks above, so it is not gated on `show_info`/
    // `-loglevel` the way they are; the reference writes it regardless.
    // A URL this build cannot open silently gets no progress data, the same
    // "not fatal" policy `report::open`'s caller already applies.
    if any_output
        && let Some(target) = progress::target(argv)
        && let Ok(mut sink) = output::create(&target)
    {
        let block = progress::render(&report, started);
        let _ = sink.write(block.as_bytes());
        let _ = sink.flush();
    }
    Ok(ExitCode::OK)
}

/// The usage block. Written for Vaco: D9 puts the reference's help text outside
/// what we reproduce, so `-h` output cannot be byte-identical by design.
fn usage() -> Vec<String> {
    vec![
        format!("vaco version {VERSION}"),
        "usage: vaco [options] -i <input> ... [options] <output> ...".to_owned(),
        String::new(),
        "Use -h to get full help, or -formats to list what this build contains.".to_owned(),
    ]
}

#[cfg(test)]
mod tests;
