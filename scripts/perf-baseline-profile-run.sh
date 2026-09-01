#!/bin/bash
# Record + symbolicate a samply profile for one workload.
#
# Requires a `cargo build --profile dist` binary (private --target-dir), and
# a dSYM produced by running `dsymutil` on that binary -- see
# planning/PERF-BASELINE.md section 2 for the full recipe and why every step
# here is load-bearing (presymbolication alone resolves almost nothing).
#
# Env vars: SCRATCH (working dir for output files, default: cwd),
# VACO_BIN (default: $SCRATCH/target/dist/vaco),
# VACO_DSYM (default: $SCRATCH/vaco.dSYM/Contents/Resources/DWARF/vaco).
#
# Usage: profile_run.sh <out_prefix> <label> -- <vaco args...>
set -euo pipefail
SCRATCH="${SCRATCH:-.}"
VACO="${VACO_BIN:-$SCRATCH/target/dist/vaco}"
DSYM="${VACO_DSYM:-$SCRATCH/vaco.dSYM/Contents/Resources/DWARF/vaco}"

OUT_PREFIX="$1"; shift
LABEL="$1"; shift
if [ "$1" != "--" ]; then echo "expected --"; exit 1; fi
shift

echo "=== recording $LABEL ===" >&2
samply record --rate 4000 --save-only -o "$SCRATCH/${OUT_PREFIX}.json.gz" -- "$VACO" "$@" >"$SCRATCH/${OUT_PREFIX}.samply.log" 2>&1
echo "=== symbolicating $LABEL ===" >&2
python3 "$SCRATCH/symbolicate.py" "$SCRATCH/${OUT_PREFIX}.json.gz" "$DSYM" vaco --top 20 \
  > "$SCRATCH/${OUT_PREFIX}.result.json" 2> "$SCRATCH/${OUT_PREFIX}.result.log"
echo "done: $SCRATCH/${OUT_PREFIX}.result.json" >&2
tail -30 "$SCRATCH/${OUT_PREFIX}.result.log"
