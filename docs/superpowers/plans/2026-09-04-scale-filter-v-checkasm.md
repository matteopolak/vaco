# Scale Vertical-Filter Checkasm Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Differential-check and benchmark `vaco-scale`'s generic and fixed vertical filters with honest Linux PMU cycles or explicitly labelled nanoseconds elsewhere.

**Architecture:** A default-off, doc-hidden child module in `exec.rs` owns an opaque synthetic case and calls the existing private production callees directly. A normal `vaco-checkasm::Kernel` adapter maps `scalar` to generic and `vector` to fixed, while the unchanged benchmark backend selects `perf-event/cycles` or `instant/ns`.

**Tech Stack:** Rust workspace features, `vaco-checkasm` differential and benchmark APIs, `vaco-hw-perf-event` on supported Linux targets.

---

### Task 1: Add the private production adapter

**Files:**
- Modify: `crates/signal/vaco-scale/Cargo.toml`
- Modify: `crates/signal/vaco-scale/src/exec.rs`

- [ ] **Step 1: Declare a default-off tooling feature**

Add:

```toml
[features]
checkasm = []
```

Do not add `default = ["checkasm"]`; ordinary `vaco-scale` consumers must not
compile the adapter.

- [ ] **Step 2: Make the private grid cloneable for an owned checkasm case**

Change only the private derive:

```rust
#[derive(Debug, Clone)]
pub(crate) struct Grid {
```

- [ ] **Step 3: Add the feature-gated child adapter beside the vertical filters**

Add `#[cfg(feature = "checkasm")] #[doc(hidden)] pub mod checkasm` in
`exec.rs`. It must define:

```rust
#[derive(Debug, Clone)]
pub struct FilterVCase {
    bank: FilterBank,
    src: Grid,
    shift: u8,
}

impl FilterVCase {
    pub fn synthetic(taps: usize, width: usize, dst_rows: usize) -> Option<Self>;
    pub fn taps(&self) -> usize;
    pub fn output_len(&self) -> usize;
}

pub fn run_generic(case: &FilterVCase) -> Vec<i32>;
pub fn run_fixed(case: &FilterVCase) -> Vec<i32>;
```

The constructor accepts only 2/4/6/8 taps, checks all size arithmetic, builds
`dst_rows + taps - 1` source rows, uses offset `d` for destination row `d`, and
normalises every coefficient row exactly to `COEFF_ONE`. Source sample and
coefficient patterns must vary with both row and column/tap.

The constructor and each runner allocate adapter-owned buffers through
`Budget::alloc` under `Limits::strict()`. Each runner allocates equal output and
scratch buffers before the row loop. Lane zero is completion (`1` on a complete
run); pixels start at lane one. `run_fixed` must call
`filter_v_fixed::<2/4/6/8>` directly and combine its boolean results, never call
`filter_v`.

### Task 2: Wire the checkasm kernel and RED tests

**Files:**
- Modify: `crates/tool/vaco-checkasm/Cargo.toml`
- Create: `crates/tool/vaco-checkasm/src/kernels/scale_filter_v.rs`
- Modify: `crates/tool/vaco-checkasm/src/kernels/mod.rs`
- Modify: `crates/tool/vaco-checkasm/src/main.rs`
- Modify: `crates/tool/vaco-checkasm/tests/bench_cli.rs`

- [ ] **Step 1: Enable only the dependency feature**

Use:

```toml
vaco-scale = { path = "../../signal/vaco-scale", features = ["checkasm"] }
```

- [ ] **Step 2: Implement the production adapter kernel**

Create `ScaleFilterVKernel` with:

```rust
impl Kernel for ScaleFilterVKernel {
    const NAME: &'static str = "vaco-scale::filter_v_generic_vs_fixed";
    type Case = FilterVCase;
    type Lane = i32;

    fn cases() -> Vec<Self::Case> {
        let widths = edge::element_widths(4);
        [2, 4, 6, 8]
            .into_iter()
            .flat_map(|taps| {
                edge::lengths_around(&widths).into_iter().flat_map(move |width| {
                    [1, 2, 3].into_iter().filter_map(move |rows| {
                        FilterVCase::synthetic(taps, width, rows)
                    })
                })
            })
            .collect()
    }

    fn benchmark_case() -> Option<Self::Case> {
        FilterVCase::synthetic(8, 1920, 1080)
    }

    fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
        run_generic(case)
    }

    fn vector(case: &Self::Case) -> Vec<Self::Lane> {
        run_fixed(case)
    }
}
```

- [ ] **Step 3: Add focused differential tests**

Add one test that runs `Differential::<ScaleFilterVKernel>::run()`, asserts a
non-empty corpus, asserts all tap counts `[2, 4, 6, 8]` occur, and calls
`assert_clean()`. Add one production-case test asserting exact generic/fixed
equality and length `1920 * 1080 + 1`.

- [ ] **Step 4: Register the module and CLI entry**

Export `pub mod scale_filter_v;`, import `ScaleFilterVKernel`, and add one
`Entry` with `verify_report::<ScaleFilterVKernel>` and
`bench_kernel::<ScaleFilterVKernel>`.

- [ ] **Step 5: Point the CLI integration test at this adapter**

Change its `--test` argument and stdout assertion to the exact new kernel name.
Keep the two legal pairs exhaustive:

