# Vaco — the single entry point for every developer command.
#
# MULTI-AGENT BUILDS (plan 19 §4). Cargo takes an exclusive lock per target
# directory, so agents sharing one `target/` serialise on it. Every cargo recipe
# below threads a private target dir through as a FLAG.
#
#   VACO_TARGET_DIR=/tmp/vaco-flac-k3f9 just check vaco-codec-flac
#
# Pass it as `--target-dir`, never as the CARGO_TARGET_DIR environment variable:
# sccache hashes CARGO_* env vars into its cache keys, and the env-var form
# measured 0% cache hits where the flag form measured 78-94%.
#
# Delete your directory when you finish, by its literal name. Never glob.

VACO_TARGET_DIR := env_var_or_default("VACO_TARGET_DIR", "target")
JOBS            := env_var_or_default("VACO_JOBS", "4")
TD              := "--target-dir " + VACO_TARGET_DIR + " -j " + JOBS

default:
    @just --list

# ---------------------------------------------------------------- build & test

# Check one crate (what an agent runs constantly).
check crate:
    cargo check -p {{crate}} --all-targets --locked {{TD}}

# Check the whole workspace (a wave-boundary activity, not an agent activity).
check-all:
    cargo check --workspace --all-targets --locked {{TD}}

build:
    cargo build --workspace --locked {{TD}}

# Cranelift dev build: 2-7x faster compiles, needs nightly. Optional convenience,
# NOT a requirement — D12 moved the project to stable, so this is opt-in.
build-fast:
    RUSTUP_TOOLCHAIN=nightly CARGO_PROFILE_DEV_CODEGEN_BACKEND=cranelift \
      cargo +nightly build -Zcodegen-backend --workspace {{TD}}

test crate:
    cargo test -p {{crate}} --locked --no-fail-fast {{TD}}

test-all:
    cargo test --workspace --locked --no-fail-fast {{TD}}

# ------------------------------------------------------------------- hygiene

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --workspace --all-targets --locked {{TD}} -- -D warnings

# Every gate CI runs, in the order CI runs them.
ci: fmt-check clippy check-all test-all licence layer-check dep-gate unsafe-audit docs-check

# ------------------------------------------------------- policy gates (D3/D10)

# D3: licence allowlist + the [bans] that enforce D10 Gate 1 structurally.
licence:
    cargo deny check licenses bans sources

# RUSTSEC advisories (D10 Gate 3).
audit:
    cargo audit

# Measure unsafe in the dependency graph. The forbid(unsafe_code) guarantee is
# about OUR code; this is how we see the rest (D10, D12).
geiger:
    cargo geiger --workspace

# D10 Gate 1, structurally: fail on any `links` key or third-party build.rs.
dep-gate:
    cargo xtask dep-gate

# The layer graph in planning/10-architecture.md must stay acyclic and downward.
layer-check:
    cargo xtask layer-check

# Assert `unsafe` appears only in the crates D2/D13 permit.
unsafe-audit:
    cargo xtask unsafe-audit

# Assert patent-encumbered features are absent from a default build (D4).
patent-check:
    cargo xtask patent-check

# ------------------------------------------------------------ generated files

# The registry is assembled from per-crate vaco-component.toml fragments so that
# ~120 crates can register themselves with zero contention (plan 19 §3.4).
gen-registry:
    cargo xtask gen-registry

gen-docs-index:
    cargo xtask gen-docs-index

# The 268-entry pixel-format table is generated from a declarative family
# description, not hand-written: hand-maintaining that much metadata guarantees
# silent drift, and drift here corrupts every frame that touches the format.
gen-pixfmt:
    cargo xtask gen-pixfmt

gen-fuzz:
    cargo xtask gen-fuzz

gen:  gen-registry gen-docs-index gen-pixfmt gen-fuzz

# CI re-runs the generators and fails if the committed output differs.
docs-check:
    cargo xtask gen-registry --check
    cargo xtask gen-docs-index --check
    cargo xtask gen-pixfmt --check
    cargo xtask gen-fuzz --check

# ------------------------------------------------------ benchmarks & profiling

bench filter="":
    cargo bench --workspace {{TD}} -- {{filter}}

# Verify every SIMD kernel against its scalar reference (plan 12 §5).
checkasm:
    cargo run --release -p vaco-checkasm {{TD}}

checkasm-bench:
    cargo run --release -p vaco-checkasm {{TD}} -- --bench

# Two-pass PGO build (plan 12 §7).
build-pgo:
    cargo xtask pgo

# ------------------------------------------------------------- conformance (D6)

# Differential comparison against the pinned reference ffmpeg binary.
conformance suite="smoke":
    cargo run --release -p vaco-conformance {{TD}} -- --suite {{suite}}

