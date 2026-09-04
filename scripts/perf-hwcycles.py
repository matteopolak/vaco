#!/usr/bin/env python3
"""Interleaved Vaco/reference benchmarks using Linux hardware counters.

This is the real-PMU companion to ``perf-icount.py``.  It runs every command
through ``perf stat`` and records cycles, retired instructions, task clock,
context switches, migrations, and perf's own user/sys/elapsed times.  It never
derives a "cycle" count from elapsed time: when the host cannot expose a PMU,
the command fails with an explanation instead of emitting a substitute metric.

The input is the JSON emitted by ``perf-baseline-gen-spec.py``.  Results are
interleaved by round and the starting command rotates, matching the baseline
protocol.  At least ten measured rounds are required because these counters
still move with core placement, frequency, interrupts, and input state.

Usage:
    python3 scripts/perf-hwcycles.py spec.json --out cycles.json \
        --jobs '^h264_decode_sd' --cmds '^(vaco|ffmpeg_t1)$'
"""

import argparse
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


EVENTS = (
    "cycles",
    "instructions",
    "task-clock",
    "context-switches",
    "cpu-migrations",
)
HARDWARE_EVENTS = ("cycles", "instructions")


def median(values):
    """Return the median without depending on a third-party package."""
    ordered = sorted(values)
    if not ordered:
        return None
    middle = len(ordered) // 2
    if len(ordered) % 2:
        return ordered[middle]
    return (ordered[middle - 1] + ordered[middle]) / 2


def _number(value):
    value = value.strip().replace(",", "")
    if not value or value.startswith("<"):
        return None
    try:
        number = float(value)
    except ValueError:
        return None
    return int(number) if number.is_integer() else number


def _canonical_event(name):
    """Collapse generic and hybrid-PMU spellings to one result key."""
    name = name.strip()
    base = name.split(":", 1)[0]
    if "/" in base:
        parts = [part for part in base.split("/") if part]
        for wanted in EVENTS:
            if wanted in parts:
                return wanted
    return base


def parse_perf_stat(path, minimum_running_pct=99.0):
    """Parse ``perf stat -x ';'`` output into counters and timing metadata.

    Linux perf's documented CSV field order is stable, but the number of
    trailing metric columns and the placement of timing-only rows varies by
    release.  Counter rows are therefore keyed by the documented event field;
    timing rows are found by their labels.  Hybrid PMUs may emit separate
    ``cpu_core`` and ``cpu_atom`` rows, which are summed.
    """
    counters = {}
    running_pct = {}
    unavailable = []
    timings = {}

    with open(path, "r", encoding="utf-8", errors="replace") as handle:
        for raw_line in handle:
            fields = [field.strip() for field in raw_line.rstrip("\n").split(";")]
            joined = " ".join(fields)
            timing_labels = {
                "seconds time elapsed": "elapsed",
                "seconds user": "user",
                "seconds sys": "sys",
            }
            timing_key = next(
                (key for label, key in timing_labels.items() if label in joined),
                None,
            )
            if timing_key is not None:
                value = next((_number(field) for field in fields if _number(field) is not None), None)
                if value is not None:
                    timings[timing_key] = value
                continue

            if len(fields) < 3:
                continue
            raw_value, raw_event = fields[0], fields[2]
            event = _canonical_event(raw_event)
            if event not in EVENTS:
                continue
            value = _number(raw_value)
            if value is None:
                if raw_value.startswith("<") and event not in unavailable:
                    unavailable.append(event)
                continue
            counters[event] = counters.get(event, 0) + value

            if len(fields) >= 5:
                pct = _number(fields[4].rstrip("%"))
                if pct is not None:
                    previous = running_pct.get(event)
                    running_pct[event] = pct if previous is None else min(previous, pct)

    multiplexed = [
        event for event in HARDWARE_EVENTS
        if event in running_pct and running_pct[event] < minimum_running_pct
    ]
    usable = (
        all(event in counters for event in HARDWARE_EVENTS)
        and not any(event in unavailable for event in HARDWARE_EVENTS)
        and not multiplexed
    )
    return {
        "counters": counters,
        "counter_running_pct": running_pct,
        "unavailable_events": unavailable,
        "multiplexed_events": multiplexed,
        "hardware_counts_usable": usable,
        "timings_seconds": timings,
    }


