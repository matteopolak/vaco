#!/usr/bin/env python3
"""Measurement harness for the vaco perf baseline.

Rules followed (per planning/AGENT-CONSTRAINTS.md and the task brief):
- every subprocess's exit status is checked; a nonzero exit aborts that sample
  and is reported, never silently timed.
- ffmpeg is always invoked with -y (never prompts) and writes to /dev/null
  through -f null/-f rawvideo > /dev/null, never through a bare pipe with
  2>/dev/null (which would swallow /usr/bin/time's own stderr output).
- stdout of the decoded stream is redirected to /dev/null via a *file*
  descriptor, never merged with stderr (2>&1), so progress/log text on
  stderr cannot contaminate a byte-exactness check done separately.
- wall clock via time.perf_counter(); no cycle counter is available
  (unsafe is forbidden workspace-wide).
- results are interleaved: each round runs vaco then ffmpeg (or the reverse,
  alternated) before moving to the next round, and medians/ranges are
  reported, not single numbers.
- the 1-minute load average is sampled around every run and reported beside the
  timing. Above --max-load (default: one per core) a wall-clock number on this
  machine is not a measurement of the program; PERF-BASELINE.md has rows with an
  11x spread inside a single interleaved job for exactly this reason. Use
  --refuse-under-load in CI or any unattended run. For a load-immune comparison,
  use scripts/perf-icount.py instead -- see
  docs/instruction-count-benchmarking.md.
"""
import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path


def load_average():
    """1-minute load average, or None where the platform has none."""
    try:
        return os.getloadavg()[0]
    except (OSError, AttributeError):
        return None

# bench.py itself is fixture/binary-agnostic -- every command it runs comes
# from the spec JSON (see gen_spec.py, which does read VACO_BIN/E2E_DIR).


def run_timed(argv, label):
    """Run argv, discarding stdout to devnull, checking exit status.

    Returns (duration, returncode, stderr_tail, load) -- load is the 1-minute
    average sampled immediately after the run.
    """
    t0 = time.perf_counter()
    with open("/dev/null", "wb") as devnull:
        proc = subprocess.run(argv, stdout=devnull, stderr=subprocess.PIPE)
    t1 = time.perf_counter()
    load = load_average()
    if proc.returncode != 0:
        return None, proc.returncode, proc.stderr.decode("utf-8", "replace")[-2000:], load
    return t1 - t0, 0, None, load


def median(xs):
    xs = sorted(xs)
    n = len(xs)
    if n == 0:
        return None
    if n % 2:
        return xs[n // 2]
    return (xs[n // 2 - 1] + xs[n // 2]) / 2


def interleaved(cmds, rounds, max_load=None):
    """cmds: dict[name] -> argv. Runs all names once per round, in rotating
    start order, and returns (dict[name] -> list of durations, list of loads).
    Aborts loudly (prints to stderr, keeps going) on any nonzero exit."""
    names = list(cmds.keys())
    results = {n: [] for n in names}
    loads = []
    for r in range(rounds):
        order = names[r % len(names):] + names[: r % len(names)]
        for n in order:
            dur, rc, err, load = run_timed(cmds[n], n)
            if load is not None:
                loads.append(load)
            if rc != 0:
                print(f"  [round {r}] FAILED name={n} rc={rc} argv={cmds[n]}\n    stderr_tail={err}", file=sys.stderr)
                continue
            results[n].append(dur)
            flag = ""
            if max_load is not None and load is not None and load > max_load:
                flag = f"  <-- LOAD {load:.1f} > {max_load:.1f}: wall clock here is noise"
            print(f"  [round {r}] {n}: {dur:.4f}s (load {load})" + flag, file=sys.stderr)
    return results, loads


def summarize(results):
    out = {}
    for n, xs in results.items():
        if not xs:
            out[n] = {"n": 0, "median": None, "min": None, "max": None}
            continue
        out[n] = {
            "n": len(xs),
            "median": median(xs),
            "min": min(xs),
            "max": max(xs),
            "all": xs,
        }
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("spec", help="path to a JSON file describing the workload matrix")
    ap.add_argument("--rounds", type=int, default=5)
    ap.add_argument("--out", required=True)
    ap.add_argument("--max-load", type=float, default=float(os.cpu_count() or 1),
                    help="1-minute load average above which wall-clock results are "
                         "flagged as unusable (default: one per core)")
    ap.add_argument("--refuse-under-load", action="store_true",
                    help="exit nonzero instead of producing numbers taken above --max-load")
    args = ap.parse_args()

    start_load = load_average()
    if start_load is not None and start_load > args.max_load:
        msg = (f"load average is {start_load:.1f}, above --max-load {args.max_load:.1f}. "
               "Wall-clock numbers taken now measure the machine, not the program.")
        if args.refuse_under_load:
            print(f"REFUSING: {msg}", file=sys.stderr)
            return 2
        print(f"WARNING: {msg}", file=sys.stderr)

    spec = json.loads(Path(args.spec).read_text())
    all_results = {}
    flagged = []
    for job in spec:
        name = job["name"]
        cmds = {k: v for k, v in job["cmds"].items()}
        rounds = job.get("rounds", args.rounds)
        print(f"=== {name} ({rounds} rounds) ===", file=sys.stderr)
        res, loads = interleaved(cmds, rounds, args.max_load)
        summary = summarize(res)
        peak = max(loads) if loads else None
        summary["_load"] = {
            "max_load_threshold": args.max_load,
            "peak_1min_load": peak,
            # The flag is on the JSON, not just the console, so a later reader of
            # the results file cannot quote a number without seeing this.
            "unusable_wall_clock": bool(peak is not None and peak > args.max_load),
        }
        if summary["_load"]["unusable_wall_clock"]:
            flagged.append(name)
        all_results[name] = summary
        Path(args.out).write_text(json.dumps(all_results, indent=2))
    print(json.dumps(all_results, indent=2))
    if flagged:
        print(f"\n{len(flagged)} job(s) ran with the load average above "
              f"{args.max_load:.1f} and are flagged unusable_wall_clock in the "
              f"results: {', '.join(flagged)}", file=sys.stderr)
        if args.refuse_under_load:
            return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
