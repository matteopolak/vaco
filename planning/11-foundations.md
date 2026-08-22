# 11 — Layer 0 & Layer 1: Foundations and Media Data Model

Implementation plan for `vaco-core`, `vaco-simd`, `vaco-opts`, `vaco-expr`, `vaco-bitstream`,
`vaco-pixfmt`, `vaco-sampfmt`, `vaco-chlayout`, `vaco-color`, `vaco-frame`, `vaco-packet`, `vaco-pool`,
plus the workspace configuration every later crate inherits.

Binding inputs: `planning/00-decisions.md` (D1–D10), `planning/10-architecture.md`,
`planning/research/01-libavutil-swr-sws.md` (the feature inventory), `planning/research/08-performance-simd.md`,
`planning/research/09-dependency-licence-register.md`.

**Clean-room note.** Nothing here is derived from reading FFmpeg source. Names, option spellings, enum
value assignments that are spec code points, and CLI-visible behaviour are interface facts (D7) and are
matched deliberately. Every structural decision below is made from the published specifications
(ITU-T H.273, ISO/IEC 23091, ITU-T H.264/H.265 syntax layers, IETF RFCs) and from the inventory documents.

---

## 0. How to read this

Each crate section is: **Purpose · Public API · Internal design · Dependencies · Tests · Effort/Blocks**.

Fork-in-the-road decisions are numbered **F1…F14** and are referenced from the crate sections. They are
decisions, not options: each states the alternatives, the choice, and two sentences of justification.

Everything in Layer 0/1 carries `#![forbid(unsafe_code)]` in the crate root. None of these crates is on the
D2 allowlist, and none may ever be added to it.

---

## 1. Workspace configuration

### 1.1 `Cargo.toml` (workspace root)

```toml
# NOTE (D12): the `cargo-features = ["codegen-backend"]` line that used to open this file
# has been REMOVED. It made the whole manifest nightly-only, and after D12 dropped
# `std::simd` there is no mandatory nightly feature left. Cranelift dev builds are still
# available, but they are now opted into per-invocation via an environment variable rather
# than baked into the committed manifest. See §1.2.

[workspace]
resolver = "3"
members  = ["crates/*/*", "xtask"]
exclude  = ["fuzz"]                        # fuzz/ is its own workspace — see F1

[workspace.package]
version      = "0.1.0"
edition      = "2024"
license      = "MIT OR Apache-2.0"
repository   = "https://github.com/vaco-media/vaco"
authors      = ["The Vaco Authors"]
rust-version = "1.94"                      # informational only; rust-toolchain.toml is authoritative

[workspace.dependencies]
# --- internal, layer 0 ---
vaco-core       = { path = "crates/core/vaco-core",       version = "0.1.0" }
vaco-simd       = { path = "crates/core/vaco-simd",       version = "0.1.0" }
vaco-opts       = { path = "crates/core/vaco-opts",       version = "0.1.0" }
vaco-opts-derive= { path = "crates/core/vaco-opts-derive",version = "0.1.0" }
vaco-expr       = { path = "crates/core/vaco-expr",       version = "0.1.0" }
vaco-bitstream  = { path = "crates/core/vaco-bitstream",  version = "0.1.0" }
# --- internal, layer 1 ---
vaco-pixfmt     = { path = "crates/core/vaco-pixfmt",     version = "0.1.0" }
vaco-sampfmt    = { path = "crates/core/vaco-sampfmt",    version = "0.1.0" }
vaco-chlayout   = { path = "crates/core/vaco-chlayout",   version = "0.1.0" }
vaco-color      = { path = "crates/core/vaco-color",      version = "0.1.0" }
vaco-frame      = { path = "crates/core/vaco-frame",      version = "0.1.0" }
vaco-packet     = { path = "crates/core/vaco-packet",     version = "0.1.0" }
vaco-pool       = { path = "crates/core/vaco-pool",       version = "0.1.0" }

# --- external: the entire layer-0/1 external surface. Each assessed against D10's three
#     gates in §3, and recorded in docs/dependencies.md before it may be used. ---
#
# `fearless_simd` (D12) is the SIMD substrate. Per D11 it is named here and in
# `crates/core/vaco-simd/Cargo.toml` and NOWHERE ELSE; `xtask dep-gate` (§1.8) asserts
# exactly one occurrence under `crates/`. Zero required dependencies of its own.
fearless_simd = { version = "0.7", default-features = false, features = ["std"] }
thiserror    = "2"
tracing      = { version = "0.1", default-features = false, features = ["std", "attributes"] }
proc-macro2  = "1"
quote        = "1"
syn          = { version = "2", default-features = false, features = ["parsing", "printing", "proc-macro", "derive", "full", "extra-traits", "clone-impls"] }
# dev-only
proptest     = { version = "1", default-features = false, features = ["std"] }
arbitrary    = { version = "1", features = ["derive"] }
divan        = "0.1"
insta        = "1"

[workspace.lints.rust]
unsafe_code                    = "forbid"
missing_docs                   = "warn"
missing_debug_implementations  = "warn"
unreachable_pub                = "warn"
unused_qualifications          = "warn"
elided_lifetimes_in_paths      = "warn"
trivial_numeric_casts          = "warn"
let_underscore_drop            = "warn"
non_ascii_idents               = "deny"
unnameable_types               = "warn"
unexpected_cfgs                = "deny"

[workspace.lints.clippy]
# Base tiers. `priority = -1` so the individual entries below override them.
all      = { level = "warn", priority = -1 }
pedantic = { level = "warn", priority = -1 }
# --- promoted to deny: these are correctness/robustness rules for a media library ---
unwrap_used                     = "deny"   # library code must not panic on hostile input (D6)
expect_used                     = "deny"
panic                           = "deny"
todo                            = "deny"
unimplemented                   = "deny"
unreachable                     = "deny"
indexing_slicing                = "deny"   # crate-level allow in DSP crates; see F2
integer_division_remainder_used = "warn"
float_cmp                       = "deny"
mem_forget                      = "deny"
exit                            = "deny"
dbg_macro                       = "deny"
print_stdout                    = "deny"   # allowed only in cli/ crates
print_stderr                    = "deny"
allow_attributes                = "deny"   # use #[expect(...)], which rots loudly
allow_attributes_without_reason = "deny"
undocumented_unsafe_blocks      = "deny"   # vacuous here, cheap insurance
large_stack_arrays              = "warn"
large_types_passed_by_value     = "warn"
missing_const_for_fn            = "warn"
# --- demoted: pedantic lints that fight DSP and format-table code ---
cast_possible_truncation = "allow"   # see F2 — replaced by an explicit cast vocabulary
cast_sign_loss           = "allow"
cast_precision_loss      = "allow"
cast_lossless            = "allow"
module_name_repetitions  = "allow"
similar_names            = "allow"
too_many_lines           = "allow"
many_single_char_names   = "allow"   # `y, u, v, a` is the domain vocabulary
inline_always            = "allow"   # bit readers and kernels genuinely need it
struct_excessive_bools   = "allow"
doc_markdown             = "allow"

# ---------------- profiles (D8) ----------------
[profile.dev]
# `codegen-backend = "cranelift"` was here. It is a nightly-only manifest key and is now set
# per-invocation instead (`just dev-fast`, §1.2). Committing it would make `cargo build`
# fail on stable for every contributor.
opt-level       = 1
debug           = "line-tables-only"
overflow-checks = true
incremental     = true

[profile.dev.package."*"]        # dependencies + proc macros are optimised
opt-level       = 2

[profile.test]
inherits        = "dev"
opt-level       = 2              # conformance tests decode real media; -O1 is too slow
overflow-checks = true

[profile.release]
opt-level       = 3
lto             = "fat"
codegen-units   = 1
panic           = "unwind"       # so `cargo test --release` works; see `dist`
debug           = "line-tables-only"
overflow-checks = false
strip           = "none"

[profile.dist]                   # what we actually ship
inherits = "release"
panic    = "abort"
strip    = "symbols"

[profile.bench]
inherits = "release"
debug    = "full"

[profile.fuzz]
inherits        = "release"
debug-assertions = true
overflow-checks  = true          # D6: overflow is a fuzz finding, not a shrug
lto              = "thin"
```

Every member crate carries:

```toml
[lints]
workspace = true
```

…and every member crate's `lib.rs` opens with `#![forbid(unsafe_code)]`. Both, deliberately: the workspace
lint catches new crates that forget the attribute, the attribute survives someone editing the workspace
table. `xtask lint-attrs` greps for the attribute in every `crates/*/*/src/lib.rs` and fails CI on absence,
consulting a one-line allowlist file that only the D2 crates may appear in.

### 1.2 `rust-toolchain.toml` — **stable, per D12**

> **Superseded (2026-08-21, D12).** This section previously pinned `nightly-2026-08-06`, because
> `std::simd` (`portable_simd`) is a nightly feature and every DSP kernel depended on it. D12 replaces
> `std::simd` with `fearless_simd`, which builds on **stable 1.89+**. That removes the only *mandatory*
> reason for nightly. The old conclusion and its reasoning are kept at the end of this section.

```toml
[toolchain]
channel    = "1.89"                        # MSRV == pinned channel; see "Why pin a stable" below
components = ["rustfmt", "clippy", "rust-src", "rust-analyzer", "llvm-tools"]
targets    = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu",
              "aarch64-apple-darwin",     "x86_64-apple-darwin",
              "x86_64-pc-windows-msvc"]
profile    = "minimal"
```

**Everything that ships is built by this toolchain.** `cargo build`, `cargo test`, `cargo bench`, the
`dist` profile, the PGO pipeline and the release artefacts all use stable. No `-Z` flag appears anywhere
in the release path.

#### What was nightly, and where each item went

| Nightly item | Was needed for | Status after D12 |
|---|---|---|
| `portable_simd` (`std::simd`) | every DSP kernel | **Gone.** Replaced by `fearless_simd` (stable, MSRV 1.89). |
| `f16` | `vaco-pixfmt`/`vaco-color` half-float formats | **Not taken.** Still unstable at 1.89, and `fearless_simd` has no f16 lane type either, so there is nothing to lose by not having it. We take the `vaco_core::F16` software newtype unconditionally. The escape hatch described below survives: nothing outside `vaco-core` may name `f16` directly, so adopting the primitive later is a one-type-alias change. |
| `cargo-features = ["codegen-backend"]` + `profile.*.codegen-backend` | Cranelift dev builds (D8) | **Moved out of the manifest.** See below. |
| `-Zremark-dir` | plan 12 §3.3(a), the optimisation-remark lane of `vaco-vecheck` | **Optional.** `-Cremark=` is stable, `-Zremark-dir` is not. The remark lane becomes a nightly-only developer aid and one non-gating CI job. The *gating* vectorization check is plan 12 §3.3(c), `cargo-show-asm` instruction assertions, which is stable and was already described there as the ground truth. |
| `miri` | UB detection | **Non-gating nightly job.** We `#![forbid(unsafe_code)]`, so Miri only ever has dependency internals to find. Keep it, do not gate on it. |

#### Cranelift without a nightly manifest

Cranelift genuinely does still require nightly — it is `rustc-codegen-cranelift-preview`, a rustup
component that only exists on the nightly channel. But it is a **local iteration convenience**, not a
correctness or performance requirement, so it must not be allowed to drag the whole project back onto
nightly. Two mechanical facts make the separation clean:

1. Cargo reads `CARGO_PROFILE_DEV_CODEGEN_BACKEND` as an environment variable, so the profile key never
   has to be written into a committed `Cargo.toml`.
2. `RUSTUP_TOOLCHAIN` overrides `rust-toolchain.toml` per-invocation.

So the entire nightly surface reduces to one `just` recipe:

```make
# justfile
nightly := "nightly-2026-08-06"

# Default: stable. What CI runs, what releases are built with, what a new contributor gets.
build:      cargo build --workspace --locked
test:       cargo test  --workspace --locked

# Opt-in fast local iteration. Requires `just setup-nightly` once.
dev-fast:
    RUSTUP_TOOLCHAIN={{nightly}} \
    CARGO_PROFILE_DEV_CODEGEN_BACKEND=cranelift \
    RUSTFLAGS="-Zcodegen-backend=cranelift" \
    cargo build --workspace

setup-nightly:
    rustup toolchain install {{nightly}} \
      --component rustc-codegen-cranelift-preview,miri,rust-src
```

A contributor who never runs `just dev-fast` never installs nightly and never notices it exists.

#### Why pin a *stable* channel at all

The pin is no longer about feature availability — it is entirely a **performance-stability mechanism**
(plan 12 §3.5). LLVM version changes move vectorization and scheduling decisions, and our kernels have
instruction-level assertions attached to them. So:

- `channel` names an exact stable release, not `"stable"`. Floating would let a CI runner silently change
  codegen under a benchmark.
- **Re-pin cadence changes from quarterly to per-stable-release (6 weeks).** Stable releases are far less
  likely to break us than nightly was, and staying close to the head keeps the diff per bump small.
- The bump PR still carries the full obligation from plan 12 §3.5: a complete `vecheck` run and a full
  benchmark comparison against the previous pin on the reference machine. >1% regression on any of the
  nine scenarios is reverted or lands with a quantified justification.
- `channel` and the workspace `rust-version` (MSRV) are kept equal. We are not in the business of
  supporting a range of compilers; we are in the business of knowing exactly which one produced a number.

#### What this actually buys us

- **Packaging.** Debian, Fedora, Arch and Homebrew will not ship a media library that requires a nightly
  compiler. Under the old pin, distro packaging was effectively foreclosed. It is now routine.
- **Reproducibility.** `rustup toolchain install 1.89 && cargo build --locked` reproduces a release from
  a version of rustc that will still exist in five years. Dated nightlies are garbage-collected.
- **CI.** The matrix becomes: **stable-pinned (gating, all targets)**, **beta (non-gating, early warning
  of a codegen change before it reaches stable)**, and **nightly (non-gating: Miri + the remark lane)**.
  Previously every job needed the same dated nightly, and a rustup outage for that date broke everything.
- **Onboarding.** `rustup default stable && just test`. No toolchain file surprises, no
  `error[E0554]: #![feature] may not be used on the stable release channel` as a first impression.
- **Dependency headroom.** Crates that pin `rust-version` conservatively no longer conflict with a
  nightly that reports an unrelated version.

#### Superseded reasoning, kept for the trail

> The original text read: *"Pinned nightly (`rust-toolchain.toml`), required for `std::simd`
> (`portable_simd`)"* (D8), and this section re-pinned quarterly while re-verifying `portable_simd`,
> `f16` and the `codegen-backend` profile key. That was correct given `std::simd`. It was also a
> standing liability — it foreclosed distro packaging, it made every CI job depend on one dated
> nightly artefact remaining downloadable, and it meant a `portable_simd` API change upstream could
> break every kernel we own. D12 removes the cause, so it removes the liability.

### 1.3 `clippy.toml`

```toml
avoid-breaking-exported-api = false          # pre-1.0, we break freely
too-many-arguments-threshold = 10            # DSP kernels legitimately take many slices
type-complexity-threshold    = 350
enum-variant-size-threshold  = 512           # SideData carries fat typed payloads by design

disallowed-methods = [
  { path = "std::time::SystemTime::now", reason = "libraries must be deterministic; take a clock as a parameter" },
  { path = "std::env::var",              reason = "configuration arrives via vaco-opts, not the environment" },
  { path = "std::process::exit",         reason = "return an error; only vaco/vaco-probe/vaco-play may exit" },
  { path = "std::collections::HashMap::new", reason = "use vaco_core::Dict for option/metadata maps (ordered, multikey)" },
]

disallowed-types = [
  { path = "std::sync::RwLock", reason = "use Mutex or Arc-CoW; RwLock write starvation has bitten media pipelines" },
]
```

`disallowed-methods` entries are relaxed per-crate with a documented `#[expect]` where genuinely needed
(the CLI crates lift `env::var` and `process::exit`).

### 1.4 `rustfmt.toml`

```toml
edition = "2024"
max_width = 100
use_field_init_shorthand = true
use_try_shorthand = true
imports_granularity = "Module"      # nightly-only rustfmt option: applied by `just fmt` under
                                    # the opt-in nightly (§1.2); a no-op warning on stable, never gating
group_imports = "StdExternalCrate"
newline_style = "Unix"
format_code_in_doc_comments = true
wrap_comments = true
comment_width = 100
```

### 1.5 `deny.toml` additions for this layer

Take §10 of the licence register verbatim (that is Gate 2), and add the bans that enforce
Gate 1 and the architecture:

```toml
[bans]
multiple-versions = "warn"
deny = [
  # architecture §10: link-section registration requires unsafe and link tricks
  { name = "inventory" },
  { name = "linkme"    },
  { name = "ctor"      },
  { name = "lazy_static", use-instead = "std::sync::OnceLock" },
  { name = "once_cell",   use-instead = "std::sync::OnceLock" },
  # D10 Gate 1: anything that exists to link a foreign library
  { name = "cc"        },   # a build-script C compiler in the tree IS the violation
  { name = "cmake"     },
  { name = "pkg-config"},
  { name = "bindgen"   },
  { name = "libloading"},
]

[sources]
unknown-registry = "deny"
unknown-git      = "deny"
allow-registry   = ["https://github.com/rust-lang/crates.io-index"]
```

`cc`/`cmake`/`pkg-config`/`bindgen` are denied by name because they are the *mechanism* of Gate 1
violations: no `-sys` crate can exist in the tree without at least one of them. This is a structural
check that does not depend on trusting crate names or declared metadata — which D9 records as
demonstrably unreliable. `xtask dep-gate` additionally walks every resolved package's manifest and
fails on any `links = "…"` key or any `build.rs` in a non-workspace package, catching the vendored-C
case that ships its own compiler invocation.

### 1.6 `.cargo/config.toml`

```toml
# Revised per D12. These now set the *floor*, not the ceiling: `fearless_simd` dispatches
# above the compiled baseline at runtime, so there is no reason to raise the baseline and
# every reason to keep it low and portable.
[target.'cfg(target_arch = "x86_64")']
rustflags = ["-C", "target-cpu=x86-64-v2"]     # SSE4.2 floor. See F5' in §5.3.
[target.'cfg(target_arch = "aarch64")']
rustflags = ["-C", "target-cpu=generic"]       # ARMv8.0-A + baseline NEON

[alias]
xtask = "run --package xtask --"
```

**Why v2 and not v3 (changed by D12).** This file used to say `x86-64-v3`, because under F5 the compiled
baseline *was* the ceiling — anything not in the baseline was unreachable. With runtime dispatch the
relationship inverts: raising the baseline only shrinks the set of machines the binary runs on, while
AVX2 and AVX-512 are selected at runtime regardless. Setting the floor at **v2** also has a concrete
codegen benefit: `fearless_simd`'s `dispatch!` prunes any level at or below the ambient target baseline
into a single terminal backend, so a v2 floor collapses the `Sse2` arm and leaves three monomorphisations
(SSE4.2 / AVX2 / AVX-512) instead of four — a direct saving against the binary-size cost recorded in §5.3.

CI's `dist` job still overrides `target-cpu` explicitly per published artefact; it never inherits this
file. There is now exactly **one** x86 artefact and one aarch64 artefact, not the five that F5 required.

### 1.7 `xtask` (new, tiny, member of the workspace)

`xtask` is a normal binary crate — no dependencies beyond `std` — invoked through the `Justfile`. Layer-0/1
responsibilities:

| Subcommand | What it does |
|---|---|
| `xtask layer-check` | Reads `layers.toml` (crate → layer number) and every crate's `Cargo.toml`; fails if any crate depends on a crate of equal-or-higher layer. Architecture §1.1. |
| `xtask lint-attrs` | Asserts `#![forbid(unsafe_code)]` in every `src/lib.rs` not on the D2 allowlist. |
| `xtask gen-pixfmt` | Regenerates `crates/core/vaco-pixfmt/src/table.rs`. `--check` mode diffs and fails. §9. |
| `xtask docs-check` | Asserts a `docs/<crate>.md` exists for every `crates/*/*`. Architecture §9. |
| `xtask dep-gate` | D10 Gate 1 and the adoption record: fails on any resolved package with a `links` key or a `build.rs`, and on any direct dependency absent from `docs/dependencies.md`. |

### 1.8 Dependency governance (D10)

D10 makes adding a dependency a reviewed decision. The machinery that enforces it:

| Tool | Gate | What it checks | Where it runs |
|---|---|---|---|
| `cargo-deny` | 2 (licence), plus bans | Licence allow-list, the `[bans]` list above, `[sources]` registry pinning, duplicate versions | Every PR; blocking |
| `xtask dep-gate` | 1 (pure Rust) | No `links` key, no third-party `build.rs`, every direct dep present in `docs/dependencies.md` | Every PR; blocking |
| `cargo-audit` | 3 (sound) | RUSTSEC advisories against `Cargo.lock` | Every PR **and** nightly on `main` (advisories land after merge) |
| `cargo-geiger` | 3 (unsafe-light) | `unsafe` expression counts per crate, ours and transitive | Weekly on `main`; report committed to `docs/dependencies.md` as a dated appendix |
| `cargo-about` | D3 | Generates `THIRD_PARTY.md` | Release builds |
| `cargo tree --duplicates` / `--depth` | 3 (shallow) | Tree size and duplication; reviewed at adoption, not gated | Adoption review |

`docs/dependencies.md` is the adoption record required by D10. One entry per **direct** dependency,
appended never rewritten:

```markdown
### `tracing` 0.1
- **Adopted:** 2026-08-25 · **Signed off:** <reviewer>
- **Job:** structured logging façade for the whole workspace (architecture §3, layer 0).
- **Gate 1 — pure Rust:** clears. No `build.rs`, no `links`, no FFI in the tree.
- **Gate 2 — licence:** MIT. On the D3 allow-list.
- **Gate 3 — trusted:**
  - Alive: releases within the last 12 months; tokio-rs org, active triage.
  - Adopted: top-20 crates.io crate by downloads; thousands of reverse deps.
  - Sound: `cargo-audit` clean as of the adoption date.
  - Shallow: N transitive crates with `default-features = false` (record the actual count).
  - Unsafe-light: `cargo-geiger` count recorded here; concentrated in the span registry.
  - Vendorable: yes — a fork is tractable; the API surface we use is small and stable.
- **Build-vs-buy:** buy. Logging is orthogonal infrastructure; nothing media-specific is lost.
- **Blast radius if abandoned:** low. Our own `vaco_core::log` façade is the only caller surface,
  so a replacement is a one-crate change.
```

`Justfile` targets (D8 makes `just` the only interface):

```make
licence-check:   cargo deny check licenses bans sources
dep-gate:        cargo xtask dep-gate
audit:           cargo audit --deny warnings
geiger:          cargo geiger --output-format GitHubMarkdown --all-features > target/geiger.md
dep-report:      just licence-check && just dep-gate && just audit && just geiger
licence-report:  cargo about generate about.hbs -o THIRD_PARTY.md
ci:              just fmt-check lint test layer-check lint-attrs docs-check dep-report
```

---

## 2. Cross-cutting decisions

**F1 — `fuzz/` is a separate Cargo workspace.**
Options: (a) fuzz targets as workspace members; (b) a nested excluded workspace. `libfuzzer-sys` requires
`unsafe` in the generated harness, and `#![forbid(unsafe_code)]` applies to macro-expanded code, so fuzz
targets cannot live under our workspace lints. **Chosen: (b).** `fuzz/` has its own `Cargo.toml` workspace
with `unsafe_code = "allow"`, path-depends on our crates, and is excluded from the root workspace; this
keeps the forbid absolute for everything we ship while letting `cargo-fuzz` work unmodified.

**F2 — Integer-cast discipline instead of pedantic cast lints.**
Options: (a) leave `clippy::cast_possible_truncation` on and paper the codebase in `#[expect]`;
(b) turn it off globally and provide an explicit cast vocabulary. Casts are the substance of format
conversion code, so (a) produces thousands of suppressions that stop being read. **Chosen: (b):**
`vaco_core::num` exports `sat<T>(x)`, `narrow<T>(x) -> Option<T>`, `wrap<T>(x)`, and `q(x) -> f64`, and the
rule (enforced by review plus a `clippy::cast_possible_truncation = "warn"` override in the *parsing* crates
— `vaco-bitstream`, all demuxers, all header parsers) is that any value derived from untrusted input crosses
width boundaries through `narrow`/`sat`, never through `as`.

**F3 — `indexing_slicing` is denied by default, allowed per-crate for kernels.**
Slice indexing panics; a panic on hostile input is a D6 bug. **Chosen:** deny at the workspace level; the
DSP-shaped crates (`vaco-simd`, and later `vaco-scale`/`vaco-resample`/`vaco-codec-dsp-*`) set
`indexing_slicing = "allow"` in their own `[lints]` because their indices are loop-derived and
locally proven, and they compensate with exhaustive proptests over dimensions. Parsing crates never get the
allow.

**F4 — No sentinel values. `Option` everywhere.**
FFmpeg's `AV_NOPTS_VALUE`, `AV_PIX_FMT_NONE`, `-1` channel counts and `0` timebases are C artefacts.
Timestamps are `Option<i64>`, formats are `Option<PixelFormat>`, unset options are `Option<T>`.
Cost: one match per access. Benefit: the entire class of "forgot to check for NOPTS" bugs is unreachable,
and it is *the* recurring bug class in timestamp handling.

**F5′ — SIMD level is selected at *runtime*, via capability tokens.** See §5.3. Per D12 this
**supersedes the original F5** ("SIMD width is selected at build time"), whose reasoning is preserved
in §5.3. It remains the single most consequential consequence of D2 in this layer: what changed is that
`fearless_simd` lets us have runtime ISA dispatch *and* `#![forbid(unsafe_code)]`, which F5 concluded
was impossible.

**F6 — Layer-0 crates never name a layer-1 type.**
`vaco-opts` must parse `pixel_fmt`, `sample_fmt` and `chlayout` options, but is layer 0 while
`vaco-pixfmt` is layer 1. Options: (a) move `vaco-opts` above layer 1; (b) invert the dependency with a
trait. **Chosen: (b).** `vaco-opts` defines `trait OptValue` and the `OptKind` tag enum (which *names* the
21 kinds, since a tag is not a type dependency); `vaco-pixfmt` provides `impl OptValue for PixelFormat`.
Layering stays acyclic, `vaco-opts` stays testable in isolation, and adding a new option-carrying type
never touches `vaco-opts`.

**F7 — Errors use `thiserror`.**
Options: (a) hand-write `Display`/`Error` for ~35 variants because the message text is a CLI-visible
compatibility surface; (b) derive them. `thiserror`'s `#[error("…")]` gives exactly the character-level
control (a) was reaching for, it clears all three D10 gates and contributes nothing at runtime, and D10
says prefer the crate that clears the gates. **Chosen: (b)**, with `#[error(transparent)]` for `Io` and a
hand-written `Error::code()` for the stable short codes the differential harness keys on.

**F8 — Alignment is achieved by over-allocating a `Vec<u8>` and sub-slicing, not by a custom allocator.**
Options: (a) `bytemuck`/`Vec<Aligned64>` reinterpretation; (b) `alloc` with a custom `Layout` (needs
unsafe); (c) allocate `len + 63` bytes and take the first 64-aligned subslice. (b) is forbidden, (a) needs a
dependency whose safety relies on internal unsafe. **Chosen: (c)** — `ptr::addr()` is a safe operation, the
`Vec` is never reallocated after construction so the alignment is stable, and it costs 63 bytes per
allocation which the pool amortises to nothing. Full code in §15.

