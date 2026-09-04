# Hardware-cycle and instruction-count benchmarking

## What it is

Two complementary counter-based harnesses answer performance questions without
mistaking one noisy wall-clock observation for program cost:

- `scripts/perf-hwcycles.py` uses Linux `perf stat` to collect real
  process-and-child hardware cycles and retired instructions, plus task clock,
  context switches, CPU migrations, and user/sys/elapsed time. It is the
  preferred same-session Vaco:ffmpeg comparison when a Linux host exposes its
  PMU.
- `scripts/perf-icount.py` uses cachegrind's deterministic simulated instruction
  count. It remains useful for change A/B, attribution, and loaded machines, but
  does not model real cycles or SIMD throughput.

A load-immune way to answer *"did this change make the code do less work?"* on a
machine where wall clock cannot answer it. `scripts/perf-icount.py` runs a
workload under Valgrind's **cachegrind**, which *simulates* execution rather than
sampling it, and reports the instruction count (`Ir`). For a deterministic
single-threaded run the same input gives the same number whatever else the
machine is doing.

It does **not** replace `scripts/perf-baseline-bench.py`. Instruction count is
not time. The two instruments answer different questions and the roles are
assigned deliberately:

| # | instrument | answers | needs a quiet machine? |
|---|---|---|---|
| 1 | **hardware cycles** (`perf-hwcycles.py`, Linux perf) | "how many actual CPU cycles did each program consume?" — same-session Vaco:ffmpeg ratio, IPC, and threading-inclusive CPU work | **less than wall time**, but pin comparable cores and inspect migrations |
| 2 | **instruction count** (`perf-icount.py`, cachegrind) | "does this code do less work than it did?" — A/B of a change, CI regression gate, per-function attribution | **no** |
| 3 | **interleaved wall + CPU vs a same-session ffmpeg** (`perf-baseline-bench.py`) | "how long does the job take?" — latency/throughput, retained beside counters | **yes**, and it now says so |
| 4 | **Samply** | "which functions and call paths consume the cycles?" — sampled flame graph and timeline | profiling perturbs the run; do not use it as the ratio |
| 5 | thread occupancy / stall accounting | "is the pipeline saturated?" | not built — see *What this cannot see* |

Hardware cycles are closer to CPU cost than wall time, but they are not a
universal constant. Frequency, interrupts, migration, and heterogeneous core
types can change the count. `perf-hwcycles.py` therefore uses the same
interleaved/alternating order for Vaco and ffmpeg, requires at least 10 rounds,
reports every paired ratio, records migrations, and rejects unavailable or
meaningfully multiplexed counters. It does not convert seconds to cycles.

## Real hardware cycles on Linux

Generate the normal shared workload specification, then select a matched Vaco
and reference command. Build Vaco before taking the measurement; compilation is
never included in the counters.

```sh
VACO_BIN=/private/tmp/vaco-perf-target/dist/vaco \
VACO_PROBE_BIN=/private/tmp/vaco-perf-target/dist/vaco-probe \
E2E_DIR=/path/to/e2e \
  python3 scripts/perf-baseline-gen-spec.py > /private/tmp/vaco-perf-spec.json

python3 scripts/perf-hwcycles.py /private/tmp/vaco-perf-spec.json \
  --jobs '^h264_decode_sd' --cmds '^(vaco|ffmpeg_t1)$' \
  --rounds 10 --out /private/tmp/h264-cycles.json
```

The result stores raw per-round counters, medians/ranges, paired ratios, win
counts, perf's percentage-running field, and the exact argv. On hybrid x86
systems perf may emit separate `cpu_core/.../` and `cpu_atom/.../` rows; the
harness sums them and retains the lowest running percentage. For tight A/B work,
pin both commands to the same core class in the spec with `taskset` and reject a
run whose migration counts differ materially.

`perf stat` access is controlled by the host kernel. If it reports permission
errors, configure `/proc/sys/kernel/perf_event_paranoid` according to local
policy. Docker Desktop on Apple silicon does not expose a PMU to its Linux VM,
so neither a privileged container nor a time-derived estimate is a substitute.

## Why not CPU-seconds

