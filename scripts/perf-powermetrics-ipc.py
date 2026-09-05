#!/usr/bin/env python3
"""Fail-closed macOS process-counter benchmark through ``powermetrics``.

The macOS ``powermetrics --show-process-ipc`` sampler reports per-process ARM
instruction and cycle rates.  This driver turns one delta sample into a count
only after validating the sampler's elapsed interval, the launched PID, and its
exact process name.  It refuses a missing, stale, invalid, or ambiguous task
row rather than borrowing another process's counters.

The sampler needs root.  Run this script as root or grant a narrowly scoped
passwordless sudo rule; an interactive sudo prompt is intentionally not used in
the measurement loop.
"""

import argparse
import json
import math
import os
import plistlib
import platform
import re
import resource
import shutil
import subprocess
import sys
import time
from pathlib import Path


REQUIRED_COUNTERS = ("cycles", "instructions")
_RATE_KEYS = {"instructions": "cpu_instructions", "cycles": "cpu_cycles"}


def median(values):
    ordered = sorted(values)
    middle = len(ordered) // 2
    if len(ordered) % 2:
        return ordered[middle]
    return (ordered[middle - 1] + ordered[middle]) / 2


def validate_rounds(rounds):
    if rounds < 10:
        raise ValueError("at least 10 measured rounds are required")


def rotating_order(labels, round_number):
    offset = round_number % len(labels)
    return labels[offset:] + labels[:offset]


def _number(value, key):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError(f"powermetrics task field {key!r} is not numeric")
    value = float(value)
    if not math.isfinite(value) or value < 0:
        raise ValueError(f"powermetrics task field {key!r} is not a finite non-negative value")
    return value


def parse_process_sample(raw, pid, process_name):
    """Parse exactly one NUL-delimited ``powermetrics --format plist`` delta.

    ``cpu_instructions`` and ``cpu_cycles`` are the tool's per-second values.
    The sole permitted conversion is multiplication by the same sample's
    explicit ``elapsed_ns``; the raw rates and interval remain in the result so
    consumers can audit the resulting process delta.
    """
    documents = [chunk for chunk in raw.split(b"\0") if chunk.strip()]
    if len(documents) != 1:
        raise ValueError(
            f"powermetrics must emit exactly one delta plist, found {len(documents)}"
        )
    try:
        sample = plistlib.loads(documents[0])
    except (plistlib.InvalidFileException, ValueError) as exc:
        raise ValueError(f"powermetrics emitted invalid plist: {exc}") from exc
    if not isinstance(sample, dict):
        raise ValueError("powermetrics plist root is not a dictionary")
    if sample.get("is_delta") is not True:
        raise ValueError("powermetrics sample is not a delta; refusing lifetime counters")
    elapsed_ns = sample.get("elapsed_ns")
    if isinstance(elapsed_ns, bool) or not isinstance(elapsed_ns, int) or elapsed_ns <= 0:
        raise ValueError("powermetrics delta is missing a positive integer elapsed_ns")
    tasks = sample.get("tasks")
    if not isinstance(tasks, list):
        raise ValueError("powermetrics delta is missing its tasks array")
    matches = [task for task in tasks if isinstance(task, dict) and task.get("id") == pid]
    if len(matches) != 1:
        raise ValueError(
            f"powermetrics expected one task with id {pid}, found {len(matches)}; "
            "the launched target was not isolated"
        )
    task = matches[0]
    if task.get("name") != process_name:
        raise ValueError(
            f"powermetrics task id {pid} is named {task.get('name')!r}, expected "
            f"{process_name!r}; refusing a recycled or wrapper PID"
        )
    if task.get("invalid") is True:
        raise ValueError(f"powermetrics marked task {process_name!r} invalid")
    rates = {metric: _number(task.get(key), key) for metric, key in _RATE_KEYS.items()}
    elapsed_seconds = elapsed_ns / 1_000_000_000
    return {
        "process": process_name,
        "pid": pid,
        "elapsed_ns": elapsed_ns,
        "raw_rates_per_second": rates,
        "counters": {metric: rate * elapsed_seconds for metric, rate in rates.items()},
        "counter_contract": "powermetrics task delta rate multiplied by its reported elapsed_ns",
        "process_scope": "launched PID only; child processes are excluded",
    }


def _sampler_command(sudo, powermetrics, sample_rate_ms):
    command = [
        powermetrics,
        "--samplers", "tasks",
        "--show-process-ipc",
        "--show-process-amp",
        "--format", "plist",
        "--sample-rate", str(sample_rate_ms),
        "--sample-count", "1",
    ]
    return command if os.geteuid() == 0 else [sudo, "-n"] + command