```rust
backend=instant unit=ns
backend=perf-event unit=cycles
```

and require exactly two JSONL rows, each carrying one legal backend/unit pair.

### Task 3: Document the feature and measurement contract

**Files:**
- Modify: `docs/signal/vaco-scale.md`
- Modify: `docs/tool/vaco-checkasm.md`

- [ ] **Step 1: Document the scale adapter boundary**

In the performance/testing sections, explain that the `checkasm` feature is
default-off and doc-hidden, keeps `Grid` and both filter functions private, and
exists only so the tool can compare the shipped generic/fixed callees directly.

- [ ] **Step 2: Document checkasm semantics**

Add the new adapter to the wired-in examples. State that `scalar` means generic,
`vector` means fixed for this non-SIMD specialization, its benchmark is
1920×1080 at 8 taps, and its symmetric allocation remains
`adapter-inclusive`. Update configuration/dependencies to note that
`vaco-checkasm` enables the dependency feature while `vaco-scale` leaves it off
by default.

### Task 4: Verify without mislabelling measurements

**Files:**
- Test: `crates/tool/vaco-checkasm/src/kernels/scale_filter_v.rs`
- Test: `crates/tool/vaco-checkasm/tests/bench_cli.rs`

- [ ] **Step 1: Run focused RED/GREEN checks in the private target**

```sh
CARGO_INCREMENTAL=0 RUSTC_WRAPPER= CARGO_BUILD_JOBS=2 cargo test -j 2 \
  --target-dir /private/tmp/vaco-scale-filter-v-checkasm-target \
  -p vaco-checkasm scale_filter_v -- --nocapture
CARGO_INCREMENTAL=0 RUSTC_WRAPPER= CARGO_BUILD_JOBS=2 cargo test -j 2 \
  --target-dir /private/tmp/vaco-scale-filter-v-checkasm-target \
  -p vaco-checkasm --test bench_cli -- --nocapture
```

Expected: all selected unit and CLI tests pass; the CLI test accepts only the
two truthful measurement pairs.

- [ ] **Step 2: Run the complete affected suites and strict lint/format gates**

```sh
CARGO_INCREMENTAL=0 RUSTC_WRAPPER= CARGO_BUILD_JOBS=2 cargo test -j 2 \
  --target-dir /private/tmp/vaco-scale-filter-v-checkasm-target -p vaco-scale
CARGO_INCREMENTAL=0 RUSTC_WRAPPER= CARGO_BUILD_JOBS=2 cargo test -j 2 \
  --target-dir /private/tmp/vaco-scale-filter-v-checkasm-target -p vaco-checkasm
CARGO_INCREMENTAL=0 RUSTC_WRAPPER= CARGO_BUILD_JOBS=2 cargo clippy -j 2 \
  --target-dir /private/tmp/vaco-scale-filter-v-checkasm-target \
  -p vaco-scale -p vaco-checkasm --all-targets --all-features -- -D warnings
rustfmt --edition 2024 --check crates/signal/vaco-scale/src/exec.rs \
  crates/tool/vaco-checkasm/src/kernels/scale_filter_v.rs \
  crates/tool/vaco-checkasm/src/kernels/mod.rs \
  crates/tool/vaco-checkasm/src/main.rs \
  crates/tool/vaco-checkasm/tests/bench_cli.rs
```

Expected: all tests, clippy, and scoped format checks pass.

- [ ] **Step 3: Run the isolated benchmark smoke**

```sh
CARGO_INCREMENTAL=0 RUSTC_WRAPPER= CARGO_BUILD_JOBS=2 cargo run -j 2 \
  --target-dir /private/tmp/vaco-scale-filter-v-checkasm-target \
  -p vaco-checkasm --release -- bench \
  --test 'vaco-scale::filter_v_generic_vs_fixed' \
  --bench-cache hot --min-samples 30 --budget 250 \
  --json /private/tmp/vaco-scale-filter-v-checkasm.jsonl
```

Expected on permitted Linux x86_64/aarch64: two rows labelled
`backend=perf-event unit=cycles`. Expected on macOS, unsupported targets, or
Linux without a usable direct unmultiplexed PMU: a warning followed by two rows
labelled `backend=instant unit=ns`. Any cycles label on a time result fails the
task.

- [ ] **Step 4: Run repository documentation and provenance gates**

```sh
CARGO_INCREMENTAL=0 RUSTC_WRAPPER= CARGO_BUILD_JOBS=2 cargo run -j 2 \
  --target-dir /private/tmp/vaco-scale-filter-v-checkasm-target \
  -p xtask -- comment-check
CARGO_INCREMENTAL=0 RUSTC_WRAPPER= CARGO_BUILD_JOBS=2 cargo run -j 2 \
  --target-dir /private/tmp/vaco-scale-filter-v-checkasm-target \
  -p xtask -- provenance-check
CARGO_INCREMENTAL=0 RUSTC_WRAPPER= CARGO_BUILD_JOBS=2 cargo run -j 2 \
  --target-dir /private/tmp/vaco-scale-filter-v-checkasm-target \
  -p xtask -- gen-docs-index --check
```

Expected: all three gates pass. Commit only owned paths through the repository's
private-index compare-and-swap recipe, with the required scale clean-room
trailers, after all fresh outputs are green.
