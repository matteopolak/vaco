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
ci: fmt-check clippy check-all test-all licence licence-report-check layer-check dep-gate unsafe-audit \
    wasm-check time-gate patent-gate owner-gate dup-check provenance-check docs-check fuzz-check

# ------------------------------------------------------- policy gates (D3/D10)

# D3: licence allowlist + the [bans] that enforce D10 Gate 1 structurally.
#
# Includes `advisories` (D10 Gate 3) as of QA-10 (#182): it used to be left
# out here, and neither this recipe nor the separate `audit` recipe below
# was wired into `ci`, so RUSTSEC vulnerabilities were never actually gated.
# Running `cargo deny check` for the first time (2026-08-28) found real,
# reachable advisories that had been shipping unnoticed; see deny.toml's
# `[advisories]` comments for what was found and how each was assessed.
licence:
    cargo deny check

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

# D15 / plan 13 §6: every large constant table names its source, and every
# commit touching implementation code carries its provenance trailers.
provenance-check:
    cargo xtask provenance-check

# `fuzz/` is its own workspace (see gen-fuzz above), so neither check-all nor
# clippy ever compiles it — a target rots silently against an ordinary,
# correct API change until someone tries to fuzz with it. Build-only: an
# actual run belongs in `fuzz-all`/CI's `fuzz-regressions` job, not a gate
# every `just ci` pays for. Skips with a message rather than failing when
# nightly + cargo-fuzz are not installed — see `xtask::fuzz_check` for why
# that is the right degrade for this one, unlike wasm-check.
fuzz-check:
    cargo xtask fuzz-check

# Install the git hooks and, on first run, take the DCO + clean-room
# attestation. Run once per clone; see docs/provenance.md.
setup:
    git config core.hooksPath .githooks
    @echo "hooks installed (core.hooksPath = .githooks)"
    @test -f .git/vaco-attestation || echo "run .githooks/attest to record your DCO + clean-room attestation"

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

# The format-coverage table backs a claim in
# docs/why-some-formats-are-not-included.md that a hand-written list would
# falsify within a week.
gen-coverage:
    cargo xtask gen-coverage

gen:  gen-registry gen-docs-index gen-pixfmt gen-fuzz gen-coverage

# CI re-runs the generators and fails if the committed output differs.
docs-check:
    cargo xtask gen-registry --check
    cargo xtask gen-docs-index --check
    cargo xtask gen-pixfmt --check
    cargo xtask gen-fuzz --check
    cargo xtask gen-coverage --check

# ------------------------------------------------------------- conformance

# The differential harness: run both binaries over every case and diff.
#
# Media is synthesised by the reference at run time and discarded (D6), so this
# needs `ffmpeg`/`ffprobe` on PATH but no corpus. Without them every case skips
# rather than failing, which is deliberate — `cargo test` must pass on a machine
# that has no reference (plan 13 §1.5.4).
#
# Builds both binaries under test: `vaco-probe` for the `probe` tool suites,
# `vaco` for the `transcode` tool suites (`tests/conformance/transcode/`) —
# a suite whose binary is not built skips with a message naming which
# `VACO_BIN_*` variable would point at it, rather than a mystery zero.
conformance tier="core":
    cargo build -p vaco-probe -p vaco-cli {{TD}}
    target_dir="$(cargo metadata --format-version 1 --no-deps | \
        sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"; \
      VACO_BIN_PROBE="$target_dir/debug/vaco-probe" \
      VACO_BIN_VACO="$target_dir/debug/vaco" \
      cargo run -p vaco-conformance {{TD}} -- run --tier {{tier}}

# One case, by the id printed with every failure — reproduces it regardless of
# its declared tier.
conformance-run case:
    cargo build -p vaco-probe -p vaco-cli {{TD}}
    target_dir="$(cargo metadata --format-version 1 --no-deps | \
        sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"; \
      VACO_BIN_PROBE="$target_dir/debug/vaco-probe" \
      VACO_BIN_VACO="$target_dir/debug/vaco" \
      cargo run -p vaco-conformance {{TD}} -- run --case "{{case}}"

# ------------------------------------------------------ benchmarks & profiling

bench filter="":
    cargo bench --workspace {{TD}} -- {{filter}}

# Verify every SIMD kernel against its scalar reference (plan 12 §5).
checkasm:
    cargo run --release -p vaco-checkasm {{TD}}

checkasm-bench:
    cargo run --release -p vaco-checkasm {{TD}} -- --bench

# Two-pass PGO build (plan 12 §7). NOT IMPLEMENTED — PF-0.8 (#98) has not been
# built. This recipe called `cargo xtask pgo`, which is not a subcommand, so it
# failed with an unhelpful clap error that read like a broken install.
build-pgo:
    @echo "build-pgo is not implemented yet — see PF-0.8 (#98)." >&2
    @exit 1

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

# Watch free space while a wave builds, reclaiming build outputs under a floor.
# Reports Steam, container and node_modules candidates rather than deleting
# them: reclaiming something a human has to re-download is a decision, and an
# unattended loop is the wrong place to make one.
disk-watch minutes="45" floor="25":
    ./scripts/disk-watch.sh {{minutes}} {{floor}}

