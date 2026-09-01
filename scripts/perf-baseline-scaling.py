#!/usr/bin/env python3
"""Scaling + memory harness: /usr/bin/time -l at 1/2/4/8/16 threads, on named
fixtures, one launch per thread count (interleaved across thread counts within
each round), wall clock + CPU% + peak RSS parsed from the -l output.
Checks exit status; a failed run is reported, not silently timed.
"""
import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

import os

VACO = os.environ.get("VACO_BIN", "./target/dist/vaco")

TIME_RE = re.compile(r"^\s*([\d.]+)\s+real\s+([\d.]+)\s+user\s+([\d.]+)\s+sys")
RSS_RE = re.compile(r"^\s*(\d+)\s+maximum resident set size")


def run_once(fixture, threads, extra_args):
    cmd = ["/usr/bin/time", "-l", VACO, "-threads", str(threads), "-i", fixture,
           "-map", "0:v:0", "-c:v", "rawvideo", "-f", "null", "-"] + extra_args
    proc = subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    if proc.returncode != 0:
        return None
    err = proc.stderr
    wall = user = sys_ = None
    rss = None
    for line in err.splitlines():
        m = TIME_RE.match(line)
        if m:
            wall, user, sys_ = (float(m.group(1)), float(m.group(2)), float(m.group(3)))
        m2 = RSS_RE.match(line.strip())
        if m2:
            rss = int(m2.group(1))
    if wall is None or rss is None:
        print("PARSE FAILURE, stderr tail:\n" + "\n".join(err.splitlines()[-15:]), file=sys.stderr)
        return None
    cpu_pct = 100.0 * (user + sys_) / wall if wall > 0 else None
    return {"wall": wall, "user": user, "sys": sys_, "cpu_pct": cpu_pct, "peak_rss_mib": rss / (1024 * 1024)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("fixture")
    ap.add_argument("--label", required=True)
    ap.add_argument("--threads", default="1,2,4,8,16")
    ap.add_argument("--rounds", type=int, default=3)
    ap.add_argument("--out", required=True)
    ap.add_argument("--extra", nargs="*", default=[])
    args = ap.parse_args()

    thread_counts = [int(x) for x in args.threads.split(",")]
    out_path = Path(args.out)
    all_results = json.loads(out_path.read_text()) if out_path.exists() else {}
    results = {t: [] for t in thread_counts}

    for r in range(args.rounds):
        order = thread_counts[r % len(thread_counts):] + thread_counts[: r % len(thread_counts)]
        for t in order:
            res = run_once(args.fixture, t, args.extra)
            if res is None:
                print(f"[round {r}] threads={t} FAILED", file=sys.stderr)
                continue
            results[t].append(res)
            print(f"[round {r}] threads={t}: wall={res['wall']:.3f}s cpu={res['cpu_pct']:.0f}% rss={res['peak_rss_mib']:.0f}MiB", file=sys.stderr)

    all_results[args.label] = results
    out_path.write_text(json.dumps(all_results, indent=2))
    print(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()
