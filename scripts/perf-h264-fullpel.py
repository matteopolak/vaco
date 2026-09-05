#!/usr/bin/env python3
"""Strict H.264 full-pel motion-compensation A/B harness.

It streams each decode into a byte counter and SHA-256 digest, never storing
raw video, then records interleaved baseline/candidate/ffmpeg wall and child
CPU seconds.  A run is invalid unless all three outputs agree at every
requested thread count.  This is intentionally a harness, not a fixture
generator or build driver.
"""
import argparse
import hashlib
import json
import os
import resource
import subprocess
import sys
import threading
import time
from pathlib import Path


def median(values):
    ordered = sorted(values)
    if not ordered:
        return None
    middle = len(ordered) // 2
    if len(ordered) % 2:
        return ordered[middle]
    return (ordered[middle - 1] + ordered[middle]) / 2


def parse_threads(value):
    threads = [int(item) for item in value.split(",")]
    if not threads or any(item <= 0 for item in threads) or len(set(threads)) != len(threads):
        raise argparse.ArgumentTypeError("threads must be unique positive integers, such as 1,2,4,8")
    return threads


def load_average():
    try:
        return os.getloadavg()[0]
    except (OSError, AttributeError):
        return None


def vaco_command(binary, fixture, threads):
    return [
        binary, "-threads", str(threads), "-i", str(fixture), "-map", "0:v:0",
        "-c:v", "rawvideo", "-f", "rawvideo", "-",
    ]


def ffmpeg_command(binary, fixture, threads):
    return [
        binary, "-v", "error", "-threads", str(threads), "-i", str(fixture),
        "-map", "0:v:0", "-pix_fmt", "yuv420p", "-f", "rawvideo", "-",
    ]


def child_cpu_seconds():
    usage = resource.getrusage(resource.RUSAGE_CHILDREN)
    return usage.ru_utime + usage.ru_stime


def read_stderr(stream, sink):
    data = stream.read()
    sink.append(data.decode("utf-8", "replace")[-2000:])


def stream_decode(argv):
    """Run one command and return its complete-output identity and timing."""
    stderr = []
    before_cpu = child_cpu_seconds()
    started = time.perf_counter()
    process = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    assert process.stdout is not None
    assert process.stderr is not None
    stderr_thread = threading.Thread(target=read_stderr, args=(process.stderr, stderr))
    stderr_thread.start()
    digest = hashlib.sha256()
    byte_count = 0
    while chunk := process.stdout.read(1 << 20):
        digest.update(chunk)
        byte_count += len(chunk)
    returncode = process.wait()
    stderr_thread.join()
    return {
        "argv": argv,
        "returncode": returncode,
        "bytes": byte_count,
        "sha256": digest.hexdigest(),
        "wall_seconds": time.perf_counter() - started,
        "child_cpu_seconds": child_cpu_seconds() - before_cpu,
        "load_1min": load_average(),
        "stderr_tail": stderr[0] if stderr else "",
    }


def identity(record):
    return record["returncode"], record["bytes"], record["sha256"]


def verify_thread(commands, thread_count):
    records = {name: stream_decode(argv) for name, argv in commands.items()}
    failures = {name: record for name, record in records.items() if record["returncode"] != 0}
    if failures:
        detail = "; ".join(
            f"{name} exit={record['returncode']} stderr={record['stderr_tail']!r}"
            for name, record in failures.items()
        )
        raise RuntimeError(f"threads={thread_count}: decode command failed: {detail}")
    baseline = identity(records["baseline"])
    mismatches = {name: identity(record) for name, record in records.items() if identity(record) != baseline}
    if mismatches:
        raise RuntimeError(
            f"threads={thread_count}: rawvideo identity mismatch; baseline={baseline}, others={mismatches}"
        )
    return records


def paired_ratio(records, left, right, metric):
    values = []
    for round_records in records:
        denominator = round_records[right][metric]
        if denominator <= 0:
            continue
        values.append(round_records[left][metric] / denominator)
    return {
        "n": len(values),
        "median": median(values),
        "min": min(values) if values else None,
        "max": max(values) if values else None,
        "all": values,
        "wins": sum(value < 1.0 for value in values),
    }


def benchmark_thread(commands, thread_count, rounds, max_load):
    labels = list(commands)
    results = []
    for round_number in range(rounds):
        order = labels[round_number % len(labels):] + labels[:round_number % len(labels)]
        round_records = {}
        for label in order:
            record = stream_decode(commands[label])
            if record["returncode"] != 0:
                raise RuntimeError(
                    f"threads={thread_count} round={round_number} {label}: exit={record['returncode']} "
                    f"stderr={record['stderr_tail']!r}"
                )
            if max_load is not None and record["load_1min"] is not None and record["load_1min"] > max_load:
                raise RuntimeError(
                    f"threads={thread_count} round={round_number} {label}: load "
                    f"{record['load_1min']:.1f} exceeds quiet-lane limit {max_load:.1f}"
                )
            round_records[label] = record
        baseline = identity(round_records["baseline"])
        if any(identity(record) != baseline for record in round_records.values()):
            raise RuntimeError(f"threads={thread_count} round={round_number}: rawvideo identity changed")
        results.append(round_records)
    summary = {}
    for label in labels:
        summary[label] = {
            metric: {
                "median": median([round_records[label][metric] for round_records in results]),
                "all": [round_records[label][metric] for round_records in results],
            }
            for metric in ("wall_seconds", "child_cpu_seconds")
        }
    for metric in ("wall_seconds", "child_cpu_seconds"):
        summary[f"candidate_over_baseline_{metric}"] = paired_ratio(
            results, "candidate", "baseline", metric
        )
        summary[f"candidate_over_ffmpeg_{metric}"] = paired_ratio(
            results, "candidate", "ffmpeg", metric
        )
    return {"rounds": results, "summary": summary}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture", required=True, type=Path)
    parser.add_argument("--baseline", required=True, help="pre-change Vaco binary")
    parser.add_argument("--candidate", required=True, help="candidate Vaco binary")
    parser.add_argument("--ffmpeg", default="ffmpeg")
    parser.add_argument("--threads", default="1,2,4,8", type=parse_threads)
    parser.add_argument("--rounds", type=int, default=12)
    parser.add_argument("--max-load", type=float, required=True,
                        help="refuse all measurements above this 1-minute load")
    parser.add_argument("--verify-only", action="store_true")
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    if args.rounds < 12 and not args.verify_only:
        parser.error("--rounds must be at least 12 for a performance claim")
    if not args.fixture.is_file():
        parser.error(f"fixture is not a file: {args.fixture}")
    if load_average() is not None and load_average() > args.max_load:
        parser.error(f"refusing non-quiet lane: load {load_average():.1f} > {args.max_load:.1f}")

    result = {
        "fixture": str(args.fixture),
        "threads": args.threads,
        "rounds": args.rounds,
        "max_load": args.max_load,
        "verification": {},
        "benchmarks": {},
    }
    for thread_count in args.threads:
        commands = {
            "baseline": vaco_command(args.baseline, args.fixture, thread_count),
            "candidate": vaco_command(args.candidate, args.fixture, thread_count),
            "ffmpeg": ffmpeg_command(args.ffmpeg, args.fixture, thread_count),
        }
        result["verification"][str(thread_count)] = verify_thread(commands, thread_count)
        if not args.verify_only:
            result["benchmarks"][str(thread_count)] = benchmark_thread(
                commands, thread_count, args.rounds, args.max_load
            )
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
