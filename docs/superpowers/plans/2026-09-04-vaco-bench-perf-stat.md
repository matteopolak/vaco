# Truthful `vaco-bench` CPU-Cycle Backend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a real Linux `perf stat` cycle backend to `vaco-bench`, with paired subprocess-overhead correction, truthful metadata, deterministic parsing, and explicit nanosecond fallback.

**Architecture:** Keep the current in-process `Instant` path as the default and isolate external counter handling in `perf_stat.rs`. A hidden child command performs one calibrated single-filter work or control batch; the parent alternates paired work/control processes, parses only direct usable cycle counts, and stores raw, control, and corrected per-iteration distributions under a distinct backend/scope/unit identity.

**Tech Stack:** Rust standard library, Linux `perf stat` CLI, existing registry/filter APIs, JSONL tracker, Cargo unit/integration tests.

---

## File map

- Create `crates/tool/vaco-bench/src/perf_stat.rs`: perf CSV parser, command runner, owned counter-file cleanup, and parser tests.
- Modify `crates/tool/vaco-bench/src/lib.rs`: backend selection, batch calibration, child batch implementation, paired cycle statistics, JSONL fields, and fallback tests.
- Modify `crates/tool/vaco-bench/src/main.rs`: `--backend` parsing, unit-neutral output, and hidden child dispatch.
- Modify `crates/tool/vaco-bench/tests/cli.rs`: portable/default labels and unsupported forced-backend behavior.
- Modify `docs/tool/vaco-bench.md`: operation, scope, configuration, errors, and dependencies.
- Modify `docs/README.md`: regenerate the docs index after adding the design and plan pages.

### Task 1: Pin the perf-output contract

**Files:**
- Create: `crates/tool/vaco-bench/src/perf_stat.rs`
- Modify: `crates/tool/vaco-bench/src/lib.rs`

- [ ] **Step 1: Write parser tests before production parsing code**

Add captured-output tests for a direct count, two hybrid-PMU rows, an event below 99% running, `<not counted>`, `<not supported>`, a malformed numeric count, and output without any cycle event. The desired public boundary is:

```rust
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PerfStatError {
    MissingCycles,
    Unavailable(String),
    MalformedCount(String),
    Multiplexed(f64),
}

pub(crate) fn parse_cycles(output: &str) -> Result<u64, PerfStatError>;
```

Use representative semicolon-delimited rows such as:

```text
1250000;;cycles:u;500000;100.00;;
750000;;cpu_core/cycles/u;500000;100.00;;
250000;;cpu_atom/cycles/u;500000;100.00;;
<not counted>;;cycles:u;0;0.00;;
```

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```sh
CARGO_INCREMENTAL=0 RUSTC_WRAPPER= cargo test -p vaco-bench --lib perf_stat::tests --locked --offline --target-dir /private/tmp/vaco-128-target -j2
```

Expected: compile failure because `parse_cycles` and `PerfStatError` do not yet exist.

- [ ] **Step 3: Implement the minimal parser**

Split non-empty lines on `;`, recognize `cycles`, `cycles:u`, and hybrid spellings containing a `/cycles/` component, parse the no-grouping integer value, and sum multiple usable rows with `checked_add`. Reject unavailable markers and percentages below `99.0`; never parse time fields or scale counts.

- [ ] **Step 4: Re-run the focused test and verify GREEN**

Run the Task 1 command. Expected: every parser case passes without invoking `perf`.

### Task 2: Pin calibrated child work and control

**Files:**
- Modify: `crates/tool/vaco-bench/src/lib.rs`
- Modify: `crates/tool/vaco-bench/src/main.rs`

- [ ] **Step 1: Write failing library tests**

Add tests that resolve a registry filter by its generated name, reject an unknown name and zero iterations, preserve a supplied `created` or `rejected` expectation in work mode, and complete the same iteration count in control mode without constructing the filter. Define the intended API in the test:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildBatchMode { Work, Control }

pub fn run_filter_child_batch(
    mode: ChildBatchMode,
    name: &str,
    iterations: usize,
    expected: &'static str,
) -> Result<(), BenchError>;
```

- [ ] **Step 2: Run focused tests and verify RED**

Run the Task 1 Cargo command with the child-batch test names. Expected: compile failure for the missing child API.

- [ ] **Step 3: Implement the child batch**

Resolve the requested name from `filter_cases()` so there is no second registry list. In work mode, run `instantiate_filter` and verify every result matches the expected stable outcome. In control mode, black-box the same case/name in the same loop shape. Add `UnknownFilter`, `InvalidOutcome`, and the existing invalid-config errors to `BenchError` as needed.

- [ ] **Step 4: Add hidden CLI dispatch**

Dispatch `__filter-batch <work|control> <filter> <iterations> <created|rejected>` before public commands. Parse strictly, emit no measurement text, and keep this command out of `--help`.

- [ ] **Step 5: Re-run focused tests and verify GREEN**

Run the Task 2 test selection. Expected: work and control paths pass without invoking Cargo benchmarks or `perf`.

### Task 3: Implement external cycle collection and suite-wide fallback

**Files:**
- Modify: `crates/tool/vaco-bench/src/perf_stat.rs`
- Modify: `crates/tool/vaco-bench/src/lib.rs`

- [ ] **Step 1: Write failing backend-policy tests**

Introduce an internal fake cycle runner and tests that prove:

- paired sample order is work/control, then control/work;
- raw and control totals are divided by the identical calibrated iteration count;
- corrected samples are `max(raw - control, 0) / iterations`;
- a backend error after an earlier row causes `auto` to discard every perf row and rerun the complete suite as `instant/ns`;
- forced `perf-stat` returns the backend error;
- instant rows expose no synthetic control count and retain `instantiate`, `instant`, and `ns`.

The configuration boundary becomes:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MeasurementBackend {
    Auto,
    #[default]
    Instant,
    PerfStat,
}
```

