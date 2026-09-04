# Checkasm Linux perf-event backend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `subagent-driven-development` (recommended) or `executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Report in-process Linux per-kernel hardware CPU-cycle samples when the PMU grants an unmultiplexed counter, with an explicit `Instant` nanosecond fallback everywhere else.

**Architecture:** Keep `bench.rs`'s sampling, cache protocol, and no-op calibration independent of the measurement source. A persistent Linux `perf_event::Counter` wraps exactly one batch: reset/enable before the call loop, then disable/read after it. Any setup, ioctl, read, zero-running-time, or multiplexed result rejects the PMU sample and switches the entire benchmark invocation to `instant`/`ns`; rows are never labelled `cycles` unless they are direct unmultiplexed CPU-cycle counts.

**Tech Stack:** Rust standard library, Linux-target-only `perf-event 0.4.9`, existing integration tests.

---

### Task 1: Lock measurement identity and fallback behavior with tests

**Files:**

- Modify: `crates/tool/vaco-checkasm/tests/bench_mode.rs`
- Modify: `crates/tool/vaco-checkasm/src/bench.rs`

- [ ] **Step 1: Write failing identity tests**

  Add an injected measurement source that returns a named metric and samples. Assert a `perf-event`/`cycles` source labels both the no-op and kernel rows identically, and that a stored row with either `backend = "instant"` or `unit = "ns"` cannot attach a baseline ratio to a `perf-event`/`cycles` result.

- [ ] **Step 2: Run the focused test and confirm the pre-backend code cannot provide the injected source**

  Run: `CARGO_INCREMENTAL=0 cargo test -p vaco-checkasm --test bench_mode measurement_identity --target-dir /private/tmp/vaco-target-checkasm-cycles`

  Expected: compilation failure because the injected backend seam does not exist.

- [ ] **Step 3: Implement the smallest measurement seam**

  Define one private `Metric { backend, unit }` pair and a private measurement abstraction that returns a batch value in that metric. Make calibration, no-op samples, and kernel samples take the same mutable source. Preserve the current `instant` source as the universal fallback.

- [ ] **Step 4: Re-run the focused test**

  Run the Task 1 command again.

  Expected: PASS.

### Task 2: Add the Linux PMU source and public observability

**Files:**

- Modify: `crates/tool/vaco-checkasm/Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `crates/tool/vaco-checkasm/src/bench.rs`
- Modify: `crates/tool/vaco-checkasm/src/main.rs`
- Modify: `crates/tool/vaco-checkasm/tests/bench_cli.rs`

- [ ] **Step 1: Write failing Linux-gated source-selection tests**

  Test that synthetic PMU creation/read failure selects `instant`/`ns` and records the reason, while a valid synthetic unmultiplexed cycle source selects `perf-event`/`cycles`. Keep the test OS-independent through the injected measurement source.

- [ ] **Step 2: Run the focused test and confirm failure**

  Run: `CARGO_INCREMENTAL=0 cargo test -p vaco-checkasm --test bench_mode fallback --target-dir /private/tmp/vaco-target-checkasm-cycles`

  Expected: FAIL until PMU selection/fallback is implemented.

- [ ] **Step 3: Add the Linux-only dependency and source**

  Add `[target.'cfg(target_os = "linux")'.dependencies] perf-event = "0.4.9"`. Build a persistent `Hardware::CPU_CYCLES` counter with `pinned(true)` (and the wrapper defaults that exclude kernel and hypervisor execution). For each batch call `reset`, `enable`, the existing call loop, `disable`, then `read_count_and_time`; require `time_running == time_enabled` and both non-zero. On any failure, abandon the PMU source before reporting a row and re-run that variant through `Instant`; the selection warning is emitted by the CLI, not embedded in a numeric field.

- [ ] **Step 4: Re-run focused tests and CLI integration test**

  Run: `CARGO_INCREMENTAL=0 cargo test -p vaco-checkasm --test bench_mode --test bench_cli --target-dir /private/tmp/vaco-target-checkasm-cycles`

  Expected: PASS. On macOS, the CLI assertion remains `backend=instant unit=ns`; Linux integration accepts the only two legal metric pairs.

### Task 3: Document and verify the contract

**Files:**

- Modify: `docs/tool/vaco-checkasm.md`

- [ ] **Step 1: Document the exact Linux and fallback conditions**

  Replace the unimplemented-backend note with: Linux selects `perf-event`/`cycles` only after a pinned, per-thread `CPU_CYCLES` counter returns complete, non-multiplexed samples. Lack of permission, unsupported events, counter ioctl/read failures, or multiplexing selects `instant`/`ns` and writes a warning. macOS uses `instant`/`ns`.

- [ ] **Step 2: Run checks only while one-minute load is below 8**

  Run: `uptime`; if the one-minute load is below 8, run `CARGO_INCREMENTAL=0 cargo test -p vaco-checkasm --target-dir /private/tmp/vaco-target-checkasm-cycles`, `CARGO_INCREMENTAL=0 cargo clippy -p vaco-checkasm --all-targets -- -D warnings`, `cargo fmt --check`, `cargo run -p xtask -- docs-index-check`, and `cargo run -p xtask -- comment-check` with the same private target directory where each command supports it. Remove `/private/tmp/vaco-target-checkasm-cycles` after verification.

- [ ] **Step 3: Linux acceptance check**

  On a Linux host with a permitted PMU, run `vaco-checkasm bench --test vaco-simd::ops::select_u8 --bench-cache hot --min-samples 3 --budget 50`. Close #93 only when output and JSONL say `backend=perf-event unit=cycles`; otherwise report the exact host/permission blocker and leave the issue open.