`/usr/bin/time`'s CPU figure is widely assumed to be the load-immune metric. On
this machine it is not, and the reason is structural rather than incidental: it
is a 10-core Apple silicon part, 4 performance + 6 efficiency cores, and the
same work costs materially more CPU-seconds on an E core.

Measured here, 8 interleaved rounds, alternating order, `vaco -threads 1`
decoding a 640x480 H.264 clip, macOS host at load average ~54–83:

| scheduling | wall clock | CPU-seconds (user+sys) |
|---|---|---|
| default QoS | 0.25 – 0.60 s | 0.17 – 0.20 s |
| background QoS (`taskpolicy -b`, i.e. E cores) | 1.38 – 9.28 s | **0.28 s, every single run** |

Identical binary, identical input, identical output — **1.56x more CPU-seconds**
purely as a function of which core type ran it. Under load the scheduler mixes
the two, so CPU-seconds inherits load-dependence. An Instruments "CPU Counters"
recording of one 1.4 s single-threaded `vaco` run shows the migration directly:
that one thread was sampled on CPUs 0, 2, 3, 4 and 5 (E cores) and 6, 7 and 8
(P cores) within the run.

## The load-immunity demonstration

Controlled, interleaved (8 CPUs, 0.25 CPU, 8 CPUs, 0.25 CPU), same container,
same binary, same fixture — `vaco -threads 1` decoding `h264_sd.mp4`
(640x480, 125 frames):

| container CPU quota | wall clock (3 runs each round) | CPU-seconds | cachegrind `Ir` |
|---|---|---|---|
| `--cpus=8` | 1.032 / 1.061 / 1.036 s, then 1.098 / 0.975 / 0.995 s | 1.033 – 1.098 | **11,925,069,875** (both rounds) |
| `--cpus=0.25` | 4.389 / 4.892 / 4.707 s, then 4.450 / 6.187 / 4.703 s | 1.117 – 1.538 | **11,925,069,850** (both rounds) |

- wall clock moves **4.55x** at the median and spans **6.3x** across all twelve runs;
- CPU-seconds moves **1.14x**, so it is better than wall clock and still not stable;
- the instruction count moves by **25 instructions in 11.9 billion — 0.0000002%**,
  and is bit-identical between the two rounds of each condition.

The same holds across the real, uncontrolled machine load: an identical harness
invocation run at host load average ~26 and again at ~84 returned
`11,925,021,187` both times for H.264 SD — *the same integer* — and 5.50x /
11.78x / 20.07x for the AAC, H.264 and MP3 vaco:ffmpeg ratios in the first run
against 5.50x / 11.78x / 20.08x in the second.

## Measured results

Environment: `HEAD` = `6c06fe7a`, `--profile dist` (`opt-level=3`, fat LTO,
`codegen-units=1`, `debug=0` — debug info does not change codegen and cachegrind
resolves names from the ELF symbol table), toolchain `nightly-2026-08-07`,
Debian 13 arm64 in Docker Desktop on Apple silicon, ffmpeg **7.1.5** (Debian),
valgrind 3.24.0.

**These are not the same builds as `planning/PERF-BASELINE.md` §1.** That table
is macOS/`aarch64-apple-darwin` against Homebrew ffmpeg 9.0.1. Comparing a wall
ratio from there with an instruction ratio from here would cross two toolchains,
two libcs and two ffmpeg versions, so the wall column below was re-measured in
*this* container on *these* fixtures instead.

| workload (SD/short fixtures) | wall ratio (here) | Ir ratio | Ir ratio minus startup |
|---|---:|---:|---:|
| H.264 decode, 640x480, 125 frames, `-threads 1` | 4.34x | 11.78x | 14.42x |
| HEVC decode, 640x480, 125 frames, `-threads 1` | 3.14x | 7.53x | 9.07x |
| AAC decode, 30 s | 1.48x | 5.50x | 12.30x |
| MP3 decode, 30 s | 3.50x | 20.07x | 38.90x |
| remux mkv→mp4 stream copy | 0.04x | 0.02x | 0.17x |
| probe, H.264 1080p | 0.03x | 0.02x | 0.08x |
| probe, mkv | 0.02x | 0.02x | 0.26x |

