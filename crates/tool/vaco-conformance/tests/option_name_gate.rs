//! The option-*name*-recognition gate: a sibling to `option_consts_gate.rs`,
//! not merged into it, for the reason its own module doc gives — a filter
//! legitimately not implementing an option is a feature gap, and mixing a
//! feature-gap signal into a bug-class signal drowns the one worth failing
//! the build over. See `KNOWN_GAPS` below for the boundary this file draws.
//!
//! # What it checks, and what it deliberately does not
//!
//! For every filter this corpus can reach, this asserts one thing:
//! [`FilterRegistry::create`] rejects an option name the reference does
//! not document at all for that filter. That is it. It does **not** assert
//! that every option the reference documents is accepted — a first
//! attempt at that (kept only in this file's own history, not shipped)
//! tried exactly that and found ~300 apparent failures on a single probe
//! run, the overwhelming majority not a name-recognition problem at all
//! but a *different* filter requiring another option to be set first
//! (`aevalsrc` needs `exprs`, `lut3d`/`lut1d` need `file`, `removelogo`
//! needs `filename`, …) — the single-option probe this gate's design
//! depends on cannot tell "this name is unrecognised" from "this filter
//! needed a second, unrelated option too" without a per-filter minimal-
//! valid-invocation fixture, which is real, per-filter work, not a
//! mechanical check. That half is not built here; see
//! `planning/INTERFACE-GAPS.md`-style follow-up instead of a gate that
//! would need hundreds of entries to tell signal from a solvable-only-by-
//! more-plumbing confound.
//!
//! The half this file *does* check has no such confound: setting one
//! extra, deliberately-invented key alongside otherwise-default arguments
//! either gets rejected (the reference's own behaviour — confirmed
//! directly: `ffmpeg -vf null=zzz_fake=1` and
//! `ffmpeg -vf hqdn3d=zzz_fake=1` both answer "Option not found") or it
//! does not, and there is no other option required for that answer to
//! mean what it says.
//!
//! # `KNOWN_GAPS`
//!
//! Sixty-one filters across seven crates currently accept an invented
//! option name — a real, confirmed divergence in every case (verified
//! against the reference above), not a class this gate's own design
//! doubts. `vaco-filter-aeffects`/`-adynamics`/`-aeq`/`-aanalysis` already
//! had this same bug across their entire `Instantiate::named`-based
//! option surface, fixed by `common::ensure_known_options` in each crate;
//! these sixty-one are the same bug reached by building this gate rather
//! than a targeted sweep, in crates that were not part of that fix. Left
//! for a follow-up pass rather than folded into this dispatch, which is
//! already the gate this file exists to report, not to silently absorb
//! into a bigger scope.
//!
//! `vaco-filter-video-geometry`'s three (`hflip`, `scale`, `vflip`) are
//! additionally not this crate's to fix at all — `assigned`, no
//! completion date, per `planning/ASSIGNMENTS.md`, same standing rule as
//! `vaco-filter-color` in the sibling gate.

#![expect(
    clippy::expect_used,
    reason = "a failing expectation in a test is a failing test"
)]

use vaco_conformance::refbin::{self, Discovery, RefSpec};
use vaco_conformance::registries::REGISTRIES;
use vaco_filter_graph::ast;
use vaco_filter_graph::registry::Instantiate;

