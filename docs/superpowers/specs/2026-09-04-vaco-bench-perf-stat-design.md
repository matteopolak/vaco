# Truthful `vaco-bench` CPU-cycle backend design

## Goal

Add an external Linux `perf stat` measurement backend to `vaco-bench` without
introducing `unsafe`, estimating cycles from time, or comparing unlike metrics.
The existing `instant/ns` backend remains the default for CI and the only
automatic choice on macOS.

## Existing backends

`vaco-checkasm` already has the right inner-loop instrument: on Linux x86_64
and aarch64 it uses a persistent, pinned `perf_event_open` counter around each
in-process adapter batch. It reports `perf-event/cycles` only for a direct,
unmultiplexed read and reruns the benchmark as `instant/ns` otherwise. The
Linux UAPI boundary lives in the permitted `vaco-hw-perf-event` crate; no
checkasm code uses `unsafe`.

`vaco-bench` currently measures every registry filter with `Instant`, reports
`instant/ns`, and stores machine, OS, architecture, CPU, compiler, profile,
backend, and unit in each JSONL row. This design extends that tool rather than
replacing checkasm's lower-overhead per-thread counter.

The standalone `scripts/perf-hwcycles.py` remains the process-level Vaco versus
reference harness. Its complete command measurements are a different scope
from a filter-construction row and are not imported into `vaco-bench` history.

## CLI and backend selection

`vaco-bench filter --backend instant|auto|perf-stat` selects the instrument.
The default is `instant`, preserving current CI cost and history. `auto` tries
`perf stat` only on Linux, then reruns the complete suite with `Instant` after
one clear warning if the executable, event, permission, or counter is
unavailable. No output row from the failed attempt is retained. An explicit
`perf-stat` request returns a clear unavailable error instead of silently
changing its requested unit.

Successful external measurements use `backend = "perf-stat"`, `unit =
"cycles"`, and a distinct `scope = "subprocess-instantiate-batch"`. Portable
rows remain `backend = "instant"`, `unit = "ns"`, and `scope = "instantiate"`.
Those identity fields make cross-backend and cross-scope baseline matches
impossible.

## Measurement flow

The parent first validates the registry and performs the existing in-process
warm-up and calibration. A perf row uses at least a 20 ms calibrated batch,
subject to the existing maximum-iteration cap, so one process launch is
amortized over repeated construction of exactly one filter.

For every independently measured sample, the parent runs two children under
`perf stat`: one work batch and one matched control batch. Their order
alternates between samples. Both children resolve the same filter case and run
the same loop count; the work child constructs the filter, while the control
child performs a black-box control operation. This makes executable startup,
CLI parsing, registry lookup, and loop overhead visible instead of presenting
them as filter cycles.

Each row stores three distributions:

- raw cycles per iteration from the work child;
- control cycles per iteration from the matched child;
- corrected cycles per iteration, `max(raw - control, 0)` for each pair.

The existing `median`, `mad`, `min`, and `p95` fields remain the corrected
distribution used for comparisons. New raw and control summary fields quantify
the subprocess cost. A row never claims an isolated kernel measurement: the
scope name and documentation explicitly include process batching and filter
construction. Expensive filter kernels continue to use focused Divan or
checkasm suites.

The child entrypoint is intentionally hidden from public help. It accepts one
registry name, an iteration count, an expected construction outcome, and
`work` or `control`. It inherits the parent's owned temporary working
directory, so constructors with relative outputs cannot dirty the checkout.

## `perf stat` contract and parser

The runner executes the current binary under a command equivalent to:

```text
perf stat --no-big-num -x ; -e cycles:u -o <owned-temp-file> --
  vaco-bench __filter-batch <mode> <filter> <iterations> <outcome>
```

The parser consumes perf's delimited output, identifies generic and hybrid-PMU
cycle event spellings, and sums usable hybrid rows. It rejects missing counts,
`<not counted>`, `<not supported>`, malformed numeric fields, and a reported
running percentage below 99%. It never scales a multiplexed count and never
converts elapsed seconds to cycles.

Unit tests use captured representative text for a normal count, hybrid-PMU
rows, unsupported/not-counted events, malformed output, and a multiplexed
event. Parser tests do not require Linux, PMU permissions, or an installed
`perf` executable. CLI tests cover explicit unsupported-platform behavior and
the unchanged `instant/ns` default.

## Errors and cleanup

Temporary counter files use owned, collision-resistant paths inside the
benchmark sandbox and are removed after parsing. Spawn failure, child failure,
missing output, parse failure, and counter rejection become a typed backend
error with the relevant filter name. `auto` handles that error once at suite
level and starts over on `Instant`; forced `perf-stat` returns it to the user.

The design adds no performance threshold and makes no cycle claim from this
macOS development host. Linux runtime evidence is recorded only when a Linux
host actually exposes a usable PMU.

## Files and extension points

- `crates/tool/vaco-bench/src/perf_stat.rs` owns parsing and external command
  execution.
- `crates/tool/vaco-bench/src/lib.rs` owns backend selection, calibration,
  row construction, and matched work/control statistics.
- `crates/tool/vaco-bench/src/main.rs` parses `--backend` and dispatches the
  hidden child batch.
- `crates/tool/vaco-bench/tests/cli.rs` pins user-visible selection and labels.
- `docs/tool/vaco-bench.md` explains operation, scope, configuration, and
  dependencies; `docs/tool/vaco-checkasm.md` records the backend audit and the
  distinction between the two counter paths.

To add another truthful counter source later, give it a new backend and scope,
return values in its physical unit, and extend baseline identity before making
it comparable. Do not route a failed counter through a time-to-cycle estimate.