# Reclaim build scratch. Safe to run mid-wave: it never touches a target dir a
# running agent owns, only the orchestrator's own and stale ones.
#
# Concurrent agents each carry a private target dir (plan 19 §4), so a wave of
# six can hold 10-20 GiB between them on top of sccache. One wave took the disk
# from comfortable to 98% full.
disk-report:
    #!/usr/bin/env bash
    df -h /System/Volumes/Data | tail -1
    echo "--- build scratch ---"
    du -sh /tmp/vaco-* 2>/dev/null | sort -rh | head -10
    echo "--- sccache ---"
    sccache --show-stats 2>/dev/null | grep -iE "cache size|max cache" || true

# Remove the orchestrator's own target dirs and any scratch older than a day.
# Agents' dirs (/tmp/vaco-<crate>-<rand>) are left alone unless stale — an agent
# deletes its own by literal name when it finishes, so a day-old one is orphaned.
disk-clean:
    #!/usr/bin/env bash
    set -uo pipefail
    before=$(df -g /System/Volumes/Data | awk 'NR==2{print $4}')
    rm -rf /tmp/vaco-p0 /tmp/vaco-fresh /tmp/vaco-fuzzlog /tmp/vaco-*.log
    find /tmp -maxdepth 1 -name 'vaco-*' -type d -mtime +1 -print -exec rm -rf {} + 2>/dev/null || true
    after=$(df -g /System/Volumes/Data | awk 'NR==2{print $4}')
    echo "freed $((after - before))GiB; ${after}GiB now free"

# Every library still builds for wasm32 (D18).
wasm-check:
    cargo xtask wasm-check

# One definition per concept (D19).
dup-check:
    cargo xtask dup-check

# Public API that only tests use. A REPORT, not a gate — read it at a wave
# boundary. Expected to be large while the substrate is ahead of its consumers,
# and to shrink as codecs, muxers and filters land.
dead-code:
    cargo xtask dead-code

# Cargo.lock moved only by dependency EDGES, never by packages (plan 19 §3.3).
# Safe to run mid-wave: concurrent agents reconcile the lock against whatever
# manifests exist, and this proves the reconciliation added nothing reviewable.
lock-gate:
    #!/usr/bin/env bash
    set -uo pipefail
    moved=$(git diff HEAD -- Cargo.lock \
        | grep -E '^[+-](name|version|source|checksum|\[\[package\]\])' || true)
    if [ -n "$moved" ]; then
        echo "Cargo.lock moved a PACKAGE, not just an edge — this needs D10 review:"
        echo "$moved"
        exit 1
    fi
    # Consistent with every manifest in the tree, including other agents'.
    # `metadata` resolves the graph WITHOUT compiling, so this stays meaningful
    # mid-wave, when a crate under active construction does not build yet.
    cargo metadata --locked --format-version 1 -q > /dev/null 2>&1 || {
        echo "lock is not consistent with the workspace manifests:"
        cargo metadata --locked --format-version 1 2>&1 >/dev/null | head -20
        exit 1; }
    echo "lock-gate: edges only, consistent with all manifests"

# One target, interactively. Ctrl-C to stop.
fuzz target:
    cargo +nightly fuzz run {{target}}

# Every target, for `secs` each. The exit code is the oracle — see plan 19 §13.
# Reports the exec count per target so a target that never ran cannot pass.
fuzz-all secs="120":
    #!/usr/bin/env bash
    set -uo pipefail
    log=$(mktemp -d); fail=0
    for t in $(cargo +nightly fuzz list); do
        cargo +nightly fuzz run "$t" -- -max_total_time={{secs}} -rss_limit_mb=4096 \
            > "$log/$t.log" 2>&1
        rc=$?
        n=$(grep -oE '^#[0-9]+' "$log/$t.log" | tail -1)
        printf '%-28s exit=%d execs=%s\n' "$t" "$rc" "${n:-NONE}"
        [ "$rc" -eq 0 ] || { fail=1; tail -30 "$log/$t.log"; }
        [ -n "${n:-}" ] || { echo "  ^ no execs: the target never ran"; fail=1; }
    done
    # An artifact on disk is a crash whatever the logs said.
    # Crashes, slow units and OOMs all land here. Only the first makes the run
    # exit non-zero, so the directory is the only evidence for the other two.
    if [ -n "$(find fuzz/artifacts -type f 2>/dev/null)" ]; then
        echo "fuzz artifacts present (crash / slow-unit / oom are all findings):"
        find fuzz/artifacts -type f; fail=1
    fi
    exit $fail

corpus-fetch:
    cargo xtask corpus-fetch

# --------------------------------------------------------------------- misc

docs:
    cargo doc --workspace --no-deps {{TD}}

# THIRD_PARTY.md for release artifacts.
licence-report:
    cargo about generate about.hbs -o THIRD_PARTY.md
