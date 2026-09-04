# vaco-vecheck

`vaco-vecheck` is the vectorization-contract checker. It turns a checked SIMD
kernel's LLVM loop-vectorizer remark and `cargo-show-asm` output into an
actionable assertion: a required loop must have vectorized, and each ISA-tier
assembly body must contain and omit the instructions declared for it.

## How it works

The root [`vecheck.toml`](../../vecheck.toml) is the only declaration of a
kernel id, variant, package, and compiler symbol. Mark its free-function source
with `#[vaco_simd::vaco::must_vectorize]` and `#[inline(always)]`. The attribute
accepts no id or symbol strings, so it cannot become a second registry; it only
rejects the wrong item shape or a kernel whose target-feature context might not
inline into its loop body.

On a nightly developer lane, compile the selected package with LLVM's
loop-vectorizer remarks enabled and pass the resulting YAML to the checker:

```sh
RUSTFLAGS='-Cremark=loop-vectorize -Zremark-dir=/tmp/vaco-remarks' \
  cargo +nightly build -p vaco-simd
cargo run -p vaco-vecheck -- remarks \
  --config vecheck.toml --remarks /tmp/vaco-remarks/vaco_simd.yaml
```

`remarks` fails for either a missing `Passed` record or a matching `Missed`
record. The latter includes LLVM's original reason text. The remark lane is a
nightly diagnostic; the merge-gating check is the assembly assertion, because
that is available to the pinned stable toolchain.

For each `(kernel, tier)`, collect disassembly with `cargo-show-asm` and check
the exact emitted tier monomorphization:

```sh
cargo asm --package vaco-simd --simplify \
  --target x86_64-unknown-linux-gnu --target-cpu x86-64-v2 \
  'vectorize_avx2::<vaco_simd::example::yuv420p_to_rgb24_row_dispatched::{closure#1}' \
  > /tmp/yuv.s
cargo run -p vaco-vecheck -- asm --config vecheck.toml \
  --target x86_64-v3 --kernel scale/yuv2rgb/yuv420p_to_rgb24 --asm /tmp/yuv.s
```

Omit `--asm` to let the tool invoke `cargo asm` for the configured package and
symbol. It always gives that child a nested target directory, avoiding a Cargo
lock deadlock when `vaco-vecheck` itself was launched with `cargo run`. Set
`VACO_VECHECK_ASM_TARGET_DIR=/private/target` to reuse a warmed private build,
or `VACO_CARGO_ASM=/absolute/path/to/cargo-asm` when the executable is installed
outside `PATH`. Supplying an assembly file is useful for explicit per-tier
builds and for CI artifacts.
Patterns are literal alternatives separated by `|`; an
optional leading or trailing `\\b` requests an ASCII word boundary. This small
grammar covers instruction names without adding a regex dependency to the
developer toolchain. `forbid` is matched against the complete emitted function.
`max_insns` counts non-label, non-directive instructions only in the unique
backward-edge loop that contains every `require` pattern. No matching loop, or
more than one, is an error: the checker never picks a loop arbitrarily.

The worked AVX2 kernel measures 108 hot-loop instructions with
nightly-2026-08-07 and `cargo-show-asm` 0.2.62. Its ceiling is 112: four
instructions of compiler-drift room. The earlier 96-instruction estimate was
rejected after measuring the correct AVX2 monomorphization. Keeping arithmetic
at native 256-bit width while narrowing and interleaving fixed 16-pixel blocks
is the smallest byte-exact lowering the safe portable substrate produced; a
native-width narrowing experiment regressed to 126 instructions.

`vaco-vecheck validate --config vecheck.toml` rejects expired waivers and a
sum of live `cost_pct` values above `max_live_waiver_cost_pct`. A waiver must
name its kernel and variant, say why it exists, link the upstream issue, specify
an expiry date, and record its measured cost. It is an explicit temporary
exception, not a way to disable a contract indefinitely.

## How to change it

When adding a vector kernel, first apply `#[vaco_simd::vaco::must_vectorize]`
to its `#[inline(always)]` free-function body. Then add exactly one `[[kernel]]`
entry with its id, variant, package, symbol and ISA-tier expectations. Verify
the emitted assembly on each declared tier before choosing requirements and
ceilings; do not copy another architecture's mnemonics. If LLVM emits a
`Missed` remark, preserve the reason in the failure report and restructure the
loop or move to explicit portable SIMD rather than waiving it by default.

Keep instruction requirements narrow enough to describe the operation and
forbidden patterns broad enough to catch outlined calls, panic traps, gathers,
or architecture-specific bad paths. A threshold should cover the extracted
kernel body, not a caller whose inlining decisions can vary. Expire any waiver
on the next planned toolchain review and include the current measured cost.

## Configuration

`vecheck.toml` supports these root and table fields:

```toml
max_live_waiver_cost_pct = 3.0

[[kernel]]
id = "area/kernel"
variant = "x16"
symbol = "crate::module::function"
asm_symbol = "optional::emitted::selector"
package = "cargo-package"
cargo_target = "optional-rust-target-triple"
cargo_target_cpu = "optional-target-cpu"
cargo_artifact = "--lib"

[kernel.expect.x86_64-v3]
require = ["instruction|alternative"]
forbid = ["\\bcall\\b"]
max_insns = 96

[[waiver]]
kernel = "area/kernel"
variant = "x16"
reason = "measured toolchain regression"
upstream = "https://github.com/rust-lang/rust/issues/12345"
expires = "2026-12-01"
cost_pct = 1.5
```

`--today YYYY-MM-DD` makes waiver checking deterministic in tests and CI. The
`cargo_target` and `cargo_target_cpu` select the Rust target and ambient CPU for
automatic `cargo asm` collection. The ambient CPU can deliberately be below the
asserted tier: the worked x86 contract builds at the shipped x86-64-v2 floor so
the runtime AVX2 closure remains a distinct emitted item. Building ambiently at
x86-64-v3 inlines that closure and can leave only the AVX-512 closure selectable.
`cargo_artifact` disambiguates workspaces that define multiple artifacts. These
fields do not replace the `target` tier key used to select an assertion.
`asm_symbol` is required when `#[inline(always)]` means the source `symbol` has
no standalone emitted item; the checker requires the first emitted function
label to contain this selector and fails closed if `cargo-show-asm` resolves a
different ISA closure.

## Dependencies

The checker itself uses only the Rust standard library. The attribute macro uses
the workspace's existing `syn`, `quote`, and `proc-macro2` dependencies.
Assembly collection requires the externally installed `cargo-show-asm` Cargo
subcommand. LLVM optimization-remark directory output requires nightly Rust;
the parser remains useful for archived nightly artifacts and does not make the
stable assembly gate depend on nightly.
