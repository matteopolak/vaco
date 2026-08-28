//! The named-integer-constant parsing gate.
//!
//! # What it is
//!
//! For every filter this corpus can reach, asks the real reference
//! (`ffmpeg -h filter=<name>`) which options declare named integer
//! constants (`mode` accepting `abs`/`diff`, not just `0`/`1`), then feeds
//! each `option=constant` pair through [`FilterRegistry::create`] and
//! asserts it parses. This is the gate the campaign that fixed twenty
//! filters (fillborders, field, il, perspective, lut1d, lut3d, haldclut,
//! and all thirteen `vaco-filter-deinterlace` filters) asked for: a way to
//! *notice* the "named constant rejected as an unknown value" bug class
//! without a human re-running a 45-entry survey by hand every time a new
//! filter or option is added.
//!
//! # Why `FilterRegistry::create`, not source inspection
//!
//! A static grep for `#[opt(unit = ...)]` only sees filters that use
//! `#[derive(vaco_opts::Options)]` in the first place. A large minority of
//! filters parse their own arguments by hand, calling [`Instantiate::named`]
//! directly — invisible to that grep, but not to this gate, because
//! `create` is the one entry point every filter answers through regardless
//! of how it parses. Going through [`vaco_filter_graph::ast::parse`] to
//! build the `Instantiate` (rather than hand-building an `Arg`) means the
//! gate exercises the exact same escaping/splitting path a real
//! `-vf name=option=value` command line does, not an approximation of it.
//!
//! # The `aresample` case
//!
//! `ffmpeg -h filter=aresample` prints the filter's own `aresample
//! AVOptions:` section (one option, `sample_rate`, no constants) followed
//! by a nested `SWResampler AVOptions:` section whose *dash-prefixed*
//! options (`-dither_method`, `-resampler`) carry real constants at the
//! same indentation as the outer section's own constant rows. A parser
//! that does not treat `AVOptions:` as a hard section boundary will
//! misattribute those constants to `sample_rate`, which has none. This
//! exact case broke an earlier throwaway survey script and is now pinned
//! as [`refhelp`]'s own `aresample_nested_avoptions_section_is_excluded`
//! unit test, using real captured reference output — see that module
//! before trusting this gate's parsing.
//!
//! # Skips gracefully
//!
//! No reference on `PATH` (or `VACO_REF_FFMPEG`/`VACO_REF_FFPROBE`) means
//! this test prints `SKIPPED` and passes, the same §1.5.4 contract every
//! other test in this crate honours — this crate has no authority to
//! demand an installation that is not there.
//!
//! # How to change it
//!
//! [`REGISTRIES`] is deliberately explicit and alphabetised by crate name,
//! the same shape `filterexec::REGISTRIES` uses, for the same reason: a
//! filter crate added to this gate is a real, reviewed line, not something
//! that starts being swept just by existing in the tree. Unlike
//! `filterexec::REGISTRIES`, this list is not limited to crates the
//! in-process frame-execution path can drive — `FilterRegistry::create`
//! alone never builds a `Graph` or touches a frame, so there is no
//! single-input / pixel-format / pad-count constraint to inherit — which is
//! why this list already covers the full registered-filter surface rather
//! than the handful `filterexec.rs` wires today (a number that keeps
//! growing as that module reaches more crates — see its own doc).

#![expect(
    clippy::expect_used,
    reason = "a failing expectation in a test is a failing test"
)]

use std::time::Duration;

use vaco_conformance::refbin::{self, Discovery, RefSpec};
use vaco_conformance::refhelp;
use vaco_conformance::registries::REGISTRIES;
use vaco_conformance::run::{self, Invocation};
use vaco_filter_graph::ast;
use vaco_filter_graph::registry::Instantiate;

/// `(filter, option)` pairs this gate already knows do not parse named
/// constants, and does not fail on. Empty now: `vaco-filter-color`'s
/// `colorchannelmixer`'s `pc`, `colorlevels`'s `preserve`, and
/// `pseudocolor`'s `p`/`preset` (one field, two names via `alias`) were
/// the last four here, closed once that crate's stale `ASSIGNMENTS.md` row
/// was reclaimed 2026-08-28. A fresh failure anywhere fails the build; see
/// this file's own module doc for what "failure" means here.
const KNOWN_GAPS: &[(&str, &str)] = &[];