def preflight(sudo, powermetrics):
    """Reject missing noninteractive root access before a benchmark starts."""
    try:
        completed = subprocess.run(
            _sampler_command(sudo, powermetrics, 1),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
    except OSError as exc:
        raise RuntimeError(
            "powermetrics needs noninteractive root access; could not launch its "
            f"sudo preflight: {exc.strerror or exc}"
        ) from exc
    if completed.returncode == 0:
        return
    detail = completed.stderr.decode("utf-8", "replace").strip().splitlines()
    tail = detail[-1] if detail else f"exit {completed.returncode}"
    raise RuntimeError(
        "powermetrics needs noninteractive root access; run this script with sudo "
        "or authorize this exact sampler command, then retry. " + tail
    )


def _exit_status(status):
    if os.WIFEXITED(status):
        return os.WEXITSTATUS(status)
    if os.WIFSIGNALED(status):
        return -os.WTERMSIG(status)
    return -1


def run_one(argv, label, sudo, powermetrics, sample_rate_ms):
    """Launch one direct target and collect its single, PID-matched delta."""
    if not argv:
        raise RuntimeError(f"{label} has an empty argv")
    target = subprocess.Popen(argv)
    start = time.perf_counter()
    try:
        # A row for an already-exited process may be billed to DEAD_TASKS.
        time.sleep(min(sample_rate_ms / 4_000, 0.05))
        if target.poll() is not None:
            raise RuntimeError(f"{label} exited before powermetrics could sample its PID")
        sampler = subprocess.run(
            _sampler_command(sudo, powermetrics, sample_rate_ms),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        _, status, usage = os.wait4(target.pid, 0)
        wall_seconds = time.perf_counter() - start
    except BaseException:
        if target.poll() is None:
            target.kill()
            os.waitpid(target.pid, 0)
        raise
    if _exit_status(status) != 0:
        raise RuntimeError(f"{label} exited {_exit_status(status)}")
    if sampler.returncode != 0:
        detail = sampler.stderr.decode("utf-8", "replace").strip().splitlines()
        raise RuntimeError(f"powermetrics exited {sampler.returncode}: {detail[-1] if detail else ''}")
    parsed = parse_process_sample(sampler.stdout, target.pid, Path(argv[0]).name)
    parsed.update(
        cpu_seconds=usage.ru_utime + usage.ru_stime,
        wall_seconds=wall_seconds,
        argv=list(argv),
    )
    return parsed


def paired_ratios(runs_by_label):
    output = {}
    labels = list(runs_by_label)
    vacos = [label for label in labels if label.startswith("vaco")]
    references = [label for label in labels if label.startswith(("ffmpeg", "ffprobe"))]
    candidates = [label for label in labels if label.startswith("candidate")]
    for numerator, denominator in [
        *((vaco, reference) for vaco in vacos for reference in references),
        *((candidate, vaco) for candidate in candidates for vaco in vacos),
    ]:
        by_round = {label: {run["round"]: run for run in runs_by_label[label]}
                    for label in (numerator, denominator)}
        common = sorted(by_round[numerator].keys() & by_round[denominator].keys())
        metrics = {}
        for metric in (*REQUIRED_COUNTERS, "cpu_seconds", "wall_seconds"):
            values = []
            for round_number in common:
                left = by_round[numerator][round_number]
                right = by_round[denominator][round_number]
                left_value = left.get("counters", {}).get(metric, left.get(metric))
                right_value = right.get("counters", {}).get(metric, right.get(metric))
                if left_value is not None and right_value:
                    values.append(left_value / right_value)
            if values:
                metrics[metric] = {"n": len(values), "median": median(values), "all": values}
        output[f"{numerator}/{denominator}"] = metrics
    return output


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("spec", help="workload spec emitted by perf-baseline-gen-spec.py")
    parser.add_argument("--out", required=True)
    parser.add_argument("--rounds", type=int, default=10)
    parser.add_argument("--warmups", type=int, default=1)
    parser.add_argument("--jobs")
    parser.add_argument("--cmds")
    parser.add_argument("--powermetrics", default="/usr/bin/powermetrics")
    parser.add_argument("--sudo", default="sudo")
    parser.add_argument("--sample-rate-ms", type=int, default=250)
    args = parser.parse_args()
    try:
        validate_rounds(args.rounds)
    except ValueError as exc:
        parser.error(str(exc))
    if args.warmups < 0 or args.sample_rate_ms <= 0:
        parser.error("--warmups must be non-negative and --sample-rate-ms must be positive")
    if platform.system() != "Darwin":
        parser.error("powermetrics IPC counters require macOS")
    if shutil.which(args.powermetrics) is None:
        parser.error(f"powermetrics was not found: {args.powermetrics}")
    if os.geteuid() != 0 and shutil.which(args.sudo) is None:
        parser.error(f"sudo was not found: {args.sudo}")
    try:
        preflight(args.sudo, args.powermetrics)
    except RuntimeError as exc:
        parser.error(str(exc))

    spec = json.loads(Path(args.spec).read_text(encoding="utf-8"))
    job_pattern = re.compile(args.jobs) if args.jobs else None
    command_pattern = re.compile(args.cmds) if args.cmds else None
    results = {
        "backend": "powermetrics-show-process-ipc",
        "counter_contract": "PID-matched delta rates times reported elapsed interval",
        "rounds": args.rounds,
        "warmups": args.warmups,
        "sample_rate_ms": args.sample_rate_ms,
        "host": {"platform": platform.platform(), "machine": platform.machine()},
        "jobs": {},
    }
    failures = 0
    for job in spec:
        if job_pattern and not job_pattern.search(job["name"]):
            continue
        commands = {label: argv for label, argv in job["cmds"].items()
                    if not command_pattern or command_pattern.search(label)}
        if len(commands) < 2:
            continue
        labels = list(commands)
        runs = {label: [] for label in labels}
        orders = []
        for iteration in range(args.warmups + args.rounds):
            order = rotating_order(labels, iteration)
            measured = iteration >= args.warmups
            if measured:
                orders.append(order)
            for label in order:
                try:
                    record = run_one(commands[label], label, args.sudo, args.powermetrics,
                                     args.sample_rate_ms)
                except RuntimeError as exc:
                    failures += 1
                    print(f"FAILED {job['name']} {label}: {exc}", file=sys.stderr)
                    continue
                if measured:
                    record["round"] = iteration - args.warmups
                    runs[label].append(record)
        results["jobs"][job["name"]] = {
            "commands": commands,
            "rotation_orders": orders,
            "runs": runs,
            "ratios": paired_ratios(runs),
        }
        Path(args.out).write_text(json.dumps(results, indent=2), encoding="utf-8")
    if failures:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