**Read the wall and Ir columns together, never one alone.** H.264 SD is 11.8x
more instructions but only 4.3x slower: the reference's hand-written vector code
retires far fewer instructions per unit of work, and pays for it in cycles per
instruction. MP3 is the extreme case — 20x the instructions for 3.5x the time.
An optimiser that chased the Ir column alone here would be chasing a number that
is 4x worse than the thing anyone cares about.

**Startup is a fixed cost that lands in the count in full.** Measured with
`--startup-floor` (`<binary> -version` under cachegrind):

| binary | Ir before doing any work |
|---|---:|
| `vaco` | 612,649 |
| `vaco-probe` | 426,199 |
| `ffmpeg` | 185,451,888 |
| `ffprobe` | 185,303,503 |

That 303x gap is most of vaco's measured win on `probe` and `remux`, and it is a
real win on a short job — but it is not a claim about the demux or mux inner
loops, and the "minus startup" column exists so nobody reads it as one.

### Per-function attribution

`--top N` gives an exact instruction share per function, with no sampling error.
H.264 SD decode, `vaco -threads 1`, of 11,925,021,187 instructions:

| share | function |
|---:|---|
| 29.84% | `reconstruct::reconstruct_mb` |
| 14.59% | `<PictureReconstructor>::deblock_row` |
| 11.28% | `deblock::boundary_strength` |
| 10.73% | `reconstruct::sample_chroma_2x2` |
| 8.36% | `reconstruct::sample_luma_partition` |
| 4.70% | `frame_task::build_frame` |
| 4.12% | `<DeblockCtx>::chroma_mb_row` |
| 4.00% | `vaco_codec_dsp_deblock::batch::filter_luma_edge` |

Consistent in ordering with `PERF-BASELINE.md` §2.1's sampled time profile,
which is the cross-check that matters: a share of *instructions* and a share of
*time* are different quantities, and where they disagree the disagreement is the
finding.

### A defect this instrument found on its first run

MP3 decode's profile is **74.38% `cos`** (5,720,068,487 instructions), plus 3.24%
`sin` — because `crates/codec/vaco-codec-mpegaudio/src/layer3.rs:442`'s
`windowed_imdct` calls `vaco_tx::reference::imdct`, the O(n²) transform whose own
module doc says *"Verification only. Nothing in the crate's fast paths calls this
module."* This is the exact sibling of the AAC defect `PERF-BASELINE.md` §7
candidate 1 named and commit `273d60fb` fixed on 2026-09-01; the sweep for other
production callers found this one and no others (`vaco-conformance`'s
`reference::rdft` use is a deliberate oracle in a measurement tool).

## How it works

1. **`scripts/perf-icount.Dockerfile`** builds an arm64 Debian image with
   valgrind, ffmpeg and a Rust toolchain. Valgrind has no macOS/Apple-silicon
   port; Docker Desktop runs arm64 Linux *natively* here, so this is not
   emulation, and the same image runs on a Linux CI runner.
2. **`scripts/perf-icount-docker.sh build`** exports `git archive HEAD` into a
   docker volume and builds `vaco` and `vaco-probe` there. HEAD, not the working
   tree: several agents share this checkout and the tree usually contains
   somebody's half-written edit — the first attempt at this died on exactly that.
3. **`scripts/perf-icount-fixtures.sh`** generates a small fixture set whose file
   names match `PERF-BASELINE.md`'s, so `scripts/perf-baseline-gen-spec.py` —
   the single definition of every command shape — can be pointed at it unchanged
   through `E2E_DIR`. Cachegrind measured **~30x** slower than native here, so
   the fixtures are SD and short; there is deliberately no 4K equivalent.
4. **`scripts/perf-icount.py`** runs each command under
   `valgrind --tool=cachegrind`, checks its exit status, parses the cachegrind
   output file directly (the format is documented; `cg_annotate`'s human output
   is not), and reports `Ir`, per-function shares, the startup floor and the
   vaco:ffmpeg ratios. It runs each command twice by default *to verify
   determinism, not to average*: a spread is printed as a `NOTE`.

Typical determinism observed: vaco 0–5 instructions of drift in 10⁹–10¹⁰ (≤2e-7%),
ffmpeg 0.002–0.07%.

## What this cannot see

- **Threading and saturation.** Valgrind serialises threads. The instrument is
  blind to precisely the problem the scheduler rewrite is aimed at, which is why
  instrument 2 stays the ground truth and instrument 3 is still owed.
