# `vaco-bench` — registry-complete benchmark tracking

## What it is

`vaco-bench` gives every enabled filter a stable machine-readable benchmark row
and compares new measurements with like-for-like JSONL history. It complements
the focused `divan` suites in individual crates: those measure algorithm hot
paths, while this crate's generated `filters` Divan suite and tracker make
complete registry coverage and CI history possible.

## How it works

The filter suite iterates `vaco_registry::filters()` directly. For each
descriptor it resolves and constructs the default filter through the production
`vaco_registry::Filters` implementation. That `instantiate` scope catches
regressions in option parsing, lookup, filter setup, and construction; it does
not claim to measure the filter's per-frame kernel. Expensive kernels should
keep a focused `divan` bench beside their crate.

Some filters require a filename, timestamp list, or another mandatory option.
For those names the same default request intentionally reaches the validation
error path and the row says `outcome = "rejected"`; all other rows say
`outcome = "created"`. Outcome is part of comparison identity, so adding a
default later starts a new series rather than comparing different work.

Both entrypoints change into an owned temporary working directory for the
duration of the suite. Constructors such as `stabdetect` open a relative output
file from their defaults; the sandbox contains and deletes those files so a
benchmark cannot dirty the checkout. A process-wide guard serializes direct
library callers because changing the working directory affects every thread.

Each filter is warmed up, calibrated to a minimum batch duration, and sampled
independently. JSONL rows contain median, median absolute deviation, minimum,
and p95 nanoseconds per construction. They also carry the machine label, OS,
architecture, CPU model, `rustc` version, build profile, and git commit.
The one-minute load average is recorded as run context but is deliberately not
part of comparison identity.

A baseline matches only when filter, scope, backend, unit, and the complete
machine/toolchain fingerprint agree. An absent baseline, a new filter, a CPU
change, or a toolchain change is `incomparable`, never a regression. When an
explicit `--fail-under R` is supplied, only matched rows below that
`baseline/current` ratio make the command fail.

When history contains more than one matching row, the baseline is the median of
the seven most recent measurements by its recorded Unix timestamp. A noisy
single run therefore cannot silently replace the comparison point.

```sh
cargo bench -p vaco-bench --bench filters

VACO_TARGET_DIR=/private/tmp/vaco-bench-target VACO_JOBS=2 \
  just bench-filter --json /private/tmp/filter-bench.jsonl

VACO_TARGET_DIR=/private/tmp/vaco-bench-target VACO_JOBS=2 \
  just bench-filter-compare /private/tmp/filter-bench.jsonl \
  --json /private/tmp/filter-bench-next.jsonl
```

`vaco-bench list` prints the registry-derived benchmark ids without measuring.
It is the coverage generator: there is no second hand-maintained list to drift
from `-filters`.

## CI result tracking

`.github/workflows/filter-benchmarks.yml` has two paths:

- Pull requests that touch filters, their registry, or this harness run a
  single iteration of every registry-derived Divan case followed by a
  three-sample advisory tracker smoke, then upload its JSONL for 30 days.
- A daily or manually dispatched job assembles prior JSONL from the orphan
  `bench-results` branch, runs 11 samples, compares matching identities at D8's
  recorded 5% threshold, uploads the result for 90 days, and commits the new
  file under `results/filter/<machine>/<yyyy-mm>/<git-sha>.jsonl`. A run that
  crosses the threshold remains available as an artifact but is not promoted
  into the rolling baseline.

Both jobs are advisory because GitHub-hosted runners are shared and timing noise
can exceed small code changes. The comparison still identifies algorithmic
shifts and preserves the measurements for inspection. A dedicated controlled
runner can make the same command blocking without changing the schema.

## How to change it

Do not add a filter-name list to this crate. Register the filter normally; the
next `list`, measurement, and coverage test pick it up automatically. Add or
adjust a focused per-frame `divan` suite in the owning filter crate when the
algorithm itself needs a performance guard.

If the measurement scope changes, give it a new `scope` string rather than
silently comparing old and new numbers. If fingerprint fields change, bump the
JSONL schema and retain the old parser until stored history has aged out.

## Configuration

`filter` accepts `--warmup`, `--samples`, `--target-sample-ns`,
`--max-iterations`, `--json`, `--baseline`, and `--fail-under`. Defaults are 8
warmups, 11 samples, a 100 microsecond target batch, and at most 1,048,576 calls
per sample. `VACO_BENCH_MACHINE` overrides the machine label used by CI; the CPU
and toolchain still have to match independently.

## Dependencies

The tracker uses the Rust standard library for timing, statistics, JSONL, and
host fingerprinting, including the temporary-directory containment. The crate depends on `vaco-registry` and
`vaco-filter-graph` to exercise the real generated filter registry; its
development-only benchmark dependency is the workspace's chosen `divan`
version. It adds no serialization or statistics dependency.
