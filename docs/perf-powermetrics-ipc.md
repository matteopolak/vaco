# macOS powermetrics IPC counters

`scripts/perf-powermetrics-ipc.py` is a fail-closed experimental benchmark
backend for macOS Apple-silicon hosts. It uses `powermetrics --show-process-ipc`
to obtain a single, PID-matched process sample containing instruction and cycle
rates. It is not enabled as a performance evidence source until a privileged
sample has been captured and independently checked on this host.

## How it works

The script reads the workload format emitted by
`scripts/perf-baseline-gen-spec.py`, warms up once, then runs at least ten
measured rounds. Command order rotates for every round and is recorded in the
output JSON. Each command launches directly, without a shell or timing wrapper,
so the sampler can require its exact PID and executable basename.

For each invocation, the script runs one `tasks` sampler with
`--show-process-ipc --show-process-amp --format plist`. It accepts exactly one
NUL-delimited plist only when all of these are true:

- `is_delta` is true and `elapsed_ns` is a positive integer;
- the `tasks` array has exactly one row whose `id` equals the launched PID and
  whose `name` equals the executable basename;
- that row is not `invalid` and has finite non-negative `cpu_instructions` and
  `cpu_cycles` rates.

The reported counter values are each rate multiplied by the same sample's
reported elapsed interval. Raw rates and `elapsed_ns` remain in the JSON for
audit. The scope is the launched PID only: child processes are intentionally
excluded rather than silently attributed to their parent. CPU seconds come from
`wait4` for that target and wall seconds from the surrounding monotonic timer.

## How to change it

The parser is deliberately tied to the installed tool's `tasks` plist contract.
If a macOS update changes that shape, add an actual captured, sanitized plist
fixture to `scripts/tests/test_perf_powermetrics_ipc.py` before relaxing it. Do
not use a lifetime sample, a `DEAD_TASKS` row, a process-name-only match, or a
second sample in place of the PID-matched delta.

Before accepting the backend for a speedup, first record a controlled target
whose process row, counter delta and scope can be inspected. Then run the
normal Vaco/ffmpeg comparison with at least ten rotated rounds and independently
verify the output at one, two, four and eight threads. The script's successful
exit establishes only the counter/sample contract; it does not replace codec or
filter output checks.

## Configuration

`powermetrics` requires root. The script uses `sudo -n` when it is not already
root and fails before the benchmark if a password prompt would be needed. Either
run the script under `sudo` or arrange a narrowly scoped noninteractive rule
for its exact sampler command.

```sh
sudo -v
python3 scripts/perf-powermetrics-ipc.py /private/tmp/vaco-spec.json \
  --jobs '^qoi_decode$' --cmds '^(vaco|ffmpeg_t1)$' \
  --rounds 10 --sample-rate-ms 250 --out /private/tmp/qoi-powermetrics.json
```

`--sample-rate-ms` must be short enough that each target remains alive through
the sample. A target that exits before sampling, returns nonzero, or is absent
from the task row fails the run and is not paired into a ratio.

## Dependencies

- macOS on ARM and `/usr/bin/powermetrics` with `--show-process-ipc` support;
- root or an active noninteractive `sudo` credential for that sampler;
- Python 3 standard library; and
- the shared workload spec and ffmpeg only as a black-box reference process.
