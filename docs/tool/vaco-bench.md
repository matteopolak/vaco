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
and p95 in the row's declared physical unit. They also carry the machine label,
OS, architecture, CPU model, `rustc` version, build profile, git commit,
measurement backend, and unit. The one-minute load average is recorded as run
context but is deliberately not part of comparison identity.

The default `instant` backend measures in-process construction and reports
`scope = "instantiate"`, `backend = "instant"`, and `unit = "ns"`. This is the
portable path used by CI and the only automatic path on macOS. It does not
estimate CPU cycles from elapsed time.

On Linux, `--backend perf-stat` runs calibrated single-filter child batches
under the external `perf stat` command and reports
`scope = "subprocess-instantiate-batch"`, `backend = "perf-stat"`, and `unit =
"cycles"`. Each child repeats one filter for at least 20 ms, subject to
`--max-iterations`, to amortize process startup. Every work sample is paired
with a matched empty-control child using the same filter lookup and iteration
count; pair order alternates. JSONL records raw, control, and
`max(raw - control, 0)` corrected cycles per iteration, so executable startup,
CLI parsing, registry lookup, and loop overhead are quantified rather than
silently presented as filter work.

`perf stat` must expose a direct user-space CPU-cycle event. Missing,
`<not counted>`, `<not supported>`, malformed, or less-than-99%-running events
are rejected; counts are never multiplexing-scaled. `--backend auto` tries that
path on Linux and, after one warning, discards any partial cycle rows and reruns
the complete suite as `instant/ns` when it is unavailable. A forced
`--backend perf-stat` returns the backend error instead of changing units.

The subprocess scope includes dynamic process and filter initialization that a
matched empty child cannot remove perfectly. It is useful for registry-wide
tracking, not a claim about an isolated filter kernel. Focused algorithm work
belongs in the owning crate's Divan suite. `vaco-checkasm bench` has a different
Linux counter path: its permitted `vaco-hw-perf-event` dependency wraps an
in-process adapter batch and reports `perf-event/cycles`; those rows cannot be
compared with `vaco-bench` process batches.

A baseline matches only when filter, scope, backend, unit, and the complete
machine/toolchain fingerprint agree. An absent baseline, a new filter, a CPU
change, or a toolchain change is `incomparable`, never a regression. When an
explicit `--fail-under R` is supplied, only matched rows below that
`baseline/current` ratio make the command fail.

When history contains more than one matching row, the baseline is the median of
the seven most recent measurements by its recorded Unix timestamp. A noisy
single run therefore cannot silently replace the comparison point.

`vaco-bench report` turns combined JSONL history into a self-contained static
HTML table. It retains only the newest row for each complete identity and asks
the same trailing-seven matcher for that row's baseline; it does not implement
its own comparison rule. The table carries the current summary, baseline,
ratio, status, source commit, and full fingerprint. An explicit
`--generated-unix-ms` makes output reproducible for branch commits. See the
[results-store report](vaco-bench-results.md) for the branch layout and
extension rules.

```sh
cargo bench -p vaco-bench --bench filters

VACO_TARGET_DIR=/private/tmp/vaco-bench-target VACO_JOBS=2 \
  just bench-filter --json /private/tmp/filter-bench.jsonl

VACO_TARGET_DIR=/private/tmp/vaco-bench-target VACO_JOBS=2 \
  just bench-filter-compare /private/tmp/filter-bench.jsonl \
  --json /private/tmp/filter-bench-next.jsonl

VACO_TARGET_DIR=/private/tmp/vaco-bench-target VACO_JOBS=2 \
  just bench-report /private/tmp/filter-history.jsonl /private/tmp/filter-report.html \
  --generated-unix-ms 1700000000123

# Linux with a permitted PMU and perf installed; substantially slower.
VACO_TARGET_DIR=/private/tmp/vaco-bench-target VACO_JOBS=2 \
  just bench-filter --backend perf-stat \
  --json /private/tmp/filter-bench-cycles.jsonl
```

`vaco-bench list` prints the registry-derived benchmark ids without measuring.
It is the coverage generator: there is no second hand-maintained list to drift
from `-filters`.

### Gated-runner preflight

`vaco-bench machine-check` is the fail-closed preflight for the future whole-
pipeline macro suite. It only observes the host; changing machine state remains
an administrator action. A successful result requires Linux evidence for every
plan 12 §4.4 control: `performance` governor on every online CPU, disabled
turbo and SMT, this process pinned to exactly one CPU, that CPU in `nohz_full`,
ASLR disabled, and no IRQ affinity list that includes the pinned CPU. It also
records the transparent-huge-page setting, thermal-zone readings, and kernel
release without treating unavailable sensors as a pass.