# Every library still builds for wasm32 (D18).
wasm-check:
    cargo xtask wasm-check

# The OS clock is reached only through vaco-time (D18).
#
# The companion to `wasm-check`, and it catches what that one structurally
# cannot: `std::time::Instant::now()` compiles for wasm32 and panics at run
# time, so a crate can pass the compile gate and still be unusable.
time-gate:
    cargo xtask time-gate

# No patent-encumbered component is in the default build (D4).
#
# Asserts on the *compiled* feature list rather than on what a manifest claims,
# which is D4's own wording. Building an encumbered codec yourself is supported
# and expected; shipping one is not.
patent-gate:
    cargo xtask patent-gate

# Each third-party media crate has exactly one owner (D11).
#
# Deliberately a named list rather than "every external dependency": bitflags is
# in ten crates and smallvec in six, and neither is what D11 guards. A gate that
# fires ten times on its first run for reasons nobody can act on is one people
# learn to suppress.
owner-gate:
    cargo xtask owner-gate

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

# The feature that gates one fuzz target's own crate, read out of the
# generated manifest (`cargo xtask gen-fuzz`): every path dependency there is
# `optional = true`, switched on by the feature named after the crate the
# declaring target needs.
[private]
fuzz-feature target:
    #!/usr/bin/env bash
    set -euo pipefail
    feature=$(sed -n "/^name = \"{{target}}\"\$/,/^required-features/p" fuzz/Cargo.toml \
        | sed -n 's/^required-features = \["\([^"]*\)".*/\1/p')
    if [ -z "$feature" ]; then
        echo "no [[bin]] named {{target}} in fuzz/Cargo.toml" >&2
        exit 1
    fi
    echo "$feature"

# One target, interactively. Ctrl-C to stop.
# `--no-default-features --features <feature>` builds only this target's own
# crate and whatever it references, so a syntax error in an unrelated crate
# elsewhere in the tree cannot block it. `default` still lists every feature,
# so a plain `cargo +nightly fuzz run <target>` (no flags) keeps working when
# the whole tree is healthy.
fuzz target:
    #!/usr/bin/env bash
    set -euo pipefail
    feature=$(just fuzz-feature {{target}})
    cargo +nightly fuzz run {{target}} --no-default-features --features "$feature"

# Every target, for `secs` each. The exit code is the oracle — see plan 19 §13.
# Reports the exec count per target so a target that never ran cannot pass.
# Each target is scoped to its own feature (see `fuzz-feature` above), so one
# crate that does not compile fails only the targets that depend on it.
fuzz-all secs="120":
    #!/usr/bin/env bash
    set -uo pipefail
    log=$(mktemp -d); fail=0
    for t in $(cargo +nightly fuzz list); do
        feature=$(just fuzz-feature "$t") || { fail=1; continue; }
        cargo +nightly fuzz run "$t" --no-default-features --features "$feature" \
            -- -max_total_time={{secs}} -rss_limit_mb=4096 \
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

# Coverage-guided corpus minimisation (issue #571's "cmin wiring" half):
# shrink `fuzz/corpus/<target>` in place to the smallest subset that still
# reaches the same coverage, via libFuzzer's own `-merge`. Operates on
# whatever corpus already exists (`just corpus-fetch`/an existing fuzz run
# populate it); a target with no corpus yet has nothing to minimise.
# Scoped to the target's own feature, same as `fuzz`/`fuzz-all` above, so an
# unrelated crate's build failure cannot block it.
corpus-minimise target:
    #!/usr/bin/env bash
    set -euo pipefail
    feature=$(just fuzz-feature {{target}})
    mkdir -p fuzz/corpus/{{target}}
    before=$(find fuzz/corpus/{{target}} -type f | wc -l | tr -d ' ')
    cargo +nightly fuzz cmin {{target}} --no-default-features --features "$feature"
    after=$(find fuzz/corpus/{{target}} -type f | wc -l | tr -d ' ')
    echo "corpus-minimise {{target}}: $before -> $after files"

# The "semantic minimiser" half of #571: shrink one crash-reproducing input
# structurally — whole ISO base media boxes or EBML elements, with parent
# size fields patched, not just contiguous byte ranges — before falling back
# to byte-level delta-debugging for whatever structure can't explain. See
# `fuzz/src/bin/semantic_min.rs`'s module doc for why `cmin`/`tmin` alone
# leave this on the table. `file` must already reproduce a crash in `target`
# (e.g. something under `fuzz/artifacts/<target>/` or `fuzz/seeds/<target>/`
# before it was fixed); the result is written next to it as `<file>.min`.
semantic-min target file *ARGS:
    cargo run --manifest-path fuzz/Cargo.toml --bin semantic_min \
        --no-default-features -- {{target}} {{file}} {{ARGS}}

