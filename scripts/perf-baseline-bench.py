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
"""
import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

# bench.py itself is fixture/binary-agnostic -- every command it runs comes
# from the spec JSON (see gen_spec.py, which does read VACO_BIN/E2E_DIR).


def run_timed(argv, label):
    """Run argv, discarding stdout to devnull, checking exit status."""
    t0 = time.perf_counter()
    with open("/dev/null", "wb") as devnull:
        proc = subprocess.run(argv, stdout=devnull, stderr=subprocess.PIPE)
    t1 = time.perf_counter()
    if proc.returncode != 0:
        return None, proc.returncode, proc.stderr.decode("utf-8", "replace")[-2000:]
    return t1 - t0, 0, None


def median(xs):
    xs = sorted(xs)
    n = len(xs)
    if n == 0:
        return None
    if n % 2:
        return xs[n // 2]
    return (xs[n // 2 - 1] + xs[n // 2]) / 2


def interleaved(cmds, rounds):
    """cmds: dict[name] -> argv. Runs all names once per round, in rotating
    start order, and returns dict[name] -> list of durations (successes only).
    Aborts loudly (prints to stderr, keeps going) on any nonzero exit."""
    names = list(cmds.keys())
    results = {n: [] for n in names}
    for r in range(rounds):
        order = names[r % len(names):] + names[: r % len(names)]
        for n in order:
            dur, rc, err = run_timed(cmds[n], n)
            if rc != 0:
                print(f"  [round {r}] FAILED name={n} rc={rc} argv={cmds[n]}\n    stderr_tail={err}", file=sys.stderr)
                continue
            results[n].append(dur)
            print(f"  [round {r}] {n}: {dur:.4f}s", file=sys.stderr)
    return results


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
    args = ap.parse_args()

    spec = json.loads(Path(args.spec).read_text())
    all_results = {}
    for job in spec:
        name = job["name"]
        cmds = {k: v for k, v in job["cmds"].items()}
        rounds = job.get("rounds", args.rounds)
        print(f"=== {name} ({rounds} rounds) ===", file=sys.stderr)
        res = interleaved(cmds, rounds)
        all_results[name] = summarize(res)
        Path(args.out).write_text(json.dumps(all_results, indent=2))
    print(json.dumps(all_results, indent=2))


if __name__ == "__main__":
    main()
