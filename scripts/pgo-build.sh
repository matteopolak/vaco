#!/bin/sh
# Build Vaco with LLVM instrumentation, collect the representative workload,
# merge its raw counters, and rebuild with the pinned/use profile.
set -eu

ROOT="$(cd -- "$(dirname -- "$0")/.." && pwd)"
TARGET_DIR=${VACO_PGO_TARGET_DIR:-${VACO_TARGET_DIR:-$ROOT/target/pgo}}
RAW_DIR=${VACO_PGO_RAW_DIR:-$ROOT/pgo-data/raw}
PROFILE=${VACO_PGO_PROFDATA:-$ROOT/pgo-data/vaco.profdata}
MANIFEST=${VACO_PROFILE_MANIFEST:-$ROOT/profile/workload.toml}
LOCKFILE=${VACO_PGO_LOCKFILE:-$ROOT/profile/lockfile.toml}
if [ -z "${LLVM_PROFDATA:-}" ]; then
    # Rust's instrumented objects use the LLVM revision bundled with rustc;
    # Homebrew/system LLVM may be a different raw-profile format version.
    RUSTC_BIN=${RUSTC:-rustc}
    RUST_SYSROOT=$("$RUSTC_BIN" --print sysroot)
    RUST_HOST=$("$RUSTC_BIN" -vV | awk '/^host: / { print $2; exit }')
    BUNDLED_PROFDATA="$RUST_SYSROOT/lib/rustlib/$RUST_HOST/bin/llvm-profdata"
    if [ -x "$BUNDLED_PROFDATA" ]; then
        LLVM_PROFDATA=$BUNDLED_PROFDATA
    else
        LLVM_PROFDATA=$(command -v llvm-profdata || true)
    fi
fi

if [ -z "$LLVM_PROFDATA" ]; then
    echo "pgo-build: llvm-profdata is required (install the LLVM toolchain)" >&2
    exit 2
fi

if [ ! -f "$MANIFEST" ]; then
    echo "pgo-build: manifest not found: $MANIFEST" >&2
    exit 2
fi
if [ ! -f "$LOCKFILE" ]; then
    echo "pgo-build: lockfile not found: $LOCKFILE" >&2
    exit 2
fi
python3 "$ROOT/scripts/pgo.py" check-lock "$LOCKFILE" "$MANIFEST"

mkdir -p "$TARGET_DIR" "$RAW_DIR" "$(dirname -- "$PROFILE")"
if [ -n "${VACO_PGO_PROFILE:-}" ]; then
    PROFILE=${VACO_PGO_PROFILE}
    if [ ! -f "$PROFILE" ]; then
        echo "pgo-build: pinned profile is unavailable: $PROFILE" >&2
        echo "pgo-build: falling back to a plain release build" >&2
        CARGO_INCREMENTAL=0 cargo build --release --locked -p vaco --bins \
            --target-dir "$TARGET_DIR"
        exit 0
    fi
else
    echo "pgo-build: instrumented build" >&2
    rm -f "$RAW_DIR"/*.profraw
    CARGO_INCREMENTAL=0 RUSTFLAGS="${RUSTFLAGS:-} -Cprofile-generate=$RAW_DIR" \
        cargo build --release --locked -p vaco --bins --target-dir "$TARGET_DIR"
    if [ -z "${VACO_PROFILE_FIXTURES:-}" ]; then
        echo "pgo-build: set VACO_PROFILE_FIXTURES before running vaco-profile" >&2
        exit 2
    fi
    "$ROOT/scripts/vaco-profile"
    set -- "$RAW_DIR"/*.profraw
    if [ ! -f "$1" ]; then
        echo "pgo-build: workload produced no .profraw files" >&2
        exit 1
    fi
    echo "pgo-build: merging profile" >&2
    "$LLVM_PROFDATA" merge -sparse -o "$PROFILE" "$@"
fi

echo "pgo-build: checking profile coverage" >&2
DUMP=$(mktemp "${TMPDIR:-/tmp}/vaco-pgo-profdata.XXXXXX")
trap 'rm -f "$DUMP"' EXIT INT TERM
"$LLVM_PROFDATA" show --all-functions --counts "$PROFILE" > "$DUMP"
python3 "$ROOT/scripts/pgo.py" check-profile "$MANIFEST" "$DUMP"

echo "pgo-build: profile-use build" >&2
CARGO_INCREMENTAL=0 RUSTFLAGS="${RUSTFLAGS:-} -Cprofile-use=$PROFILE -Cllvm-args=-pgo-warn-missing-function" \
    cargo build --release --locked -p vaco --bins --target-dir "$TARGET_DIR"
echo "pgo-build: binaries are in $TARGET_DIR/release" >&2