def run_one(argv, perf, workdir, minimum_running_pct):
    """Run one command under perf stat and return its parsed counter record."""
    stat_path = Path(workdir) / "perf-stat.csv"
    if stat_path.exists():
        stat_path.unlink()
    command = [
        perf,
        "stat",
        "--no-big-num",
        "-x",
        ";",
        "-e",
        ",".join(EVENTS),
        "-o",
        str(stat_path),
        "--",
    ] + list(argv)
    with open(os.devnull, "wb") as devnull:
        proc = subprocess.run(command, stdout=devnull, stderr=subprocess.PIPE)
    if proc.returncode != 0:
        stderr = proc.stderr.decode("utf-8", "replace")[-2000:]
        raise RuntimeError(f"exit {proc.returncode} from {argv}\n{stderr}")
    if not stat_path.exists():
        raise RuntimeError(f"perf stat wrote no output for {argv}")
    parsed = parse_perf_stat(stat_path, minimum_running_pct)
    if not parsed["hardware_counts_usable"]:
        raise RuntimeError(
            "hardware counters unavailable or multiplexed for "
            f"{argv}: unavailable={parsed['unavailable_events']} "
            f"multiplexed={parsed['multiplexed_events']} "
            f"counts={parsed['counters']}"
        )
    return parsed


def summarize_runs(runs_by_label):
    """Summarize each numeric counter across successful runs."""
    output = {}
    for label, runs in runs_by_label.items():
        metrics = {}
        names = sorted({name for run in runs for name in run.get("counters", {})})
        for name in names:
            values = [run["counters"][name] for run in runs if name in run.get("counters", {})]
            metrics[name] = {
                "n": len(values),
                "median": median(values),
                "min": min(values),
                "max": max(values),
                "all": values,
            }
        output[label] = metrics
    return output


def paired_ratios(runs_by_label):
    """Return per-round Vaco/reference ratios for every shared counter."""
    vacos = [label for label in runs_by_label if label.startswith("vaco")]
    refs = [
        label for label in runs_by_label
        if label.startswith("ffmpeg") or label.startswith("ffprobe")
    ]
    output = {}
    for vaco in vacos:
        for ref in refs:
            metric_output = {}
            vaco_by_round = {
                run["round"]: run for run in runs_by_label[vaco] if "round" in run
            }
            ref_by_round = {
                run["round"]: run for run in runs_by_label[ref] if "round" in run
            }
            if vaco_by_round or ref_by_round:
                paired = [
                    (vaco_by_round[round_number], ref_by_round[round_number])
                    for round_number in sorted(vaco_by_round.keys() & ref_by_round.keys())
                ]
            else:
                paired = list(zip(runs_by_label[vaco], runs_by_label[ref]))
            metric_names = sorted({
                name
                for left, right in paired
                for name in left.get("counters", {})
                if name in right.get("counters", {})
            })
            for name in metric_names:
                values = []
                for left, right in paired:
                    denominator = right["counters"].get(name)
                    numerator = left["counters"].get(name)
                    if numerator is not None and denominator:
                        values.append(numerator / denominator)
                if values:
                    metric_output[name] = {
                        "n": len(values),
                        "median": median(values),
                        "min": min(values),
                        "max": max(values),
                        "all": values,
                        "wins": sum(value < 1.0 for value in values),
                    }
            output[f"{vaco}/{ref}"] = metric_output
    return output


def _acquire_lock(path):
    try:
        os.mkdir(path)
    except FileExistsError as exc:
        raise RuntimeError(
            f"measurement lock exists at {path}; another benchmark may be running. "
            "Wait for it, or remove the directory only after confirming it is stale."
        ) from exc