/// Run `ffmpeg -hide_banner -h filter=<name>` and return stdout+stderr,
/// concatenated in that order — the reference's own `-h` output has always
/// landed entirely on stdout in every case observed so far, but
/// concatenating both costs nothing and does not depend on that holding
/// forever.
fn reference_help(ffmpeg: &std::path::Path, name: &str) -> Option<String> {
    let inv = Invocation::new(ffmpeg, ["-hide_banner", "-h", &format!("filter={name}")])
        .with_timeout(Duration::from_secs(10));
    let obs = run::run(&inv).ok()?;
    let mut text = obs.stdout_text().into_owned();
    text.push_str(&obs.stderr_text());
    Some(text)
}

/// Build an [`Instantiate`] for `name` with exactly one `option=value`
/// argument, by round-tripping it through the real filtergraph parser —
/// [`vaco_filter_graph::ast::parse`] — rather than hand-building an `Arg`,
/// so this gate exercises the same escaping/splitting path a real
/// `-vf name=option=value` command line does.
fn instantiate_with_one_option(
    name: &str,
    option: &str,
    value: &str,
) -> Result<vaco_filter_graph::registry::Instance, String> {
    let args = format!("{option}={value}");
    let src = format!("{name}={args}");
    let parsed = ast::parse(&src).map_err(|e| format!("parsing `{src}`: {e:?}"))?;
    let spec = parsed
        .chains
        .first()
        .and_then(|c| c.filters.first())
        .ok_or_else(|| format!("`{src}` parsed to zero filters"))?;
    let arguments = spec
        .arguments()
        .map_err(|e| format!("splitting arguments of `{src}`: {e:?}"))?;
    let req = Instantiate {
        name,
        instance: name,
        args: spec.args.as_deref(),
        arguments: &arguments,
    };
    let registry = REGISTRIES
        .iter()
        .find(|(_, r)| r.contains(name))
        .map(|(_, r)| *r)
        .ok_or_else(|| format!("no registry in this gate answers to `{name}`"))?;
    registry.create(&req)
}

#[test]
fn named_integer_constants_parse_through_the_real_registry() {
    let spec = RefSpec::load().expect("refspec.toml loads");
    let discovery = refbin::discover(&spec);
    let Discovery::Found(reference) = &discovery else {
        println!("SKIPPED (no reference): {discovery:?}");
        return;
    };

    let mut checked = 0usize;
    let mut known_gaps = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for (crate_name, registry) in REGISTRIES {
        for name in registry.names() {
            let Some(help) = reference_help(&reference.ffmpeg, name) else {
                // The reference does not know this filter name (a vaco-only
                // filter, or a name mismatch) — not this gate's business;
                // `vaco-registry`'s own generated table is what audits name
                // coverage.
                continue;
            };
            let consts = refhelp::parse(&help, name);
            for (option, values) in &consts {
                for (const_name, raw_value) in values {
                    // Precondition: does this filter implement `option` at
                    // all, accepting its own raw integer form? If not, this
                    // is a whole-option feature gap (this filter hasn't
                    // implemented `option` yet), a separate and much larger
                    // tracked backlog than this gate's business — skip it
                    // rather than drowning the named-constant signal in
                    // "Option not found" noise from options nobody has
                    // written yet. The named-constant bug class this gate
                    // exists for is specifically "the raw integer works but
                    // the reference's own name for it does not", the exact
                    // shape pixelize/convolution/maskedthreshold all had.
                    if instantiate_with_one_option(name, option, &raw_value.to_string()).is_err() {
                        continue;
                    }
                    checked += 1;
                    if let Err(e) = instantiate_with_one_option(name, option, const_name) {
                        let message = format!(
                            "{crate_name}: `{name}={option}={const_name}` (reference \
                             constant for raw value {raw_value}, from \
                             `ffmpeg -h filter={name}`) did not parse even though \
                             `{option}={raw_value}` does: {e}"
                        );
                        if KNOWN_GAPS.contains(&(name, option.as_str())) {
                            known_gaps += 1;
                        } else {
                            failures.push(message);
                        }
                    }
                }
            }
        }
    }

    println!(
        "option_consts_gate: checked {checked} (filter, option, named-constant) \
         triples across {} registries ({known_gaps} in KNOWN_GAPS, a separate \
         open assignment, excluded from the pass/fail verdict)",
        REGISTRIES.len()
    );

    assert!(
        !failures.is_empty() || checked > 0,
        "the gate found zero named-integer-constant options across every \
         registered filter — that almost certainly means reference_help or \
         refhelp::parse broke, not that no filter has any, since aresample \
         alone proves the reference's own -h output carries them"
    );

    assert!(
        failures.is_empty(),
        "{} of {checked} named-constant option(s) did not parse through \
         FilterRegistry::create:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}
