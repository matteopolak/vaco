# Filter Benchmark Tracking Design

## Goal

Give every enabled filter a stable, registry-derived benchmark identity and make
like-for-like regressions visible in CI without treating noisy or incomparable
measurements as failures.

## Architecture

`vaco-bench` is a layer-10 tool. Its filter suite reads
`vaco_registry::filters()` at runtime, so adding or removing a registered filter
changes benchmark coverage automatically. Every row measures the same narrow
operation: default filter instantiation through the production `Filters`
registry, including the stable validation path for filters whose required
arguments deliberately reject a default request. The crate also exposes one
registry-derived argument per filter through its `divan` suite. Both entrypoints
run inside an owned temporary working directory so constructors with relative
default outputs cannot dirty the checkout. Existing per-crate `divan` suites
remain the place for algorithm hot paths; this tool supplies complete per-filter
coverage and machine-readable tracking that `divan` 0.1 does not expose.

Results are JSONL. Identity includes filter name, scope, outcome, backend/unit,
machine label, operating system, architecture, CPU, Rust compiler, and build
profile. A baseline attaches only when all those fields match. The comparison
value is the median of the seven most recent matching measurements. Missing or
incomparable rows are reported but never fail. An explicit `--fail-under` ratio
may gate matched rows; no threshold is embedded in the tool.

## CI and storage

Pull requests run a short advisory smoke and retain its JSONL as an artifact.
A scheduled/manual workflow runs the measured suite, compares it with the latest
like-for-like row on the `bench-results` branch when one exists, and records the
new result. The branch contains results only; the main checkout is not mutated.

## Verification

Tests prove registry-derived one-row-per-filter coverage, JSONL round-tripping,
full-identity baseline matching, and the non-failing absent/incomparable baseline
rule. A release smoke proves the real registry is measurable and records current
machine/load context without asserting that the numbers are portable.
