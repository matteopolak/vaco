# Profile-guided optimisation

## What it is

The PGO pipeline builds the `vaco` binaries with LLVM instrumentation, runs a
representative T0/T1 workload, merges the counters, and rebuilds with the
resulting profile. The workload and its reproducibility metadata are committed;
large `.profdata` files remain release-channel artifacts.

## How it works

Set `VACO_PROFILE_FIXTURES` to a directory containing the fixture names used by
[`profile/workload.toml`](../profile/workload.toml), then run:

```text
just pgo-build
```

`scripts/pgo-build.sh` performs the instrumented and profile-use release builds
in `VACO_PGO_TARGET_DIR` (default `target/pgo`). `scripts/vaco-profile` executes
each manifest row without a shell, enforces the 25-minute total budget, and can
be previewed with `--dry-run`. `just pgo-coverage profile=/path/to/vaco.profdata`
runs `llvm-profdata show --all-functions --counts` and checks the profile.
The equivalent dependency-free task is `cargo xtask profile-coverage /path/to/vaco.profdata`
for CI callers that already use `xtask`.

The checker requires a non-zero count for every crate in the manifest's
`required_components` list and rejects a workload whose declared component
weight exceeds 15%. LLVM counter volume is used only for coverage: a decoder
naturally executes more counters than CLI parsing, and using it as the weight
would reject a representative profile. This crate-level mapping is intentional:
LLVM's text dump has function symbols, not registry descriptor identities. A
component addition must update the list and add a workload row; omission is
therefore visible in review.

The disposable fixture generator keeps the VP9 and VP8 clips short while
retaining decode, seek, and scale coverage. A separate MJPEG clip supplies
the encode/remux rows; the strict checker still decides whether the resulting
raw profile is suitable for a release build.

`scripts/tests/test_pgo.py` validates manifest parsing, lock pinning, profile
coverage, and the distinction between declared training weight and raw counter
volume. `scripts/vaco-profile --dry-run` prints the exact argument vectors
without requiring a build.

## How to change it

Add or alter a `[[workload]]` row only when it exercises a distinct path. Keep
all eight groups (`decode`, `demux`, `transcode`, `scale`, `resample`, `encode`,
`seek`, and `cli`) represented and keep the total run under the declared
budget. Update `required_components` when a new default component crate needs
coverage. Recompute `manifest_sha256` in `profile/lockfile.toml` after changing
the manifest. Do not commit `.profraw`, `.profdata`, or generated fixtures.

## Configuration

| Variable | Default | Purpose |
|---|---|---|
| `VACO_PROFILE_FIXTURES` | unset | T0/T1 fixture directory; required for local profile generation |
| `VACO_PROFILE_MANIFEST` | `profile/workload.toml` | workload manifest |
| `VACO_PGO_TARGET_DIR` | `target/pgo` | private Cargo target directory |
| `VACO_PGO_RAW_DIR` | `pgo-data/raw` | instrumented `.profraw` output |
| `VACO_PGO_PROFDATA` | `pgo-data/vaco.profdata` | merged profile output |
| `LLVM_PROFDATA` | PATH lookup | `llvm-profdata` executable |
| `VACO_PGO_PROFILE` | unset | existing profile; missing paths fall back to a plain release build |

## Dependencies

The pipeline relies on the pinned Rust toolchain, Cargo, LLVM's
`llvm-profdata`, Python 3.9+ (standard library only), and fixture-generation
tools supplied by the caller. The refresh workflow installs `ffmpeg` only to
produce disposable black-box fixtures; no reference source is read.