- [ ] **Step 2: Run backend-policy tests and verify RED**

Run the focused library tests in the private target. Expected: compile failure for missing backend configuration and runner abstractions.

- [ ] **Step 3: Implement the external runner**

On Linux, spawn:

```text
perf stat --no-big-num -x ; -e cycles:u -o <owned-path> --
  <current-exe> __filter-batch <mode> <name> <iterations> <outcome>
```

Set `LC_ALL=C`, discard child stdout, retain bounded stderr for diagnostics, read and parse the owned output file, and remove it on success or failure. Off Linux return a typed unsupported error without attempting to spawn `perf`.

- [ ] **Step 4: Implement paired perf suite measurement**

Warm and calibrate in-process. Perf calibration uses `max(config.target_sample_ns, 20_000_000)` and respects `max_iterations`. For each requested sample, alternate work/control order and collect three per-iteration vectors: raw, control, and saturating corrected. Summarize all three.

Add `raw_stats: Statistics` and `control_stats: Option<Statistics>` to each row. Instant rows set `raw_stats == stats` and `control_stats == None`; perf rows set all three summaries. Successful perf rows use `subprocess-instantiate-batch`, `perf-stat`, and `cycles`.

- [ ] **Step 5: Implement selection and fallback**

`Instant` directly runs the portable suite. `PerfStat` runs the external suite or returns `BenchError::BackendUnavailable`. `Auto` tries perf only on Linux; on any backend failure it emits one warning, discards the partial vector, and reruns every filter with Instant. Off Linux `Auto` selects Instant directly without warning.

- [ ] **Step 6: Re-run backend-policy tests and verify GREEN**

Run the focused library tests. Expected: every fake-runner policy test passes without requiring host PMU access.

### Task 4: Expose truthful CLI and JSONL metadata

**Files:**
- Modify: `crates/tool/vaco-bench/src/main.rs`
- Modify: `crates/tool/vaco-bench/src/lib.rs`
- Modify: `crates/tool/vaco-bench/tests/cli.rs`

- [ ] **Step 1: Write failing CLI/JSONL tests**

Extend the existing instant smoke to require `scope=instantiate`,
`backend=instant`, `unit=ns`, raw summary fields equal to corrected fields, and
null control fields. Add `--backend nonsense` usage rejection. On non-Linux,
add a forced `--backend perf-stat` test that requires a clear unsupported error
and no JSONL output.

Add a library test constructing one synthetic perf result and asserting JSONL
contains `scope=subprocess-instantiate-batch`, `backend=perf-stat`,
`unit=cycles`, and numeric raw/control/corrected summary fields.

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```sh
CARGO_INCREMENTAL=0 RUSTC_WRAPPER= cargo test -p vaco-bench --tests --locked --offline --target-dir /private/tmp/vaco-128-target -j2
```

Expected: failures for missing CLI flag and metadata fields.

- [ ] **Step 3: Implement CLI and schema output**

Parse `--backend instant|auto|perf-stat` into `FilterBenchConfig`. Replace
hard-coded `ns` suffixes in terminal output with `row.unit`. Extend JSONL with
raw and control median/MAD/min/p95 fields while retaining the corrected fields
and complete machine/backend/unit identity. Baseline parsing must continue to
match only exact scope/backend/unit/machine/toolchain identities.

- [ ] **Step 4: Re-run focused tests and verify GREEN**

Run the Task 4 test command. Expected: all vaco-bench tests pass.

### Task 5: Document, verify, and report honestly

**Files:**
- Modify: `docs/tool/vaco-bench.md`
- Modify: `docs/README.md`

- [ ] **Step 1: Update developer documentation**

Document the three backend modes, Linux PMU/perf requirement, >=20 ms batch,
paired alternating control subtraction, physical units, distinct scopes,
suite-wide auto fallback, forced error, macOS behavior, runtime cost, and why
CI remains instant. Contrast checkasm's direct per-thread
`perf-event/cycles` backend with vaco-bench's external process-batch scope in
the vaco-bench page without editing checkasm's independently owned docs.

- [ ] **Step 2: Regenerate the docs index**

After a Cargo slot is granted, run:

```sh
CARGO_INCREMENTAL=0 RUSTC_WRAPPER= cargo run -p xtask --locked --offline --target-dir /private/tmp/vaco-128-target -j2 -- gen-docs-index
```

Expected: the generated crate table remains current without losing concurrent
package rows. Design and implementation-plan pages are intentionally outside
the crate index.

- [ ] **Step 3: Run final verification**

Run targeted rustfmt, all vaco-bench tests, strict all-target Clippy, the default
one-sample registry suite, explicit auto fallback on this macOS host, docs-index
check, layer check, comment check, unsafe check, and provenance check using the
same private target and `-j2`. Do not claim Linux cycle execution from macOS.

- [ ] **Step 4: Run Linux perf smoke only when available and authorized**

On a real Linux PMU host, run one named row with one measured sample, record
backend/unit/scope plus raw/control/corrected values, and retain the exact perf
version and counter-running percentage in evidence. If no such host is
available, report parser/policy coverage and the unavailable path only.

- [ ] **Step 5: Commit and report**

Use the private-index CAS recipe for only the files in this plan, verify the
commit is non-empty and remains an ancestor of `main`, push it, and comment on
#95 with exact test evidence. Leave #95 open because this infrastructure slice
does not complete its macro scenarios, corpus tooling, or machine-control
acceptance scope; #128 remains separate H.264 implementation work.
