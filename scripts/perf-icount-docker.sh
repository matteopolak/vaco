#!/bin/bash
# Drive the instruction-count harness from macOS, through an arm64 Linux
# container. On a Linux host, skip this and call scripts/perf-icount.py directly.
#
# Subcommands:
#   image     build the measurement image
#   build     build vaco/vaco-probe for linux/arm64 into the work volume
#   fixtures  generate the small fixture set inside the volume
#   run ...   run scripts/perf-icount.py inside the container (args passed through)
#   bench ... run scripts/perf-baseline-bench.py (wall clock) on the same
#             binaries and fixtures, so a wall ratio and an instruction ratio
#             come from ONE environment and are actually comparable
#   shell     interactive shell in the container
#   du        report the volume's disk usage
#   clean     delete the work volume (do this when you are done -- disk is tight)
#
# Environment:
#   ICOUNT_IMAGE   image tag              (default vaco-icount:1)
#   ICOUNT_VOLUME  docker volume name     (default vaco-icount-work)
#   ICOUNT_JOBS    cargo -j value         (default 2)
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE="${ICOUNT_IMAGE:-vaco-icount:1}"
VOLUME="${ICOUNT_VOLUME:-vaco-icount-work}"
# 2, not nproc: the dist profile is fat-LTO/codegen-units=1 and Docker Desktop's
# VM had 8 GiB here, where -j 6 got the `vaco` link OOM-killed -- cargo reports
# that as a plain build failure, and the run then "succeeds" having produced only
# the smaller binary. Raise it only if the VM has the memory.
JOBS="${ICOUNT_JOBS:-2}"

# Disk on the development machine has been filled twice by target/ alone; a
# container build adds a second full dependency graph. Refuse rather than be the
# process that stops every other agent.
require_disk() {
    local need_gb="$1" avail
    avail=$(df -g / | awk 'NR==2 {print $4}')
    if [ "${avail:-0}" -lt "$need_gb" ]; then
        echo "refusing: ${avail}GiB free on /, need ${need_gb}GiB. Free space first." >&2
        exit 1
    fi
}

in_container() {
    docker run --rm -v "$VOLUME:/work" -v "$REPO:/src:ro" -w /work "$IMAGE" "$@"
}

case "${1:-}" in
image)
    require_disk 6
    docker build -f "$REPO/scripts/perf-icount.Dockerfile" -t "$IMAGE" "$REPO/scripts"
    ;;
build)
    require_disk 8
    docker volume create "$VOLUME" >/dev/null
    # Source comes from `git archive HEAD`, not the working tree. Several agents
    # share this checkout, so the tree routinely contains someone else's
    # half-written edit -- the first attempt at this build died on exactly that.
    # Measuring committed code is also what makes a number reproducible.
    HEAD_SHA="$(git -C "$REPO" rev-parse HEAD)"
    echo "building vaco at $HEAD_SHA"
    git -C "$REPO" archive HEAD | docker run --rm -i -v "$VOLUME:/work" "$IMAGE" \
        bash -c 'rm -rf /work/src && mkdir -p /work/src && tar -x -C /work/src && echo extracted'
    # -i is load-bearing: without it docker gives bash an empty stdin, `bash -s`
    # reads nothing, and the whole build "succeeds" in 0.2s having done nothing.
    docker run --rm -i -e "CARGO_JOBS=$JOBS" -e "HEAD_SHA=$HEAD_SHA" \
        -v "$VOLUME:/work" -w /work "$IMAGE" bash -s <<'BUILD'
set -euo pipefail
export RUSTUP_HOME=/usr/local/rustup
export CARGO_HOME=/work/cargo
export CARGO_TARGET_DIR=/work/target
export CARGO_INCREMENTAL=0
# .cargo/config.toml sets rustc-wrapper=sccache, which is not installed here;
# that file documents RUSTC_WRAPPER="" as the supported way to disable it.
export RUSTC_WRAPPER=""
# Debug info off. It does not change codegen, and cachegrind resolves function
# names from the ELF symbol table (the dist profile keeps strip="none").
export CARGO_PROFILE_DIST_DEBUG=0
mkdir -p /work/cargo/bin /work/target /work/bin
cp -a /usr/local/cargo/bin/. /work/cargo/bin/
export PATH=/work/cargo/bin:$PATH
cd /work/src
rustup toolchain install --profile minimal "$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)"
rustc -V
cargo build --profile dist -j "${CARGO_JOBS:-2}" -p vaco-cli -p vaco-probe \
  --features vaco-registry/patent-encumbered-h264-decode,vaco-registry/patent-encumbered-hevc-decode,vaco-registry/patent-encumbered-aac-decode
cp /work/target/dist/vaco /work/target/dist/vaco-probe /work/bin/
echo "$HEAD_SHA" > /work/bin/HEAD_SHA
ls -la /work/bin
BUILD
    ;;
fixtures)
    in_container bash -c 'ICOUNT_FIXTURES=/work/fixtures bash /src/scripts/perf-icount-fixtures.sh'
    ;;
run)
    shift
    in_container bash -c '
        set -euo pipefail
        # Harness and spec generator come from the mounted working tree (/src),
        # the binary under test from the volume (built from HEAD).
        VACO_BIN=/work/bin/vaco VACO_PROBE_BIN=/work/bin/vaco-probe E2E_DIR=/work/fixtures \
            python3 /src/scripts/perf-baseline-gen-spec.py > /work/spec.json
        exec python3 /src/scripts/perf-icount.py --spec /work/spec.json "$@"
    ' _ "$@"
    ;;
bench)
    shift
    in_container bash -c '
        set -euo pipefail
        VACO_BIN=/work/bin/vaco VACO_PROBE_BIN=/work/bin/vaco-probe E2E_DIR=/work/fixtures \
            python3 /src/scripts/perf-baseline-gen-spec.py > /work/spec.json
        exec python3 /src/scripts/perf-baseline-bench.py /work/spec.json "$@"
    ' _ "$@"
    ;;
shell)
    docker run --rm -it -v "$VOLUME:/work" -v "$REPO:/src:ro" -w /work "$IMAGE" bash
    ;;
du)
    in_container bash -c 'du -sh /work/* 2>/dev/null | sort -h'
    ;;
clean)
    docker volume rm "$VOLUME"
    ;;
*)
    sed -n '2,20p' "${BASH_SOURCE[0]}"
    exit 2
    ;;
esac