**F9 — Guaranteed zero padding is recovered *safely* by making the padding part of the allocation.**
FFmpeg over-reads into 64 bytes of promised-zero slop. We allocate the slop *inside* the buffer and hand
the reader a slice that includes it, with a separate `logical_len`. Reading the padding is then an ordinary
in-bounds read. This recovers essentially all of the C fast path with zero unsafe; see §8.3.

**F10 — Every layer-1 metadata table is generated from a declarative source by `xtask`, and the generated
file is committed.** Options: (a) `build.rs`; (b) committed generated code checked by CI. **Chosen: (b)**,
consistent with architecture §6's registry decision: the table is greppable, rustdoc-visible, diffable in
review, and does not slow every build. `xtask gen-pixfmt --check` in CI makes drift impossible.

**F11 — Frames allocate one buffer per plane, not one buffer per frame.** Argued in §13.2.

**F12 — Expressions compile to a flat stack machine, not an AST.** Argued in §7.2.

**F13 — Bit readers use a sticky overrun flag, not `Result` per read.** Argued in §8.2.

**F14 — Enum discriminants are ours; names are the compatibility surface.**
D1 forbids ABI compatibility, so `PixelFormat`, `SampleFormat` and `FrameSideDataKind` get dense
discriminants assigned by our generator (which is what makes `DESCRIPTORS[fmt as usize]` a single load).
Where a discriminant *is* a specification code point — `ColorPrimaries`, `ColorTransfer`, `ColorSpace`,
`ChromaLocation`, all per ITU-T H.273 — we use the spec value, because it is spec-dictated data and gets
serialised into bitstreams.

---

## 3. External dependency assessment for Layer 0/1

Assessed against D10's three gates. **Gate 1** = pure Rust, zero FFI. **Gate 2** = licence per D3.
**Gate 3** = trusted and maintained (alive, adopted, sound, shallow, unsafe-light, vendorable).
Every "adopt" row needs a `docs/dependencies.md` entry with a dated sign-off before first use.

### 3.1 Adopt

| Crate | Job | G1 | G2 | G3 | Notes / what must be recorded at adoption |
|---|---|---|---|---|---|
| `fearless_simd` 0.7 | The SIMD substrate (D12). Reachable from `vaco-simd` only, per D11 | yes | Apache-2.0 OR MIT | yes | **Zero required dependencies** — the shallowest tree in the workspace. Contains internal `unsafe` by design; that is the entire point of the crate and the alternative is writing it ourselves (§5.3). Record the `cargo-geiger` count. MSRV 1.89, edition 2024; v1.0 due early Sept 2026, so expect a version bump shortly after adoption. `xtask dep-gate` asserts it appears in exactly one `Cargo.toml` under `crates/`. Full assessment in §5.7. |
| `thiserror` 2 | Error boilerplate in `vaco-core` and every crate with its own error enum | yes | MIT OR Apache-2.0 | yes | Proc-macro only; nothing at runtime. Trivially vendorable — worst case we hand-write the impls, which is F7's rejected alternative. |
| `tracing` 0.1 | The logging façade behind `vaco_core::log` (architecture §3) | yes | MIT | yes | Pin `default-features = false, features = ["std","attributes"]` to keep the tree shallow. Contains internal `unsafe` — record the `cargo-geiger` count. Blast radius is low: `vaco_core::log` is the only surface other crates see, so replacing it is a one-crate change. |
| `proc-macro2`, `quote`, `syn` 2 | `vaco-opts-derive` | yes | MIT OR Apache-2.0 | yes | Build-time only, explicitly blessed as tooling. Record `syn`'s feature set — §6's attribute grammar needs `full` + `extra-traits`. |
| `proptest` 1 | dev-dependency in every crate; D6 requires it | yes | MIT OR Apache-2.0 | yes | Dev-only, so Gate 3's "shallow" bar is relaxed — it never reaches a shipped binary. |
| `divan` 0.1 | Benchmarks; D8 permits Criterion or divan | yes | MIT OR Apache-2.0 | yes | Chosen over Criterion for lower harness overhead and no `plotters`/`rayon` tail — a direct Gate 3 "shallow" call. Younger and smaller than Criterion, so record the maintenance assessment honestly; Criterion is the fallback if it stalls. |
| `insta` 1 | Snapshot tests for schema dumps, `-h` output and text-writer output | yes | Apache-2.0 | yes | Dev-only. Earns its place in §6 and §9, where the deliverable *is* a large exact text blob. |
| `arbitrary` 1, `libfuzzer-sys` | fuzz targets only | see note | MIT OR Apache-2.0 (`libfuzzer-sys` also Apache-2.0 WITH LLVM-exception) | yes | Both live in the excluded `fuzz/` workspace (F1). `libfuzzer-sys` links libFuzzer, which is a genuine Gate 1 question; it is resolved by `fuzz/` never being part of a shipped artefact and never appearing in the root workspace's `cargo tree`. Record this as an explicit, scoped exception rather than letting it pass unremarked. |

### 3.2 Write our own — with the reason

Each of these is a D10 "judgement call" outcome, not a prohibition. The reason is written down so it can
be revisited when the landscape changes.

| Instead of | We write | Why |
|---|---|---|
| `bitflags` | `vaco-opts`' `opt_flags!` macro | **Model does not fit.** Our flag types must carry a per-flag `name`, `help` string and unit name so `-h filter=x` can print them; `bitflags` has nowhere to put that, and we must generate the option schema from the same declaration anyway. Adopting it would mean declaring every flag twice, which is exactly the drift F10 exists to prevent. |
| `bytemuck` | F8's over-allocate-and-subslice | **Not needed.** F8 gets 64-byte alignment in five lines of ordinary safe code. Taking on a crate whose value is encapsulated `unsafe`, to avoid five safe lines, is the wrong side of §3.4's trade. |
| `memchr` | `vaco_simd::scan` | **Capability + hot path.** We need a 3-byte *sequence* scan (`00 00 01`), not a byte-set scan, so `memchr` does not do the job as-is; and start-code scanning is decode-hot, where D10's fourth judgement-call bullet applies — it belongs in the `KernelSet`/`differential_test!` regime with every other kernel. |
| `crossbeam-queue` | `Mutex<Vec<_>>` in `vaco-pool` | **Not needed yet.** The pool is touched once per frame, not per pixel; an uncontended `Mutex` is ~20 ns. Revisit on a benchmark showing pool contention — at which point `crossbeam` clears the gates and is the right answer. |
| `rustfft` / `realfft` | `vaco-tx` (layer 3, outside this plan) | **Capability**, exactly as D10 names it: bit-exact `i32` fixed-point transforms for audio-codec conformance, which neither crate offers. |
| `yuv` (yuvutils-rs), `dcv-color-primitives` | `vaco-color` + `vaco-scale` | **Model + hot path.** They are conversion libraries carrying their own buffer and threading models; we need colorimetry as *data* (§12) feeding the ops-graph scaler (architecture §7.2 priority 1), with our own SIMD and allocation control. Both stay useful as prior-art reference points, which is not the same as a dependency. |

### 3.3 Cannot be adopted

| Crate | Why |
|---|---|
| `multiversion` | Expands to `#[target_feature]` + `unsafe` **inside our crate**, and `#![forbid(unsafe_code)]` applies to macro-expanded code. Not a gate failure — a D2 incompatibility. See F5′ (§5.3), which explains why `fearless_simd::dispatch!` does *not* have this problem: its expansion contains no `unsafe` at all. |
| `fearless_simd`'s `kernel!` macro *(a partial exclusion, not a crate)* | We adopt the crate but **may not use `kernel!`**, which expands `unsafe { … }` into the calling crate — the same D2 incompatibility as `multiversion`. Recorded here so nobody reaches for it. Consequence: raw `core::arch` intrinsics stay unreachable, and §5.6's gaps must be composed rather than bypassed. |
| Any `-sys` crate, any binding, anything with a `links` key or a native `build.rs` | Gate 1, absolutely. Enforced structurally by the `deny.toml` bans and `xtask dep-gate` (§1.5, §1.8), never by trusting declared metadata — D9 records that metadata as demonstrably unreliable. |

### 3.4 The unsafe tension, stated plainly

`#![forbid(unsafe_code)]` is a per-crate lint. It stops at our crate boundary and cannot reach a
dependency. The honest claim is therefore: **every line of media logic Vaco ships is safe Rust, and our
dependencies are chosen so the unsafe we transitively carry is small, auditable, and off the media
path.** It is not "this process contains no unsafe" — `std` alone refutes that, and so does `tracing`.

How we hold that line:

1. **Measure, don't assume.** `cargo-geiger` runs weekly on `main`; its output is appended to
   `docs/dependencies.md`. A dependency whose unsafe count grows materially between releases is a
   review trigger, recorded like any other adoption decision.
2. **Prefer the lower-unsafe option when two crates are otherwise comparable.** A real tiebreaker, not
   a slogan — it is part of why `divan` beat Criterion, alongside tree size.
3. **Weight by position, not by count.** Unsafe in a build-time proc macro (`syn`) has no runtime
   consequence at all. Unsafe in a span registry runs once per log event, off the pixel path. Unsafe in
   a codec inner loop runs a billion times a minute on attacker-controlled bytes. Only the third kind
   justifies writing our own code to avoid it — and Layer 0/1 contains none of it.
4. **When the guarantee must be end-to-end, we write the component.** Every Layer 0/1 crate that touches
   untrusted bytes — `vaco-bitstream`, `vaco-pixfmt`, `vaco-chlayout`, `vaco-frame`, `vaco-packet`,
   `vaco-expr` — is ours, with no external runtime dependency beyond `vaco-core`. That is deliberate,
   and it is what lets us make the strong form of the claim about the parsing path specifically.
5. **Publish the true statement.** README and release notes carry the claim in the wording above.
   Overstating it once destroys the credibility D2 exists to buy.

**Net for Layer 0/1:** seven external crates, of which two (`thiserror`, `tracing`) reach a shipped
binary and five are build-time or dev-only. `planning/research/09-dependency-licence-register.md` covers
media crates but has no section for toolchain/infrastructure crates; adding one, plus the corresponding
`docs/dependencies.md` entries, is action item #1 for this layer and blocks first merge.

---

## 4. `vaco-core`

### 4.1 Purpose

Everything every other crate needs and nothing anyone can build on top: the error taxonomy, rational
arithmetic, the timestamp/timebase model, `MediaType`, the ordered dictionary, string escaping, the
`parseutils` family (duration, video rate, image size, colour), numeric cast helpers, and the logging
façade. Depends on `std`, `thiserror` and `tracing`, nothing else.

### 4.2 Public API

```rust
#![forbid(unsafe_code)]

// ---------------- errors (F7) ----------------
pub type Result<T, E = Error> = core::result::Result<T, E>;

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Malformed or truncated coded data. The workhorse: equivalent of AVERROR_INVALIDDATA.
    InvalidData { what: &'static str, detail: Option<Box<str>> },
    /// End of the input stream, reached cleanly.
    Eof,
    /// The operation needs more input before it can produce output.
    Again,
    /// Feature recognised but not implemented. Carries a tracking hint (AVERROR_PATCHWELCOME).
    Unsupported { what: Box<str> },
    /// A named component was not found.
    NotFound { kind: ComponentKind, name: Box<str> },
    /// Option-system failure; see `vaco-opts`.
    Option(OptionError),
    /// Underlying I/O failure.
    Io(std::io::Error),
    /// Requested buffer/allocation exceeds the configured limit (never an abort — see 4.4).
    LimitExceeded { what: &'static str, requested: u64, limit: u64 },
    /// Numeric overflow in timestamp or size arithmetic that cannot be represented.
    Overflow { what: &'static str },
    Experimental { what: Box<str> },
    /// Codec/format recognised the data but the stream changed incompatibly mid-stream.
    InputChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ComponentKind { Demuxer, Muxer, Decoder, Encoder, Filter, Protocol, Bsf, Option, Stream }

impl Error {
    /// Stable short code used by `-loglevel` output and by the differential harness.
    #[must_use] pub const fn code(&self) -> &'static str;
    #[must_use] pub const fn is_retryable(&self) -> bool;   // Again | Eof
}
impl core::fmt::Display for Error { /* hand-written; text is a compatibility surface */ }
impl std::error::Error for Error { fn source(&self) -> Option<&(dyn std::error::Error + 'static)>; }
impl From<std::io::Error> for Error {}

// ---------------- rational ----------------
/// Exact rational. `den` is always > 0 and `gcd(num, den) == 1` after `reduce`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rational { num: i32, den: i32 }

impl Rational {
    pub const ZERO: Self;
    pub const ONE:  Self;
    /// Panics only on `den == 0`, which is a programming error, never input-derived.
    #[must_use] pub const fn new(num: i32, den: i32) -> Self;      // reduces + normalises sign
    #[must_use] pub const fn new_raw(num: i32, den: i32) -> Option<Self>;
    #[must_use] pub const fn num(self) -> i32;
    #[must_use] pub const fn den(self) -> i32;
    #[must_use] pub fn to_f64(self) -> f64;
    /// Stern–Brocot best rational approximation with a denominator bound.
    #[must_use] pub fn from_f64(x: f64, max_den: i32) -> Self;
    #[must_use] pub fn inv(self) -> Option<Self>;
    #[must_use] pub fn checked_add(self, o: Self) -> Option<Self>;
    #[must_use] pub fn checked_mul(self, o: Self) -> Option<Self>;
    #[must_use] pub fn checked_div(self, o: Self) -> Option<Self>;
    /// Never overflows: compares via i64 cross-multiplication.
    #[must_use] pub fn cmp_exact(self, o: Self) -> core::cmp::Ordering;
    #[must_use] pub fn reduce(num: i64, den: i64, max: i64) -> (Self, bool);
}
impl core::str::FromStr for Rational {}          // "30000/1001", "25", "1.5"
impl core::fmt::Display for Rational {}          // "30000/1001"
impl PartialOrd for Rational {} impl Ord for Rational {}

// ---------------- time ----------------
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimeBase(Rational);

/// Presentation timestamp in some `TimeBase`. There is no NOPTS sentinel (F4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ts(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rounding { pub mode: RoundMode, pub pass_min_max: bool }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RoundMode { Zero, Inf, Down, Up, #[default] NearInf }

impl TimeBase {
    pub const MICROSECONDS: Self;               // 1/1_000_000, the "AV_TIME_BASE" role
    #[must_use] pub const fn new(num: i32, den: i32) -> Self;
    #[must_use] pub fn rescale(self, ts: Ts, to: TimeBase, r: Rounding) -> Result<Ts>;
    #[must_use] pub fn to_seconds(self, ts: Ts) -> f64;
}

/// Exact `a * b / c` with i128 intermediates and the requested rounding.
/// Returns `Err(Overflow)` rather than saturating silently.
pub fn rescale_rnd(a: i64, b: i64, c: i64, r: Rounding) -> Result<i64>;
/// Order two timestamps expressed in different time bases without converting either.
pub fn compare_ts(a: Ts, atb: TimeBase, b: Ts, btb: TimeBase) -> core::cmp::Ordering;

/// Duration in microseconds. The `duration` option type (§6) parses into this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Duration(pub i64);

// ---------------- media type ----------------
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MediaType { Video, Audio, Subtitle, Data, Attachment }
impl MediaType {
    #[must_use] pub const fn name(self) -> &'static str;             // "video", "audio", …
    #[must_use] pub const fn abbrev(self) -> char;                   // 'v', 'a', 's', 'd', 't'
    pub fn parse(s: &str) -> Option<Self>;
}

// ---------------- ordered dictionary ----------------
/// Insertion-ordered, case-sensitive-by-default, multi-key capable string map.
/// Metadata and option maps are small (< 64 entries), so this is a Vec with linear scan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dict { entries: Vec<(Box<str>, Box<str>)> }

#[derive(Debug, Clone, Copy, Default)]
pub struct DictFlags { pub match_case: bool, pub ignore_suffix: bool,
                       pub dont_overwrite: bool, pub append: bool,
                       pub multikey: bool, pub dedup: bool }

impl Dict {
    pub fn get(&self, key: &str) -> Option<&str>;
    pub fn get_with(&self, key: &str, prev: Option<usize>, f: DictFlags) -> Option<(usize, &str, &str)>;
    pub fn set(&mut self, key: &str, val: &str);
    pub fn set_with(&mut self, key: &str, val: &str, f: DictFlags);
    pub fn remove(&mut self, key: &str) -> Option<Box<str>>;
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)>;
    pub fn len(&self) -> usize;  pub fn is_empty(&self) -> bool;
    /// Parse "k=v:k2=v2" with the given separators and the standard escaping rules.
    pub fn parse_string(&mut self, s: &str, kv_sep: &str, pairs_sep: &str, f: DictFlags) -> Result<()>;
    pub fn to_string_with(&self, kv_sep: char, pairs_sep: char) -> String;
}

// ---------------- string escaping (the 3-tier scheme, research §04 §34) ----------------
pub mod escape {
    #[derive(Debug, Clone, Copy)]
    pub enum Mode { Auto, Backslash, Quote }
    /// Escape `special` characters for one level of the filtergraph/option grammar.
    pub fn escape(s: &str, special: &str, mode: Mode) -> String;
    /// One level of unescaping; `\` escapes and `'…'` quoting.
    pub fn unescape(s: &str) -> Result<String, EscapeError>;
    /// Split on `sep`, honouring `\` escapes and `'…'` quoting. Never allocates for the common case.
    pub fn split<'a>(s: &'a str, sep: char) -> impl Iterator<Item = Result<std::borrow::Cow<'a, str>, EscapeError>>;
}

// ---------------- parseutils ----------------
pub mod parse {
    /// "1920x1080", "hd1080", "vga", "qcif", … Table of ~40 abbreviations.
    pub fn image_size(s: &str) -> Result<(u32, u32)>;
    /// "25", "30000/1001", "ntsc", "pal", "film", "ntsc-film", …
    pub fn video_rate(s: &str) -> Result<Rational>;
    /// "12:34:56.789", "-1:02.5", "1234.5", "5ms", "2s"; ± allowed. Result is microseconds.
    pub fn duration(s: &str) -> Result<Duration>;
    /// "2024-01-02T03:04:05Z", "now", "23:59:59" — absolute wall-clock parse.
    pub fn date(s: &str, now: std::time::SystemTime) -> Result<i64>;
    /// "#rrggbb[aa]", "0xRRGGBB", "red", "Red@0.5", "random". ~140 named colours (X11/SVG set).
    pub fn color(s: &str) -> Result<Rgba>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgba { pub r: u8, pub g: u8, pub b: u8, pub a: u8 }

// ---------------- numeric cast vocabulary (F2) ----------------
pub mod num {
    /// Saturating narrowing cast.
    pub fn sat<T: Saturate>(x: impl Into<i128>) -> T;
    /// Checked narrowing cast; `None` on truncation. Use for anything input-derived.
    pub fn narrow<T: TryFrom<i128>>(x: impl Into<i128>) -> Option<T>;
    pub fn gcd(a: u64, b: u64) -> u64;
    /// Integer log2 rounded down; 0 for 0.
    pub const fn log2_floor(x: u64) -> u32;
    pub const fn align_up(x: usize, align: usize) -> usize;
    pub const fn clip(x: i64, lo: i64, hi: i64) -> i64;
    /// Clamp-to-u8 used everywhere in pixel code; branchless.
    pub const fn clip_u8(x: i32) -> u8;
}

// ---------------- logging façade ----------------
pub mod log {
    /// The ffmpeg level ladder, because `-loglevel` names are a CLI compatibility surface.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub enum Level { Quiet, Panic, Fatal, Error, Warning, Info, Verbose, Debug, Trace }
    impl Level {
        pub fn parse(s: &str) -> Option<Self>;
        pub const fn name(self) -> &'static str;
        pub const fn to_tracing(self) -> tracing::Level;
    }
    /// Installed once by the binaries; libraries only emit through `tracing` macros.
    pub fn init(level: Level, flags: LogFlags);
    #[derive(Debug, Clone, Copy, Default)]
    pub struct LogFlags { pub repeat: bool, pub level_prefix: bool, pub timestamps: bool, pub colour: Colour }
}
```

### 4.3 Internal design

- **`Rational` is `i32/i32`, matching the interface fact, but every operation runs in `i64`/`i128` and
  returns `Option`.** FFmpeg's `av_mul_q` silently reduces with rounding; ours returns `None` on
  representability failure and offers `mul_q_approx` for the places (display aspect ratio, guessed frame
  rate) where approximation is the documented behaviour. This makes the lossy sites visible.
- **`rescale_rnd` uses `i128` natively.** No 64×64→128 emulation, no unsafe, no overflow. This function is
  called for literally every packet and frame in the pipeline; `i128` multiply/divide compiles to two
  instructions on x86-64 and is not the bottleneck.
- **`Dict` is a `Vec<(Box<str>, Box<str>)>`.** Media metadata dictionaries have single-digit entry counts;
  a hash map costs more in allocation and hashing than a linear scan saves, and a `Vec` preserves the
  insertion order that muxers depend on for byte-identical output (D5). Multi-key and `IGNORE_SUFFIX`
  semantics fall out naturally from an ordered vector; they do not from a `HashMap`.
- **Escaping is a first-class module, not a helper.** The three-tier escaping scheme in research §04 is the
  single largest source of user-visible bugs in FFmpeg's CLI. Modelling it as an explicit `Mode` +
  round-trip-tested `escape`/`unescape`/`split` trio, exercised by proptest, is cheap insurance for the
  whole CLI layer.
- **`log::Level` is our own ladder mapped onto `tracing`.** `tracing` has five levels; ffmpeg has nine and
  the names are user-visible (`-loglevel verbose`). We keep our ladder as the public surface and map to
  `tracing` targets/levels internally, with `Verbose`/`Debug`/`Trace` distinguished by a `tracing` field.

### 4.4 Allocation limits

`Error::LimitExceeded` exists because D6 lists "unbounded allocation" as a fuzz finding. `vaco-core`
exports:

```rust
pub struct Limits { pub max_alloc: u64, pub max_frame_bytes: u64, pub max_packet_bytes: u64,
                    pub max_streams: u32, pub max_side_data: u32 }
impl Default for Limits { /* 1 GiB / 512 MiB / 256 MiB / 4096 / 256 */ }
pub fn try_reserve<T>(v: &mut Vec<T>, n: usize, limits: &Limits, what: &'static str) -> Result<()>;
```

Every parser that allocates from an input-derived count calls `try_reserve`, which combines the limit check
with `Vec::try_reserve` so OOM is an `Err`, not an abort. This is enforced by the fuzz targets, not by
convention.

### 4.5 Dependencies

`std`, `thiserror`, `tracing`. Dev: `proptest`, `insta`.

Gate assessment (D10): all three clear Gates 1–3 as recorded in §3.1; `docs/dependencies.md` entries are a
prerequisite for the first merge. Nothing here is media-specific — a rational type, a timestamp model, an
ordered dictionary and an escaping grammar are the parts no general-purpose crate can supply because they
encode *our* semantics, so there is no buy option to weigh. `num-rational` was considered and rejected on
model grounds: it is generic over integer types and allocates for `BigInt`, whereas we need exactly
`i32/i32` with `i128` intermediates, `Option`-returning arithmetic and a `Display` that matches the CLI.

### 4.6 Test strategy

| Kind | Content |
|---|---|
| Unit | Error `Display` text pinned against a golden table extracted from `ffmpeg -loglevel` output. `MediaType` name/abbrev round-trip. Colour-name table spot checks. |
| Proptest | `Rational`: `reduce` idempotence; `cmp_exact` is a total order agreeing with `to_f64` comparison wherever f64 is exact; `checked_*` never produces `den <= 0`; `from_f64(x, d).to_f64()` within `1/d` of `x`. `rescale_rnd`: agreement with an `i128`-exact reference for all five rounding modes, including `i64::MIN/MAX` with and without `pass_min_max`. `escape`: `unescape(escape(s, spec, m)) == s` for arbitrary `s` and all modes; `split(join(parts))  == parts`. `Dict::parse_string`/`to_string_with` round-trip. |
| Fuzz | `parse_duration`, `parse_color`, `parse_image_size`, `parse_video_rate`, `dict_parse_string`, `escape_unescape` — each asserting no panic, bounded allocation, and (where applicable) round-trip. |
| Differential | `parse` helpers are directly reachable through the reference CLI: `ffmpeg -t <s>`/`-ss <s>` echo the parsed duration in `-loglevel verbose` output; `ffplay`-free colour parsing is reachable via `ffmpeg -f lavfi -i color=c=<spec>:s=2x2 -frames 1 -f rawvideo -` and comparing the four output bytes. Both are cheap, high-value oracles: drive them from a generated corpus of ~5 000 colour and duration strings and diff. `-loglevel` name parsing compared against the reference's acceptance/rejection. |

### 4.7 Effort and blocking

**2 person-weeks.** Blocks *everything*. This is the one crate that must be interface-frozen on day 1 —
publish the `Error`, `Rational`, `Ts`/`TimeBase`, `Dict` and `MediaType` signatures as a stub crate in the
first 48 hours so all other tracks can compile against it while the bodies are written.

---

## 5. `vaco-simd`

### 5.1 Purpose

**Revised by D12 (2026-08-21).** `vaco-simd` is no longer a set of helpers *over* `std::simd`. It is a
**thin adapter over `fearless_simd`**, and it is the only crate in the workspace that names
`fearless_simd` — per D11, exactly as if the substrate were an external codec.

It provides: capability detection and reporting, the `KernelSet` pattern from architecture §7.3, our own
`dispatch_kernel!` wrapper, the `ops` module (our named vocabulary, including compositions for every
operation the substrate lacks — §5.6), byte scanning for `vaco-bitstream`, and the differential-testing
machinery that keeps every SIMD kernel in lockstep with a scalar reference. **No codec, format or pixel
knowledge whatsoever** — it takes slices of primitives.

### 5.2 Public API

```rust
// crates/core/vaco-simd/src/lib.rs
#![forbid(unsafe_code)]          // survives — see the note at the end of §5.3

//! The SIMD substrate adapter (D12). `fearless_simd` appears in this crate's `Cargo.toml`
//! and in no other; `xtask dep-gate` asserts exactly one occurrence under `crates/`.

// ---------------- our vocabulary ----------------

/// Our name for a **capability token**: a zero-sized value that witnesses, in the type system,
/// that a particular CPU level is available. Every kernel body is generic over `L: Lanes`, and
/// holding an `L` is what makes the substrate's intrinsic calls safe at the call site.
///
/// This is a re-export under our own name, not a newtype. See "the honest boundary" in §5.3.
pub use fearless_simd::Simd as Lanes;

#[doc(hidden)]
pub mod __substrate {
    //! Re-exported for `dispatch_kernel!` to name from a downstream crate. `macro_rules!`
    //! expands in the caller's scope, so the macro cannot say `fearless_simd::` — the
    //! caller does not depend on it, and per D11 must not. This module is the one path
    //! by which the substrate's macro is reachable, and it is `#[doc(hidden)]`.
    pub use fearless_simd::dispatch;
}

/// Everything a kernel module needs in scope. Kernels import this and nothing else.
pub mod prelude {
    pub use crate::{Lanes, Tier, KernelSet, KernelSlot, Variant, ops, dispatch_kernel};
    // SimdBase, SimdInt, SimdFloat, SimdMask, SimdInto/SimdFrom, Select, Bytes,
    // SimdWiden, SimdNarrow, SimdCombine, SimdSplit, SimdInterleaved.
    pub use fearless_simd::prelude::*;
}

// ---------------- capability reporting ----------------

/// The CPU capability level, detected at runtime. A newtype over the substrate's level enum:
/// the foreign type never appears in any signature here.
///
/// Replaces the old build-time `Tier` (F5). `Tier::detect()` is now load-bearing rather than
/// diagnostic — it is what selects kernels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tier(fearless_simd::Level);

