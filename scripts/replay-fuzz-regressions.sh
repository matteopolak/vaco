#!/usr/bin/env bash
# QA-05/QA-07: replay every committed regression seed through its own fuzz
# target, once each, as a fast per-PR check rather than a fuzzing campaign.
#
# `fuzz/seeds/<target>/*` holds inputs that once crashed a target and have
# since been fixed (see fuzz/seeds/README.md) -- durable proof a bug stays
# fixed. Today the only documented way to replay one is `cargo +nightly fuzz
# run <target> <path>` by hand (same README); this script is that command,
# looped over every seed, so CI can run the whole set on every PR instead of
# a human remembering to check any particular one.
#
# Every file directly under a target's seed directory is replayed, not just
# ones prefixed `regression-`: the directory holds both renamed seeds
# (`regression-*`) and seeds that kept the crashing run's own artifact name
# (`crash-*`, `oom-*`, `slow-unit-*`, or a free-form description) -- the
# README's own examples mix both, and the directory's whole purpose (per its
# own title, "inputs that once crashed") makes anything sitting in it fair
# game to replay.
#
# `fuzz/seeds/diff/` is a different namespace (real media for the
# differential fuzzer to mutate, not crash regressions) and is skipped.
#
# A failure to BUILD is reported as exactly that, never as a regression.
# Run 33820528109 printed "27 seed(s) replayed, 27 failure(s)" when every
# one of the 27 was the same dependency failing to compile: `cargo fuzz run`
# returns nonzero for a build error and for a crash alike, and this script
# used to attribute both to the seed. Two things stop that now:
#
#   1. `cargo metadata --locked` on the fuzz manifest, before anything is
#      built. `cargo fuzz` has no `--locked` flag, so a crate missing from
#      `fuzz/Cargo.lock` resolves to the newest release at build time; that
#      is how an unpinned `tinyvec` picked up a 1.13.0 that does not compile
#      without `std`. The lock has to be complete and committed, and this is
#      the one place that can enforce it.
#   2. Each target is built once, up front, with `cargo fuzz build`. Only a
#      target that built gets its seeds replayed, so a nonzero `fuzz run`
#      afterwards means the seed, not the toolchain.
#
# Exit code: nonzero if the lock is stale, if any target fails to build, or
# the moment any seed makes its target crash -- each reported in its own
# words, so a failure is immediately actionable.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

seeds_dir="fuzz/seeds"
if [ ! -d "$seeds_dir" ]; then
    echo "replay-fuzz-regressions: no $seeds_dir directory, nothing to replay"
    exit 0
fi

log_dir="${TMPDIR:-/tmp}/replay-fuzz"
mkdir -p "$log_dir"

if ! cargo +nightly metadata --manifest-path fuzz/Cargo.toml --format-version 1 --locked \
        >/dev/null 2>"$log_dir/metadata.log"; then
    echo "replay-fuzz-regressions: BUILD PROBLEM, not a regression -- fuzz/Cargo.lock is" \
         "incomplete or stale for the current manifests. Regenerate it (any" \
         "\`cargo +nightly fuzz build\` rewrites it) and commit fuzz/Cargo.lock."
    tail -n 20 "$log_dir/metadata.log"
    exit 1
fi

targets=()
for target_dir in "$seeds_dir"/*/; do
    target="$(basename "$target_dir")"
    [ "$target" = "diff" ] && continue
    [ "$target" = "README.md" ] && continue
    targets+=("$target")
done

build_failures=0
built=()
for target in "${targets[@]}"; do
    echo "replay-fuzz-regressions: building $target"
    if cargo +nightly fuzz build "$target" >"$log_dir/build-$target.log" 2>&1; then
        built+=("$target")
    else
        echo "replay-fuzz-regressions: BUILD FAILED, not a regression -- $target did not compile"
        tail -n 40 "$log_dir/build-$target.log"
        build_failures=$((build_failures + 1))
    fi
done

failures=0
replayed=0
for target in "${built[@]}"; do
    for seed in "$seeds_dir/$target"/*; do
        [ -f "$seed" ] || continue
        [ "$(basename "$seed")" = "README.md" ] && continue
        replayed=$((replayed + 1))
        echo "replay-fuzz-regressions: $target <- $(basename "$seed")"
        if ! cargo +nightly fuzz run "$target" "$seed" -- -runs=1 >"$log_dir/run-$target.log" 2>&1; then
            echo "replay-fuzz-regressions: FAILED -- $target/$(basename "$seed") crashes again"
            tail -n 40 "$log_dir/run-$target.log"
            failures=$((failures + 1))
        fi
    done
done

echo "replay-fuzz-regressions: ${#targets[@]} target(s), $build_failures build failure(s)," \
     "$replayed seed(s) replayed, $failures regression(s)"
[ "$build_failures" -eq 0 ] && [ "$failures" -eq 0 ]