/// `(crate, filter)` pairs that currently accept an invented option name.
/// Every entry here is a real, reference-confirmed divergence (see the
/// module doc), not a doubted case — this is a tracking list for a
/// follow-up fix, not a way of pretending the gate covers more than it
/// does. Delete an entry once its filter's crate closes the gap; the gate
/// does not detect that on its own (an entry that stops failing is
/// quietly ignored rather than flagged as stale — the same known
/// limitation `option_consts_gate.rs`'s own `KNOWN_GAPS` carries).
const KNOWN_GAPS: &[(&str, &str)] = &[
    ("vaco-filter-analysis", "bbox"),
    ("vaco-filter-analysis", "blackdetect"),
    ("vaco-filter-analysis", "blackframe"),
    ("vaco-filter-analysis", "cropdetect"),
    ("vaco-filter-analysis", "entropy"),
    ("vaco-filter-analysis", "identity"),
    ("vaco-filter-analysis", "msad"),
    ("vaco-filter-analysis", "psnr"),
    ("vaco-filter-analysis", "showinfo"),
    ("vaco-filter-analysis", "signalstats"),
    ("vaco-filter-analysis", "ssim"),
    ("vaco-filter-audio", "adecorrelate"),
    ("vaco-filter-audio", "aformat"),
    ("vaco-filter-audio", "amultiply"),
    ("vaco-filter-audio", "asetnsamples"),
    ("vaco-filter-audio", "asetrate"),
    ("vaco-filter-audio", "channelmap"),
    ("vaco-filter-audio", "channelsplit"),
    ("vaco-filter-audio", "join"),
    ("vaco-filter-deinterlace", "repeatfields"),
    ("vaco-filter-deinterlace", "separatefields"),
    ("vaco-filter-deinterlace", "vfrdet"),
    ("vaco-filter-denoise", "atadenoise"),
    ("vaco-filter-denoise", "dctdnoiz"),
    ("vaco-filter-denoise", "fftdnoiz"),
    ("vaco-filter-denoise", "hqdn3d"),
    ("vaco-filter-denoise", "nlmeans"),
    ("vaco-filter-denoise", "owdenoise"),
    ("vaco-filter-denoise", "removegrain"),
    ("vaco-filter-denoise", "vaguedenoiser"),
    ("vaco-filter-geometry", "alphaextract"),
    ("vaco-filter-geometry", "alphamerge"),
    ("vaco-filter-geometry", "swapuv"),
    ("vaco-filter-mm", "acopy"),
    ("vaco-filter-mm", "anull"),
    ("vaco-filter-mm", "anullsink"),
    ("vaco-filter-mm", "areverse"),
    ("vaco-filter-mm", "alatency"),
    ("vaco-filter-mm", "copy"),
    ("vaco-filter-mm", "null"),
    ("vaco-filter-mm", "nullsink"),
    ("vaco-filter-mm", "reverse"),
    ("vaco-filter-mm", "latency"),
    ("vaco-filter-temporal", "decimate"),
    ("vaco-filter-temporal", "deflicker"),
    ("vaco-filter-temporal", "dejudder"),
    ("vaco-filter-temporal", "framestep"),
    ("vaco-filter-temporal", "freezedetect"),
    ("vaco-filter-temporal", "freezeframes"),
    ("vaco-filter-temporal", "lagfun"),
    ("vaco-filter-temporal", "mpdecimate"),
    ("vaco-filter-temporal", "random"),
    ("vaco-filter-temporal", "tblend"),
    ("vaco-filter-temporal", "tlut2"),
    ("vaco-filter-temporal", "tmedian"),
    ("vaco-filter-temporal", "tmidequalizer"),
    ("vaco-filter-temporal", "tmix"),
    ("vaco-filter-temporal", "tpad"),
    ("vaco-filter-video-geometry", "hflip"),
    ("vaco-filter-video-geometry", "scale"),
    ("vaco-filter-video-geometry", "vflip"),
];

/// A key guaranteed not to be a real option for anything: no reference
/// filter uses this spelling, and it carries no digits/case pattern that
/// would coincide with a real option by accident.
const INVENTED_KEY: &str = "zzz_totally_invented_option_name_xyz";

#[test]
fn invented_option_names_are_rejected() {
    let spec = RefSpec::load().expect("refspec.toml loads");
    let discovery = refbin::discover(&spec);
    let Discovery::Found(_reference) = &discovery else {
        println!("SKIPPED (no reference): {discovery:?}");
        return;
    };

    let mut checked = 0usize;
    let mut known_gaps = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for (crate_name, registry) in REGISTRIES {
        for name in registry.names() {
            let src = format!("{name}={INVENTED_KEY}=1");
            let Ok(parsed) = ast::parse(&src) else {
                continue;
            };
            let Some(spec) = parsed.chains.first().and_then(|c| c.filters.first()) else {
                continue;
            };
            let Ok(arguments) = spec.arguments() else {
                continue;
            };
            let req = Instantiate {
                name,
                instance: name,
                args: spec.args.as_deref(),
                arguments: &arguments,
            };
            checked += 1;
            if registry.create(&req).is_ok() {
                if KNOWN_GAPS.contains(&(*crate_name, name)) {
                    known_gaps += 1;
                } else {
                    failures.push(format!(
                        "{crate_name}: `{name}={INVENTED_KEY}=1` (an option name no reference \
                         filter documents) was silently accepted instead of rejected"
                    ));
                }
            }
        }
    }

    println!(
        "option_name_gate: checked {checked} filters across {} registries ({known_gaps} in \
         KNOWN_GAPS, real confirmed divergences tracked for a follow-up fix, excluded from the \
         pass/fail verdict)",
        REGISTRIES.len()
    );

    assert!(
        checked > 100,
        "the gate found suspiciously few filters to check ({checked}) — that almost certainly \
         means the parser or registry list broke, not that this corpus shrank"
    );

    assert!(
        failures.is_empty(),
        "{} filter(s) newly accept an invented option name (not already in KNOWN_GAPS):\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}