impl Tier {
    /// Detect what this CPU supports. Cached: the substrate's detection sits on top of
    /// `is_x86_feature_detected!`, which memoises into an atomic on first call.
    #[must_use] pub fn detect() -> Tier;
    /// What the compiled target guarantees without any detection. The floor.
    #[must_use] pub fn baseline() -> Tier;
    /// The scalar-equivalent tier, for the `vaco-checkasm` reference and for
    /// `VACO_KERNEL_OVERRIDE=...=scalar` bisection.
    #[must_use] pub fn scalar() -> Tier;

    /// Stable short name: "fallback" | "sse2" | "sse4.2" | "avx2" | "avx512" | "neon" | "simd128".
    /// Used in benchmark labels, checkasm variant names and `-cpuflags` output.
    #[must_use] pub fn name(self) -> &'static str;
    /// Native vector width in bytes at this tier: 1 (scalar), 16, 32 or 64.
    #[must_use] pub fn vector_bytes(self) -> usize;
    /// Ordering for "at most this tier". Total, and consistent with `vector_bytes`.
    #[must_use] pub fn rank(self) -> u8;
    /// Cap to at most `max`. This is how the `-cpuflags` CLI option and `VACO_TIER` are honoured.
    #[must_use] pub fn cap(self, max: Tier) -> Tier;

    /// Parse a `-cpuflags`-style name. Returns `None` for a name this build does not know.
    pub fn from_name(s: &str) -> Option<Tier>;

    #[doc(hidden)]  // the single, deliberate crack in the D11 boundary; see §5.3.
    pub fn __level(self) -> fearless_simd::Level { self.0 }
}

/// Human-readable flag list for `-cpuflags` / `-hide_banner` output. Built from the safe
/// `is_x86_feature_detected!` / `is_aarch64_feature_detected!` std macros, independently of
/// `Tier`, because it reports *everything* the CPU has, not just what we dispatch on.
#[must_use] pub fn cpu_flag_names() -> Vec<&'static str>;

/// Process-wide tier cap, set once from the CLI before any component is constructed.
pub fn set_tier_cap(t: Tier);
#[must_use] pub fn tier_cap() -> Tier;

// ---------------- dispatch ----------------

/// Run a level-generic kernel body at `tier`, monomorphised for that level.
///
/// Wraps `fearless_simd::dispatch!`. This macro is the only place the substrate's dispatch
/// macro is named. Expansion contains **no `unsafe`** — it is a `match` over the level enum
/// that binds a token and calls the substrate's `vectorize()` trait method, which is where the
/// `#[target_feature]` lives. That is precisely why `#![forbid(unsafe_code)]` survives.
///
/// ```ignore
/// dispatch_kernel!(tier, lanes => yuv420p_to_rgb24(lanes, src, dst, stride, k))
/// ```
#[macro_export]
macro_rules! dispatch_kernel {
    ($tier:expr, $lanes:pat => $body:expr) => {
        $crate::__substrate::dispatch!($crate::Tier::__level($tier), $lanes => $body)
    };
}

// ---------------- the KernelSet pattern ----------------

/// A family of kernels selected together, once, at component construction.
///
/// Unchanged in shape from the pre-D12 design — deliberately. What changed is that `for_tier`
/// is now handed a *runtime-detected* tier instead of a compile-time constant, and each variant
/// is a dispatching wrapper rather than a directly-monomorphised function. Consumers see no
/// difference.
pub trait KernelSet: Copy + Sized + 'static {
    /// Build the set for a given tier. Must be total: every tier yields a working set.
    fn for_tier(tier: Tier) -> Self;
    /// The scalar reference set. `vaco-checkasm` compares every other tier against this.
    fn reference() -> Self { Self::for_tier(Tier::scalar()) }
    /// Stable names of each kernel, for checkasm reporting and benchmark labels.
    fn kernel_names() -> &'static [&'static str];
    #[inline]
    fn select() -> Self { Self::for_tier(Tier::detect().cap(tier_cap())) }
}

// ---------------- ops: OUR vocabulary, including the gap compositions ----------------

/// Every operation a kernel is allowed to use that is not a plain operator or a `SimdBase`
/// method. In particular **every entry in the gap table (§5.6) is here and only here** — a
/// kernel never open-codes a composition, so if the substrate grows a native operation we
/// change one function and every kernel gets it.
pub mod ops {
    use crate::Lanes;
    use fearless_simd::prelude::*;

    // -- gap compositions (§5.6). Each documents its instruction count. --

    /// Unsigned saturating add. 3 ops: `not`, `min`, `add`. (`paddusb` is 1.)
    ///
    /// `a.min(!b) + b`: if `a + b` would exceed `MAX` then `a > !b`, so the `min` yields `!b`
    /// and the sum is exactly `MAX`. Exact, in-width, no widening, no branch.
    #[inline(always)]
    pub fn saturating_add<L: Lanes, V>(a: V, b: V) -> V
    where V: SimdBase<L> + core::ops::Add<Output = V> + core::ops::Not<Output = V>;

    /// Unsigned saturating sub. 2 ops: `max`, `sub`. (`psubusb` is 1.)
    #[inline(always)]
    pub fn saturating_sub<L: Lanes, V>(a: V, b: V) -> V
    where V: SimdBase<L> + core::ops::Sub<Output = V>;

    /// Signed saturating add for i16 lanes: widen -> 2x add -> `saturating_narrow`. ~5 ops.
    /// Prefer restructuring so the saturation happens at an existing narrowing step instead;
    /// see the note in §5.6.
    #[inline(always)]
    pub fn saturating_add_i16<L: Lanes>(lanes: L, a: L::i16s, b: L::i16s) -> L::i16s;

    /// Rounded average, `(a + b + 1) >> 1`, on unsigned lanes. 4 ops, exact, in-width:
    /// `(a | b) - ((a ^ b) >> 1)`. (`pavgb`/`urhadd` is 1.) No widening, no overflow.
    #[inline(always)]
    pub fn avg_round<L: Lanes, V>(a: V, b: V) -> V where V: SimdBase<L>;

    /// Truncating average, `(a + b) >> 1`: `(a & b) + ((a ^ b) >> 1)`. 4 ops.
    #[inline(always)]
    pub fn avg_trunc<L: Lanes, V>(a: V, b: V) -> V where V: SimdBase<L>;

    /// Lanewise absolute difference on unsigned lanes. 3 ops: `max`, `min`, `sub`.
    /// Exact and in-width — this is the *cheap* half of SAD; the expensive half is the
    /// accumulation, which needs widening. See `sad_u8_row`.
    #[inline(always)]
    pub fn abs_diff<L: Lanes, V>(a: V, b: V) -> V where V: SimdBase<L> + core::ops::Sub<Output = V>;

    /// Lanewise absolute value on **signed** lanes. The substrate's `abs` is on `SimdFloat`
    /// only; there is no integer `abs`. 2 ops: `x.max(zero - x)`.
    #[inline(always)]
    pub fn abs_int<L: Lanes, V>(lanes: L, x: V) -> V
    where V: SimdBase<L> + core::ops::Sub<Output = V>;

    /// Horizontal sum of i32 lanes into an i64. `log2(N)` rounds of
    /// `rotate_elements_left` + `add`, then lane 0.
    ///
    /// **Call this once per kernel invocation, never inside the inner loop.** Keep a vector
    /// accumulator across the loop and reduce at the end; the reduction is then O(1) per call.
    #[inline(always)]
    pub fn hsum_i32<L: Lanes>(x: L::i32s) -> i64;
    #[inline(always)]
    pub fn hmax_u8<L: Lanes>(x: L::u8s) -> u8;
    #[inline(always)]
    pub fn hmin_u8<L: Lanes>(x: L::u8s) -> u8;

    /// Widening multiply-accumulate with a **broadcast** coefficient:
    /// `acc += widen(a) * splat(c)`, u8 input, i16 accumulation.
    ///
    /// This is the shape to reach for, *not* the `pmaddwd` pairwise-dot shape. Broadcast
    /// coefficients avoid the widening-multiply gap almost entirely: 2 muls + 2 adds per
    /// already-widened source vector. See §5.6 and plan 12 §1.1 for why this matters.
    #[inline(always)]
    pub fn wmla_u8_i16<L: Lanes>(acc: (L::i16s, L::i16s), a: L::u8s, c: i16) -> (L::i16s, L::i16s);

    /// Pairwise widening dot product, the `pmaddwd` shape:
    /// `out[i] = a[2i]*b[2i] + a[2i+1]*b[2i+1]`, i16 -> i32.
    ///
    /// **~12 ops for what `pmaddwd` does in 1 per output vector.** Provided so kernels that
    /// genuinely need it have one correct implementation, and marked `#[deprecated]`-in-review:
    /// a kernel PR that calls this must justify why `wmla_u8_i16` does not fit.
    #[inline(always)]
    pub fn madd_i16_i32<L: Lanes>(a: L::i16s, b: L::i16s) -> (L::i32s, L::i32s);

    // -- widening / narrowing chains --

    /// One native u8 vector -> four native i32 vectors, ascending lane order.
    /// `widen` twice then `bitcast`; this is the `pmovzxbd`/`ushll` chain.
    #[inline(always)]
    pub fn widen_u8_to_i32<L: Lanes>(v: L::u8s) -> [L::i32s; 4];

    /// Four native i32 vectors -> one native u8 vector, with an arithmetic right shift and
    /// saturation. The standard "pack back to pixels" step. Uses the substrate's
    /// `saturating_narrow`, which is native (`packusdw`+`packuswb` / `sqxtun`) — this one is
    /// *not* a gap.
    #[inline(always)]
    pub fn pack_shift_u8<L: Lanes>(x: [L::i32s; 4], shift: u32) -> L::u8s;

    // -- packed-pixel store helpers --

    /// Store three planar u8 vectors as interleaved RGB24.
    ///
    /// Operates on **128-bit blocks** (`L::u8s::Block`) regardless of native width, because
    /// `swizzle_dyn` is exactly one `pshufb`/`tbl` on a 128-bit vector but lane-crossing (and
    /// therefore multi-instruction) at 256/512 bits. Cost: 3 `swizzle_dyn` + 2 `select` per
    /// output block. See plan 12 §2.4.
    #[inline(always)]
    pub fn store_rgb24<L: Lanes>(lanes: L, r: L::u8s, g: L::u8s, b: L::u8s, out: &mut [u8]);

    /// Store four planar u8 vectors as interleaved RGBA32. Uses the substrate's native
    /// `store_four_interleaved` — no shuffle tables, no gap. Prefer RGBA over RGB24 wherever
    /// the pipeline allows it (plan 12 §2.4 design consequence).
    #[inline(always)]
    pub fn store_rgba<L: Lanes>(lanes: L, r: L::u8s, g: L::u8s, b: L::u8s, a: L::u8s, out: &mut [u8]);

    /// Byte-set search returning a bitmask of matching lanes; the primitive under start-code
    /// scanning. `simd_eq` then `to_bitmask` — both native.
    #[inline(always)]
    pub fn eq_mask<L: Lanes>(x: L::u8s, v: u8) -> u64;
}

// ---------------- byte scanning (used by vaco-bitstream, §8.6) ----------------
pub mod scan {
    /// Index of the first occurrence of the 3-byte sequence `00 00 01`, at or after `from`.
    pub fn find_start_code(buf: &[u8], from: usize) -> Option<usize>;
    /// Index of the first `00 00 03` emulation-prevention triple, at or after `from`.
    pub fn find_emulation(buf: &[u8], from: usize) -> Option<usize>;
    /// Copy `src` into `dst`, removing emulation-prevention bytes. Returns bytes written.
    pub fn rbsp_unescape_into(src: &[u8], dst: &mut Vec<u8>) -> usize;
    pub mod scalar {  // the reference implementations, always compiled, used by checkasm
        pub fn find_start_code(buf: &[u8], from: usize) -> Option<usize>;
        pub fn find_emulation(buf: &[u8], from: usize) -> Option<usize>;
        pub fn rbsp_unescape_into(src: &[u8], dst: &mut Vec<u8>) -> usize;
    }
}

// ---------------- differential testing harness ----------------
/// Generates a proptest asserting the SIMD kernel matches the scalar reference bit-exactly,
/// over randomised inputs plus a fixed edge-case corpus (all-zero, all-max, alternating,
/// lengths 0..=3*LANES+1, misaligned start offsets).
#[macro_export]
macro_rules! differential_test { /* … see 5.5 … */ }
```

### 5.3 F5′ — Runtime SIMD dispatch via capability tokens

> **Supersedes F5 (2026-08-21, per D12).** The original F5 concluded *build-time* SIMD selection, on the
> reasoning that runtime ISA dispatch was structurally unreachable under `#![forbid(unsafe_code)]`. That
> reasoning was correct about `std::simd` and is preserved verbatim at the end of this section. It is
> obsolete because a third option appeared that neither of us had considered: `fearless_simd` proves
> feature availability with a **capability token** rather than asserting it with `unsafe`.

#### The decision

**Runtime ISA dispatch, via `fearless_simd`'s capability tokens.** One binary detects its CPU at startup
and runs AVX-512, AVX2, SSE4.2 or NEON kernels accordingly — which is what FFmpeg does and what F5 said
we could not have.

#### Why it works, mechanically

The problem F5 identified is real and unchanged: `#[target_feature(enable = "avx2")]` functions are
`unsafe` to call, because the *caller* cannot prove to the compiler that the CPU has AVX2.

`fearless_simd` supplies the missing proof as a value. `Avx2`, `Neon`, `Sse4_2`, `Avx512`, `Sse2`,
`WasmSimd128` and `Fallback` are zero-sized token structs, each implementing the `Simd` trait
(our alias: `Lanes`). A token can only be obtained from a checked runtime detection —
`Level::new()` followed by `Level::as_avx2() -> Option<Avx2>` — or from the compile-time baseline.
**Holding the token is the proof.** A kernel written as `fn k<L: Lanes>(lanes: L, …)` is monomorphised
once per level; `dispatch!` matches on the detected `Level` and calls the right monomorphisation with the
right token.

The `#[target_feature]` attribute still exists, but it lives on one function inside `fearless_simd`
(`Simd::vectorize`), applied per level, and the `unsafe` that calls it is written once there. **Nothing
in the expansion of `dispatch!` is `unsafe`.** Verified against the v0.7.0 source: `dispatch!` expands to
a `match` over `Level` that binds a token and calls `Simd::vectorize(token, || body)` — a plain safe
trait method. `#![forbid(unsafe_code)]` therefore holds in `vaco-simd` and in every kernel crate, with no
allowlist entry and no exception.

This is exactly the distinction that disqualified `multiversion` in §3.3: `multiversion` expands
`unsafe` into *our* crate, `fearless_simd`'s `dispatch!` does not.

#### Options reconsidered

| Option | Verdict |
|---|---|
| (a) `#[target_feature]` + `unsafe` dispatch in `vaco-simd` | Still violates D2. Still requires an allowlist amendment. **No longer needed.** |
| (b) `multiversion` crate | Still expands `unsafe` inside our crate. Still out. |
| (c) Per-tier `cdylib`s loaded at runtime | Still absurd. |
| (d) Build-time target selection, one binary per microarchitecture level | **Superseded.** Was chosen under F5. It cost 3x build time, 3x artefact size, a launcher process, and it still left AVX-512 unreachable for anyone running the shipped default. |
| (e) **Capability-token runtime dispatch (`fearless_simd`)** | **Chosen.** One artefact per platform, full ISA reach at runtime, `forbid(unsafe_code)` intact. |

#### What (e) costs, honestly

1. **Binary size.** On x86, `dispatch!` compiles every level-generic function once per level. With the
   v2 floor set in §1.6 that is three monomorphisations (SSE4.2 / AVX2 / AVX-512), not four — the `Sse2`
   arm collapses into the ambient baseline. Mitigations, in order: `codegen-units = 1` and `lto = "fat"`
   are already set in `[profile.release]`; beyond that, `--cfg disable_dispatch_avx512` in `RUSTFLAGS`
   prunes a level entirely, and the substrate supports disabling a level for one function. **Measure
   before mitigating** — this is a real cost and must appear in the release report, not be assumed away.
2. **Dispatch overhead per call.** A `match` on a cached enum plus an indirect call. Paid once per kernel
   *invocation* — per row, per plane, per frame — never per pixel. Identical to the cost model the
   `KernelSet` fn-pointer table already had.
3. **A missing escape hatch.** `fearless_simd` offers a `kernel!` macro for calling raw `core::arch`
   intrinsics safely under a token. **We cannot use it.** Its expansion contains
   `unsafe { __fearless_simd_kernel(...) }` in the *calling* crate, and `#![forbid(unsafe_code)]` rejects
   macro-expanded `unsafe` (the crate is stable-only, so it cannot use `#[allow_internal_unsafe]` to
   suppress the lint). Consequence: any operation the substrate does not expose, we compose from ones it
   does — we cannot reach through to the intrinsic. This is the single most important limitation of the
   choice and it is what §5.6 is about.
4. **We are downstream of someone else's operation set.** Under `std::simd` a missing operation was a
   Rust-project problem; now it is a `fearless_simd` issue. Mitigated by the crate being small, active,
   zero-dependency, Apache-2.0 OR MIT, and forkable — and by §5.6's compositions meaning a gap is slow,
   never blocking.

#### The honest boundary — how strictly D11 actually holds

D12 consequence 2 says `vaco-simd` is an adapter "exactly as D11 prescribes". That is true in the part
that matters and slightly overstated in the letter, so state it precisely:

- **Held strictly.** `fearless_simd` appears in exactly one `Cargo.toml` under `crates/`, CI-enforced by
  `xtask dep-gate`. The substrate's `Level` enum, its `dispatch!` macro and its `kernel!` macro are named
  in `vaco-simd` and nowhere else. Every operation with a semantic we care about — every gap composition,
  every widening chain, every packed store — is behind `vaco_simd::ops` under our own name, so a
  substrate change is a change to `ops`, not to kernels.
- **Held loosely, deliberately.** `Lanes` is a `pub use` of the substrate's `Simd` trait, and
  `vaco_simd::prelude` re-exports the substrate's vector traits. Kernels therefore write `v.widen()` and
  `a + b` directly against substrate types. Wrapping 1,453 generated trait methods in newtypes would cost
  more than the swap it insures against, and — decisively — it would fight the substrate's design, which
  depends on `#[inline(always)]` propagating the target-feature context through the entire call chain.
  A newtype layer that failed to inline would silently produce non-vectorised code.
- **So the real blast radius of a substrate swap is:** a rewrite of `vaco-simd` (design work), plus a
  mechanical rename pass across kernel bodies if the replacement's vocabulary differs. Not zero changes
  outside one crate. One crate of *thought*, and `sed` elsewhere. That is worth having and it is worth
  not overselling.

#### Escalation trigger

The F5 escalation (propose adding `vaco-simd` to the D2 allowlist) is **withdrawn** — it existed to buy
AVX-512, which we now have for free. It is replaced by a narrower one:

> If a named kernel's gap composition (§5.6) measures worse than **1.3x** the instruction count of the
> native instruction *and* that kernel is on a benchmark path that matters, the response is, in order:
> (1) restructure the algorithm to avoid the operation — `wmla_u8_i16` over `madd_i16_i32` is the model;
> (2) **file it upstream**. `fearless_simd` v1.0 is targeted for early September 2026 and the project is
> actively taking feedback; a missing `saturating_add` is a reasonable thing to ask for and we are not
> the only consumer who wants it. Only if both fail does an unsafe exception get discussed, and it would
> be scoped to one operation in `vaco_simd::ops`, not to dispatch.

#### Superseded reasoning — F5 as originally written, kept for the trail

> *This is the hardest consequence of D2 in the whole layer, so it gets stated bluntly.*
>
> *`std::simd` is* portable, *not* dispatching. *`Simd<u8, 32>` compiles to whatever the* **compile-time**
> *target features allow: on a baseline `x86-64` build it becomes two SSE2 register pairs, not one AVX2
> register. Runtime ISA selection in Rust requires `#[target_feature]`, and although safe
> `#[target_feature]` functions exist on our toolchain, calling one from a context that does not
> statically have the feature — which is exactly what runtime dispatch is — requires `unsafe`, and
> coercing one to a plain `fn` pointer is not permitted. Crates that paper over this (`multiversion`)
> expand to `unsafe`* inside our crate, *which `#![forbid(unsafe_code)]` rejects at expansion time.*
>
> ***Chosen: (d)** build-time target selection. `Tier::compiled()` is a `const fn` over
> `cfg!(target_feature = "avx512f")` / `"avx2"` / `"sse4.2"` / `"neon"`, so kernel selection const-folds
> to a single variant and the `KernelSet` indirect call is eliminated entirely in a monomorphic build. We
> publish `x86-64-v2` and `x86-64-v3` artefacts (and `aarch64` with NEON+dotprod), and source builds use
> `-C target-cpu=native`.*
>
> ***Cost, honestly stated:** a distributed `v3` binary cannot use AVX-512 on machines that have it.*
> *Escalation trigger: if `vaco-checkasm` measures >15% on the end-to-end 10-bit HEVC decode benchmark
> between a `v3` build and a `v4` build on the same machine, raise a D2 amendment proposing that
> `vaco-simd` — and only `vaco-simd` — be added to the allowlist.*

**Why the trail is worth keeping:** F5's analysis was not wrong, and the structure it demanded is what
made this revision cheap. `KernelSet::for_tier(Tier)` was designed so the tier could become runtime-valued
without changing a single consumer, and that is exactly what happened — §5.2's `KernelSet` is unchanged in
shape. The lesson to carry forward is that the *interface* between "which kernel" and "who calls it" was
the load-bearing decision; the mechanism behind it was replaceable, and it got replaced.

#### aarch64 — confirmed

`Level::Neon(Neon)` exists under `#[cfg(target_arch = "aarch64")]`, with `Level::as_neon() -> Option<Neon>`
and a fully generated backend (`fearless_simd/src/generated/neon.rs`). The crate documents
"Aarch64: Baseline NEON" and builds docs for `aarch64-apple-darwin`. D12's open risk 2 is **closed**.

One consequence worth stating because it is good news: aarch64 has exactly **one** level. So on Apple
Silicon and ARM servers `dispatch!` is a single-arm match, there is one monomorphisation rather than
three, the binary-size cost of §5.3 point 1 does not apply at all, and `L::u8s` is always 16 bytes. The
aarch64 build is simpler after D12 than it was under F5, which needed a baseline and a `-v82` artefact.

### 5.4 How a kernel is actually written

**Revised by D12.** A kernel is a function generic over the **capability token**, not over the lane
count, with a scalar sibling. Worked example — the shape used by `vaco-scale`'s horizontal filter and
`vaco-resample`'s FIR. (The fuller template, with chroma upsampling and a packed store, is plan 12 §2.)

```rust
// crates/core/vaco-simd/src/kernels/fir.rs
use crate::prelude::*;

/// Reference. Always compiled, never conditionally. Definitionally correct.
pub fn fir_u8_scalar(src: &[u8], coeffs: &[i16], shift: u32, dst: &mut [u8]) {
    for (i, o) in dst.iter_mut().enumerate() {
        let mut acc: i32 = 1 << (shift - 1);
        for (t, &c) in coeffs.iter().enumerate() {
            acc += i32::from(src[i + t]) * i32::from(c);
        }
        *o = vaco_core::num::clip_u8(acc >> shift);
    }
}

/// One generic body, monomorphised once per CPU level by `dispatch_kernel!`.
///
/// `#[inline(always)]` is MANDATORY and is not a performance suggestion: it is how the
/// target-feature context of the dispatched level reaches this body. A kernel that fails to
/// inline is compiled at the baseline and silently loses its dispatch. Enforced by the
/// `forbid = ["call"]` assertion in plan 12 §3.3(c).
#[inline(always)]
pub fn fir_u8_simd<L: Lanes>(lanes: L, src: &[u8], coeffs: &[i16], shift: u32, dst: &mut [u8]) {
    let n = L::u8s::N;                    // native width: 16 (NEON/SSE), 32 (AVX2), 64 (AVX-512)
    let taps = coeffs.len();
    let round = L::i16s::splat(lanes, 1 << (shift - 1));

    let mut chunks = dst.chunks_exact_mut(n);
    let mut base = 0usize;
    for out in &mut chunks {
        // Two i16 accumulators cover one u8 vector's worth of lanes.
        let mut acc = (round, round);
        for (t, &c) in coeffs.iter().enumerate() {
            let v = L::u8s::from_slice(lanes, &src[base + t .. base + t + n]);
            // Broadcast-coefficient MAC. NOT the pairwise-dot shape — see §5.6.
            acc = ops::wmla_u8_i16::<L>(acc, v, c);
        }
        acc.0.shr(shift).saturating_narrow(acc.1.shr(shift)).store_slice(out);
        base += n;
    }
    let rem = chunks.into_remainder();
    if !rem.is_empty() {
        fir_u8_scalar(&src[base .. base + rem.len() + taps - 1], coeffs, shift, rem);
    }
}

/// The dispatching wrapper. One per kernel; generated by `vaco_simd::kernel_variants!`.
/// This is what goes in the `KernelSet` fn-pointer table.
fn fir_u8_dispatched(src: &[u8], coeffs: &[i16], shift: u32, dst: &mut [u8]) {
    dispatch_kernel!(Tier::detect(), lanes => fir_u8_simd(lanes, src, coeffs, shift, dst))
}

/// The KernelSet. Selection happens once, in the caller's constructor. Shape unchanged
/// from the pre-D12 design — see §5.3's note on why that was the load-bearing decision.
#[derive(Clone, Copy)]
pub struct FirKernels {
    pub fir_u8: fn(&[u8], &[i16], u32, &mut [u8]),
}

impl KernelSet for FirKernels {
    fn for_tier(t: Tier) -> Self {
        Self {
            fir_u8: if t == Tier::scalar() { fir_u8_scalar } else { fir_u8_dispatched },
        }
    }
    fn kernel_names() -> &'static [&'static str] { &["fir_u8"] }
}
```

Consumer side — unchanged, and this is the point. The indirect call is per *row*, not per pixel:

```rust
pub struct HScaler { k: FirKernels, coeffs: Vec<i16>, shift: u32 }
impl HScaler {
    pub fn new(/* … */) -> Self { Self { k: FirKernels::select(), /* … */ } }
    pub fn scale_row(&self, src: &[u8], dst: &mut [u8]) { (self.k.fir_u8)(src, &self.coeffs, self.shift, dst) }
}
```

Five properties fall out and are non-negotiable rules for every kernel in the project:

1. **The tail is handled by calling the scalar reference.** No duplicated edge logic, therefore no
   possibility of the tail disagreeing with the body.
2. **Accumulation order is identical between scalar and vector.** Integer kernels must be bit-identical;
   float kernels must state their tolerance in the `differential_test!` invocation and justify it.
3. **`chunks_exact_mut` gives LLVM a provably-sized inner loop**, which is what makes the bounds checks
   inside `from_slice` disappear. Any kernel written with manual indexing and a `while` loop is rejected
   in review.
4. **(new) Every SIMD function is `#[inline(always)]`, all the way down to the `dispatch_kernel!`
   boundary.** This is a correctness-of-codegen requirement, not a tuning knob. Where forcing inlining is
   genuinely undesirable — a large body called from several places — use the substrate's
   `Lanes::vectorize(lanes, || …)` to re-establish the context without inlining.
5. **(new) Anything in the §5.6 gap table is called through `vaco_simd::ops`, never open-coded.** One
   composition, one place to fix, one place to delete when the substrate grows the operation natively.

### 5.5 The differential harness

