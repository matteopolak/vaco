#!/usr/bin/env python3
"""Protocol-strict Scalar/NEON H.264 MC criterion harness.

Build `h264_mc_criterion` in a private release target first, then pass its
path here. Each pair is run in alternating order for at least ten rounds.
The worker validates exact output; this harness records its inner-loop wall
time, whole-child CPU time, and load around every subprocess.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import resource
import statistics
import subprocess
import sys
import time
from pathlib import Path

KERNELS = (
    "fir-bilinear",
    "fir-h264",
    "h264-luma",
    "h264-chroma",
    "h264-uni",
    "h264-bi",
)
VARIANTS = ("scalar", "neon")


def child_cpu_seconds() -> float:
    usage = resource.getrusage(resource.RUSAGE_CHILDREN)
    return usage.ru_utime + usage.ru_stime


def invoke(binary: Path, kernel: str, variant: str, iterations: int, max_load: float) -> dict:
    load_before = os.getloadavg()[0]
    if load_before > max_load:
        raise RuntimeError(
            f"load {load_before:.2f} exceeds --max-load {max_load:.2f} before "
            f"{kernel}/{variant}"
        )
    cpu_before = child_cpu_seconds()
    wall_before = time.perf_counter()
    completed = subprocess.run(
        [str(binary), kernel, variant, str(iterations)],
        check=False,
        capture_output=True,
        text=True,
    )
    process_wall = time.perf_counter() - wall_before
    process_cpu = child_cpu_seconds() - cpu_before
    load_after = os.getloadavg()[0]
    if completed.returncode != 0:
        raise RuntimeError(
            f"{kernel}/{variant} exited {completed.returncode}: {completed.stderr.strip()}"
        )
    try:
        worker = json.loads(completed.stdout.strip().splitlines()[-1])
    except (IndexError, json.JSONDecodeError) as error:
        raise RuntimeError(
            f"{kernel}/{variant} did not emit worker JSON: {completed.stdout!r}"
        ) from error
    if load_after > max_load:
        raise RuntimeError(
            f"load {load_after:.2f} exceeds --max-load {max_load:.2f} after "
            f"{kernel}/{variant}"
        )
    return {
        **worker,
        "process_wall_seconds": process_wall,
        "child_cpu_seconds": process_cpu,
        "load_before": load_before,
        "load_after": load_after,
    }


def calibrate(binary: Path, kernel: str, min_run_ms: float, max_load: float) -> int:
    iterations = 1
    target_ns = min_run_ms * 1_000_000.0
    while True:
        samples = [invoke(binary, kernel, variant, iterations, max_load) for variant in VARIANTS]
        if max(sample["elapsed_ns"] for sample in samples) >= target_ns:
            return iterations
        if iterations >= 1 << 30:
            raise RuntimeError(f"unable to calibrate {kernel}")
        iterations *= 2


def median_per_call(rows: list[dict], field: str) -> float:
    if field == "elapsed_ns":
        return statistics.median(row[field] / row["iterations"] for row in rows)
    return statistics.median(row[field] / row["iterations"] for row in rows)


def build_summary(records: list[dict]) -> dict:
    summary: dict[str, dict] = {}
    for kernel in KERNELS:
        by_variant = {
            variant: [
                row for row in records if row["kernel"] == kernel and row["variant"] == variant
            ]
            for variant in VARIANTS
        }
        scalar_ns = median_per_call(by_variant["scalar"], "elapsed_ns")
        neon_ns = median_per_call(by_variant["neon"], "elapsed_ns")
        summary[kernel] = {
            "iterations": by_variant["scalar"][0]["iterations"],
            "scalar_median_ns_per_call": scalar_ns,
            "neon_median_ns_per_call": neon_ns,
            "speedup_scalar_over_neon": scalar_ns / neon_ns,
            "scalar_median_child_cpu_seconds_per_call": median_per_call(
                by_variant["scalar"], "child_cpu_seconds"
            ),
            "neon_median_child_cpu_seconds_per_call": median_per_call(
                by_variant["neon"], "child_cpu_seconds"
            ),
            "checksum": by_variant["scalar"][0]["checksum"],
        }
    return summary


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--rounds", type=int, default=12)
    parser.add_argument("--min-run-ms", type=float, default=100.0)
    parser.add_argument("--max-load", type=float, required=True)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    if args.rounds < 10:
        parser.error("--rounds must be at least 10")
    if args.min_run_ms <= 0:
        parser.error("--min-run-ms must be positive")
    if not args.binary.is_file():
        parser.error(f"worker binary does not exist: {args.binary}")
    return args


def main() -> int:
    args = parse_args()
    iterations = {
        kernel: calibrate(args.binary, kernel, args.min_run_ms, args.max_load)
        for kernel in KERNELS
    }
    records: list[dict] = []
    checksums: dict[str, int] = {}
    for round_index in range(args.rounds):
        for kernel_index, kernel in enumerate(KERNELS):
            order = VARIANTS if (round_index + kernel_index) % 2 == 0 else VARIANTS[::-1]
            for variant in order:
                row = invoke(args.binary, kernel, variant, iterations[kernel], args.max_load)
                row["round"] = round_index
                row["order"] = list(order)
                previous = checksums.setdefault(kernel, row["checksum"])
                if previous != row["checksum"]:
                    raise RuntimeError(
                        f"{kernel} checksum mismatch: expected {previous}, got {row['checksum']}"
                    )
                records.append(row)

    result = {
        "machine": platform.machine(),
        "platform": platform.platform(),
        "rounds": args.rounds,
        "min_run_ms": args.min_run_ms,
        "max_load": args.max_load,
        "iterations": iterations,
        "records": records,
        "summary": build_summary(records),
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(result, indent=2) + "\n")
    for kernel, row in result["summary"].items():
        print(
            f"{kernel}: scalar={row['scalar_median_ns_per_call']:.3f} ns "
            f"neon={row['neon_median_ns_per_call']:.3f} ns "
            f"speedup={row['speedup_scalar_over_neon']:.3f}x"
        )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except RuntimeError as error:
        print(f"perf-h264-mc: {error}", file=sys.stderr)
        raise SystemExit(1) from None