- **Out-of-order execution, prefetch, and the real cost of a branch miss.**
  Cachegrind's cache model is a simplification and is not this CPU. `--cache-sim`
  and `--branch-sim` add D1/LL and branch-predictor estimates; treat them as
  hints, not as this machine.
- **SIMD efficiency.** One NEON instruction can do 16 lanes. A change that
  lowers the instruction count by de-vectorising is a regression that this
  number will applaud. Never accept an Ir win without a wall-clock check.
- **Anything about the reference's internals.** ffmpeg is measured strictly as a
  black box (D6/D7): the binary is run, its instruction count is recorded. Its
  source is never read, and its symbol names are not reproduced here.

The cachegrind limitations above do not all apply to `perf-hwcycles.py`: its
cycle and instruction totals include real SIMD execution, out-of-order effects,
all child threads, and stalls. It still does not explain *where* those cycles
went. Use Samply for attribution after the counter ratio identifies a workload.

Two smaller traps, both observed:

- **The count depends on argv and environment.** The same decode invoked with
  different paths differed by 0.0004%. Compare like-for-like invocations.
- **Inside the container, `getloadavg()` reports the VM's load, not the host's.**
  It read ~1.1 while the Mac was at ~30. `perf-baseline-bench.py`'s `--max-load`
  guard is therefore only meaningful when that script runs on the host.

## Instruments considered and not adopted

| option | verdict |
|---|---|
| In-process cycle counter (`cntvct_el0`) | Needs inline asm. `unsafe_code = "forbid"` (D2). Not available, and not worth an exception. |
| `xctrace` / Instruments "CPU Counters" | **Works**, and is the only way to get real PMU data on this host. But the template records *sampled per-core* counters (`kdebug-counters-with-time-sample`), not a process total, so a total means differencing per-core counters across every migration; a record-and-save cycle took minutes of wall time for a 1.4 s workload, and one attempt did not return inside 300 s under load. Cycles are core-type-dependent anyway. Kept as a diagnostic, not as the A/B instrument. |
| Linux `perf stat` | **Adopted on real Linux hosts** through `scripts/perf-hwcycles.py`. Inside Docker Desktop on Apple silicon it still returns `<not supported>` even when privileged: that VM exposes no PMU. The harness refuses rather than silently falling back. |
| `getrusage` / `/usr/bin/time` CPU-seconds | Kept, demoted. Still reported beside wall clock; see *Why not CPU-seconds* for why it is not the load-immune metric. |

## Samply flamegraph workflow

Use the exact command and fixture whose hardware-cycle or instruction ratio is
bad. A profile is diagnostic, not a benchmark result. The dist binary needs
debug information preserved in a dSYM on macOS:

```sh
CARGO_INCREMENTAL=0 cargo build -j 1 --profile dist -p vaco-cli \
  --features vaco-registry/patent-encumbered-h264-decode \
  --target-dir /private/tmp/vaco-perf-target
dsymutil /private/tmp/vaco-perf-target/dist/vaco \
  -o /private/tmp/vaco-perf-target/vaco.dSYM

samply record --rate 4000 --save-only \
  -o /private/tmp/h264-vaco.json.gz -- \
  /private/tmp/vaco-perf-target/dist/vaco -threads 1 \
  -i /path/to/e2e/h264_sd.mp4 -map 0:v:0 -c:v rawvideo -f null -

python3 scripts/perf-baseline-symbolicate.py \
  /private/tmp/h264-vaco.json.gz \
  /private/tmp/vaco-perf-target/vaco.dSYM/Contents/Resources/DWARF/vaco \
  vaco --top 30 > /private/tmp/h264-vaco-top.json
python3 scripts/perf-baseline-symbolicate.py \
  /private/tmp/h264-vaco.json.gz \
  /private/tmp/vaco-perf-target/vaco.dSYM/Contents/Resources/DWARF/vaco \
  vaco --top 30 --innermost > /private/tmp/h264-vaco-inline-top.json
samply load /private/tmp/h264-vaco.json.gz
```

The outermost view identifies the physical function the compiler emitted. The
`--innermost` view attributes an inlined sample to its most specific source
function, which can reveal a shared helper hidden inside a codec-local body. The
interactive `samply load` view provides the flame graph and thread timeline.
Profile Vaco for code attribution. The reference binary is measured only as a
black box; its source is never consulted.