```rust
#[macro_export]
macro_rules! differential_test {
    (
        name:  $name:ident,
        simd:  $simd:path,
        scalar:$scalar:path,
        args:  ($($arg:ident : $gen:expr),* $(,)?),
        out:   $out_len:expr,
        tol:   $tol:expr $(,)?
    ) => {
        #[cfg(test)]
        mod $name {
            use super::*;
            proptest::proptest! {
                #![proptest_config(proptest::prelude::ProptestConfig::with_cases(512))]
                #[test]
                fn matches_scalar($($arg in $gen),*) {
                    let mut a = vec![0u8; $out_len];
                    let mut b = vec![0u8; $out_len];
                    $scalar($($arg.as_ref()),*, &mut a);
                    $simd  ($($arg.as_ref()),*, &mut b);
                    $crate::testing::assert_close(&a, &b, $tol);
                }
            }
            #[test] fn edge_corpus() { $crate::testing::run_edge_corpus($simd, $scalar, $out_len, $tol); }
        }
    };
}
```

`run_edge_corpus` sweeps: all-zero, all-`MAX`, alternating `00/FF`, single-`1` at every bit position,
lengths `0..=3*64+1`, and source offsets `0..64` (to exercise every alignment relationship). This is our
clean-room `checkasm` equivalent, and it lives in `vaco-simd` so every crate gets it for free.
`vaco-checkasm` (tools) drives the same `KernelSet::kernel_names()` reflection for cycle benchmarking.

### 5.6 Known `fearless_simd` gaps — the authoritative list

**Replaces the old "Known `std::simd` gaps" table.** Established by reading the v0.7.0 source: the
generated `Simd` trait carries 1,453 methods, and the complete operation vocabulary is
`abs`(float only) · `add` · `sub` · `mul` · `div` · `neg` · `min`/`max`(+`_precise`) · `sqrt` ·
`approximate_recip` · `copysign` · `mul_add`/`mul_sub`(+`_precise`, float only) · `floor`/`ceil`/`trunc`/
`round_ties_even`/`fract` · `and`/`or`/`xor`/`not` · `shl`/`shr` (uniform) and `shlv`/`shrv` (per-lane) ·
`simd_eq`/`lt`/`le`/`ge`/`gt` · `select` · `any_true`/`all_true`/`any_false`/`all_false` ·
`to_bitmask`/`from_bitmask` · `widen` · `narrow`/`saturating_narrow`/`relaxed_narrow` · `combine`/`split` ·
`zip_low`/`zip_high` · `unzip_low`/`unzip_high` · `interleave`/`deinterleave` ·
`load_four_interleaved`/`store_four_interleaved` (128-bit only) · `slide`/`slide_within_blocks` ·
`rotate_elements_*`/`shift_elements_*` · `swizzle_dyn`(+`_precise`, +`_within_blocks`) · `splat`/`set`/
`from_fn`/`from_slice`/`store_slice`/`as_array` · `cvt_*` · `to_bytes`/`from_bytes`/`bitcast`.

Anything not in that list does not exist, and — because `kernel!` is closed to us (§5.3 cost 3) — cannot
be reached through to the intrinsic. It must be composed.

#### Genuinely missing, with the composition and its cost

| Missing | Native | Composition (in `vaco_simd::ops`) | Ops | Verdict |
|---|---|---|---|---|
| **unsigned `saturating_add`** | `paddusb` / `uqadd`, 1 | `a.min(!b) + b` | **3** | Cheap. Exact, in-width, no widening. |
| **unsigned `saturating_sub`** | `psubusb` / `uqsub`, 1 | `a.max(b) - b` | **2** | Cheap. |
| **signed `saturating_add`/`sub` (i16)** | `paddsw`, 1 | `widen` → 2× add → `saturating_narrow` | **~5** | Moderate. See "the exposure is smaller than it looks" below. |
| **rounded average** (`pavgb`) | `pavgb` / `urhadd`, 1 | `(a \| b) - ((a ^ b) >> 1)` | **4** | Better than expected. Exact, in-width, no widening, no overflow. The obvious widen-add-shift-narrow route would have cost ~6. |
| **truncating average** | `pavgb`-family, 1 | `(a & b) + ((a ^ b) >> 1)` | **4** | Same. |
| **lanewise abs-diff** | part of `psadbw`, 1 | `a.max(b) - a.min(b)` | **3** | Cheap in isolation; the cost is in the accumulation, below. |
| **integer `abs`** *(not in D12's list — found during this review)* | `pabsw`, 1 | `x.max(zero - x)` | **2** | Cheap. `abs` exists on `SimdFloat` only; there is no integer `abs` and no integer `mul_add`. |
| **horizontal reduce** (sum/min/max) | tree, ~2·log₂N | `log₂N` × (`rotate_elements_left` + `add`), then lane 0 | **2·log₂N + 1** (≈7 at N=8) | Fine *if* hoisted. Keep a vector accumulator across the loop and reduce once per invocation. Only mask reductions (`any_true`/`all_true`) exist natively. |
| **widening multiply** / **`pmaddwd`** | `pmaddwd`, 1 per output vec | `widen`(a), `widen`(b), 2× i32 `mul`, `unzip_low`/`unzip_high` + `add` | **~12** for 2 output vectors ⇒ **~6x** | **The one that hurts.** Avoid rather than pay — see below. |
| **`pmaddubsw`** (u8×i8 pairwise, 2 taps at once) | 1 | widen u8→i16 (hoisted out of the tap loop), then per tap `slide` + `mul` + `add` | **~2.2–2.5x** instruction count on an 8-tap u8 horizontal FIR | Plan 12's largest named risk. Unchanged in kind by D12 — `std::simd` did not have it either — but see the honesty note. |
| **`crc32` / AES-NI** | 1 | none possible | — | Unchanged and uncontained-by-composition. Still handled as plan 12 Risk E: table CRC, and it is not a throughput path. |

#### Three things that turned out better than D12 assumed

- **`saturating_narrow` is native.** D12 already noted this; it is worth repeating because it removes most
  of the apparent signed-saturation exposure. The dominant use of saturating arithmetic in a video codec
  is the reconstruct step `clip(pred + residual)` — an i16 add followed by a narrow to u8 — and the
  narrowing half is exactly where the saturation is needed. Likewise every "pack back to pixels" step.
  The genuinely unavoidable signed `saturating_add` cases are narrower than the gap table suggests.
- **The `pavg` composition is 4 ops, not 6.** The bit identity `(a|b) - ((a^b)>>1)` stays in-width and
  cannot overflow, so motion compensation's bi-prediction averaging never has to widen.
- **`swizzle_dyn` is a first-class, portable operation.** This is a straight *improvement* over
  `std::simd`, where dynamic byte shuffle was listed as an outright gap. It removes the entire
  const-index-array / per-`N`-macro apparatus that plan 12 §2.4 was built around. Caveat: it is
  lane-crossing at 256/512 bits, so cost-sensitive shuffles should use `swizzle_dyn_within_blocks` or
  operate on the 128-bit `Block` type.

#### How to avoid the widening-multiply gap (this is the important paragraph)

`pmaddwd` and `pmaddubsw` compute a *pairwise dot product against a coefficient vector*. That is the
shape hand-written asm reaches for because the instruction exists. **It is not the only shape, and it is
not the one our kernels should be written in.**

With a **broadcast** coefficient — one scalar tap splatted across all lanes, which is what a separable
FIR, a colour matrix and an interpolation filter all actually need — the operation is
`acc += widen(pixel) * splat(c)`, and the widening happens *once per source vector* rather than once per
tap. Hoist the widen out of the tap loop (which is the two-pass i16-intermediate structure plan 12 Risk C
mitigation 2 already prescribes) and the per-tap cost is `slide` + `mul` + `add` = 3 ops per output
vector, against `pmaddubsw` + `paddsw` = 2 ops for *two* taps. That is the ~2.2–2.5x figure in the table,
and it is the floor, not the ceiling — batching (Risk C mitigation 1) and const-generic tap unrolling
(mitigation 3) both attack it further.

`ops::madd_i16_i32` exists for the cases that genuinely need the pairwise shape, and a kernel PR that
calls it must say in review why `ops::wmla_u8_i16` does not fit.

#### One honesty note about what we gave up

Under `std::simd`, several of these gaps were expected to be closed *by LLVM*: `combineAddToPMADDWD`
recovers `pmaddwd` from a widen-mul-adjacent-add pattern, `combineBasicSADPattern` recovers `psadbw`, and
`combineAVG` recovers `pavgb`. Those combines operate on generic IR. `fearless_simd` emits explicit
`core::arch` intrinsics, which LLVM still models as shuffles and arithmetic — so the combines *may* still
fire, but far less reliably, and we no longer get to assume it.

So the trade is real and it is worth naming: **we gave up a set of possible peephole wins and bought a
guaranteed runtime-dispatch win.** For a distributed binary that is not close — under F5 the shipped
artefact could not use AVX-512 at all, and a conservatively-packaged one would have been stuck at v2.
For a `-C target-cpu=native` source build on a machine whose baseline already includes the instruction,
the comparison is genuinely closer, and the honest answer is that we do not know until we measure.
That measurement is item 1 of the adoption checklist in plan 12 §11.

### 5.7 Dependencies

`fearless_simd` 0.7 (D12), `vaco-core`. Dev: `proptest`, `divan`. **No `std::simd`, no nightly feature.**

Gate assessment (D10), recorded per D12:

| Gate | Result |
|---|---|
| **1 — pure Rust, zero FFI** | Pass. Zero required dependencies (only an optional `libm` for `no_std`, which we do not enable). No `-sys`, no `links`, no native `build.rs`. |
| **2 — licence** | Pass. Apache-2.0 OR MIT — an exact match for our own. |
| **3 — trusted & maintained** | Pass. Linebender (Raph Levien). v0.7.0 released 11–12 Aug 2026, v1.0 targeted early September 2026, API stable for ~a year with no breaking changes planned for 1.0. Zero dependencies ⇒ the shallowest possible tree. ~12 direct dependents. Small enough to fork. MSRV 1.89, edition 2024. |

The unsafe tension (D10, §3.4) applies and is stated plainly: **`fearless_simd` contains `unsafe`
internally.** Our guarantee remains "no unsafe in our code", not "no unsafe in the process". What makes
this the right trade rather than a shrug: the unsafe is small, centralised in one crate, written by
people who specialise in exactly this problem, shared with the wider Rust graphics ecosystem, and — the
part that actually matters — it replaces a design in which *we* would have had to write and maintain the
equivalent unsafe ourselves (F5's escalation path), or else ship a binary that could not use half the
hardware. Record the `cargo-geiger` count at adoption and re-check each release.

Candidates not taken: `wide` and `simba` (portable-SIMD wrappers with no runtime dispatch — they solve
the problem `std::simd` already solved, not the one we have), `pulp` (the closest comparable design and
`fearless_simd`'s acknowledged inspiration; re-assess at v1.0 if `fearless_simd`'s gap list proves
fatal — it is the natural fallback substrate and the reason §5.3's adapter boundary is worth its cost),
and `memchr` (fails on capability, not on gates — §3.2).

### 5.8 Test strategy

Unit: `Tier::detect()` returns a level the CPU actually supports on each CI target, `Tier::baseline()`
agrees with `cfg!`, and `Tier::cap()` is a total order consistent with `vector_bytes()`.
**Gap compositions:** every `ops` function in the §5.6 table gets an exhaustive test against a scalar
oracle over the full input domain where that is finite (`saturating_add`/`sub`, `avg_round`/`avg_trunc`,
`abs_diff` on u8 are all 2^16 pairs — exhaustive, not sampled), and a `cargo-show-asm` assertion pinning
its instruction count so a substrate upgrade that adds a native operation is *noticed*.
**Dispatch:** a test that forces each `Tier` in turn via `VACO_TIER` and asserts all tiers agree
bit-exactly with the scalar reference — this is what proves the monomorphisations are equivalent. `cpu_flag_names()` compared against
`ffmpeg -hide_banner -loglevel debug` CPU-flag reporting on the same machine (a genuine differential
check: the reference prints its detected flag set).
Proptest: every kernel via `differential_test!`.
Fuzz: `scan_start_code` and `rbsp_unescape` differentially against `scan::scalar::*` on arbitrary byte
strings — these two are the only `vaco-simd` functions that consume untrusted data directly.
Bench: `divan` benchmarks for every kernel at each tier, with CI regression tracking (D8).

### 5.9 Effort and blocking

**3.5 person-weeks**, revised from 3.0 by D12 (1 for the `Tier`/`dispatch_kernel!`/`KernelSet`/testing
scaffolding — *smaller* than before, because the substrate supplies the dispatch we were going to build;
1.5 for the `ops` module, which is *larger* than before, because it now owns nine gap compositions
(§5.6) each of which needs its own differential test and assembly assertion; 1 for `scan` + benchmarks).
The `ops` half is the schedule risk and should be the first thing benchmarked, per plan 12 §11. Blocks `vaco-bitstream`'s fast start-code scanning,
and all of Layer 3/4 DSP. The scaffolding half (`Tier`, `KernelSet`, `differential_test!`) should land in
week 1 and be frozen — it is what makes the DSP crates parallelisable.

---

## 6. `vaco-opts`

### 6.1 Purpose

The `AVOption`/`AVClass` equivalent, and the single most load-bearing crate in Layer 0: it is what makes
`-h filter=scale`, `scale=w=1280:h=-1:flags=lanczos`, `-movflags +faststart`, `avio` option dictionaries,
`process_command` runtime updates and `av_opt_serialize` round-trips all work through one mechanism.
Every configurable component in the project — demuxer, muxer, decoder, encoder, filter, protocol, scaler,
resampler — declares its options with one derive and gets parsing, validation, serialisation, help output
and runtime mutation for free.

Two crates: `vaco-opts` (runtime, no proc-macro dependency) and `vaco-opts-derive` (proc macro),
re-exported so consumers only name `vaco-opts`.

### 6.2 The type model

The inventory's 21 `AVOptionType` values map to a 20-variant base plus an orthogonal array flag, because
`FLAG_ARRAY` is a modifier in the C model too:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum OptBase {
    Flags,        // u64 bitmask; named consts via `unit`; "+a-b" syntax
    Int,          // i32
    Int64,        // i64
    UInt,         // u32
    UInt64,       // u64
    Double,       // f64
    Float,        // f32
    Bool,         // tri-state at the wire level: "auto" | true | false  (see 6.6)
    String,       // Option<String>
    Rational,     // vaco_core::Rational, "num/den"
    Binary,       // Option<Vec<u8>>, hex-encoded
    Dict,         // vaco_core::Dict, nested "k=v:k=v"
    Const,        // never a field; a named constant belonging to a `unit`
    ImageSize,    // (u32, u32), "1920x1080" | "hd1080"
    PixelFmt,     // vaco_pixfmt::PixelFormat        (impl lives in vaco-pixfmt, F6)
    SampleFmt,    // vaco_sampfmt::SampleFormat      (impl lives in vaco-sampfmt, F6)
    ChLayout,     // vaco_chlayout::ChannelLayout    (impl lives in vaco-chlayout, F6)
    VideoRate,    // vaco_core::Rational, "25" | "ntsc"
    Duration,     // vaco_core::Duration, microseconds
    Color,        // vaco_core::Rgba
}

#[derive(Debug, Clone, Copy)]
pub struct ArrayDesc { pub sep: char, pub min_len: u32, pub max_len: u32 }

#[derive(Debug, Clone, Copy)]
pub struct OptKind { pub base: OptBase, pub array: Option<ArrayDesc> }
```

`OptBase::Const` names the kind for introspection; it is never a struct field. `FLAG_ARRAY` is
`OptKind { base: Flags, array: Some(_) }`.

### 6.3 Public API

```rust
#![forbid(unsafe_code)]

// ---------------- per-context flags ----------------
/// Bit flags classifying which tool/context an option applies to. Reproduced from the inventory
/// because they are the interface fact behind `-h full`'s flag column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OptFlags(u32);

impl OptFlags {
    pub const ENCODING:   Self;  pub const DECODING:  Self;
    pub const AUDIO:      Self;  pub const VIDEO:     Self;   pub const SUBTITLE: Self;
    pub const EXPORT:     Self;  pub const READONLY:  Self;
    pub const BSF:        Self;  pub const RUNTIME:   Self;   pub const FILTERING: Self;
    pub const DEPRECATED: Self;  pub const CHILD_CONSTS: Self;
    #[must_use] pub const fn contains(self, o: Self) -> bool;
    #[must_use] pub const fn union(self, o: Self) -> Self;
    /// The `-h full` flag column, e.g. "..FV.....T." — exact layout pinned by snapshot test (6.9).
    #[must_use] pub fn column(self) -> [u8; 11];
}

// ---------------- descriptors ----------------
#[derive(Debug, Clone, Copy)]
pub struct OptionDesc {
    pub name:    &'static str,
    /// Additional accepted spellings. The inventory's `isr`/`in_sample_rate` pattern.
    pub aliases: &'static [&'static str],
    pub help:    &'static str,
    pub kind:    OptKind,
    pub flags:   OptFlags,
    /// The `unit` grouping named constants under this option (the inventory's §5 mechanism).
    pub unit:    Option<&'static str>,
    /// Named constants in this option's unit. Empty unless `unit` is set.
    pub consts:  &'static [ConstDesc],
    /// For display and `query_ranges` only. The authoritative check is typed — see 6.7.
    pub range:   Option<OptRangeDisplay>,
    /// Rendered default, exactly as `-h full` prints it.
    pub default_repr: &'static str,
    pub id: OptId,
}

#[derive(Debug, Clone, Copy)]
pub struct ConstDesc {
    pub name: &'static str,
    pub help: &'static str,
    pub unit: &'static str,
    /// Constants are i64 or f64 in the C model; we keep both and the schema says which.
    pub value: ConstValue,
    pub flags: OptFlags,
}

#[derive(Debug, Clone, Copy)] pub enum ConstValue { Int(i64), Float(f64) }
#[derive(Debug, Clone, Copy)] pub struct OptRangeDisplay { pub min: f64, pub max: f64 }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)] pub struct OptId(pub u16);

#[derive(Debug, Clone, Copy)]
pub struct Schema {
    pub class_name: &'static str,
    pub options:    &'static [OptionDesc],
    /// Child schemas reachable for option lookup (`AVClass` child iteration).
    pub children:   &'static [&'static Schema],
}

impl Schema {
    pub fn find(&'static self, name: &str) -> Option<&'static OptionDesc>;
    pub fn find_recursive(&'static self, name: &str) -> Option<(&'static Schema, &'static OptionDesc)>;
    pub fn consts_for_unit(&'static self, unit: &str) -> impl Iterator<Item = &'static ConstDesc>;
    /// Iterate in declaration order — positional filter arguments depend on this being stable.
    pub fn iter(&'static self) -> impl Iterator<Item = &'static OptionDesc>;
}

// ---------------- the value trait (F6) ----------------
/// Implemented by every type usable as an option field. Layer-1 crates implement it for their
/// own types; `vaco-opts` never names `PixelFormat`.
pub trait OptValue: core::any::Any + Send + Sync + core::fmt::Debug {
    /// Static kind. Used by the derive to fill in `OptionDesc::kind`.
    const BASE: OptBase where Self: Sized;
    fn base(&self) -> OptBase;
    /// Parse `s` into `self`. `ctx` supplies the named constants of this option's unit,
    /// so enums, flags and int-with-consts all go through one code path.
    fn parse_into(&mut self, s: &str, ctx: &ParseCtx<'_>) -> Result<(), OptError>;
    /// Append the canonical string form. Must round-trip through `parse_into`.
    fn serialize(&self, out: &mut String, ctx: &SerCtx<'_>);
    /// Numeric view for range checking and `query_ranges`; `None` for non-numeric kinds.
    fn as_f64(&self) -> Option<f64> { None }
    fn eq_dyn(&self, other: &dyn OptValue) -> bool;
    fn as_any(&self) -> &dyn core::any::Any;
    fn as_any_mut(&mut self) -> &mut dyn core::any::Any;
}

pub struct ParseCtx<'a> {
    pub consts: &'a [ConstDesc],
    pub unit:   Option<&'a str>,
    pub range:  Option<OptRangeDisplay>,
    pub array:  Option<ArrayDesc>,
}

// ---------------- the object trait ----------------
/// Implemented by the derive. Object-safe, so components can be held as `dyn`.
pub trait Options: core::fmt::Debug {
    fn schema(&self) -> &'static Schema;
    fn slot(&self, id: OptId) -> Option<&dyn OptValue>;
    fn slot_mut(&mut self, id: OptId) -> Option<&mut dyn OptValue>;
    /// Child objects, for recursive option lookup.
    fn children(&self) -> &[&dyn Options] { &[] }
    fn children_mut(&mut self) -> Vec<&mut dyn Options> { Vec::new() }
    /// A fresh instance at defaults, for `is_set_to_default`.
    fn defaults(&self) -> Box<dyn Options>;
}

/// Blanket helpers. Available on every `Options` implementor and on `dyn Options`.
pub trait OptionsExt: Options {
    fn set_str(&mut self, name: &str, value: &str) -> Result<(), OptError>;
    fn get_str(&self, name: &str) -> Result<String, OptError>;
    fn set_typed<T: OptValue + Clone>(&mut self, name: &str, v: T) -> Result<(), OptError>;
    fn get_typed<T: OptValue + Clone>(&self, name: &str) -> Result<T, OptError>;
    /// Parse "k=v:k2=v2", the filter/protocol/muxer argument grammar. Honours `escape::split`
    /// and positional arguments (values before the first `=` map to declaration order).
    fn set_from_string(&mut self, s: &str, kv: &str, pairs: &str) -> Result<(), OptError>;
    /// Apply every entry of a dictionary; unconsumed keys are returned, matching the
    /// `av_opt_set_dict2` contract that demuxers rely on to reject unknown options.
    fn apply_dict(&mut self, d: &Dict) -> Result<Dict, OptError>;
    /// `av_opt_serialize`. Round-trips through `set_from_string`.
    fn serialize(&self, f: SerializeFlags) -> String;
    fn is_set_to_default(&self, name: &str) -> Result<bool, OptError>;
    fn query_ranges(&self, name: &str) -> Result<Vec<OptRangeDisplay>, OptError>;
    /// Runtime mutation gate: rejects options without `OptFlags::RUNTIME`.
    fn process_command(&mut self, name: &str, value: &str) -> Result<(), OptError>;
}
impl<T: Options + ?Sized> OptionsExt for T {}

#[derive(Debug, Clone, Copy, Default)]
pub struct SerializeFlags { pub skip_defaults: bool, pub skip_deprecated: bool, pub only: OptFlags }

#[derive(Debug, thiserror::Error)]
pub enum OptError {
    #[error("Option not found: {name}")]                       NotFound { name: String },
    #[error("Invalid value for {name}: {value}")]              InvalidValue { name: String, value: String },
    #[error("Value {value} for {name} out of range {min}..{max}")]
                                                               OutOfRange { name: String, value: f64, min: f64, max: f64 },
    #[error("Option {name} is read-only")]                     ReadOnly { name: String },
    #[error("Option {name} is not settable at runtime")]       NotRuntime { name: String },
    #[error("Unknown constant '{value}' for option {name}")]   UnknownConst { name: String, value: String },
    #[error("Array for {name} has {len} elements, expected {min}..{max}")]
                                                               ArrayLen { name: String, len: u32, min: u32, max: u32 },
    #[error("Too many positional arguments for {class}")]      TooManyPositional { class: &'static str },
}
```

### 6.4 Derive input — the worked example

The `SwrContext` option table from inventory §10, which exercises aliases, units, ranges, bools, floats,
enums, flags, `sample_fmt` and `chlayout` in one place:

```rust
use vaco_opts::{Options, OptEnum, opt_flags};
use vaco_core::Rational;
use vaco_sampfmt::SampleFormat;
use vaco_chlayout::ChannelLayout;

opt_flags! {
    /// `flags`/`swr_flags`, unit "swr_flags".
    #[unit = "swr_flags"]
    pub struct SwrFlags: u64 {
        /// force resampling even when the rates match
        const RES = 1 << 0 => "res";
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, OptEnum)]
#[opt_enum(unit = "dither_method", base = "int")]
pub enum DitherMethod {
    #[opt_const(name = "none",              help = "no dithering")]                  None,
    #[opt_const(name = "rectangular",       help = "rectangular dither")]            Rectangular,
    #[opt_const(name = "triangular",        help = "triangular dither")]             Triangular,
    #[opt_const(name = "triangular_hp",     help = "triangular dither with highpass")] TriangularHp,
    #[opt_const(name = "lipshitz",          help = "Lipshitz noise shaping")]        NsLipshitz,
    #[opt_const(name = "shibata",           help = "Shibata noise shaping")]         NsShibata,
    // …
}

#[derive(Debug, Clone, Options)]
#[options(name = "SwrContext", help = "audio resampling and rematrixing")]
pub struct ResampleOptions {
    #[opt(name = "isr", alias = "in_sample_rate", help = "input sample rate",
          default = 0, range = 0..=i32::MAX, flags(audio, param))]
    pub in_sample_rate: i32,

    #[opt(name = "osr", alias = "out_sample_rate", help = "output sample rate",
          default = 0, range = 0..=i32::MAX, flags(audio, param))]
    pub out_sample_rate: i32,

    #[opt(name = "isf", alias = "in_sample_fmt", help = "input sample format",
          default = SampleFormat::None, flags(audio, param))]
    pub in_sample_fmt: SampleFormat,

    #[opt(name = "ichl", alias = "in_chlayout", help = "input channel layout",
          default = ChannelLayout::UNSPEC, flags(audio, param))]
    pub in_chlayout: ChannelLayout,

    #[opt(name = "clev", alias = "center_mix_level", help = "center mix level",
          default = 0.707_106_78_f32, range = -32.0..=32.0, flags(audio, param))]
    pub center_mix_level: f32,

    #[opt(name = "flags", alias = "swr_flags", help = "engine flags",
          unit = "swr_flags", default = SwrFlags::empty(), flags(audio, param))]
    pub flags: SwrFlags,

    #[opt(name = "dither_method", help = "dither method",
          unit = "dither_method", default = DitherMethod::None, flags(audio, param))]
    pub dither_method: DitherMethod,

    #[opt(name = "phase_shift", help = "resampler phase shift",
          default = 10, range = 0..=24, flags(audio, param))]
    pub phase_shift: i32,

    #[opt(name = "linear_interp", help = "interpolate between filter phases",
          default = true, flags(audio, param))]
    pub linear_interp: bool,

    #[opt(name = "cutoff", alias = "resample_cutoff", help = "cutoff frequency ratio",
          default = 0.0, range = 0.0..=1.0, flags(audio, param))]
    pub cutoff: f64,

    #[opt(name = "first_pts", help = "assumed first PTS, in samples",
          default = None, flags(audio, param))]
    pub first_pts: Option<i64>,

    /// Array-valued: an explicit channel remap, "0|1|2|3".
    #[opt(name = "channel_map", help = "explicit input->output channel map",
          array(sep = '|', max_len = 64), flags(audio, param))]
    pub channel_map: Vec<i32>,

    /// A child object: its options are reachable by name from this one.
    #[opt(child)]
    pub dither: DitherOptions,

    /// Not an option; skipped entirely.
    #[opt(skip)]
    pub cached_matrix: Option<Vec<f32>>,
}
```

Attribute grammar, complete:

| Attribute | Meaning |
|---|---|
| `#[options(name, help)]` | Struct-level: the class name and help string. |
| `name = "…"` | Primary option name. Defaults to the field name if omitted. |
| `alias = "…"` (repeatable) | Additional accepted spellings. |
| `help = "…"` | Help text. Required — `missing_docs`-style enforcement at macro level. |
| `default = <expr>` | Const expression of the field type. Also generates `impl Default`. |
| `range = <a>..=<b>` | Typed inclusive range. Emits both the typed check and the display pair. |
| `unit = "…"` | Groups named constants; makes the option accept const names. |
| `flags(a, b, …)` | Any of `encoding, decoding, audio, video, subtitle, export, readonly, bsf, runtime, filtering, deprecated, child_consts`, plus the shorthand `param` = encoding+decoding. |
| `array(sep, min_len, max_len)` | Marks the field a `Vec<T>` array option. |
| `child` | The field is itself an `Options`; its schema is a child. |
| `skip` | Not an option. |