# The differential prober (XF-04): mutate a family's seed media and check
# vaco-probe against ffprobe on every mutant. Needs `ffprobe` on PATH.
# `family` names a directory under fuzz/seeds/diff/ (mp4, matroska, mpegts,
# wav, ogg, flv, avi, mpegps, image2, aiff, caf, w64, nut). `mutator` is
# `generic` (structure-blind) or `aware` (biases toward chunk/box length
# fields and Ogg/FLV header fields it can recognise by shape — see
# `fuzz/src/bin/diff_probe.rs`'s module doc). Findings land in
# fuzz/seeds/diff/findings/<family>/ as a `.bin` + `.toml` pair; a clean run
# touches nothing there. `diff_probe` itself has no vaco-* dependency, so
# `--no-default-features` keeps its build independent of every other crate in
# the tree, same as any fuzz target.
diff-fuzz family iterations="500" mutator="generic":
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build -p vaco-probe --release {{TD}}
    cargo build --bin diff_probe --no-default-features --manifest-path fuzz/Cargo.toml
    ./fuzz/target/debug/diff_probe campaign \
        --seed-dir fuzz/seeds/diff/{{family}} \
        --vaco-probe {{VACO_TARGET_DIR}}/release/vaco-probe \
        --iterations {{iterations}} \
        --mutator {{mutator}} \
        --out fuzz/seeds/diff/findings/{{family}}

# The cadence half of XF-04: re-run every family at the committed baseline's
# own iteration count and `--rng-seed` (both fixed, so a clean tree reproduces
# the stored tally exactly) and report drift against
# fuzz/seeds/diff/baseline.txt — a changed count instead of a human having to
# notice one. Exits non-zero (per family, and if any family drifted) so this
# is CI-shaped. Pass `update="--update-baseline"` after intentionally
# accepting a new tally (a fix that closes real mismatches, a new seed) to
# re-record it; anything else leaves the baseline untouched.
diff-fuzz-baseline update="":
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build -p vaco-probe --release {{TD}}
    cargo build --release --bin diff_probe --no-default-features --manifest-path fuzz/Cargo.toml
    fail=0
    for family in mp4 matroska mpegts wav ogg flv avi mpegps image2 aiff caf w64 nut; do
        ./fuzz/target/release/diff_probe campaign \
            --seed-dir fuzz/seeds/diff/$family \
            --vaco-probe {{VACO_TARGET_DIR}}/release/vaco-probe \
            --iterations 500 --rng-seed 42 \
            --baseline fuzz/seeds/diff/baseline.txt {{update}} \
            --out fuzz/seeds/diff/findings/$family || fail=1
    done
    exit $fail

corpus-fetch:
    cargo xtask corpus-fetch

# --------------------------------------------------------------------- misc

docs:
    cargo doc --workspace --no-deps {{TD}}

# THIRD_PARTY_LICENSES.html for release artifacts (QA-10, #182): every
# linked Cargo dependency (cargo-about over about.toml) PLUS every
# permissively-licensed reference implementation a crate was translated
# from but never linked as a dependency (provenance/third-party-notices.toml)
# -- see scripts/gen_third_party_notices.py's own docstring for why both
# halves are a real legal requirement, not just the first one. Needs
# `cargo install cargo-about --locked --features cli` once per machine.
licence-report:
    python3 scripts/gen_third_party_notices.py

# CI-shaped: fails if a provenance/*.toml source looks like a permissively
# licensed reference implementation with no corresponding entry in
# third-party-notices.toml, or if cargo-about itself can't resolve a
# dependency's licence. Does not require `cargo about` to already be
# installed to catch the coverage half of that.
licence-report-check:
    python3 scripts/gen_third_party_notices.py --check

# ---------------------------------------------------------- release engineering (QA-10, #182)

# SPDX + CycloneDX SBOMs for both shipped binaries, written to dist/sbom/
# (release artifacts, not committed -- see .gitignore). `cargo-sbom` reads
# `cargo metadata` directly (no per-crate file scatter, unlike
# `cargo-cyclonedx`, which writes a *.cdx.json into every crate directory
# it touches -- unusable in a shared tree where those directories belong to
# other agents). Needs `cargo install cargo-sbom --locked` once per machine.
sbom:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p dist/sbom
    for pkg in vaco-cli vaco-probe; do
        for fmt in spdx_json_2_3 cyclone_dx_json_1_5; do
            out="dist/sbom/${pkg}.${fmt}.json"
            cargo sbom --cargo-package "$pkg" --output-format "$fmt" > "$out"
            echo "wrote $out"
        done
    done

# Builds vaco-cli and vaco-probe twice, from separate target directories,
# and diffs them byte-for-byte -- see scripts/verify-reproducible-build.sh's
# own header for what a mismatch triggers, and for why `--profile release`
# (the default here) and `--profile dist` (what actually ships -- run as
# `just verify-reproducible --profile dist`) are NOT the same question:
# measured 2026-08-28, only the former currently reproduces.
verify-reproducible *packages:
    ./scripts/verify-reproducible-build.sh {{packages}}

# One release: binaries, checksums, SBOM, attribution file, all under
# dist/. Does NOT sign or notarize -- see docs/release-engineering.md for
# why that is a separate, credential-gated step the owner runs, not this
# recipe.
release-package:
    ./scripts/package-release.sh