The command exits `2` if a required file is inaccessible, a setting differs,
or the host is not Linux. That is intentional: a local filter-tracker row can
still be useful context, but it must never become a macro-regression baseline
until a dedicated runner has supplied all controls. For example, macOS fails
with an explicit unsupported-controlled-runner diagnostic rather than deriving
cycles from elapsed time.

```sh
CARGO_INCREMENTAL=0 RUSTC_WRAPPER= cargo run --release -p vaco-bench --locked \
  --target-dir /private/tmp/vaco-bench-target -- machine-check
```

### Whole-process macro scenarios

`vaco-bench macro` is the reusable S1--S10 execution primitive. It refuses to
start until `machine-check` succeeds, resolves one named entry from
`vaco-corpus`'s embedded `vaco-media.lock`, verifies or fetches it through the
existing opt-in corpus policy, and places the verified bytes in the benchmark
sandbox. The two command templates receive exact `{input}` and `{output}`
arguments. Each implementation must write an output whose SHA-256 equals the
required `--expected-output-sha256`; a failed child, missing output, or hash
mismatch discards the run rather than publishing a fast wrong result.

The runner performs at least eleven rounds and alternates Vaco/reference order
on every round. It records monotonic wall time and reports median, MAD, min,
and p95 separately for each command. The macro process accepts a generic
template because decode, encode, scale, resample, filters, seek, startup,
memory, and thread-determinism scenarios need different CLI arguments but all
share the same corpus, interleaving, and useful-output contract.

```sh
# Both commands must use {input} and {output} as whole arguments. Set
# VACO_CORPUS_NETWORK=1 only when a cache miss may fetch the locked asset.
vaco-bench macro --scenario S1/vp9/1080p/t1 \
  --asset vp9-size-10x10 --vaco /opt/vaco/vvmpeg --reference ffmpeg \
  --vaco-arg -i --vaco-arg '{input}' --vaco-arg -f --vaco-arg framemd5 --vaco-arg '{output}' \
  --reference-arg -i --reference-arg '{input}' --reference-arg -f --reference-arg framemd5 --reference-arg '{output}' \
  --expected-output-sha256 '<known-framemd5-sha256>' --rounds 11
```

## CI result tracking

`.github/workflows/filter-benchmarks.yml` has two paths:

- Pull requests that touch filters, their registry, or this harness run a
  single iteration of every registry-derived Divan case followed by a
  three-sample advisory tracker smoke, then upload its JSONL for 30 days.
- A daily or manually dispatched job assembles prior JSONL from the orphan
  `bench-results` branch, runs 11 samples, compares matching identities at D8's
  recorded 5% threshold, uploads the result for 90 days, and commits the new
  file under `results/filter/<machine>/<yyyy-mm>/<git-sha>.jsonl` together with
  `reports/filter/<machine>/index.html`. The report is generated from the
  branch history plus that new file and is staged in the same commit. A run that
  crosses the threshold remains available as an artifact but is not promoted
  into the rolling baseline or report.

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

Do not write report-specific matching code. Extend the shared stored-row parser
and trailing-baseline helper, then adapt the resulting classified rows to HTML.
The report deliberately has no charting package, JavaScript, or remote assets.

## Configuration

`filter` accepts `--backend instant|auto|perf-stat`, `--warmup`, `--samples`,
`--target-sample-ns`, `--max-iterations`, `--json`, `--baseline`, and
`--fail-under`. Defaults are the `instant` backend, 8 warmups, 11 samples, a
100 microsecond target batch, and at most 1,048,576 calls per sample. Perf-stat
batches raise the calibration target to at least 20 ms. `VACO_BENCH_MACHINE`
overrides the machine label used by CI; the CPU and toolchain still have to
match independently. `VACO_BENCH_PERF` overrides the `perf` executable on
Linux, for example when the matching kernel-tools binary is not on `PATH`.

`machine-check` accepts no options. It reads the live `/proc` and `/sys` views;
there are deliberately no environment-variable overrides that could turn a
missing hardware control into a passing result.

`macro` requires `--scenario`, `--asset`, `--vaco`, `--reference`, and
`--expected-output-sha256`. Repeat `--vaco-arg` and `--reference-arg` to build
each command line. `--cache-dir` selects a corpus object-store root and
`--rounds` defaults to 11; lower values are rejected.

`report` accepts `--input`, `--output`, optional `--generated-unix-ms`, and
`--fail-under` (default `0.95`). Its input is combined schema-1 filter JSONL;
it fails closed on malformed or foreign rows instead of silently dropping them.

## Dependencies

The tracker uses the Rust standard library for timing, process execution,
statistics, JSONL, host fingerprinting, and temporary-directory containment.
Linux cycle mode additionally requires an installed `perf` whose kernel policy
allows user-space hardware counters. The crate depends on `vaco-registry` and
`vaco-filter-graph` to exercise the real generated filter registry; its
development-only benchmark dependency is the workspace's chosen `divan`
version. It adds no serialization, statistics, or unsafe counter dependency.