def _print_table(results):
    print("\n| workload | ratio | cycles | instructions | task-clock |", file=sys.stderr)
    print("|---|---|---:|---:|---:|", file=sys.stderr)
    for name, job in results["jobs"].items():
        for ratio_name, metrics in job["ratios"].items():
            def rendered(metric):
                value = metrics.get(metric, {}).get("median")
                return "—" if value is None else f"{value:.3f}x"
            print(
                f"| {name} | {ratio_name} | {rendered('cycles')} | "
                f"{rendered('instructions')} | {rendered('task-clock')} |",
                file=sys.stderr,
            )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("spec", help="workload spec emitted by perf-baseline-gen-spec.py")
    parser.add_argument("--out", required=True, help="write results JSON here")
    parser.add_argument("--jobs", help="regex selecting workload names")
    parser.add_argument("--cmds", help="regex selecting command labels")
    parser.add_argument("--rounds", type=int, default=10, help="measured interleaved rounds (minimum 10)")
    parser.add_argument("--warmups", type=int, default=1, help="unmeasured warm-up rounds")
    parser.add_argument("--minimum-running-pct", type=float, default=99.0,
                        help="reject hardware counters scheduled for less than this percentage")
    parser.add_argument("--perf", default=os.environ.get("PERF_BIN", "perf"))
    parser.add_argument("--lock-dir", default="/tmp/vaco-perf-measure.lock")
    args = parser.parse_args()

    if args.rounds < 10:
        parser.error("--rounds must be at least 10; the performance protocol forbids single-run claims")
    if args.warmups < 0:
        parser.error("--warmups cannot be negative")
    if platform.system() != "Linux":
        print(
            "error: real process-total hardware cycles are supported here only through "
            "Linux perf stat. This host is not Linux. Use scripts/perf-icount.py for "
            "load-immune instruction counts, and do not relabel time-derived estimates "
            "as cycles.",
            file=sys.stderr,
        )
        return 2
    perf = shutil.which(args.perf)
    if perf is None:
        print(f"error: {args.perf} not found; install Linux perf", file=sys.stderr)
        return 2
    version = subprocess.run([perf, "--version"], capture_output=True)
    if version.returncode != 0:
        print(f"error: `{perf} --version` exited {version.returncode}", file=sys.stderr)
        return 2

    spec = json.loads(Path(args.spec).read_text(encoding="utf-8"))
    job_pattern = re.compile(args.jobs) if args.jobs else None
    command_pattern = re.compile(args.cmds) if args.cmds else None
    results = {
        "backend": "linux-perf-stat",
        "perf_version": version.stdout.decode("utf-8", "replace").strip(),
        "events": list(EVENTS),
        "rounds": args.rounds,
        "warmups": args.warmups,
        "minimum_running_pct": args.minimum_running_pct,
        "host": {"platform": platform.platform(), "machine": platform.machine()},
        "jobs": {},
    }

    try:
        _acquire_lock(args.lock_dir)
    except RuntimeError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    failures = 0
    try:
        with tempfile.TemporaryDirectory() as workdir:
            for job in spec:
                name = job["name"]
                if job_pattern and not job_pattern.search(name):
                    continue
                commands = {
                    label: argv for label, argv in job["cmds"].items()
                    if not command_pattern or command_pattern.search(label)
                }
                if not commands:
                    continue
                labels = list(commands)
                runs = {label: [] for label in labels}
                print(f"== {name}", file=sys.stderr)
                for round_number in range(args.warmups + args.rounds):
                    order = labels[round_number % len(labels):] + labels[:round_number % len(labels)]
                    measured = round_number >= args.warmups
                    for label in order:
                        try:
                            record = run_one(
                                commands[label], perf, workdir, args.minimum_running_pct
                            )
                        except RuntimeError as exc:
                            failures += 1
                            print(f"  FAILED {label}: {exc}", file=sys.stderr)
                            continue
                        if measured:
                            record["round"] = round_number - args.warmups
                            runs[label].append(record)
                            counters = record["counters"]
                            print(
                                f"  round {round_number - args.warmups + 1} {label}: "
                                f"cycles={counters['cycles']:,} "
                                f"instructions={counters['instructions']:,} "
                                f"migrations={counters.get('cpu-migrations', '—')}",
                                file=sys.stderr,
                            )
                results["jobs"][name] = {
                    "commands": commands,
                    "runs": runs,
                    "summary": summarize_runs(runs),
                    "ratios": paired_ratios(runs),
                }
                Path(args.out).write_text(json.dumps(results, indent=2), encoding="utf-8")
    finally:
        try:
            os.rmdir(args.lock_dir)
        except FileNotFoundError:
            pass

    Path(args.out).write_text(json.dumps(results, indent=2), encoding="utf-8")
    _print_table(results)
    if failures:
        print(f"{failures} measurement(s) failed and were excluded", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
