# macOS CPU-counter comparisons

`scripts/perf-xctrace-counters.py` is a guarded experiment for macOS
Instruments CPU Counters. It only emits a Vaco:ffmpeg comparison when
Instruments exports named, process-aggregate CPU cycles and retired
instructions. It never converts time or sampled per-core values into cycles.

## How it works

The input is the workload array from `scripts/perf-baseline-gen-spec.py`. The
driver selects the requested jobs and command labels, performs one unmeasured
warm-up by default, then at least ten measured rounds. Command order rotates on
every round; a three-command job runs `vaco → ffmpeg → candidate`, then
`ffmpeg → candidate → vaco`, and so on.

For each invocation the driver:

1. launches the command through `/usr/bin/xctrace record` with a saved CPU
   Counters counting-mode template;
2. wraps the launched target in `/usr/bin/time -l`, so `cpu_seconds` is the
   target plus its children rather than Instruments' collector CPU time;
3. finds the `CounterMetricAggregatedForProcess` table in the trace TOC and
   exports that table as XML;
4. extracts the process named by `argv[0]`, accepting only explicitly named
   `cycles` and retired `instructions` counters; and
5. removes the per-invocation `.trace`, retaining only the JSON results passed
   to `--out`.

The result records per-round raw values, medians, ranges, paired ratios and
wins. Ratio directions are `vaco/ffmpeg` and, when a command label starts with
`candidate`, `candidate/vaco`. A failed run remains visible through a nonzero
exit status and is never paired with a different round.

The default CPU Counters “CPU Bottlenecks” mode is deliberately rejected. Its
slots are cycles plus bottleneck categories such as “Instruction Delivery
Bottleneck”; those are not retired instructions. Save a CPU Counters template
in Instruments that uses counting mode and exposes named cycle and retired
instruction events for this machine, then pass that file with `--template`.
This matters on Apple silicon, where core types differ: preserve the template,
machine/OS recorded in the JSON, and workload thread count when comparing
results. Treat ratios as same-host, same-template evidence rather than a
cross-machine baseline.

### Current Xcode 16 limitation

On 2026-09-04, a capped one-second `/usr/bin/yes` recording with Xcode's
CLI-selectable `CPU Counters` template exported
`CounterMetricAggregatedForProcess`, but each process row contained only a
four-value `uint64-array`. The trace TOC identified the active mode as
`bottleneck`; its legend was cycles, Instruction Delivery Bottleneck, Discarded
Bottleneck, Instruction Processing Bottleneck, and Useful. It did not expose a
retired-instruction event or a name for each array element. The harness rejects
that real export with a missing-`instructions` error.

`xctrace record` accepts a template path/name and instrument name, but has no
flag to select CPU Counters' event/counting configuration. Therefore a
CLI-only invocation cannot turn the stock bottleneck template into an
instructions-counting template.

On this machine with Xcode 16.0, a manually configured template containing
`Cycles (FIXED_CYCLES)` and `Instructions (FIXED_INSTRUCTIONS)` records those
named events, but its TOC contains `counters-profile` rather than
`CounterMetricAggregatedForProcess`. The exported values are sampled per-core
counter snapshots: a target can migrate between P and E cores and the values
reset to the destination core's counter. They are not process totals and must
not be summed. The driver detects this shape and refuses it with an explicit
error.

As a result, this is not currently an end-to-end macOS hardware-counter
workflow. Use `scripts/perf-hwcycles.py` on real Linux hardware with PMU access
for primary cycles/instructions, and retain macOS wall/CPU seconds only as
secondary context. If a future Instruments export exposes named process
aggregates, add its XML as a parser fixture, then re-enable this path.

Example:

```sh
python3 scripts/perf-baseline-gen-spec.py > /private/tmp/vaco-spec.json
python3 scripts/perf-xctrace-counters.py /private/tmp/vaco-spec.json \
  --template /private/tmp/Vaco-Instruction-Counts.tracetemplate \
  --jobs '^h264_decode_sd$' --cmds '^(vaco|ffmpeg_t1|candidate)$' \
  --rounds 10 --out /private/tmp/h264-xctrace-counters.json
```

Pass `--samply-dir /private/tmp/h264-samply` to record a separate diagnostic
Samply profile for each selected command after the counter passes. Open a saved
profile with `samply load /private/tmp/h264-samply/vaco.json.gz`. These profiles
are intentionally excluded from the ratios because sampling changes execution.

## How to change it

Keep workload commands in `scripts/perf-baseline-gen-spec.py`; this driver only
selects them. Extend `parse_process_counters` with a fixture in
`scripts/tests/test_perf_xctrace_counters.py` when a new Xcode release changes
the exported process-table shape. Do not accept positional counter indices:
counter names are the contract that prevents a CPU Bottlenecks category from
being labelled as retired instructions.

If the selected template no longer exposes `CounterMetricAggregatedForProcess`,
the driver stops after `xctrace export --toc` and lists the discovered schemas.
Use that output to update the saved template or parser fixture; do not fall back
to per-core sampled tables and call their sum a process total.

## Configuration

| option | meaning | default |
|---|---|---|
| `--template` | saved CPU Counters counting-mode `.tracetemplate` | required |
| `--rounds` | measured alternating rounds; values below 10 are refused | `10` |
| `--warmups` | unmeasured alternating rounds | `1` |
| `--jobs`, `--cmds` | regular expressions selecting workload names and command labels | all |
| `--xctrace` | Instruments command-line recorder | `/usr/bin/xctrace` |
| `--samply-dir` | directory for diagnostic profiles, excluded from comparison | unset |
| `--samply` | Samply executable used with `--samply-dir` | `samply` |

The command must run on Darwin. Every selected comparison needs at least two
command labels. Labels beginning with `vaco`, `ffmpeg`/`ffprobe`, and
`candidate` determine which paired ratios are emitted.

## Dependencies

- Xcode Instruments and `/usr/bin/xctrace`, including the CPU Counters
  template and permission to profile the launched process.
- A saved CPU Counters *counting-mode* template with named cycle and retired
  instruction events available on the local Apple-silicon machine. Xcode 16's
  manual fixed-counter template is intentionally rejected until it exports a
  process aggregate.
- Python 3 and the standard library.
- `/usr/bin/time` for target CPU/wall secondary metrics.
- `samply` only when diagnostic flamegraphs are requested.
- `scripts/perf-baseline-gen-spec.py` for the shared workload definition, and
  ffmpeg only as a black-box process being measured.
