# Scale Vertical-Filter Checkasm Design

## Goal

Add a checkasm adapter that directly differential-checks and benchmarks
`vaco-scale`'s generic and fixed-width vertical-filter implementations. On
supported Linux hosts the existing `perf_event` backend reports raw CPU cycles;
macOS and unsupported or restricted Linux hosts report elapsed nanoseconds
without relabelling time as cycles.

## Boundary

The production filter functions and `Grid` stay private. `vaco-scale` gains a
default-off, documentation-hidden `checkasm` feature exposing only an opaque
`FilterVCase` plus constructors and generic/fixed runners. The adapter module is
a child of `exec`, so it may call the private production functions directly
without widening the normal public API.

The feature is tooling-only and patent-neutral. It introduces no codec or
format implementation and is not enabled by `vaco-scale`'s default feature set;
only `vaco-checkasm` enables it on its downward dependency.

## Case and runner contract

`FilterVCase::synthetic(taps, width, dst_rows)` constructs a deterministic,
non-gather bank for tap counts 2, 4, 6, or 8 and a matching signed-sample grid.
Coefficients vary by output row while remaining normalised to `COEFF_ONE`, and
the source rows vary by both row and column so wrong offsets, coefficient rows,
and truncated widths remain observable.

Both runners allocate the same output vector and pre-sized scratch vector once
through the repository's `Budget` API,
then invoke their respective production callee for every destination row. The
generic runner calls `filter_v_generic`; the fixed runner dispatches directly to
`filter_v_fixed::<2/4/6/8>` rather than going through `filter_v`'s fallback. A
leading completion lane records whether every direct fixed call succeeded, so a
zero-width case cannot make an incomplete fixed path look equal by producing two
empty pixel outputs.

These allocations remain part of checkasm's existing `adapter-inclusive`
measurement scope. Keeping them symmetric prevents either implementation from
receiving an adapter-allocation advantage; the per-row filter comparison itself
does not allocate.

## Checkasm adapter

The built-in kernel name is
`vaco-scale::filter_v_generic_vs_fixed`. Its deterministic correctness corpus
crosses taps 2/4/6/8, widths around the i32 lane widths returned by
`edge::element_widths(4)`, and destination heights 1/2/3. The benchmark override
uses an 8-tap, 1920-column, 1080-row case, with 1087 input rows, and constructs
that case outside the timed closure.

The existing checkasm variant names remain `scalar` and `vector` for schema and
baseline compatibility. For this adapter they mean generic and fixed-width,
respectively; the tool documentation states that mapping explicitly.

## Measurement and verification

The existing backend contract is unchanged. On Linux x86_64/aarch64, a direct,
unmultiplexed per-thread PMU reading emits `backend=perf-event unit=cycles`.
Permission failure, unsupported hardware, counter multiplexing, and all other
targets rerun under `Instant` and emit `backend=instant unit=ns`.

Tests verify that the corpus is non-empty, all four tap counts execute, every
generic/fixed output matches exactly, the production-size case returns its full
`1920 * 1080 + 1` lanes, and the CLI emits exactly two hot-cache rows carrying
one of the two legal backend/unit pairs. No unsafe code or change to
`vaco-hw-perf-event`, the `Kernel` trait, or benchmark JSON schema is required.

## Files

- `crates/signal/vaco-scale/Cargo.toml`: declare the default-off feature.
- `crates/signal/vaco-scale/src/exec.rs`: define the hidden case adapter beside
  the private callees.
- `crates/tool/vaco-checkasm/Cargo.toml`: enable the adapter feature.
- `crates/tool/vaco-checkasm/src/kernels/scale_filter_v.rs`: implement the
  checkasm kernel and its focused tests.
- `crates/tool/vaco-checkasm/src/kernels/mod.rs` and `src/main.rs`: register the
  built-in kernel.
- `crates/tool/vaco-checkasm/tests/bench_cli.rs`: pin CLI measurement labeling
  for this exact adapter.
- `docs/signal/vaco-scale.md` and `docs/tool/vaco-checkasm.md`: document the
  feature boundary, corpus, benchmark scope, and platform-specific units.