`Option<T>` fields are supported for every kind and mean "unset"; F4 makes this the idiomatic way to
express FFmpeg's magic `-1`/`INT_MIN` defaults, and the serialiser omits them under `skip_defaults`.

### 6.5 Generated shape

For the struct above, `#[derive(Options)]` emits (abridged, but structurally exact):

```rust
const _: () = {
    use vaco_opts::{__rt as rt, OptId, OptKind, OptBase, OptFlags, OptionDesc, Schema, OptValue};

    static OPTS: &[OptionDesc] = &[
        OptionDesc {
            name: "isr", aliases: &["in_sample_rate"], help: "input sample rate",
            kind: OptKind { base: <i32 as OptValue>::BASE, array: None },
            flags: OptFlags::AUDIO.union(OptFlags::ENCODING).union(OptFlags::DECODING),
            unit: None, consts: &[],
            range: Some(rt::range_display(0f64, i32::MAX as f64)),
            default_repr: "0",
            id: OptId(0),
        },
        // …
        OptionDesc {
            name: "dither_method", aliases: &[], help: "dither method",
            kind: OptKind { base: OptBase::Int, array: None },
            flags: OptFlags::AUDIO.union(OptFlags::ENCODING).union(OptFlags::DECODING),
            unit: Some("dither_method"),
            consts: <DitherMethod as vaco_opts::OptEnumConsts>::CONSTS,   // contributed by #[derive(OptEnum)]
            range: None, default_repr: "none", id: OptId(6),
        },
        OptionDesc {
            name: "channel_map", aliases: &[], help: "explicit input->output channel map",
            kind: OptKind { base: OptBase::Int, array: Some(rt::array(b'|', 0, 64)) },
            flags: /* … */, unit: None, consts: &[], range: None,
            default_repr: "", id: OptId(11),
        },
    ];

    static SCHEMA: Schema = Schema {
        class_name: "SwrContext",
        options: OPTS,
        children: &[<DitherOptions as rt::HasSchema>::SCHEMA],
    };

    impl rt::HasSchema for ResampleOptions { const SCHEMA: &'static Schema = &SCHEMA; }

    impl vaco_opts::Options for ResampleOptions {
        fn schema(&self) -> &'static Schema { &SCHEMA }

        fn slot(&self, id: OptId) -> Option<&dyn OptValue> {
            Some(match id.0 {
                0  => &self.in_sample_rate,
                1  => &self.out_sample_rate,
                2  => &self.in_sample_fmt,
                3  => &self.in_chlayout,
                4  => &self.center_mix_level,
                5  => &self.flags,
                6  => &self.dither_method,
                7  => &self.phase_shift,
                8  => &self.linear_interp,
                9  => &self.cutoff,
                10 => &self.first_pts,
                11 => &self.channel_map,
                _  => return None,
            })
        }

        fn slot_mut(&mut self, id: OptId) -> Option<&mut dyn OptValue> {
            Some(match id.0 { 0 => &mut self.in_sample_rate, /* … */ _ => return None })
        }

        fn children(&self)         -> &[&dyn vaco_opts::Options] { /* … */ }
        fn children_mut(&mut self) -> Vec<&mut dyn vaco_opts::Options> { vec![&mut self.dither] }
        fn defaults(&self) -> Box<dyn vaco_opts::Options> { Box::new(<Self as Default>::default()) }
    }

    impl Default for ResampleOptions {
        fn default() -> Self {
            Self {
                in_sample_rate: 0,
                in_sample_fmt: SampleFormat::None,
                center_mix_level: 0.707_106_78_f32,
                flags: SwrFlags::empty(),
                dither_method: DitherMethod::None,
                phase_shift: 10,
                linear_interp: true,
                cutoff: 0.0,
                first_pts: None,
                channel_map: Vec::new(),
                dither: Default::default(),
                cached_matrix: None,
            }
        }
    }

    // Typed range enforcement lives in a generated post-parse hook, not in the descriptor,
    // so i64 options above 2^53 are checked exactly (see 6.7).
    impl rt::RangeCheck for ResampleOptions {
        fn check(&self, id: OptId) -> Result<(), vaco_opts::OptError> {
            match id.0 {
                0 => rt::check_range(self.in_sample_rate, 0, i32::MAX, "isr"),
                4 => rt::check_range_f(self.center_mix_level, -32.0, 32.0, "clev"),
                7 => rt::check_range(self.phase_shift, 0, 24, "phase_shift"),
                9 => rt::check_range_f(self.cutoff, 0.0, 1.0, "cutoff"),
                _ => Ok(()),
            }
        }
    }
};
```

Note what is *not* generated: no parsing code, no serialisation code, no help formatting. Those live once
in `vaco-opts` and operate on `&mut dyn OptValue` + `&OptionDesc`. The macro's entire job is to project
struct fields into an indexed, type-erased accessor and to lift attributes into a static table. That keeps
the macro small (~1 200 lines including the attribute parser) and puts all the interesting logic in
ordinary, unit-testable, non-macro code.

`#[derive(OptEnum)]` emits `impl OptEnumConsts { const CONSTS: &'static [ConstDesc] }`, `impl OptValue`
(parse = look the string up in `ctx.consts` then map the `i64` back to a variant; serialize = the reverse),
and `TryFrom<i64>`. `opt_flags!` emits the newtype, `const` members, `empty/contains/union/difference`, and
the `OptValue` impl implementing FFmpeg's `+flag-flag` accumulate/remove syntax over `ctx.consts`.

### 6.6 Design decisions and their rationale

**Type erasure by `&mut dyn OptValue`, not by field offsets.** FFmpeg computes `(char*)obj + offset` and
casts to the type the table claims. That is the pattern safe Rust most obviously cannot have, and a naive
port reaches for `unsafe`. Generating an indexed `match` that returns a trait object costs one jump table
and gives us the same generic machinery with the type mismatch made impossible. The typed fast path
(`set_typed::<i32>`) recovers full speed via `Any::downcast_mut`, and option setting is a
configuration-time operation anyway.

**Ranges are checked against the typed value; `f64` is display-only.** FFmpeg stores `min`/`max` as
`double`, which silently loses precision above 2^53 — a real bug class for `int64` options like
`first_pts` or muxer byte limits. We emit both: an exact typed check in `RangeCheck`, and an
`f64` pair used solely to render `-h full`'s `(from … to …)` text identically. Where the f64 rendering
would differ from ours, the snapshot test (6.9) makes the divergence explicit rather than silent.

**Named constants are a property of the *unit*, not of the option.** `consts_for_unit` scans the schema's
options for `ConstDesc`s carrying that unit name, exactly reproducing the C mechanism's shape: several
options can share one unit (`scaler`/`scaler_sub` both use `sws_scaler`), and `-h` groups the constants
under each option that references it. The derive resolves this at compile time by pointing every option
with `unit = "x"` at the same `CONSTS` slice.

**Bool is tri-state at the wire, binary in the field.** The CLI accepts `true/false/1/0/on/off/auto`, and
`auto` is genuinely distinct for options like `src_range`. Rule: a field typed `bool` accepts the boolean
spellings only; a field typed `Option<bool>` additionally accepts `auto` and serialises `None` as `auto`.
This makes the two cases distinguishable in the type system rather than by a `-1` convention.

**Positional arguments come from declaration order.** The filtergraph grammar allows
`scale=1280:720` (positional) mixed with `scale=1280:720:flags=bicubic`. `set_from_string` walks
`schema.iter()` in declaration order for positional values and switches to name lookup at the first `=`,
erroring on any positional value after a named one. The derive therefore must preserve field order — it
does, and a `#[test]` asserts it.

**Serialisation is defined by round-trip, not by inspection.** `serialize` and `set_from_string` are
specified as inverse functions and tested that way (6.9). Escaping goes through `vaco_core::escape` with
the option-value special set (`:` `'` `\`), so the three-tier scheme is implemented once.

**No `serde`.** Options are configured from CLI strings and dictionaries, not from JSON, and the
serialisation format is the FFmpeg option grammar, which `serde` would not produce. Adding `serde` would
mean maintaining a second, divergent representation of the same schema.

### 6.7 Help output

`-h filter=x`, `-h encoder=y` and `-h full` are CLI-compatibility surfaces (D5, D9's "interface names are
implementable; text is not"). `vaco-opts` provides the *data*; `vaco-cli-core` provides the layout:

```rust
pub struct HelpEntry<'a> {
    pub name: &'a str, pub kind: OptKind, pub flags_column: [u8; 11],
    pub help: &'a str, pub default_repr: &'a str, pub range: Option<OptRangeDisplay>,
    pub consts: &'a [ConstDesc],
}
pub fn help_entries(schema: &'static Schema, filter: OptFlags) -> Vec<HelpEntry<'static>>;
```

Help *strings* are ours, written fresh — D9 is explicit that option-table prose is FFmpeg's expression and
may not be reproduced. Option *names*, types, ranges, defaults and the flag column are interface facts and
must match.

### 6.8 Dependencies

`vaco-core`, `thiserror`; `vaco-opts-derive` depends on `syn`/`quote`/`proc-macro2`.
Dev: `proptest`, `insta`.

Gate assessment (D10): the derive's dependencies are tooling and clear all three gates trivially. `darling`
was considered for the attribute grammar and rejected on Gate 3 (shallow) and model grounds: it would add a
tree for something we need full control over — our grammar has repeatable `alias`, a `flags(...)` list, a
typed `range` expression and a `default` const expression, and hand-parsing with `syn` is about 400 lines
we would rather own than bend `darling` around. No media-specific capability is at stake anywhere in this
crate.

### 6.9 Test strategy

| Kind | Content |
|---|---|
| Unit | `trybuild`-style compile-fail cases for every attribute error (missing `help`, `range` on a `String`, `unit` on a `Color`, duplicate names, `array` on a non-`Vec`). Each error message asserted. |
| Unit | Parsing of every one of the 20 bases: valid, boundary, invalid, and empty input. Flags `+a-b+c` accumulation. Const-name lookup including case sensitivity. |
| Proptest | **Round-trip:** for an arbitrary instance of a test struct covering all 20 bases plus arrays, `set_from_string(serialize(x)) == x`. This is the crate's central property and it must hold with `skip_defaults` both on and off. |
| Proptest | **Range invariance:** any value accepted by `set_str` satisfies `RangeCheck`; any value rejected leaves the object unmodified (no partial application). |
| Proptest | **Dict application:** `apply_dict` consumes exactly the keys that `schema.find_recursive` resolves, and returns the rest unchanged. |
| Fuzz | `opts_set_from_string` over a reference struct exercising all kinds — assert no panic, bounded allocation, and that a successful parse always round-trips. `opts_flags_parse` on the `+/-` grammar. |
| Snapshot (`insta`) | The rendered schema of each of our option-carrying types, so a schema change shows up as a reviewable diff. |
| **Differential** | **The highest-value oracle in Layer 0.** `ffmpeg -h full` prints, for every class, every option's name, type, default, range, flag column, and every named constant grouped by unit. A harness parses that output into a normalised form, does the same for our schemas, and diffs. This validates the *entire* option model — types, ranges, defaults, units, flags — against the reference in one shot, per component, for the life of the project. Same technique against `-h filter=…`, `-h encoder=…`, `-h demuxer=…`. Build this harness in week 1; it is what tells every later crate whether its option table is right. |
| **Differential** | Round-trip against the reference's own parser: feed `scale=<serialised>` to `ffmpeg -filter_complex` and assert it is accepted, catching escaping divergences that our internal round-trip cannot see. |

### 6.10 Effort and blocking

**5 person-weeks**, the largest item in the layer: 2.5 for the derive (attribute parser, code generation,
compile-fail tests), 1.5 for the runtime (parse/serialise/apply for 20 bases plus arrays and children),
1 for the `-h full` differential harness. Blocks `vaco-cli-core` and every component crate in Layers 3–5 —
literally every one of them declares options. Ship the trait and descriptor types in week 1 as a stub so
component authors can write their option tables against the final shape while the macro is finished.

---

## 7. `vaco-expr`

### 7.1 Purpose

The `eval` mini-language from inventory §9: parser plus evaluator for the expression DSL used by filter
options (`crop=w=iw/2`), `-force_key_frames`, timeline `enable=`, `geq`, `aeval`, `volume`, `select` and
roughly a hundred other places. Pure computation over `f64` — no I/O, no allocation during evaluation.

### 7.2 F12 — Compile to a flat stack machine

Options: (a) an AST of `Box<Expr>` walked recursively at each evaluation; (b) a closure tree of
`Box<dyn Fn(&Ctx) -> f64>`; (c) compilation to a flat instruction vector executed by a small stack VM.
Expressions are evaluated per *frame* in the common case and per *sample* in `aeval` — 48 000 evaluations
per second per channel — so pointer-chasing through a heap-allocated tree and an indirect call per node
both cost real time.

**Chosen: (c).** A `Vec<Op>` is linear in memory, needs zero allocation per evaluation, constant-folds
trivially at compile time, is `Send + Sync` and cheap to clone per worker thread, and gives us a natural
place to hang a fuel counter that bounds `while` (see 7.6). The interpreter loop is ~120 lines.

### 7.3 Grammar

Precedence, lowest to highest — the standard arithmetic ladder, with `;`/`,` sequencing at the bottom
returning the last value, matching the inventory's `e_last`:

```
expr    := seq
seq     := add ( (';' | ',') add )*          // value is the last operand
add     := mul ( ('+' | '-') mul )*
mul     := pow ( ('*' | '/') pow )*
pow     := unary ( '^' pow )?                // right-associative
unary   := ('-' | '+')? primary
primary := NUMBER | IDENT | IDENT '(' args ')' | '(' expr ')'
args    := expr ( ',' expr )*                // note: ',' inside a call is an argument separator
NUMBER  := digits [ '.' digits ] [ ('e'|'E') sign digits ] [ suffix ]
suffix  := SI prefix and byte suffixes: k K M G B  (and the 'i' binary variant)
```

The `,`-is-both-a-separator-and-a-sequencing-operator ambiguity is resolved the way every real
implementation resolves it: inside a parenthesised argument list, `,` separates arguments; elsewhere it
sequences. The parser tracks call depth. This is exactly the kind of behaviour to pin with the
differential oracle in 7.8 rather than to guess at.

Names resolve in this order: user-declared variables → user-declared functions → built-in constants
(`PI`, `E`, plus `PHI` if the reference accepts it — verify) → built-in functions. Unknown identifiers are
a *parse* error, not a runtime NaN, so filter option typos are caught at graph construction.

Built-ins, complete, from inventory §9:
`sinh cosh tanh sin cos tan atan asin acos exp log abs` (unary math);
`squish gauss mod max min eq gte gt lte lt ld isnan isinf st while taylor root floor ceil trunc sqrt not
pow print random hypot if ifnot bitand bitor between clip` (special forms and n-ary functions).

### 7.4 Public API

```rust
#![forbid(unsafe_code)]

/// Names a compile-time-resolved set of variables and callbacks. Filters declare one of these
/// as a `static`, so name resolution happens once per graph, never per frame.
#[derive(Debug, Clone, Copy)]
pub struct Schema {
    /// Variable names, in slot order. `vars[i]` at eval time corresponds to `names[i]`.
    pub vars:   &'static [&'static str],
    /// One-argument callbacks the host provides, in id order.
    pub funcs1: &'static [&'static str],
    /// Two-argument callbacks.
    pub funcs2: &'static [&'static str],
}

#[derive(Debug, Clone)]
pub struct Expr { ops: Box<[Op]>, stack_depth: u16, uses_slots: bool, constant: Option<f64> }

impl Expr {
    pub fn compile(src: &str, schema: &Schema) -> Result<Self, ParseError>;
    /// True when the expression folded to a literal — the common case for sizing options
    /// like `w=iw/2` once `iw` is known, and worth short-circuiting.
    #[must_use] pub const fn as_constant(&self) -> Option<f64>;
    /// Evaluate. `vars` must be at least `schema.vars.len()` long; extra entries are ignored.
    pub fn eval<F: Funcs>(&self, vars: &[f64], st: &mut EvalState, funcs: &mut F) -> f64;
    /// Convenience for the no-callback case.
    pub fn eval_simple(&self, vars: &[f64], st: &mut EvalState) -> f64 {
        self.eval(vars, st, &mut NoFuncs)
    }
    /// Which variable slots the expression actually reads — lets a filter skip recomputing
    /// variables nothing references.
    #[must_use] pub fn var_mask(&self) -> u64;
    /// Source text, for `-h` and error messages.
    #[must_use] pub fn source(&self) -> &str;
}

/// Mutable per-evaluation state: the 10 st/ld slots, the PRNG, and the loop fuel.
#[derive(Debug, Clone)]
pub struct EvalState { slots: [f64; 10], rng: Lfg, fuel: u32, log: LogSink }

impl EvalState {
    #[must_use] pub fn new() -> Self;               // fuel = DEFAULT_FUEL
    #[must_use] pub fn with_fuel(fuel: u32) -> Self;
    #[must_use] pub const fn slot(&self, i: usize) -> f64;
    pub fn set_slot(&mut self, i: usize, v: f64);
    /// Slots persist across evaluations by design — `st()` in one frame is visible in the next,
    /// which several filters rely on. `reset_slots` is explicit.
    pub fn reset_slots(&mut self);
    #[must_use] pub const fn exhausted(&self) -> bool;
}

pub trait Funcs {
    fn call1(&mut self, id: u16, a: f64) -> f64;
    fn call2(&mut self, id: u16, a: f64, b: f64) -> f64;
}
#[derive(Debug, Default)] pub struct NoFuncs;
impl Funcs for NoFuncs { /* unreachable ids; returns NaN */ }

#[derive(Debug, thiserror::Error)]
#[error("{source_text}: {kind} at byte {offset}")]
pub struct ParseError { pub kind: ParseErrorKind, pub offset: usize, pub source_text: String }
```

Usage from a filter — the pattern every filter follows:

```rust
static CROP_SCHEMA: Schema = Schema {
    vars: &["in_w","iw","in_h","ih","out_w","ow","out_h","oh","x","y","a","sar","dar","hsub","vsub","n","t","pos"],
    funcs1: &[], funcs2: &[],
};

struct Crop { w: Expr, h: Expr, x: Expr, y: Expr, vars: [f64; 18], st: EvalState }

impl Crop {
    fn configure(opts: &CropOptions) -> Result<Self> {
        Ok(Self { w: Expr::compile(&opts.w, &CROP_SCHEMA)?, /* … */ st: EvalState::new(), vars: [0.0; 18] })
    }
    fn per_frame(&mut self, f: &Frame) {
        self.vars[15] = f.frame_number as f64;   // n
        self.vars[16] = f.pts_seconds();          // t
        let w = self.w.eval_simple(&self.vars, &mut self.st);
        // …
    }
}
```

### 7.5 The instruction set

```rust
#[derive(Debug, Clone, Copy)]
enum Op {
    Const(f64),
    LoadVar(u16),          // vars[i]
    LoadSlot,              // ld(idx): pops idx
    StoreSlot,             // st(idx, v): pops v, idx; pushes v
    Unary(UnaryFn),        // sin, cos, floor, not, isnan, …  (one table lookup, one call)
    Binary(BinaryFn),      // + - * / ^ mod max min eq gt lt gte lte bitand bitor hypot pow
    Ternary(TernaryFn),    // between, clip
    Call1(u16), Call2(u16),// host callbacks
    Random,                // random(idx)
    Print,                 // print(x [,level]) — side effect, returns x
    Jump(u32),             // absolute index
    JumpIfZero(u32),       // pops; jumps when the value is 0 or NaN
    Pop,
    /// `while(cond, body)` compiles to Jump/JumpIfZero; `taylor` and `root` need a re-runnable
    /// subprogram, so they carry a byte range into the same `ops` array.
    Taylor { body: Range<u32>, id: u8 },
    Root   { body: Range<u32>, id: u8 },
}
```

`if`/`ifnot`/`while` are compiled to jumps — they are lazy by construction, so `st()` side effects in an
untaken branch do not happen. `taylor(expr, x[, id])` and `root(expr, max)` need the *same* subexpression
evaluated repeatedly with a slot varying, so they carry a range into the shared instruction array and the
interpreter re-enters at that range; this keeps everything in one allocation. `Range<u32>` in an `Op`
makes the enum 16 bytes, which is the size we budget for.

Compile-time passes, in order: (1) parse to a temporary AST — a tree is the right shape for parsing, wrong
for evaluating; (2) constant-fold bottom-up, including `PI*2` and any pure call on literals; (3) emit ops;
(4) compute maximum stack depth (a static property; the interpreter uses a fixed `[f64; N]` array, no
`Vec`); (5) if the whole program is one `Const`, record it in `Expr::constant`.

### 7.6 Non-termination is bounded

D6 lists hangs as a fuzz finding. FFmpeg's `while` can loop forever on a hostile expression, and
expressions come from the command line and from filtergraph strings that may be user-supplied in a server
context. `EvalState::fuel` is decremented on every backward jump and every `taylor`/`root` iteration; at
zero the evaluation returns NaN and sets `exhausted()`. Default fuel is 1 000 000, tunable per host.
This is a deliberate, documented divergence from the reference and gets an allowlist entry in the
differential harness (D6 permits explicitly reviewed divergences).

### 7.7 Dependencies

`vaco-core`, `thiserror`. Dev: `proptest`.

Gate assessment (D10): `meval`, `evalexpr` and `fasteval` all exist and clear Gates 1 and 2. All three fail
the model test: we need *this* grammar, with this operator set, this `st`/`ld` slot semantics, this
`taylor`/`root` behaviour and this `,`-ambiguity resolution, because the expressions are user-visible
command lines that must evaluate identically to the reference. Adapting a general expression crate to match
a specific dialect exactly is more work than writing 900 lines of parser and interpreter, and would leave
the fidelity question permanently open. Write our own.

### 7.8 Test strategy

| Kind | Content |
|---|---|
| Unit | Precedence and associativity table (`2^3^2 == 512`, `-2^2`, `1-2-3`). Every built-in on representative inputs including NaN/inf. `st`/`ld` persistence across evaluations. Lazy `if` verified by a slot side effect in the untaken branch. |
| Proptest | **Fold equivalence:** for a randomly generated expression tree with only literals, the constant-folded program and the unfolded program produce bit-identical results. **Determinism:** the same expression + same vars + same state yields the same value. **Fuel safety:** any generated program terminates within fuel. |
| Fuzz | `expr_parse` on arbitrary UTF-8 — no panic, bounded allocation, bounded parse time (deeply nested parentheses must hit a depth limit, not a stack overflow: the parser is iterative with an explicit depth counter, capped at 128). `expr_eval` on arbitrary programs — no panic, always terminates. |
| **Differential** | **The oracle is excellent and should be built first.** `ffmpeg -f lavfi -i "aevalsrc='<EXPR>':s=48000:d=1" -f f64le -` emits, as raw samples, the value of an arbitrary expression evaluated with `n` and `t` bound — that is a numeric ground truth for any expression we can generate. A proptest generator emits syntactically valid expressions from our own grammar, runs both, and asserts bit-equality (or ULP-bounded equality for transcendentals, with the tolerance recorded). This pins the `,` ambiguity, the laziness of `if`, `taylor`'s exact iteration scheme, `root`'s convergence, and `random`'s sequence — all things we would otherwise be guessing at. Secondary oracles: `-vf geq` for 2-D variables, `-vf "select='<EXPR>'"` for frame-selection semantics. |

### 7.9 Effort and blocking

**2.5 person-weeks** (1 parser + folding, 0.75 interpreter, 0.75 the differential generator and
semantics-pinning work). Blocks `vaco-filter-core` (timeline `enable=`), most video filters, and
`-force_key_frames`. Not on the v0.1 critical path — D5's milestone has zero filters — so it can run in
parallel with, or after, the Layer 1 data-model crates. Build the differential oracle early anyway: it is
the artefact that makes the semantics knowable.

---

## 8. `vaco-bitstream`

### 8.1 Purpose

Bit and byte readers/writers, Exp-Golomb, start-code scanning and RBSP handling. Every codec parser and
every container that carries codec-level syntax sits on this crate, so its per-read cost is multiplied by
roughly the number of syntax elements in the world.

### 8.2 F13 — Sticky overrun, not `Result` per read

Options: (a) every read returns `Result<u32, Overrun>`; (b) reads return a value and set a sticky flag,
checked once per syntax structure. (a) makes a 40-line SPS parser into 40 `?`-laden lines, defeats
inlining, and produces a branch per read that the CPU must predict anyway.

**Chosen: (b).** Past the end, reads return zeros — the same deterministic value FFmpeg's zero padding
produces, so parsers behave identically — and set `overrun`. The parser checks once, at the end of the
structure, via `reader.finish()?` or `reader.check()?`. The property this preserves is the one that
matters: **a truncated or malformed bitstream can never panic and never reads out of bounds**; it produces
zeros and a flag. `try_get` exists for the rare site that must branch immediately (a length prefix used to
size an allocation, say).

### 8.3 F9 — The checked-tail / unchecked-body split, and how padding is recovered safely

FFmpeg guarantees `AV_INPUT_BUFFER_PADDING_SIZE = 64` zero bytes past every bitstream buffer so a 64-bit
refill can load 8 bytes without checking whether 8 bytes remain. We cannot over-read an allocation. But we
can do something better: **put the padding inside the allocation and slice it.**

Two reader constructions, one implementation:

```rust
/// A byte slice whose backing allocation is known to contain at least `PAD` zero bytes after
/// `logical_len`. Constructible only by crates that allocate with that guarantee — `vaco-pool`
/// and `vaco-packet` — so the invariant is established once, at allocation, not asserted per use.
#[derive(Debug, Clone, Copy)]
pub struct Padded<'a> { bytes: &'a [u8], logical_len: usize }