## How to change it

- **A new workload**: add it to `scripts/perf-baseline-gen-spec.py` (the one
  place command shapes live) and, if it needs a new file, to
  `scripts/perf-icount-fixtures.sh`. Do not add a second spec generator.
- **A different metric**: `--cache-sim` / `--branch-sim` widen the `events:`
  header of the cachegrind file; `parse_cachegrind` already keys the summary off
  that header, so new columns appear in the JSON without code changes.
- **Making it a CI gate**: run `perf-icount.py` on a fixed fixture set, compare
  `Ir` against a stored baseline, and fail on a rise beyond a threshold. Pick the
  threshold above the observed ffmpeg-side drift (0.07%), not at zero. Gate on
  vaco's own commands only; the reference binary's count moves with its version.
- **Raising `-j`**: the `vaco` link is fat-LTO and was OOM-killed at `-j 6` in an
  8 GiB VM, which cargo reports as an ordinary build failure while the smaller
  binary still gets produced. `ICOUNT_JOBS` defaults to 2 for that reason.

## Configuration

| variable | used by | default |
|---|---|---|
| `ICOUNT_IMAGE` | `perf-icount-docker.sh` | `vaco-icount:1` |
| `ICOUNT_VOLUME` | `perf-icount-docker.sh` | `vaco-icount-work` |
| `ICOUNT_JOBS` | `perf-icount-docker.sh` | `2` |
| `ICOUNT_FIXTURES` | `perf-icount-fixtures.sh` | required |
| `FFMPEG_BIN` | `perf-icount-fixtures.sh` | `ffmpeg` |
| `VALGRIND_BIN` | `perf-icount.py` | `valgrind` |
| `PERF_BIN` | `perf-hwcycles.py` | `perf` |
| `VACO_BIN`, `VACO_PROBE_BIN`, `E2E_DIR` | `perf-baseline-gen-spec.py` | set by the driver |

`perf-icount.py` flags: `--spec`, `--jobs`, `--cmds`, `--repeats`, `--top`,
`--startup-floor`, `--cache-sim`, `--branch-sim`, `--out`.
`perf-hwcycles.py` flags: `spec`, `--out`, `--jobs`, `--cmds`, `--rounds`,
`--warmups`, `--minimum-running-pct`, `--perf`, `--lock-dir`.
`perf-baseline-bench.py` gained `--max-load` (default: one per core) and
`--refuse-under-load`; every result now carries a `_load` block recording the
peak load average and an `unusable_wall_clock` flag, so a later reader cannot
quote a number without seeing that it was taken on a busy machine.

Typical session:

```sh
bash scripts/perf-icount-docker.sh image
bash scripts/perf-icount-docker.sh build       # ~4 min, ~700 MB in the volume
bash scripts/perf-icount-docker.sh fixtures
bash scripts/perf-icount-docker.sh run --jobs '^h264_decode_sd' \
    --cmds '^(vaco|ffmpeg_t1)$' --top 15 --startup-floor --out /work/icount.json
bash scripts/perf-icount-docker.sh du
bash scripts/perf-icount-docker.sh clean       # disk here is tight; do this
```

`perf-icount-docker.sh` refuses to build with less than 8 GiB free on `/`.
`target/` has filled this disk twice, and a container build adds a second full
dependency graph.

## Dependencies

- **Docker** (Docker Desktop on macOS; any engine on Linux) — arm64 Linux runs
  natively on Apple silicon. On a Linux host, skip the driver and call
  `scripts/perf-icount.py` directly.
- **Valgrind ≥ 3.24** (cachegrind). No macOS/Apple-silicon port exists.
- **Linux perf** with access to hardware events for real cycle totals. Its
  `perf-stat(1)` documentation defines the CSV fields the parser consumes.
- **Samply** for sampling profiles and flame graphs on macOS or Linux.
- **`ffmpeg` in the image**, as a black-box reference binary only (D6/D7).
- **`scripts/perf-baseline-gen-spec.py`** for the workload definitions, and
  `scripts/perf-baseline-bench.py` for the wall-clock half of any comparison.
- Python 3, no third-party packages.
