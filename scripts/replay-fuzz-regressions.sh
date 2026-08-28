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
# Exit code: nonzero the moment any seed makes its target crash, so this can
# be a normal blocking CI step. Prints which target/seed as it goes so a
# failure is immediately actionable.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

seeds_dir="fuzz/seeds"
if [ ! -d "$seeds_dir" ]; then
    echo "replay-fuzz-regressions: no $seeds_dir directory, nothing to replay"
    exit 0
fi

failures=0
replayed=0

for target_dir in "$seeds_dir"/*/; do
    target="$(basename "$target_dir")"
    [ "$target" = "diff" ] && continue
    [ "$target" = "README.md" ] && continue

    for seed in "$target_dir"*; do
        [ -f "$seed" ] || continue
        [ "$(basename "$seed")" = "README.md" ] && continue
        replayed=$((replayed + 1))
        echo "replay-fuzz-regressions: $target <- $(basename "$seed")"
        if ! cargo +nightly fuzz run "$target" "$seed" -- -runs=1 >/tmp/replay-fuzz-"$target".log 2>&1; then
            echo "replay-fuzz-regressions: FAILED -- $target/$(basename "$seed") crashes again"
            tail -n 40 /tmp/replay-fuzz-"$target".log
            failures=$((failures + 1))
        fi
    done
done

echo "replay-fuzz-regressions: $replayed seed(s) replayed, $failures failure(s)"
[ "$failures" -eq 0 ]