impl<'a> Padded<'a> {
    pub const PAD: usize = 64;
    /// The only public constructor: copies into a fresh padded buffer. Costs a memcpy, which is
    /// why the pool/packet path (which never copies) exists.
    #[must_use] pub fn from_slice_copying(src: &[u8], scratch: &'a mut Vec<u8>) -> Self;
    #[must_use] pub const fn logical_len(&self) -> usize;
    #[must_use] pub const fn as_bytes(&self) -> &'a [u8];   // includes the padding
}
```

`BitReader::new_padded(p: Padded<'_>)` sets `body_end = p.logical_len` and knows that `bytes[i..i+8]` is
in bounds for every `i <= logical_len`, because the slice really is that long. The refill is then an
unconditional 8-byte load with a bounds check that LLVM removes given the loop invariant — **the C fast
path, with no unsafe, because the padding is real memory we own rather than slop we hope is mapped.**

`BitReader::new(&[u8])` handles the unpadded case (a borrowed mmap, a slice from a caller we do not
control) by splitting once at construction:

```
body   = 0 .. len.saturating_sub(8)     // refill: one compare, one 8-byte load
tail   = len.saturating_sub(8) .. len   // refill: byte-at-a-time, zero-filling past `len`
```

The state machine is a single `if self.pos <= self.body_end` in `refill`, which is perfectly predicted
until the very last refill of the buffer.

**Cost versus C, quantified.** A refill supplies up to 57 usable bits, i.e. roughly 4–8 syntax elements for
typical CAVLC/Exp-Golomb syntax and 1 element for a 32-bit field. Per refill we pay: one compare + one
predictable branch on the unpadded path, and one bounds compare (usually hoisted) on the padded path. Call
it 0–1 extra cycles per refill against C's zero. On a bitstream-reading-bound workload — a header-only
parse, which is exactly D5's v0.1 milestone — expect **1–3%**. On full decode, where bit reading is a
single-digit percentage of total time, expect **well under 1%**, and the padded path plausibly reaches
parity. This is a price worth paying and it should be *measured* on the v0.1 ffprobe workload, not
assumed; `divan` benchmarks comparing padded and unpadded readers on the same data are part of the
deliverable.

### 8.4 Public API — reader

```rust
#![forbid(unsafe_code)]

/// MSB-first bit reader with a 64-bit cache. The universal shape for video bitstreams.
#[derive(Debug, Clone)]
pub struct BitReader<'a> {
    data: &'a [u8],
    logical_len: usize,     // bits beyond this read as zero and set `overrun`
    pos: usize,             // byte position of the next refill
    cache: u64,             // MSB-aligned
    cache_bits: u32,        // 0..=64
    body_end: usize,        // pos <= body_end ⇒ an 8-byte load is in bounds
    overrun: bool,
}

impl<'a> BitReader<'a> {
    #[must_use] pub fn new(data: &'a [u8]) -> Self;
    #[must_use] pub fn new_padded(p: Padded<'a>) -> Self;

    #[inline] pub fn get_bit(&mut self) -> u32;
    /// `n <= 32`. Debug-asserts on larger; release clamps. Never panics.
    #[inline] pub fn get(&mut self, n: u32) -> u32;
    #[inline] pub fn peek(&mut self, n: u32) -> u32;
    #[inline] pub fn skip(&mut self, n: u32);
    #[inline] pub fn get_long(&mut self, n: u32) -> u64;          // n <= 64
    #[inline] pub fn get_signed(&mut self, n: u32) -> i32;        // two's complement, n bits
    /// Immediate-error variant for the sites that must not proceed on truncation.
    pub fn try_get(&mut self, n: u32) -> Result<u32, BitstreamError>;

    #[inline] pub fn align(&mut self);
    /// Remaining readable bits; 0 once overrun.
    #[must_use] pub fn bits_left(&self) -> u64;
    #[must_use] pub fn bit_pos(&self) -> u64;
    #[must_use] pub const fn overrun(&self) -> bool;
    /// Check-and-clear; the standard "end of this syntax structure" call.
    pub fn check(&mut self) -> Result<(), BitstreamError>;
    pub fn finish(self) -> Result<(), BitstreamError>;

    /// Cheap save/restore for speculative parsing (e.g. trying both interpretations of a header).
    #[must_use] pub const fn mark(&self) -> Mark;
    pub fn restore(&mut self, m: Mark);

    /// Read the remaining bytes from an aligned position without copying.
    pub fn remaining_bytes(&self) -> &'a [u8];
}

/// Exp-Golomb, as an extension trait so the codec crates import exactly what they use.
pub trait GolombRead<'a> {
    /// ue(v). Values above the H.264/HEVC-mandated ceiling set overrun rather than looping.
    fn ue(&mut self) -> u32;
    fn se(&mut self) -> i32;
    /// ue(v) with an explicit inclusive maximum; anything larger is an error at the read site.
    fn ue_max(&mut self, max: u32) -> Result<u32, BitstreamError>;
    fn ue_golomb_k(&mut self, k: u32) -> u32;      // order-k, for AV1/VP9-adjacent syntax
    fn ue_long(&mut self) -> u64;                  // 32-bit-plus prefix, for the rare wide field
}
impl<'a> GolombRead<'a> for BitReader<'a> { /* … */ }
```

`ue()` is one `peek(32)` + `leading_zeros()` + one `get`, with a documented cap (a prefix of more than 32
zeros is malformed in every codec that uses this coding, so it sets overrun and returns 0 rather than
looping). That cap is the difference between a fuzz hang and a clean rejection.

### 8.5 Public API — writer

```rust
#[derive(Debug, Default)]
pub struct BitWriter { out: Vec<u8>, cache: u64, cache_bits: u32 }

impl BitWriter {
    #[must_use] pub fn new() -> Self;
    #[must_use] pub fn with_capacity(n: usize) -> Self;
    pub fn put(&mut self, n: u32, value: u32);
    pub fn put_long(&mut self, n: u32, value: u64);
    pub fn put_signed(&mut self, n: u32, value: i32);
    pub fn ue(&mut self, v: u32);
    pub fn se(&mut self, v: i32);
    pub fn align_zero(&mut self);
    pub fn align_one(&mut self);
    /// rbsp_trailing_bits(): a 1 bit then zeros to the byte boundary.
    pub fn rbsp_trailing(&mut self);
    #[must_use] pub fn bit_len(&self) -> u64;
    #[must_use] pub fn finish(self) -> Vec<u8>;
    /// Reuse the allocation across NAL units; the encoder path calls this per unit.
    pub fn reset(&mut self) -> Vec<u8>;
}

/// Wraps a `BitWriter`, inserting emulation-prevention bytes on the byte stream.
#[derive(Debug)]
pub struct RbspWriter { inner: BitWriter, zero_run: u8 }
```

The writer never fails: it appends to a `Vec` and grows. Allocation is bounded by the caller through
`vaco_core::try_reserve` at the call site that knows the limit.

### 8.6 Byte reader, start codes, RBSP

```rust
/// Endian-explicit cursor with the same sticky-overrun model.
#[derive(Debug, Clone)]
pub struct ByteReader<'a> { data: &'a [u8], pos: usize, overrun: bool }

impl<'a> ByteReader<'a> {
    pub fn u8(&mut self) -> u8;
    pub fn be16(&mut self) -> u16;  pub fn le16(&mut self) -> u16;
    pub fn be24(&mut self) -> u32;  pub fn le24(&mut self) -> u32;
    pub fn be32(&mut self) -> u32;  pub fn le32(&mut self) -> u32;
    pub fn be64(&mut self) -> u64;  pub fn le64(&mut self) -> u64;
    pub fn f32_be(&mut self) -> f32;  pub fn f64_be(&mut self) -> f64;
    pub fn bytes(&mut self, n: usize) -> &'a [u8];          // short slice + overrun on truncation
    pub fn skip(&mut self, n: usize);
    pub fn seek(&mut self, pos: usize);
    #[must_use] pub fn remaining(&self) -> usize;
    pub fn check(&mut self) -> Result<(), BitstreamError>;
}

/// Annex-B start-code handling. Delegates the scan to `vaco_simd::scan`.
pub mod annexb {
    /// Iterator over NAL units in an Annex-B byte stream, yielding EBSP (escaped) slices.
    pub struct NalIter<'a> { /* … */ }
    impl<'a> Iterator for NalIter<'a> { type Item = &'a [u8]; }
    pub fn nal_units(buf: &[u8]) -> NalIter<'_>;
    /// Remove emulation-prevention bytes into `scratch` (reused across calls) and return the RBSP.
    pub fn to_rbsp<'s>(ebsp: &[u8], scratch: &'s mut Vec<u8>) -> &'s [u8];
    /// Insert emulation-prevention bytes.
    pub fn to_ebsp(rbsp: &[u8], out: &mut Vec<u8>);
}

/// Length-prefixed NAL handling (the ISO-BMFF `avcC`/`hvcC` in-band form).
pub mod avcc {
    pub struct LengthPrefixedIter<'a> { /* … */ }
    pub fn nal_units(buf: &[u8], length_size: u8) -> LengthPrefixedIter<'_>;
}
```

`to_rbsp` takes a caller-owned scratch `Vec` rather than allocating, because it is called once per NAL and
a decoder processes tens of thousands. The scratch vector lives in the decoder, is cleared not freed, and
is therefore allocation-free in steady state — the same effect FFmpeg gets from its persistent
`rbsp_buffer`, achieved with ownership instead of a manual free list.

### 8.7 Internal design notes

- **The cache is MSB-aligned `u64`, holding `cache_bits` valid high bits.** `peek(n)` is
  `(cache >> (64 - n)) as u32`, `skip(n)` is `cache <<= n; cache_bits -= n`. No conditional shifts by 64
  (which are UB in C and a panic-free-but-wrong `<<` in Rust) — `n == 0` and `n == 64` are handled by the
  `get_long` path using `u128` intermediates or a two-step shift, and both are covered by unit tests.
- **`get(n)` for `n > cache_bits` refills first.** Refill loads 8 bytes and merges, so the maximum usable
  `n` in a single `get` is 57 without a second refill; `get_long` loops. This is the standard structure.
- **Everything is `#[inline]`, and the crate is compiled with `codegen-units = 1` in release** so the
  cross-function inlining into codec parsers actually happens.
- **`BitReader` is `Clone` and holds no interior mutability**, so `mark`/`restore` are just field copies
  and speculative parsing costs nothing.
- **No `Drop`.** A reader is a view; dropping it does nothing, and `finish()` is the explicit
  "I am done, was it valid?" call. Making the check explicit rather than implicit is what lets parsers
  batch it per structure.

### 8.8 Dependencies

`vaco-core`, `vaco-simd` (for `scan`), `thiserror`. Dev: `proptest`, `divan`.

Gate assessment (D10): `bitstream-io`, `bitvec` and `nom`'s bit combinators all clear Gates 1–3 and are
genuinely good crates. They fail on model and hot-path grounds: none offers the sticky-overrun contract
(all are `Result`-per-read, which is F13's rejected design), none has the `Padded` typestate that F9 depends
on, and none provides Exp-Golomb or Annex-B handling, which we would have to write on top regardless. This
crate is the innermost loop of every parser in the project and D10's fourth judgement-call bullet applies
directly. Write our own — and note that it is small (~1 100 lines), so "own it" is cheap here.

### 8.9 Test strategy

| Kind | Content |
|---|---|
| Unit | Every `n` from 0 to 64 for `get`/`peek`/`get_long`/`get_signed`, at every starting bit offset 0..8. Refill boundary crossings. `mark`/`restore` equivalence. The `n == 0` and `n == 64` shift edge cases explicitly. |
| Unit | Exp-Golomb: the first 64 `ue`/`se` code words checked against values computed from the ITU-T H.264 §9.1 definition, derived independently from the spec text. Malformed prefixes (33+ zeros) reject rather than loop. |
| Proptest | **Writer/reader round-trip:** an arbitrary sequence of `(n, value)` writes, read back, must reproduce the sequence exactly — the single most valuable property in the crate. Same for `ue`/`se` sequences and for `RbspWriter` → `to_rbsp`. |
| Proptest | **Padded/unpadded equivalence:** the same byte string read through `new` and `new_padded` produces identical value sequences *and* identical overrun behaviour. This is what keeps F9's fast path honest. |
| Proptest | **Truncation monotonicity:** reading a prefix of a buffer produces the same values as the full buffer up to the truncation point, then zeros and overrun. |
| Proptest | `to_ebsp` ∘ `to_rbsp` is the identity, and `to_rbsp` output never contains `00 00 03` in a position that would re-escape. |
| Fuzz | `bitreader_arbitrary` (arbitrary bytes + arbitrary read-width script): no panic, no hang, overrun always set once past the end. `golomb_arbitrary`. `annexb_nal_iter`: no panic, units partition the input, iterator always terminates. `rbsp_roundtrip`. |
| Differential | No direct black-box oracle exists for a bit reader in isolation — the reference exposes no such interface. Coverage comes indirectly: once `vaco-codec-h264`'s SPS/PPS parser lands (Layer 4), `ffprobe -show_streams` on a large corpus becomes a strong end-to-end differential test of this crate. Until then, `annexb::nal_units` can be checked against `ffmpeg -bsf:v trace_headers` output, which prints NAL boundaries and types — an available oracle worth wiring up early. |
| Bench | `divan`: padded vs unpadded reader on a real H.264 elementary stream; `ue()` throughput; `find_start_code` scalar vs SIMD. These numbers back the 8.3 cost claim, which must be re-measured rather than trusted. |

### 8.10 Effort and blocking

**2.5 person-weeks** (1 reader + Golomb, 0.5 writer, 0.5 Annex-B/RBSP/byte reader, 0.5 benchmarks and the
padded-path measurement). Blocks every parser and codec in the project, and is on the D5 v0.1 critical path
— MP4/Matroska/MPEG-TS all need the byte reader, and H.264/HEVC/AV1/AAC/Opus header parsing all need the
bit reader and Exp-Golomb. Start it in week 1 alongside `vaco-core`.

---

## 9. `vaco-pixfmt`

### 9.1 Purpose

The pixel-format enum (268 concrete formats per inventory §2) and its descriptor metadata: plane count,
per-component plane/step/offset/shift/depth, chroma subsampling, endianness, and the flag set
(`BE, PAL, BITSTREAM, HWACCEL, PLANAR, RGB, ALPHA, BAYER, FLOAT, XYZ`). Every scaler, filter, decoder and
encoder queries this table; it must be correct, complete, exhaustively self-consistent, and free at the
point of use.

### 9.2 F10 applied: the generator

Hand-writing 268 descriptors with four components each is ~1 300 error-prone lines whose errors are
silent — a wrong `offset` on `bgr565be` shows up as corrupted output in one conversion path, months later.
Generating them from families makes the *shape* the reviewable artefact.

**Source format.** The declarative source is a Rust `const` array inside the generator crate
(`crates/tools/vaco-pixfmt-gen`), not TOML or RON. Two reasons: the declarations get type-checked and
exhaustiveness-checked by `rustc` before they can produce a bad table, and the generator needs zero
dependencies (no data-format parser). ~35 family declarations plus ~45 explicit exceptions expand to 268
formats.

```rust
// crates/tools/vaco-pixfmt-gen/src/source.rs

pub enum Sub { S410, S411, S420, S422, S440, S444 }   // (log2_w, log2_h) pairs
pub enum End { None, LePair, BePair }                 // None = 8-bit, no endian variants
pub enum Alpha { No, Yes, Both }

pub enum Family {
    /// yuv420p, yuv422p10le, yuva444p16be, …  and the MSB-packed variants.
    PlanarYuv { stem: &'static str, subs: &'static [Sub], depths: &'static [u8],
                alpha: Alpha, end: End, msb_packed: bool },
    /// gbrp, gbrp12le, gbrap16be, gbrpf32le, …
    PlanarGbr { depths: &'static [u8], alpha: Alpha, end: End, float: Option<FloatWidth> },
    /// nv12, nv21, nv16, nv24, nv42, p010le, p210be, p416le, …
    BiplanarYuv { stem: &'static str, subs: &'static [Sub], depths: &'static [u8],
                  swapped_chroma: bool, end: End },
    /// gray8, gray10le, grayf32be, ya8, ya16le, yaf16le, …
    Gray { depths: &'static [u8], alpha: Alpha, end: End, float: Option<FloatWidth> },
    /// yuyv422, uyvy422, yvyu422, y210le, …  order given as a component permutation.
    PackedYuv { stem: &'static str, order: [Comp; 4], sub: Sub, depths: &'static [u8], end: End },
    /// rgb24, bgra, argb, rgba64le, rgbf32be, x2rgb10le, 0rgb, …
    PackedRgb { stem: &'static str, order: &'static [Chan], depths: &'static [u8],
                end: End, padding: Padding, float: Option<FloatWidth> },
    /// bayer_bggr8, bayer_rggb16le, …
    Bayer { patterns: &'static [&'static str], depths: &'static [u8], end: End },
    /// The HWACCEL surface handles: name only, no component metadata.
    HwSurface { names: &'static [&'static str] },
}

pub const FAMILIES: &[Family] = &[
    Family::PlanarYuv { stem: "yuv", subs: &[Sub::S410, Sub::S411, Sub::S420, Sub::S422,
                                             Sub::S440, Sub::S444],
                        depths: &[8], alpha: Alpha::Both, end: End::None, msb_packed: false },
    Family::PlanarYuv { stem: "yuv", subs: &[Sub::S420, Sub::S422, Sub::S444],
                        depths: &[9, 10, 12, 14, 16], alpha: Alpha::Both,
                        end: End::LePair, msb_packed: false },
    // …
];

/// Formats whose layout is not expressible as a family: pal8, monow/monob, uyyvyy411,
/// the odd low-bit RGB packings (rgb4, rgb8, bgr4_byte), ayuv/vuya/xv30/xv36, xyz12, …
pub const EXCEPTIONS: &[Exception] = &[ /* explicit descriptor literals, ~45 of them */ ];

/// Deprecated or alternate spellings that must resolve on lookup: y400a→ya8, gray8a→ya8,
/// gbr24p→gbrp, and the yuvj* full-range aliases.
pub const ALIASES: &[(&str, &str)] = &[ /* … */ ];
```

**What the generator emits** into the committed `crates/core/vaco-pixfmt/src/table.rs`:

1. `#[repr(u16)] #[non_exhaustive] pub enum PixelFormat` with dense discriminants `0..268` assigned in a
   deterministic order (family declaration order, then within-family order), so `fmt as usize` is a valid
   table index. Discriminants are ours (F14); names are the compatibility surface.
2. `static DESCRIPTORS: [PixFmtDescriptor; 268]`, index-aligned with the enum.
3. `static NAMES_SORTED: [(&str, PixelFormat); 268 + aliases]`, sorted, for binary-search lookup.
4. A `#[cfg(test)] mod generated_invariants` asserting the properties in 9.6 over every entry — generated
   alongside the table so the assertions cannot drift from it.

`xtask gen-pixfmt --check` regenerates into a temp file and diffs; CI fails on any difference. Editing
`table.rs` by hand is therefore impossible to land.

### 9.3 Public API and zero-cost queries

```rust
#![forbid(unsafe_code)]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentDesc {
    /// Index of the plane this component lives in.
    pub plane:  u8,
    /// Distance in bytes between consecutive samples of this component.
    pub step:   u8,
    /// Byte offset of the first sample within the plane row.
    pub offset: u8,
    /// Bits to shift right after masking, for sub-byte packings.
    pub shift:  u8,
    /// Significant bits.
    pub depth:  u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixFmtFlags(u16);
impl PixFmtFlags {
    pub const BE: Self;      pub const PAL: Self;    pub const BITSTREAM: Self;
    pub const HWACCEL: Self; pub const PLANAR: Self; pub const RGB: Self;
    pub const ALPHA: Self;   pub const BAYER: Self;  pub const FLOAT: Self;
    pub const XYZ: Self;
    #[must_use] pub const fn contains(self, o: Self) -> bool { self.0 & o.0 == o.0 }
}

#[derive(Debug, Clone, Copy)]
pub struct PixFmtDescriptor {
    pub name:           &'static str,
    pub nb_components:  u8,
    /// Precomputed at generation time, never derived at runtime.
    pub nb_planes:      u8,
    pub log2_chroma_w:  u8,
    pub log2_chroma_h:  u8,
    /// Average bits per pixel, padding excluded — matches `av_get_bits_per_pixel`.
    pub bits_per_pixel: u8,
    /// Including padding — matches `av_get_padded_bits_per_pixel`.
    pub padded_bits_per_pixel: u8,
    pub comp:           [ComponentDesc; 4],
    pub flags:          PixFmtFlags,
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum PixelFormat { Yuv420p = 0, Yuyv422 = 1, /* … 268 … */ }

impl PixelFormat {
    /// One array index into a static. Const, so it folds away when the format is known.
    #[inline] #[must_use]
    pub const fn desc(self) -> &'static PixFmtDescriptor { &DESCRIPTORS[self as usize] }

    #[inline] #[must_use] pub const fn name(self) -> &'static str        { self.desc().name }
    #[inline] #[must_use] pub const fn plane_count(self) -> usize        { self.desc().nb_planes as usize }
    #[inline] #[must_use] pub const fn component_count(self) -> usize    { self.desc().nb_components as usize }
    #[inline] #[must_use] pub const fn component(self, i: usize) -> ComponentDesc { self.desc().comp[i] }
    #[inline] #[must_use] pub const fn log2_chroma(self) -> (u8, u8);
    #[inline] #[must_use] pub const fn is_planar(self) -> bool  { self.desc().flags.contains(PixFmtFlags::PLANAR) }
    #[inline] #[must_use] pub const fn is_rgb(self) -> bool;
    #[inline] #[must_use] pub const fn has_alpha(self) -> bool;
    #[inline] #[must_use] pub const fn is_hw(self) -> bool;
    #[inline] #[must_use] pub const fn is_big_endian(self) -> bool;
    /// Maximum component depth. The single most-queried derived property; precomputed.
    #[inline] #[must_use] pub const fn max_depth(self) -> u8;
    /// The opposite-endianness sibling, if one exists. Generated as a table, not searched.
    #[inline] #[must_use] pub const fn swap_endianness(self) -> Option<PixelFormat>;

    pub fn from_name(s: &str) -> Option<Self>;      // binary search, includes aliases

    // --- geometry helpers (the `imgutils` role) ---
    pub fn plane_width(self, width: u32, plane: usize) -> u32;
    pub fn plane_height(self, height: u32, plane: usize) -> u32;
    /// Minimum stride for a plane at this width, before alignment.
    pub fn min_stride(self, width: u32, plane: usize) -> usize;
    /// Fill strides aligned to `align`; returns the per-plane byte sizes too.
    pub fn plane_layout(self, width: u32, height: u32, align: usize)
        -> Result<PlaneLayout, vaco_core::Error>;
}

#[derive(Debug, Clone, Copy)]
pub struct PlaneLayout { pub strides: [usize; 4], pub sizes: [usize; 4], pub planes: usize, pub total: usize }
```

**Why this is zero cost.** `desc()` is `&DESCRIPTORS[self as usize]` in a `const fn`: when the format is a
compile-time constant (which it is inside a monomorphised conversion kernel), the whole chain
`fmt.component(1).offset` const-folds to an immediate. When it is dynamic (the scaler's setup path), it is
one bounds-checked load from a 268 × 24-byte static that fits in 7 KB — resident in L2 forever. There is
no `HashMap`, no `Vec`, no `Option` in the descriptor, and `PixFmtDescriptor` is `Copy`, so a kernel can
take a descriptor by value into a register set. Deriving plane count or max depth at runtime is exactly the
kind of repeated computation the generator exists to eliminate.

**Endianness is a flag, not a type.** Formats come in BE/LE pairs; the descriptor's `BE` flag plus
`swap_endianness()` is enough for the scaler's `SWAP_BYTES` op, and duplicating the whole enum into a
generic-over-endianness type would double the table for no gain.

### 9.4 What `vaco-pixfmt` deliberately does not contain

No conversion code, no format-compatibility scoring, no "best format for" logic. Those need colour
knowledge and belong in `vaco-scale` (which depends on both this crate and `vaco-color`). This crate is
pure metadata, which is what keeps it fuzzable and trivially correct.

### 9.5 Dependencies

`vaco-core`, `vaco-opts` (for `impl OptValue for PixelFormat`, F6). No external runtime dependencies.

Gate assessment (D10): the format table is media-specific data with no crate equivalent — `yuv` and
`dcv-color-primitives` carry conversion routines, not an introspectable 268-format descriptor table, and
D10 names both as out on model grounds. The one external thing considered was `phf` for the name lookup;
rejected because a generated sorted array plus `binary_search_by_key` is faster than a runtime-constructed
perfect hash for 300 entries and adds no dependency.

### 9.6 Test strategy

| Kind | Content |
|---|---|
| Generated invariants (per format, all 268) | `nb_planes == 1 + max(comp.plane)`; every plane index in `0..nb_planes` is used by at least one component; `nb_components in 1..=4`; `depth in 1..=32`; planar formats have `step == ceil(depth/8)` for every component; packed formats have all components in plane 0; `offset + ceil(depth/8) <= step` for packed; `BE`-flagged formats have `depth > 8` or are `BITSTREAM`; `ALPHA` is set iff `nb_components` is 2 or 4 with an alpha channel; `HWACCEL` formats have `nb_components == 0`; `bits_per_pixel` equals the value recomputed from components and subsampling. |
| Generated invariants (pairwise) | Every BE format has exactly one LE sibling with an identical descriptor except the `BE` flag, and `swap_endianness` is an involution on that set. Every name is unique; every alias resolves to an existing format. |
| Unit | `plane_layout` for a spread of odd widths/heights against hand-computed values, including 4:1:0 and 4:1:1 where rounding bites, and including `align` values 1, 16, 32, 64. |
| Proptest | For every format × arbitrary `(w, h, align)`: strides are ≥ `min_stride`, aligned to `align`, sizes are consistent with `plane_height`, and `total` does not overflow (this is where a fuzzer would otherwise find an integer overflow → undersized allocation). |
| Fuzz | `pixfmt_from_name` on arbitrary UTF-8; `plane_layout` on arbitrary `(fmt, w, h, align)` asserting either a correct layout or `Err(LimitExceeded/Overflow)`, never a panic and never a silently-wrapped size. |
| **Differential** | `ffprobe -show_pixel_formats -of json` emits, per format: name, `nb_components`, `log2_chroma_w/h`, `bits_per_pixel` and the flag set — and with `-show_pixel_formats` plus the components section, the per-component `depth`. `ffmpeg -pix_fmts` gives a second, differently-shaped view (the `IO...` capability column, components, bits per pixel). A harness parses both, compares to our generated table field-by-field, and reports any format we lack or describe differently. **This validates essentially the entire crate against the reference in one automated pass** and should be the first test written — before the generator, so it also serves as the acceptance criterion for the family declarations. |

### 9.7 Effort and blocking

**3 person-weeks**: 0.5 to build the differential extractor from `ffprobe -show_pixel_formats`, 1.5 to
write the family declarations and generator until that extractor reports zero divergences, 0.5 for the
geometry helpers, 0.5 for tests and fuzzing. Blocks `vaco-frame`, `vaco-scale`, every video codec and every
video filter. On the D5 v0.1 critical path (`ffprobe` prints `pix_fmt` on every video stream).

---

## 10. `vaco-sampfmt`

### 10.1 Purpose and API

The 12 audio sample formats from inventory §4 plus buffer-geometry helpers. Small, but on the critical path
for every audio component.

```rust
#![forbid(unsafe_code)]

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum SampleFormat {
    U8, S16, S32, S64, Flt, Dbl,                 // interleaved
    U8p, S16p, S32p, S64p, Fltp, Dblp,           // planar
}

impl SampleFormat {
    #[inline] #[must_use] pub const fn name(self) -> &'static str;
    #[inline] #[must_use] pub const fn bytes(self) -> usize;         // 1,2,4,8,4,8
    #[inline] #[must_use] pub const fn bits(self) -> u32;
    #[inline] #[must_use] pub const fn is_planar(self) -> bool;
    #[inline] #[must_use] pub const fn is_float(self) -> bool;
    #[inline] #[must_use] pub const fn is_signed(self) -> bool;      // U8/U8p are the exception
    /// The planar sibling of an interleaved format and vice versa. Total function.
    #[inline] #[must_use] pub const fn to_planar(self) -> Self;
    #[inline] #[must_use] pub const fn to_packed(self) -> Self;
    pub fn from_name(s: &str) -> Option<Self>;

    /// Buffer geometry. `align` is a byte alignment applied to each plane.
    pub fn buffer_layout(self, channels: u32, nb_samples: u32, align: usize)
        -> Result<SampleLayout, vaco_core::Error>;
}

#[derive(Debug, Clone, Copy)]
pub struct SampleLayout { pub planes: u32, pub plane_size: usize, pub total: usize, pub stride: usize }
```

### 10.2 Design notes

`to_planar`/`to_packed` are total because the enum is laid out as two parallel halves — the generated
discriminants make the conversion `Self::from_u8(d ^ 6)`, checked by an exhaustive test rather than
asserted. Twelve variants do not need a generator; they are written by hand with an exhaustive
`match`-based test that fails to compile if a variant is added without updating every accessor. That is the
opposite call from `vaco-pixfmt` and it is right for the same reason: at twelve variants, exhaustive
matching is the stronger guarantee; at 268 it is unmaintainable.

### 10.3 Dependencies, tests, effort

`vaco-core`, `vaco-opts` (`impl OptValue for SampleFormat`). No external runtime dependencies; nothing to
buy — this is media-specific data.

Tests: exhaustive unit tests over all 12 variants for every accessor; proptest on `buffer_layout`
(no overflow, alignment respected, `total` consistent); differential against `ffmpeg -sample_fmts`
(name, bit depth, planar flag — a complete check of the crate's data).

**0.5 person-weeks.** Blocks every audio codec, `vaco-resample`, and audio `Frame` construction. On the
v0.1 critical path (`ffprobe` prints `sample_fmt`).

---

## 11. `vaco-chlayout`

### 11.1 Purpose

The channel-layout model from inventory §4: four ordering modes, 36 channel identifiers, the ~40 predefined
layouts, and the parse/display grammar that appears in CLI arguments and in `ffprobe` output.

### 11.2 API

```rust
#![forbid(unsafe_code)]

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Channel {
    FrontLeft = 0, FrontRight, FrontCenter, LowFrequency, BackLeft, BackRight,
    FrontLeftOfCenter, FrontRightOfCenter, BackCenter, SideLeft, SideRight,
    TopCenter, TopFrontLeft, TopFrontCenter, TopFrontRight,
    TopBackLeft, TopBackCenter, TopBackRight, StereoLeft, StereoRight,
    WideLeft, WideRight, SurroundDirectLeft, SurroundDirectRight, LowFrequency2,
    TopSideLeft, TopSideRight, BottomFrontCenter, BottomFrontLeft, BottomFrontRight,
    SideSurroundLeft, SideSurroundRight, TopSurroundLeft, TopSurroundRight,
    BinauralLeft, BinauralRight,
    Unused = 0x200, Unknown = 0x300,
    // Ambisonic ACN indices occupy 0x400..=0x7ff and are constructed, not enumerated.
}

impl Channel {
    #[must_use] pub const fn name(self) -> &'static str;              // "FL", "FR", "LFE", …
    #[must_use] pub const fn description(self) -> &'static str;       // "front left", …
    pub fn from_name(s: &str) -> Option<Self>;
    #[must_use] pub const fn ambisonic(acn: u16) -> Option<Self>;
    #[must_use] pub const fn as_mask_bit(self) -> Option<u64>;        // None above 63
}

/// A bitmask over the first 64 `Channel` values, in enum order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ChannelMask(u64);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ChannelLayout {
    /// Count known, positions unknown.
    Unspec { channels: u16 },
    /// Bitmask over `Channel`, ≤ 63 channels. The common case.
    Native(ChannelMask),
    /// Explicit per-index map. `Arc` so cloning a Frame stays cheap.
    Custom(std::sync::Arc<[Channel]>),
    /// ACN-ordered ambisonic, optionally with a trailing non-diegetic native block.
    Ambisonic { order: u16, extra: Option<ChannelMask> },
}

impl ChannelLayout {
    pub const UNSPEC: Self = Self::Unspec { channels: 0 };
    pub const MONO:   Self; pub const STEREO: Self; pub const SURROUND_5_1: Self; /* … ~40 … */

    #[must_use] pub fn channels(&self) -> u16;
    #[must_use] pub fn channel_at(&self, index: u16) -> Option<Channel>;
    #[must_use] pub fn index_of(&self, ch: Channel) -> Option<u16>;
    #[must_use] pub fn contains(&self, ch: Channel) -> bool;
    #[must_use] pub fn is_subset_of(&self, other: &Self) -> bool;
    /// True when positions are known — i.e. not `Unspec` and containing no `Unknown`.
    #[must_use] pub fn is_positional(&self) -> bool;
    /// Canonical form: `Custom` collapses to `Native` when the order matches the mask order.
    #[must_use] pub fn canonical(self) -> Self;
    /// Convert between orderings where possible; the `retype` role.
    pub fn retype(&self, order: Order) -> Option<Self>;
    /// The default layout for a channel count, e.g. 6 → 5.1.
    #[must_use] pub fn default_for(channels: u16) -> Self;

    /// Grammar: "stereo", "5.1", "FL+FR+LFE", "0x3f", "4 channels", "ambisonic 1+stereo",
    /// "3 channels (FL+FR+LFE)".
    pub fn parse(s: &str) -> Result<Self, LayoutError>;
    /// The canonical name when one exists ("5.1"), otherwise the "+"-joined form.
    #[must_use] pub fn name(&self) -> String;
    /// The `ffprobe`-style long description.
    #[must_use] pub fn describe(&self) -> String;
}
impl core::fmt::Display for ChannelLayout {}
```

### 11.3 Design notes

**`Custom` holds `Arc<[Channel]>`, not `Vec<Channel>`.** `ChannelLayout` is embedded in every audio
`Frame` and every `CodecParameters`, both of which are cloned constantly; `Arc` makes the clone a refcount
bump. The layout is immutable after construction, so there is no CoW question.

**Canonicalisation is explicit, not automatic.** `Custom([FL, FR])` and `Native(FL|FR)` are semantically
equal but structurally different, and FFmpeg has real bugs in this area. `PartialEq` compares structurally;
`canonical()` normalises; a `#[test]` asserts `a.canonical() == b.canonical()` for every equivalent pair we
can construct. Making the distinction visible in the type is better than pretending it does not exist.

**The predefined layout table is a `const` array of `(name, ChannelMask)`**, hand-written from the
inventory's list (~40 entries), and checked against `ffmpeg -layouts` by the differential test. It is small
enough not to need the generator treatment, and the differential test makes drift impossible anyway.

### 11.4 Dependencies, tests, effort

`vaco-core`, `vaco-opts` (`impl OptValue for ChannelLayout`), `thiserror`. No external runtime deps; the
model is media-specific and no crate offers it.

| Kind | Content |
|---|---|
| Unit | Every predefined layout: channel count, `name()` round-trip, `describe()` text. Ambisonic order/channel-count arithmetic ((order+1)² channels) including the `extra` block. |
| Proptest | `parse(name(x)) == x.canonical()` for arbitrary layouts. `index_of ∘ channel_at` is the identity on valid indices. `is_subset_of` is a partial order. |
| Fuzz | `chlayout_parse` on arbitrary UTF-8 — no panic, bounded allocation (an ambisonic order field is attacker-controlled and must be bounded before it sizes anything). |
| **Differential** | `ffmpeg -layouts` prints every individual channel (name + description) and every standard layout (name + decomposition) — a complete, machine-parseable check of both tables. `ffprobe -show_streams` on a multichannel corpus checks `channel_layout` rendering. `ffmpeg -af "aformat=channel_layouts=<X>"` acceptance-tests our parser output against the reference's parser. |

**1.5 person-weeks.** Blocks every audio codec, `vaco-resample`, and audio `Frame`. On the v0.1 critical
path (`ffprobe` prints `channel_layout`).

---

## 12. `vaco-color`

### 12.1 Purpose

Colour science as *data plus derivations*: the H.273 enumerations, chromaticity coordinates, matrix
derivation, transfer functions, and range conversion. Consumed by `vaco-scale`, every video codec's
metadata path, and `ffprobe`'s stream output.

### 12.2 API

```rust
#![forbid(unsafe_code)]

/// ITU-T H.273 §8.1 code points. Discriminants ARE the spec values (F14) — they go into bitstreams.
#[repr(u16)] #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)] #[non_exhaustive]
pub enum ColorPrimaries {
    Reserved0 = 0, Bt709 = 1, Unspecified = 2, Reserved3 = 3, Bt470m = 4, Bt470bg = 5,
    Smpte170m = 6, Smpte240m = 7, Film = 8, Bt2020 = 9, Smpte428 = 10, Smpte431 = 11,
    Smpte432 = 12, Ebu3213 = 22,
    /// Non-H.273 extension space, matching the reference's `EXT_BASE = 256`.
    VGamut = 256,
}

#[repr(u16)] #[derive(…)] #[non_exhaustive]
pub enum ColorTransfer {
    Reserved0 = 0, Bt709 = 1, Unspecified = 2, Reserved3 = 3, Gamma22 = 4, Gamma28 = 5,
    Smpte170m = 6, Smpte240m = 7, Linear = 8, Log = 9, LogSqrt = 10, Iec61966_2_4 = 11,
    Bt1361Ecg = 12, Iec61966_2_1 = 13, Bt2020_10 = 14, Bt2020_12 = 15, Smpte2084 = 16,
    Smpte428 = 17, AribStdB67 = 18, VLog = 256,
}

#[repr(u16)] #[derive(…)] #[non_exhaustive]
pub enum ColorSpace {
    Rgb = 0, Bt709 = 1, Unspecified = 2, Reserved3 = 3, Fcc = 4, Bt470bg = 5, Smpte170m = 6,
    Smpte240m = 7, Ycgco = 8, Bt2020Ncl = 9, Bt2020Cl = 10, Smpte2085 = 11,
    ChromaDerivedNcl = 12, ChromaDerivedCl = 13, Ictcp = 14, IptC2 = 15,
    YcgcoRe = 16, YcgcoRo = 17,
}

#[repr(u8)] #[derive(…)] pub enum ColorRange { Unspecified = 0, Limited = 1, Full = 2 }
#[repr(u8)] #[derive(…)] pub enum ChromaLocation {
    Unspecified = 0, Left = 1, Center = 2, TopLeft = 3, Top = 4, BottomLeft = 5, Bottom = 6,
}
#[repr(u8)] #[derive(…)] pub enum AlphaMode { Unspecified = 0, Premultiplied = 1, Straight = 2 }

/// CIE 1931 xy chromaticity.
#[derive(Debug, Clone, Copy, PartialEq)] pub struct Xy { pub x: f64, pub y: f64 }
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PrimariesDesc { pub r: Xy, pub g: Xy, pub b: Xy, pub white: Xy }

impl ColorPrimaries {
    /// Chromaticity coordinates from the H.273 table. `None` for Unspecified/Reserved.
    #[must_use] pub const fn chromaticity(self) -> Option<PrimariesDesc>;
    pub fn from_name(s: &str) -> Option<Self>;
    #[must_use] pub const fn name(self) -> &'static str;
    /// Nearest match to a chromaticity set, for containers that carry raw coordinates (MKV, ICC).
    #[must_use] pub fn nearest(p: PrimariesDesc, tolerance: f64) -> Option<Self>;
}

impl ColorSpace {
    /// (Kr, Kb) luma coefficients. Derived from primaries for the CHROMA_DERIVED_* values.
    #[must_use] pub fn luma_coefficients(self, prim: ColorPrimaries) -> Option<(f64, f64)>;
}

/// 3x3 matrices, row-major.
#[derive(Debug, Clone, Copy, PartialEq)] pub struct Mat3([[f64; 3]; 3]);
impl Mat3 {
    #[must_use] pub fn mul(self, o: Self) -> Self;
    #[must_use] pub fn invert(self) -> Option<Self>;
    #[must_use] pub fn apply(self, v: [f64; 3]) -> [f64; 3];
}

pub mod matrix {
    /// RGB→XYZ for a primary set, derived per SMPTE RP 177 from the chromaticities.
    pub fn rgb_to_xyz(p: PrimariesDesc) -> Option<Mat3>;
    pub fn xyz_to_rgb(p: PrimariesDesc) -> Option<Mat3>;
    /// Non-constant-luminance YCbCr→RGB from (Kr, Kb), per H.273 §8.3.
    pub fn ycbcr_to_rgb(kr: f64, kb: f64) -> Mat3;
    pub fn rgb_to_ycbcr(kr: f64, kb: f64) -> Mat3;
    /// Bradford chromatic adaptation between white points.
    pub fn adapt_white(from: Xy, to: Xy) -> Mat3;
    /// The full source→destination conversion, composed and pre-multiplied — what the
    /// ops-graph scaler's `LINEAR` op consumes.
    pub fn convert(src: ColorSpec, dst: ColorSpec) -> Mat3;
}

pub mod transfer {
    /// Scalar EOTF/OETF pairs. `Fn(f64) -> f64`, plus fixed-point LUT construction for the scaler.
    pub fn eotf(t: ColorTransfer) -> Option<fn(f64) -> f64>;
    pub fn oetf(t: ColorTransfer) -> Option<fn(f64) -> f64>;
    /// Build a `2^bits`-entry LUT for a transfer curve; the scaler uses these, never the closures.
    pub fn build_lut(t: ColorTransfer, bits: u32, out: &mut [u16]) -> Result<(), vaco_core::Error>;
    /// Reference white luminance for absolute curves (PQ). Needed for tone mapping.
    #[must_use] pub const fn reference_white(t: ColorTransfer) -> Option<f64>;
}

pub mod range {
    /// Limited↔full scale/offset for a given bit depth and component role.
    #[must_use] pub const fn luma(depth: u32, r: ColorRange) -> (i32, i32);     // (offset, scale)
    #[must_use] pub const fn chroma(depth: u32, r: ColorRange) -> (i32, i32);
}

/// The complete colour description of a frame or stream. Copy, cheap, and comparable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ColorSpec {
    pub primaries: ColorPrimaries, pub transfer: ColorTransfer, pub matrix: ColorSpace,
    pub range: ColorRange, pub chroma_loc: ChromaLocation, pub alpha: AlphaMode,
}
```

### 12.3 Design notes

**Everything is derived, nothing is a magic table.** The only committed numbers are the H.273 chromaticity
coordinates and the transfer-function parameters, both read directly from the specification text — which
D9 confirms is merger/scenes-à-faire and explicitly permitted, unlike FFmpeg's reordered or pre-scaled
variants of the same data, which are not. `ycbcr_to_rgb` is computed from (Kr, Kb) by the H.273 formula
at runtime (or in a `const fn` where possible), not transcribed as a matrix of decimals. This is both the
clean-room-safe route and the one that cannot drift.

**`f64` for derivation, fixed point for execution.** The matrices here are computed once per conversion
setup. The scaler converts them to fixed-point coefficients for its kernels; that quantisation lives in
`vaco-scale`, not here, so this crate stays exact and testable against the spec's own worked examples.

**Transfer functions are `fn` pointers plus LUT builders, never called per-pixel.** `build_lut` is the API
the hot path uses; the closures exist for testing and for the tone-mapping setup path.

### 12.4 Dependencies, tests, effort

`vaco-core`, `vaco-opts`. No external runtime deps.

Gate assessment (D10): considered and rejected on model grounds — `palette` and `colstodian` are
general colour-management crates built around their own colour types and would not give us H.273 code
points, chroma siting, or the exact enum surface that gets serialised into bitstreams and printed by
`ffprobe`; `dcv-color-primitives` and `yuv` are conversion libraries, which D10 lists as out. The
derivations here are ~600 lines of well-specified arithmetic.

| Kind | Content |
|---|---|
| Unit | Chromaticities against the H.273 tables. `rgb_to_xyz(bt709)` against the published sRGB matrix to 1e-9. `ycbcr_to_rgb(bt709)` against the H.273 worked values. PQ and HLG curves against the spec's reference points (0, 0.5, 1.0, and the HLG knee). |
| Proptest | `invert ∘ invert` is the identity within 1e-12 for every defined primary set. `xyz_to_rgb ∘ rgb_to_xyz == I`. `eotf ∘ oetf == identity` within tolerance across `[0,1]` for every invertible transfer. `adapt_white(a,b) ∘ adapt_white(b,a) == I`. |
| Unit | `range::luma/chroma` for depths 8, 10, 12, 14, 16 against the values the spec defines. |
| **Differential** | `ffprobe -show_streams` colour fields on a corpus spanning every primaries/transfer/matrix code point (generate the corpus with `ffmpeg -vf setparams=...`) — checks our enum ↔ name mapping exhaustively. `ffmpeg -h full` lists the accepted names for `color_primaries`/`color_trc`/`colorspace` options, giving the complete name table for a snapshot diff. Matrix derivation has no direct oracle; it is validated end-to-end once `vaco-scale` lands, by comparing converted pixel output. |

**2 person-weeks.** Blocks `vaco-scale` and HDR metadata handling; needed at v0.1 for `ffprobe`'s colour
fields (name mapping only — the derivations are not on the v0.1 path but are cheap to land together).

---

## 13. `vaco-frame`

### 13.1 Purpose

`Frame` — video and audio — with plane storage, strides, side data, metadata, colour description and
cropping. The refcount and copy-on-write model that FFmpeg builds by hand from `AVBufferRef` is expressed
here with `Arc` and `Arc::make_mut`, without a line of unsafe. This crate is where the "zero copy" claim
in architecture §7.4 is either true or not.

### 13.2 F11 — One buffer per plane

Options: (a) one allocation per frame with planes at computed offsets (FFmpeg's layout); (b) one
allocation per plane.

(a) makes "give thread A `&mut` to the luma plane while thread B reads chroma" inexpressible without
splitting a single `&mut [u8]`, which in turn requires the frame to be uniquely owned at that moment — the
exact thing we cannot guarantee when a reference frame is shared. It also forces a copy-on-write of the
*whole frame* when a filter modifies one plane.

**Chosen: (b).** Each plane is an independent `Arc`, so (i) disjoint mutable access across planes is
proven by the borrow checker with no runtime mechanism at all, (ii) a filter that rewrites chroma and
passes luma through shares the luma `Arc` and copies nothing, and (iii) copy-on-write granularity is the
plane, not the frame. The costs are one extra allocation per plane — which the pool amortises to a
free-list pop — and loss of inter-plane locality, which matters only for whole-frame `memcpy` and is
recovered by the `Contiguous` constructor below for the hardware-interop cases that genuinely require a
single allocation with defined plane offsets.

### 13.3 The buffer type — CoW in one line

```rust
#![forbid(unsafe_code)]
use std::sync::{Arc, Weak};

/// A refcounted, pool-aware, 64-byte-aligned byte buffer.
#[derive(Debug, Clone)]
pub struct Block(Arc<BlockInner>);

#[derive(Debug)]
struct BlockInner {
    buf:  vaco_pool::AlignedBuf,
    pool: Option<Weak<vaco_pool::PoolInner>>,
}

impl Clone for BlockInner {
    /// Invoked by `Arc::make_mut` when the block is shared. Draws a fresh buffer from the same
    /// pool (so the copy is also poolable) and copies the contents.
    fn clone(&self) -> Self {
        let mut buf = match self.pool.as_ref().and_then(Weak::upgrade) {
            Some(p) => p.acquire(self.buf.len()),
            None    => vaco_pool::AlignedBuf::new(self.buf.len(), 64),
        };
        buf.as_mut_slice().copy_from_slice(self.buf.as_slice());
        Self { buf, pool: self.pool.clone() }
    }
}

impl Drop for BlockInner {
    /// Runs when the LAST `Arc` to this block is dropped — exactly the "returns to the pool when
    /// the refcount hits zero" behaviour, with no manual refcount and no explicit release call.
    fn drop(&mut self) {
        if let Some(p) = self.pool.as_ref().and_then(Weak::upgrade) {
            p.recycle(core::mem::take(&mut self.buf));
        }
    }
}

impl Block {
    #[must_use] pub fn as_slice(&self) -> &[u8] { self.0.buf.as_slice() }

    /// Copy-on-write. This single call IS the whole model: unique ⇒ in-place, shared ⇒ clone.
    #[must_use] pub fn as_mut_slice(&mut self) -> &mut [u8] {
        Arc::make_mut(&mut self.0).buf.as_mut_slice()
    }

    #[must_use] pub fn is_writable(&self) -> bool { Arc::strong_count(&self.0) == 1 }
    /// Force the copy now, so a later `as_mut_slice` in a hot loop cannot surprise us.
    pub fn make_writable(&mut self) { let _ = Arc::make_mut(&mut self.0); }
    #[must_use] pub fn ptr_eq(&self, o: &Self) -> bool { Arc::ptr_eq(&self.0, &o.0) }
}
```

Three questions from the brief, answered directly:

**"How do you express *writable if uniquely owned, else clone*?"** `Arc::make_mut`. That is literally the
function's contract, and it is why the buffer's payload type must be `Clone`. `is_writable()` exposes the
predicate for callers who want to decide before committing, matching `av_buffer_is_writable`;
`make_writable()` forces the copy at a point of the caller's choosing, matching `av_frame_make_writable`.
The one subtlety worth writing down: `Arc::make_mut` also clones when a `Weak` exists, so we never hand
out `Weak<BlockInner>` — the only `Weak` in the design points at the *pool*, from the block, never the
reverse.

**"How does the buffer pool interact with `Arc`?"** Through `Drop for BlockInner` plus
`Weak<PoolInner>`. The block holds a weak reference so a live frame never keeps a dead pool alive; the
pool holds no reference to outstanding blocks at all. Recycling happens in `drop`, which the `Arc` calls
exactly when the strong count reaches zero. `Clone for BlockInner` deliberately re-acquires *from the
pool* rather than allocating, so a copy-on-write in steady state is also allocation-free. Nothing here is
a special case: an ordinary `Arc<T>` with a `Drop` on `T` reproduces the whole `AVBufferPool` behaviour.

### 13.4 The frame types

```rust
#[derive(Debug, Clone)]
pub struct Plane {
    block:  Block,
    /// Byte offset of row 0 within the block. Cropping adjusts this — see 13.7.
    offset: usize,
    stride: usize,
    rows:   usize,
    /// Bytes of the row that carry data; `stride - row_bytes` is padding.
    row_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct VideoFrame {
    format: PixelFormat,
    width:  u32,
    height: u32,
    planes: [Option<Plane>; 4],
    pub color: ColorSpec,
    pub sample_aspect_ratio: Rational,
    pub picture_type: PictureType,
    pub interlace: Interlace,
    pub pts: Option<Ts>,
    pub duration: Option<i64>,
    pub time_base: TimeBase,
    pub metadata: Dict,
    side_data: Vec<FrameSideData>,
    crop: Crop,
    pub flags: FrameFlags,     // KEY, DISCARD, CORRUPT, INTERLACED, TOP_FIELD_FIRST
}

#[derive(Debug, Clone)]
pub struct AudioFrame {
    format: SampleFormat,
    layout: ChannelLayout,
    nb_samples: u32,
    sample_rate: u32,
    /// One block per plane for planar formats, exactly one for interleaved.
    /// `Vec` rather than an array because 22.2 and ambisonic layouts exceed any fixed bound.
    planes: Vec<Plane>,
    pub pts: Option<Ts>, pub duration: Option<i64>, pub time_base: TimeBase,
    pub metadata: Dict,
    side_data: Vec<FrameSideData>,
    pub flags: FrameFlags,
}

#[derive(Debug, Clone)]
pub enum Frame { Video(VideoFrame), Audio(AudioFrame) }
```

`Frame` is an enum rather than a struct with `Option` fields for both media types, because every consumer
already knows which it wants and the enum makes "audio fields on a video frame" unrepresentable — a
recurring source of confusion in the C model.

### 13.5 Plane access — the concurrency answer

```rust
/// Read-only view. THE universal currency for plane access; see 13.6 for why this matters.
#[derive(Debug, Clone, Copy)]
pub struct PlaneRef<'a> { pub data: &'a [u8], pub stride: usize, pub rows: usize, pub row_bytes: usize }

/// Exclusive view.
#[derive(Debug)]
pub struct PlaneMut<'a> { pub data: &'a mut [u8], pub stride: usize, pub rows: usize, pub row_bytes: usize }

impl<'a> PlaneRef<'a> {
    #[must_use] pub fn row(&self, y: usize) -> &'a [u8];
    pub fn rows_iter(&self) -> impl Iterator<Item = &'a [u8]> + '_;
}
impl<'a> PlaneMut<'a> {
    pub fn row_mut(&mut self, y: usize) -> &mut [u8];
    /// Disjoint mutable rows — this is what feeds slice-parallel filtering.
    pub fn rows_mut(&mut self) -> impl Iterator<Item = &mut [u8]> + '_;
    /// Split into N horizontal bands for scoped parallelism, disjointness proven by the compiler.
    pub fn split_bands(self, n: usize) -> Vec<PlaneMut<'a>>;
}

impl VideoFrame {
    #[must_use] pub fn plane(&self, i: usize) -> Option<PlaneRef<'_>>;
    /// All planes at once, mutably. Disjointness is structural: `[T; 4]::each_mut` yields four
    /// independent `&mut`, and each plane owns a distinct `Arc`, so no aliasing is possible.
    #[must_use] pub fn planes_mut(&mut self) -> [Option<PlaneMut<'_>>; 4] {
        self.planes.each_mut().map(|slot| slot.as_mut().map(Plane::as_mut))
    }
    /// Copy-on-write every plane up front (`av_frame_make_writable`).
    pub fn make_writable(&mut self);
    #[must_use] pub fn is_writable(&self) -> bool;
}
```

**"How do you give a decoder mutable access to one plane while another thread reads a different plane?"**

```rust
let [y, u, v, _a] = frame.planes_mut();          // four independent &mut, disjoint by construction
let (mut y, u) = (y.expect("luma"), u.expect("cb"));

std::thread::scope(|s| {
    s.spawn(|| reconstruct_luma(&mut y));        // exclusive access to plane 0
    s.spawn(|| measure_chroma(&u));              // shared access to plane 1
});
```

That compiles, is race-free by the type system, and needs no runtime mechanism whatsoever. It works
because F11 gave each plane its own `Arc`: `each_mut` on the `[Option<Plane>; 4]` array produces four
`&mut Plane` with disjoint provenance, and each `Plane::as_mut` calls `Arc::make_mut` on a *different*
`Arc`. The three ways this could have gone wrong are all closed:

- *Planes sharing one allocation* — ruled out by F11; the `Contiguous` case (13.8) hands out
  pre-split `PlaneMut`s at construction instead.
- *Another thread holding a clone of the whole frame* — then `make_mut` on each plane copies that plane
  and the writer proceeds on its private copy. Correct, safe, and visible in a profile. Callers who must
  not pay that call `is_writable()` first, or acquire from the pool, which always yields unique blocks.
- *The reader and writer wanting the same plane* — not expressible. That is the point.

**Within one plane, across threads:** `PlaneMut::split_bands(n)` yields `n` disjoint `PlaneMut`s over row
bands, which is how slice-threaded filtering and slice-threaded decoding parallelise. Again, structural.

### 13.6 What we deliberately cannot do, and the plan for it

Frame-threaded decoding in FFmpeg lets thread B read rows 0..k of a reference picture while thread A is
still writing rows k.., synchronised by a progress counter. Safe Rust cannot express a shared buffer that
is simultaneously being written and read, however carefully the row ranges are partitioned, because the
`&mut` and `&` would overlap in provenance.

Options: (a) forbid it — frame-threaded decoders wait for a whole reference frame; (b) publish completed
row bands as separate immutable `Block`s that readers await through a `OnceLock` slot, making the plane
physically segmented; (c) escalate a D2 exception.

**Chosen: (a) for v1, with (b) designed for.** Pipeline parallelism (architecture §6 axis 1) and slice
parallelism (axis 3) already supply most of the throughput, and whole-frame reference waiting costs
latency rather than throughput on the workloads that matter. The forward-compat move is concrete and must
be honoured from day one: **all plane access goes through `PlaneRef`/`PlaneMut`, never through a bare
`&[u8]` obtained from the frame.** If we later adopt (b), `PlaneRef` gains a banded representation and an
`await_rows(n)` method, and every kernel that took a `PlaneRef` keeps compiling. Exposing raw slices from
`Frame` would foreclose that, so it is a review-blocking rule, not a style preference.

### 13.7 Cropping, side data, metadata

**Cropping is offset arithmetic, never a copy.** `Plane` carries an `offset`, so
`apply_cropping()` adjusts `offset`, `rows`, `row_bytes`, `width` and `height` and touches no bytes.
The one caveat is that a cropped plane's row 0 may not be 64-byte aligned any more; this costs performance,
not correctness, because safe `fearless_simd` slice loads (`SimdBase::from_slice`) never require
alignment — a genuine
advantage of the portable-SIMD route over intrinsics, worth naming. `apply_cropping` records the
misalignment in `Plane` so the scaler can choose to realign when it matters.

```rust
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Crop { pub top: u32, pub bottom: u32, pub left: u32, pub right: u32 }

impl VideoFrame {
    pub fn set_crop(&mut self, c: Crop) -> Result<(), Error>;   // validated against subsampling
    pub fn apply_cropping(&mut self) -> Result<(), Error>;      // zero-copy; adjusts offsets
}
```

**Side data is a typed enum, not an opaque blob.** The inventory's 36 frame side-data types become typed
variants where we parse them and a passthrough variant where we do not. This is the D1 dividend: consumers
match on a variant instead of casting a byte pointer.

```rust
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum FrameSideData {
    DisplayMatrix([i32; 9]),
    Stereo3d(Stereo3d),
    Spherical(Spherical),
    MasteringDisplay(MasteringDisplayMetadata),
    ContentLightLevel { max_cll: u32, max_fall: u32 },
    AmbientViewingEnvironment(AmbientViewingEnvironment),
    DynamicHdrPlus(Hdr10PlusMetadata),
    DynamicHdrVivid(HdrVividMetadata),
    DoviRpu(Arc<[u8]>),
    DoviMetadata(DoviMetadata),
    FilmGrainParams(FilmGrainParams),
    A53Cc(Arc<[u8]>),
    SkipSamples { start: u32, end: u32, reason_start: u8, reason_end: u8 },
    S12mTimecode([u32; 4]),
    GopTimecode(u64),
    MotionVectors(Arc<[MotionVector]>),
    RegionsOfInterest(Arc<[RegionOfInterest]>),
    VideoEncParams(VideoEncParams),
    DetectionBboxes(Arc<[DetectionBbox]>),
    IccProfile(Arc<[u8]>),
    Exif(Arc<[u8]>),
    ReplayGain(ReplayGain),
    DownmixInfo(DownmixInfo),
    MatrixEncoding(MatrixEncoding),
    AudioServiceType(AudioServiceType),
    Panscan(Panscan),
    Afd(u8),
    ViewId(i32),
    Lcevc(Arc<[u8]>),
    SeiUnregistered(Arc<[u8]>),
    ThreeDReferenceDisplays(ThreeDReferenceDisplays),
    IamfMixGain(IamfMixGain), IamfDemixingInfo(IamfDemixingInfo), IamfReconGain(IamfReconGain),
    /// Anything we round-trip without interpreting.
    Opaque { kind: SideDataKind, data: Arc<[u8]> },
}

impl VideoFrame {
    #[must_use] pub fn side_data(&self, k: SideDataKind) -> Option<&FrameSideData>;
    pub fn set_side_data(&mut self, d: FrameSideData);      // replaces same-kind entry
    pub fn remove_side_data(&mut self, k: SideDataKind) -> Option<FrameSideData>;
    pub fn side_data_iter(&self) -> impl Iterator<Item = &FrameSideData>;
}
```

Bulk payloads are `Arc<[u8]>`/`Arc<[T]>` so frame cloning stays a refcount bump even for a frame carrying
an ICC profile. `Vec<FrameSideData>` with linear lookup is right: frames carry 0–3 entries.

**`Frame::clone()` is cheap and is the intended way to share a frame.** Every field is either `Copy`, an
`Arc`, or a small `Vec` of `Arc`-backed items; the only real allocation is the `metadata` `Dict` and the
side-data `Vec` spine. A filter passing a frame downstream unmodified clones it and the pixel data is
never touched.

### 13.8 Construction

```rust
impl VideoFrame {
    /// Allocate from a pool. The normal path; allocation-free in steady state.
    pub fn alloc_pooled(pool: &FramePool, fmt: PixelFormat, w: u32, h: u32)
        -> Result<Self, Error>;
    /// Allocate standalone.
    pub fn alloc(fmt: PixelFormat, w: u32, h: u32, align: usize) -> Result<Self, Error>;
    /// Wrap existing blocks (decoder output that was written elsewhere, or a hardware map).
    pub fn from_planes(fmt: PixelFormat, w: u32, h: u32, planes: [Option<Plane>; 4])
        -> Result<Self, Error>;
    /// One allocation with all planes at defined offsets, for hardware interop and for the
    /// rare consumer that requires contiguity. Returns pre-split `PlaneMut`s at construction
    /// so 13.5's concurrency story still holds.
    pub fn alloc_contiguous(fmt: PixelFormat, w: u32, h: u32, align: usize)
        -> Result<(Self, ContiguousLayout), Error>;
}
```

`alloc_contiguous` is how (b)-style requirements are met without compromising (a): the split happens once,
at construction, while the block is provably unique, and the resulting `Plane`s each hold the *same*
`Block` with different offsets — which means they are not independently CoW-able and
`VideoFrame::make_writable` copies all of them together. That trade is stated in the constructor's docs
and is why it is not the default.

### 13.9 Dependencies

`vaco-core`, `vaco-pixfmt`, `vaco-sampfmt`, `vaco-chlayout`, `vaco-color`, `vaco-pool`, `thiserror`.
No external runtime dependencies beyond what `vaco-core` pulls in.

Gate assessment (D10): `bytes::Bytes` was the one real candidate — it clears all three gates and provides
refcounted slices. It fails on model: `Bytes` gives cheap *slicing* of immutable buffers, whereas we need
`make_mut`-style copy-on-write with pool return on last-drop, which `BytesMut`'s split/unsplit model does
not express. `Arc<T>` in `std` does exactly what we need in ~40 lines. Everything else here is the media
data model itself.

### 13.10 Test strategy

| Kind | Content |
|---|---|
| Unit | `alloc` for every pixel format at a spread of dimensions; plane sizes and strides match `PixFmtDescriptor::plane_layout`. Cropping arithmetic for every subsampling, including odd crop values that must be rejected for 4:2:0. Side-data set/get/remove/replace semantics. |
| Unit | CoW semantics: `is_writable` true for a fresh frame, false after `clone`; `as_mut_slice` on a shared block leaves the other holder's bytes unchanged; `make_writable` makes every plane unique. |
| Proptest | `clone` then mutate one plane: the clone is byte-identical to the original for every plane. Crop-then-uncrop is the identity on the visible region. `Frame` round-trips through `copy_props`. |
| Concurrency | `std::thread::scope` stress test running the 13.5 pattern across 1 000 frames with random plane assignment, under `--test-threads` pressure. Run the whole crate's test suite under **Miri** in CI: with no unsafe of our own, Miri is checking our `Arc`/`Drop`/aliasing usage and its data-race detector, which is exactly the risk surface here. |
| Pool interaction | Assert the steady-state allocation count is zero: allocate N frames from a pool, drop them, allocate N again, and assert the pool's `allocations` counter did not increase. This is the test that proves architecture §7.4's pooling claim rather than asserting it. |
| Fuzz | `frame_alloc` on arbitrary `(fmt, w, h, align)` — no panic, no overflow, either a valid frame or `LimitExceeded`. `frame_crop` on arbitrary crop values. |
| Differential | None directly — `Frame` is an internal type with no CLI surface. It is validated transitively by every decode differential test, and by the pixel-exact comparisons D6 requires. |

### 13.11 Effort and blocking

**3 person-weeks** (1 for `Block`/`Plane`/CoW and the pool interaction, 1 for the two frame types and
geometry, 0.5 for the side-data enum — which is wide but shallow — 0.5 for the concurrency and Miri
testing). Blocks every decoder, encoder, filter and the scheduler. Needed at v0.1 only in skeletal form
(D5 has no decode), so the side-data payload structs can land incrementally as the codecs that produce them
arrive — but `Block`, `Plane`, the CoW model and the pool interaction must be right from the start,
because changing them later touches everything.

---

## 14. `vaco-packet`

### 14.1 Purpose and API

The compressed-data counterpart to `Frame`: a refcounted byte payload plus timing, stream index, flags and
packet side data (inventory §6's second enumeration).

```rust
#![forbid(unsafe_code)]

#[derive(Debug, Clone)]
pub struct Packet {
    data: Block,                  // same Block type as vaco-frame; pooled and CoW
    logical_len: usize,           // payload length; the block carries >= 64 zero bytes beyond it
    pub stream_index: u32,
    pub pts: Option<Ts>,
    pub dts: Option<Ts>,
    pub duration: i64,
    pub time_base: TimeBase,
    pub pos: Option<u64>,
    pub flags: PacketFlags,       // KEY, CORRUPT, DISCARD, TRUSTED, DISPOSABLE
    side_data: Vec<PacketSideData>,
    pub opaque: Option<Arc<dyn Any + Send + Sync>>,   // scheduler correlation token
}

impl Packet {
    pub fn alloc_pooled(pool: &BufferPool, len: usize) -> Result<Self, Error>;
    pub fn from_slice(data: &[u8]) -> Result<Self, Error>;      // copies into a padded buffer
    #[must_use] pub fn payload(&self) -> &[u8];                  // exactly logical_len bytes
    /// The padded view (F9). Guaranteed >= 64 zero bytes past the payload, so a bit reader
    /// built from it uses the unchecked body path for the entire buffer.
    #[must_use] pub fn payload_padded(&self) -> Padded<'_>;
    #[must_use] pub fn payload_mut(&mut self) -> &mut [u8];      // CoW via Block
    #[must_use] pub fn is_writable(&self) -> bool;
    pub fn make_writable(&mut self);
    /// Zero-copy sub-packet, for splitting an aggregated payload. Shares the Block.
    pub fn slice(&self, range: Range<usize>) -> Result<Self, Error>;
    #[must_use] pub fn side_data(&self, k: PacketSideDataKind) -> Option<&PacketSideData>;
    pub fn set_side_data(&mut self, d: PacketSideData);
    /// Rescale every timestamp field consistently — the operation that gets hand-written and
    /// wrong everywhere in C.
    pub fn rescale_ts(&mut self, to: TimeBase) -> Result<(), Error>;
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum PacketSideData {
    Palette(Arc<[u8]>), NewExtradata(Arc<[u8]>), ParamChange(ParamChange),
    ReplayGain(ReplayGain), DisplayMatrix([i32; 9]), Stereo3d(Stereo3d),
    AudioServiceType(AudioServiceType), QualityStats(QualityStats),
    CpbProperties(CpbProperties), SkipSamples { start: u32, end: u32 },
    MpegtsStreamId(u8), MasteringDisplay(MasteringDisplayMetadata),
    ContentLightLevel { max_cll: u32, max_fall: u32 }, A53Cc(Arc<[u8]>),
    EncryptionInitInfo(Arc<[u8]>), EncryptionInfo(Arc<[u8]>), Afd(u8),
    Prft(ProducerReferenceTime), IccProfile(Arc<[u8]>), DoviConf(DoviConfig),
    S12mTimecode([u32; 4]), DynamicHdr10Plus(Hdr10PlusMetadata),
    FrameCropping(Crop), Lcevc(Arc<[u8]>), RtcpSr(RtcpSenderReport),
    HevcConf(Arc<[u8]>), Exif(Arc<[u8]>), WebvttIdentifier(Box<str>),
    WebvttSettings(Box<str>), SubtitlePosition([u32; 4]),
    MatroskaBlockAdditional { id: u64, data: Arc<[u8]> },
    Opaque { kind: PacketSideDataKind, data: Arc<[u8]> },
}
```

### 14.2 Design notes

**`Packet` always allocates padded.** `alloc_pooled` and `from_slice` request `len + Padded::PAD` bytes and
zero the tail, which makes `payload_padded()` free and gives every parser in the project the fast bit-reader
path (F9) with no per-call cost. The pool's size classes account for the padding, so it does not fragment
the free lists.

**Packets are `Clone`-cheap and immutable in practice.** Demuxer → bitstream filter → decoder passes the
same `Block`; only a BSF that rewrites the payload triggers CoW. `slice()` gives zero-copy splitting for
the aggregation cases (MPEG-TS PES, Matroska laced blocks, ADTS framing) that would otherwise copy.

**`rescale_ts` is one method, not three call sites.** `pts`, `dts` and `duration` must be rescaled with the
same rounding or the stream drifts; making it a single operation removes an entire bug class.

### 14.3 Dependencies, tests, effort

`vaco-core`, `vaco-pool`, `vaco-bitstream` (for `Padded`), `thiserror`. No external runtime deps; same
`bytes` assessment as §13.9.

Tests: CoW and `slice` sharing semantics (proptest: mutating a slice never disturbs the parent's other
bytes); `payload_padded` always yields ≥ 64 zero bytes past `logical_len` (proptest, and this is the
invariant `vaco-bitstream`'s fast path depends on, so it is also asserted in a debug assertion in
`Padded`'s constructor); `rescale_ts` against `vaco_core::rescale_rnd` for arbitrary time bases; fuzz
`packet_from_slice` and `packet_slice`. No direct differential oracle; validated transitively by demuxer
tests, where `ffprobe -show_packets` gives an exact per-packet comparison of every field this struct holds
— which makes it, in practice, very well covered from v0.1 onward.

**1 person-week.** Blocks every demuxer, muxer, parser, BSF and codec. On the v0.1 critical path.

---

## 15. `vaco-pool`

### 15.1 Purpose and API

Recycled, aligned buffers so steady-state decode and filtering do not allocate (architecture §7.4).

```rust
#![forbid(unsafe_code)]

/// A 64-byte-aligned byte buffer. F8: over-allocate and sub-slice; no unsafe, no custom allocator.
#[derive(Debug, Default)]
pub struct AlignedBuf { raw: Vec<u8>, offset: usize, len: usize }

impl AlignedBuf {
    #[must_use]
    pub fn new(len: usize, align: usize) -> Self {
        debug_assert!(align.is_power_of_two());
        let raw = vec![0u8; len + align - 1];
        // `addr()` is a safe operation; the Vec is never reallocated afterwards, so the
        // alignment we compute here holds for the buffer's whole life.
        let offset = raw.as_ptr().addr().wrapping_neg() & (align - 1);
        Self { raw, offset, len }
    }
    #[must_use] pub fn as_slice(&self) -> &[u8]          { &self.raw[self.offset..self.offset + self.len] }
    #[must_use] pub fn as_mut_slice(&mut self) -> &mut [u8] { &mut self.raw[self.offset..self.offset + self.len] }
    #[must_use] pub const fn len(&self) -> usize          { self.len }
    #[must_use] pub fn capacity(&self) -> usize           { self.raw.len() }
    /// Reuse for a different logical length that still fits. Zeroes the tail when `zero_tail`.
    pub fn reshape(&mut self, len: usize, align: usize, zero_tail: usize) -> bool;
}

#[derive(Debug)]
pub struct BufferPool { inner: Arc<PoolInner> }

#[derive(Debug)]
pub struct PoolInner {
    /// Size-classed free lists. Classes are powers of two from 4 KiB up, plus an exact-size
    /// class for the frame-plane sizes the pool has actually seen.
    free: Mutex<Vec<Vec<AlignedBuf>>>,
    align: usize,
    max_retained_bytes: usize,
    /// Observability: these back the steady-state-allocation test in §13.10.
    stats: PoolStats,
}

impl BufferPool {
    #[must_use] pub fn new(align: usize, max_retained_bytes: usize) -> Self;
    /// Pop a buffer of at least `len` from the appropriate class, or allocate.
    pub fn acquire(&self, len: usize) -> AlignedBuf;
    #[must_use] pub fn stats(&self) -> PoolStatsSnapshot;   // allocations, hits, retained_bytes
    /// Drop everything retained. Called when geometry changes (resolution switch).
    pub fn clear(&self);
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PoolStatsSnapshot { pub allocations: u64, pub hits: u64, pub retained_bytes: usize }

/// Frame-shaped pool: caches plane sets keyed by (format, width, height).
#[derive(Debug)]
pub struct FramePool { /* … */ }
impl FramePool {
    #[must_use] pub fn new(align: usize, max_retained_bytes: usize) -> Self;
    pub fn acquire_video(&self, fmt: PixelFormat, w: u32, h: u32) -> Result<Vec<AlignedBuf>, Error>;
    pub fn acquire_audio(&self, fmt: SampleFormat, planes: u32, bytes: usize) -> Result<Vec<AlignedBuf>, Error>;
    /// Geometry change: discard the cached class rather than growing without bound.
    pub fn reconfigure(&self, fmt: PixelFormat, w: u32, h: u32);
}
```

### 15.2 Design notes

**`Mutex<Vec<Vec<AlignedBuf>>>`, not a lock-free stack.** Acquire/release happen once per frame or per
plane — order 10³/s, not 10⁹/s. An uncontended `Mutex` is ~20 ns; the lock-free alternative costs a
dependency and an unsafe surface for a saving that will not appear in any profile. Revisit only with a
benchmark showing contention, at which point `crossbeam` clears the D10 gates and is the right answer
(§3.2).

**The pool is bounded; FFmpeg's is not.** `max_retained_bytes` caps what the free lists hold; buffers
beyond the cap are dropped on return. Unbounded pooling turns a resolution-switching stream into a memory
leak, and D6 names unbounded allocation as a fuzz finding — so the bound is a correctness property, not a
tuning knob.

**Recycling is driven by `Drop`, not by an explicit release.** `vaco-frame`'s `BlockInner::drop` calls
`PoolInner::recycle`. The pool exposes no `release` method at all, so it is impossible to return a buffer
twice or to forget to return one.

**Zeroing policy.** Buffers are zeroed on first allocation and *not* re-zeroed on recycle, except for the
padding tail, which `Packet` requires to be zero (F9) and which `reshape` re-zeros. This is a deliberate
performance/hygiene trade: recycled pixel buffers may contain a previous frame's data, which is fine
within one process and is what every media library does; the padding is the one region whose contents are
load-bearing.

### 15.3 Dependencies, tests, effort

`vaco-core`, `std`. No external runtime dependencies.

Gate assessment (D10): object-pool crates exist and clear the gates, but none models size classes,
alignment, a retention bound and last-`Arc`-drop return together; the whole crate is ~250 lines and it is
the allocation hot path (§3.4 point 3). Write our own.

Tests: alignment holds for every `(len, align)` combination in a proptest sweep (asserted via
`as_ptr().addr() % align == 0`); acquire/drop cycles show zero allocations after warm-up; the retention
bound is never exceeded; `reshape` preserves alignment and zeroes exactly the requested tail; a
multi-threaded stress test with 16 threads acquiring and dropping concurrently, run under Miri for the
race detector. Fuzz `pool_acquire` on arbitrary size sequences, asserting bounded `retained_bytes`.

**1 person-week.** Blocks `vaco-frame` and `vaco-packet`, and therefore everything.

---

## 16. Test and CI matrix for Layer 0/1

| Gate | What runs | Blocking |
|---|---|---|
| `just fmt-check` | `cargo fmt --check` | yes |
| `just lint` | `cargo clippy --workspace --all-targets --all-features -- -D warnings` | yes |
| `just lint-attrs` | `#![forbid(unsafe_code)]` present in every non-allowlisted crate | yes |
| `just layer-check` | acyclic, strictly-downward crate layering | yes |
| `just test` | unit + proptest, on `--no-default-features`, `default`, and `full-rf` | yes |
| `just miri` | `cargo miri test -p vaco-frame -p vaco-pool -p vaco-packet` | yes (these three only — Miri is slow and the rest have no aliasing surface) |
| `just dep-report` | `cargo-deny` + `xtask dep-gate` + `cargo-audit` (+ `cargo-geiger` weekly) | yes |
| `just docs-check` | a `docs/` entry per crate | yes |
| `just conformance` | the differential harnesses listed below | yes |
| `just fuzz-all` | 60 s per target on PR; 4 h nightly with corpus minimisation | nightly blocking |
| `just bench-compare` | `divan` with regression thresholds | advisory on PR, blocking on >5% regression |

**Differential oracles available at Layer 0/1**, ranked by value — every one of these is black-box
observation of a shipped binary, which D6 §2 explicitly sanctions:

1. `ffmpeg -h full` → the entire option model: names, types, defaults, ranges, flag columns, unit
   constants. Validates `vaco-opts` and every option table in the project. **Build this first.**
2. `ffprobe -show_pixel_formats -of json` and `ffmpeg -pix_fmts` → the whole `vaco-pixfmt` table.
3. `ffmpeg -f lavfi -i "aevalsrc='<expr>'" -f f64le -` → numeric ground truth for arbitrary expressions.
   Validates `vaco-expr` including the semantics we would otherwise guess.
4. `ffmpeg -layouts` → the complete `vaco-chlayout` channel and layout tables.
5. `ffmpeg -sample_fmts` → the complete `vaco-sampfmt` table.
6. `ffmpeg -f lavfi -i color=c=<spec>` → `vaco_core::parse::color` over a generated corpus.
7. `ffmpeg -h full` colour option constants + `ffprobe -show_streams` → `vaco-color` name mappings.
8. `ffmpeg -bsf:v trace_headers` → NAL boundaries for `vaco-bitstream`'s Annex-B splitting.
9. `ffmpeg -hide_banner -loglevel debug` → detected CPU flag set, for `vaco_simd::cpu_flag_names`.

Fuzz targets landing with Layer 0/1 (D6: a component without a fuzz target is not done): `parse_duration`,
`parse_color`, `parse_image_size`, `parse_video_rate`, `dict_parse_string`, `escape_unescape`,
`opts_set_from_string`, `opts_flags_parse`, `expr_parse`, `expr_eval`, `bitreader_arbitrary`,
`golomb_arbitrary`, `annexb_nal_iter`, `rbsp_roundtrip`, `scan_start_code`, `pixfmt_from_name`,
`pixfmt_plane_layout`, `chlayout_parse`, `frame_alloc`, `frame_crop`, `packet_from_slice`, `packet_slice`,
`pool_acquire`. Twenty-three targets, all cheap, all landing with their crate.

---

## 17. Effort, sequencing and parallel execution

| Crate | Person-weeks | Blocks | v0.1 critical path (D5) |
|---|---|---|---|
| `vaco-core` | 2.0 | everything | yes |
| `vaco-pool` | 1.0 | frame, packet | yes |
| `vaco-bitstream` | 2.5 | every parser and codec | yes |
| `vaco-simd` | 3.0 | all DSP; bitstream scanning | partial (scan only) |
| `vaco-opts` (+ derive) | 5.0 | cli-core, every component | yes |
| `vaco-expr` | 2.5 | filter-core, most video filters | no |
| `vaco-pixfmt` | 3.0 | frame, scale, video codecs/filters | yes |
| `vaco-sampfmt` | 0.5 | audio codecs, resample | yes |
| `vaco-chlayout` | 1.5 | audio codecs, resample | yes |
| `vaco-color` | 2.0 | scale, HDR metadata | partial (names only) |
| `vaco-frame` | 3.0 | codecs, filters, scheduler | skeletal |
| `vaco-packet` | 1.0 | demuxers, muxers, codecs | yes |
| **Total** | **27.0** | | |

At five engineers this is **6–7 calendar weeks** including review and integration, assuming the sequencing
below. At three engineers it is 10–11 weeks and `vaco-opts` becomes the critical path.

**Week 0 — interface freeze (2–3 days, one engineer, everyone reviewing).** Land every crate as a
compiling skeleton: full public API signatures, `todo!()`-free stubs returning
`Err(Error::Unsupported)`, and the workspace configuration from §1. Nothing else starts until this
merges, because it is what makes the rest parallel. Deliverables: workspace `Cargo.toml`,
`rust-toolchain.toml`, `clippy.toml`, `rustfmt.toml`, `deny.toml`, `Justfile`, `xtask`, twelve stub
crates, twelve `docs/*.md` skeletons, and the `docs/dependencies.md` entries for the seven external
crates in §3.1.

**Weeks 1–6, five parallel tracks:**

| Track | Weeks 1–2 | Weeks 3–4 | Weeks 5–6 |
|---|---|---|---|
| A — Core | `vaco-core` | `vaco-pool` | `vaco-packet`, integration |
| B — Options | `vaco-opts` runtime + `-h full` differential harness | `vaco-opts-derive` | derive polish, compile-fail tests, adoption by tracks C/D |
| C — Formats | `vaco-pixfmt` differential extractor + generator | `vaco-sampfmt`, `vaco-chlayout` | `vaco-color` |
| D — Bitstream | `vaco-bitstream` reader/Golomb | writer, Annex-B, benchmarks | `vaco-expr` |
| E — SIMD/Frame | `vaco-simd` scaffolding + `scan` | `vaco-simd` idioms + benches | `vaco-frame` |

Dependencies between tracks are satisfied by the week-0 stubs; each track integrates against real
implementations as they land. The two hard serialisation points: `vaco-frame` (track E, week 5) needs
`vaco-pixfmt` (track C) and `vaco-pool` (track A) done, and `vaco-opts`' derive (track B, week 3) needs
`vaco-opts`' runtime traits frozen — both are satisfied by the schedule above with a week of slack.

**Highest-risk items, and what to do about them:**

1. **`vaco-opts`' derive is the largest single unknown.** Mitigation: build the `-h full` differential
   harness *before* the macro, so "is the schema right" is answerable from day one, and write the runtime
   first so the macro's only job is projection.
2. **`vaco-pixfmt`'s 268 formats.** Mitigation: the differential extractor is the acceptance criterion and
   is written first; the family declarations are then iterated until it reports zero divergences.
3. **~~F5's build-time SIMD selection may prove insufficient.~~ Resolved by D12** — runtime dispatch via
   `fearless_simd` (F5′, §5.3) removes this risk entirely. **Replaced by:** *a `fearless_simd` operation
   gap may prove fatal on a hot kernel* (§5.6). Mitigation: the `KernelSet` abstraction makes
   the escalation a one-file change, and the measurement that would trigger it is a scheduled benchmark,
   not a vibe.
4. **`vaco-expr`'s exact semantics are underspecified by the inventory.** Mitigation: the `aevalsrc`
   oracle answers every question empirically; budget the semantics-pinning work explicitly (it is 0.75 of
   the 2.5 weeks).

---

## 18. Documentation deliverables

Per the repository standard and architecture §9, each crate lands with its `docs/` entry in the same
change — what it is, how it works, how to change it, configuration, dependencies:

`docs/README.md` (index) · `docs/core.md` · `docs/simd.md` · `docs/opts.md` · `docs/expr.md` ·
`docs/bitstream.md` · `docs/pixfmt.md` · `docs/sampfmt.md` · `docs/chlayout.md` · `docs/color.md` ·
`docs/frame.md` · `docs/packet.md` · `docs/pool.md`

Plus four cross-cutting documents this plan implies:

- `docs/dependencies.md` — the D10 adoption record. Required before first merge.
- `docs/workspace.md` — profiles, toolchain pinning, the lint set and why each promoted lint is denied,
  the `Justfile` target list.
- `docs/testing.md` — the differential oracles in §16, how to run them, and the divergence allowlist
  (which currently has exactly one entry: `vaco-expr`'s loop fuel, §7.6).
- `docs/simd-dispatch.md` — F5′ in full (§5.3), the `fearless_simd` adapter boundary, the §5.6 gap table
  with its compositions, and the measurement that would
  trigger a D2 amendment. This one matters because it is the decision most likely to be questioned later.
