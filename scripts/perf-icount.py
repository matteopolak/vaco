#!/usr/bin/env python3
"""Instruction-count harness: run a workload spec under Valgrind's cachegrind.

Why this exists: wall clock and CPU-seconds are both load-dependent on the
machines this project is developed on (see docs/instruction-count-benchmarking.md
for the measurement that shows CPU-seconds moving 1.5x for identical work purely
because the scheduler put it on efficiency cores). Cachegrind *simulates* rather
than samples, so its instruction count for a deterministic single-threaded run is
the same number on an idle machine and on one at load average 90.

It consumes the SAME spec JSON that `perf-baseline-bench.py` consumes -- see
`perf-baseline-gen-spec.py`, which reads VACO_BIN / VACO_PROBE_BIN / E2E_DIR --
so command shapes are defined once, not twice. Point E2E_DIR at a *small*
fixture set (see perf-icount-fixtures.sh): cachegrind runs ~50-100x slower than
native, so 4K fixtures are not usable here.

What this instrument CANNOT see, and must not be used for:
  * threading, saturation or stalls -- valgrind serialises threads;
  * out-of-order execution, prefetch, branch-misprediction cost -- not modelled;
  * SIMD efficiency -- one NEON instruction can do 16 lanes of work, so a lower
    instruction count is NOT necessarily faster, and an optimiser chasing this
    number can de-vectorise code and make it slower.
Wall-clock A/B against a same-session ffmpeg run (perf-baseline-bench.py) stays
the ground truth for "are we faster".

Conventions inherited from scripts/perf-baseline-*: env-var driven, and every
subprocess's exit status is checked -- a nonzero exit is reported and its sample
discarded, never silently counted.

Usage:
    python3 scripts/perf-icount.py --spec spec.json [--jobs REGEX]
                                   [--repeats N] [--cache-sim] [--branch-sim]
                                   [--top N] [--out results.json]
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


def load_average():
    try:
        return os.getloadavg()[0]
    except (OSError, AttributeError):
        return None


def parse_cachegrind(path):
    """Return (summary_events: dict, per_fn: dict[name -> Ir]).

    Parses the cachegrind output file directly rather than shelling out to
    cg_annotate: the file format is stable and documented, cg_annotate's
    human-readable output is not.
    """
    events = []
    summary = {}
    per_fn = {}
    cur_fn = None
    prev_was_calls = False
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        for line in fh:
            line = line.rstrip("\n")
            if line.startswith("events:"):
                events = line.split(":", 1)[1].split()
            elif line.startswith("summary:"):
                vals = [int(x) for x in line.split(":", 1)[1].split()]
                summary = dict(zip(events, vals))
            elif line.startswith("fn="):
                cur_fn = line[3:]
                prev_was_calls = False
            elif line.startswith("calls="):
                prev_was_calls = True
            elif line and (line[0].isdigit() or line[0] in "+-*"):
                # A cost line. The one directly after `calls=` is the callee's
                # inclusive cost, not this function's own, so it is skipped.
                if prev_was_calls:
                    prev_was_calls = False
                    continue
                parts = line.split()
                if cur_fn is not None and len(parts) >= 2:
                    try:
                        per_fn[cur_fn] = per_fn.get(cur_fn, 0) + int(parts[1])
                    except ValueError:
                        pass
            else:
                prev_was_calls = False
    return summary, per_fn


def run_one(argv, valgrind, cache_sim, branch_sim, workdir):
    """Run argv under cachegrind. Returns (summary, per_fn) or raises."""
    out_file = Path(workdir) / "cg.out"
    if out_file.exists():
        out_file.unlink()
    vg = [
        valgrind,
        "--tool=cachegrind",
        f"--cachegrind-out-file={out_file}",
        f"--cache-sim={'yes' if cache_sim else 'no'}",
        f"--branch-sim={'yes' if branch_sim else 'no'}",
        "--",
    ] + list(argv)
    with open(os.devnull, "wb") as devnull:
        proc = subprocess.run(vg, stdout=devnull, stderr=subprocess.PIPE)
    if proc.returncode != 0:
        raise RuntimeError(
            f"exit {proc.returncode} from {argv}\n"
            + proc.stderr.decode("utf-8", "replace")[-2000:]
        )
    if not out_file.exists():
        raise RuntimeError(f"cachegrind wrote no output file for {argv}")
    return parse_cachegrind(out_file)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--spec", required=True, help="workload spec JSON (see perf-baseline-gen-spec.py)")
    ap.add_argument("--jobs", default=None, help="regex; only jobs whose name matches are run")
    ap.add_argument("--cmds", default=None,
                    help="regex; only command labels matching are run. Default excludes the\n"
                         "multi-threaded variants, whose counts valgrind cannot make meaningful\n"
                         "(it serialises threads).")
    ap.add_argument("--repeats", type=int, default=2,
                    help="runs per command. >1 exists to VERIFY determinism, not to average.")
    ap.add_argument("--cache-sim", action="store_true", help="also simulate D1/LL caches (slower)")
    ap.add_argument("--branch-sim", action="store_true", help="also simulate branch prediction (slower)")
    ap.add_argument("--top", type=int, default=0, help="report the top N functions by Ir")
    ap.add_argument("--startup-floor", action="store_true",
                    help="also measure `<binary> -version` for every binary in the spec and "
                         "report floor-corrected ratios. Process startup is a FIXED cost that "
                         "an instruction count includes in full: ffmpeg's is ~190M "
                         "instructions, which on a short fixture is most of the total and "
                         "makes vaco look better than it is.")
    ap.add_argument("--out", default=None, help="write results JSON here")
    ap.add_argument("--valgrind", default=os.environ.get("VALGRIND_BIN", "valgrind"))
    args = ap.parse_args()

    valgrind = shutil.which(args.valgrind)
    if valgrind is None:
        print(
            f"error: {args.valgrind} not found. Cachegrind does not support macOS on\n"
            "Apple silicon; run this inside the arm64 Linux container built by\n"
            "scripts/perf-icount-docker.sh, or on a Linux host.",
            file=sys.stderr,
        )
        return 2

    ver = subprocess.run([valgrind, "--version"], capture_output=True)
    if ver.returncode != 0:
        print(f"error: `{valgrind} --version` exited {ver.returncode}", file=sys.stderr)
        return 2
    valgrind_version = ver.stdout.decode().strip()

    spec = json.loads(Path(args.spec).read_text())
    pattern = re.compile(args.jobs) if args.jobs else None
    cmd_pattern = re.compile(args.cmds) if args.cmds else None

    load_before = load_average()
    results = {
        "valgrind": valgrind_version,
        "load_average_before": load_before,
        "cache_sim": args.cache_sim,
        "branch_sim": args.branch_sim,
        "jobs": {},
    }
    failures = 0

    with tempfile.TemporaryDirectory() as workdir:
        if args.startup_floor:
            results["startup_floor"] = measure_floors(
                spec, pattern, cmd_pattern, valgrind, args, workdir)
        for job in spec:
            name = job["name"]
            if pattern and not pattern.search(name):
                continue
            print(f"== {name}", file=sys.stderr)
            job_out = {}
            for label, argv in job["cmds"].items():
                if cmd_pattern and not cmd_pattern.search(label):
                    continue
                runs = []
                per_fn = {}
                try:
                    for _ in range(max(1, args.repeats)):
                        summary, fns = run_one(argv, valgrind, args.cache_sim,
                                               args.branch_sim, workdir)
                        runs.append(summary)
                        if not per_fn:
                            per_fn = fns
                except RuntimeError as exc:
                    print(f"  FAILED {label}: {exc}", file=sys.stderr)
                    failures += 1
                    job_out[label] = {"error": str(exc)}
                    continue
                irs = [r.get("Ir", 0) for r in runs]
                entry = {
                    "argv": argv,
                    "runs": runs,
                    "Ir": irs[0],
                    "Ir_runs": irs,
                    "deterministic": len(set(irs)) == 1,
                }
                if len(set(irs)) != 1:
                    spread = (max(irs) - min(irs)) / max(1, min(irs))
                    entry["Ir_spread"] = spread
                    print(f"  NOTE {label}: Ir varied across runs by {spread:.4%} "
                          f"({irs}) -- expected 0 for a deterministic single-threaded run",
                          file=sys.stderr)
                if args.top:
                    entry["top_fns"] = sorted(per_fn.items(), key=lambda kv: -kv[1])[: args.top]
                job_out[label] = entry
                print(f"  {label}: Ir={irs[0]:,}", file=sys.stderr)
            job_out["_ratios"] = ratios(job_out, results.get("startup_floor"))
            results["jobs"][name] = job_out

    results["load_average_after"] = load_average()
    text = json.dumps(results, indent=2)
    if args.out:
        Path(args.out).write_text(text)
        print(f"wrote {args.out}", file=sys.stderr)
    else:
        print(text)

    print_table(results)
    if failures:
        print(f"\n{failures} command(s) failed; their samples were discarded.", file=sys.stderr)
        return 1
    return 0


def measure_floors(spec, pattern, cmd_pattern, valgrind, args, workdir):
    """Ir for `<binary> -version` for every binary the selected jobs invoke.

    Process startup -- dynamic linking, table construction, argument parsing --
    is a fixed cost that lands in full in every instruction count. It is ~190M
    instructions for the reference binaries and under 1M for vaco's, so on a
    short fixture the raw ratio is dominated by it. Reported so the corrected
    ratio can be read beside the raw one, never instead of it.
    """
    binaries = []
    for job in spec:
        if pattern and not pattern.search(job["name"]):
            continue
        for label, argv in job["cmds"].items():
            if cmd_pattern and not cmd_pattern.search(label):
                continue
            if argv and argv[0] not in binaries:
                binaries.append(argv[0])
    out = {}
    for b in binaries:
        try:
            summary, _ = run_one([b, "-version"], valgrind, args.cache_sim,
                                 args.branch_sim, workdir)
            out[b] = summary.get("Ir", 0)
            print(f"  startup floor {b}: Ir={out[b]:,}", file=sys.stderr)
        except RuntimeError as exc:
            print(f"  startup floor {b}: unavailable ({exc.args[0].splitlines()[0]})",
                  file=sys.stderr)
    return out


def ratios(job_out, floors=None):
    """vaco:ffmpeg Ir ratios, keyed <vaco label>/<ffmpeg label>."""
    vacos = [k for k, v in job_out.items() if k.startswith("vaco") and "Ir" in v]
    refs = [k for k, v in job_out.items() if ("ffmpeg" in k or "ffprobe" in k) and "Ir" in v]
    out = {}
    for v in vacos:
        for r in refs:
            denom = job_out[r]["Ir"]
            if denom:
                out[f"{v}/{r}"] = job_out[v]["Ir"] / denom
            if floors:
                fv = floors.get(job_out[v]["argv"][0])
                fr = floors.get(job_out[r]["argv"][0])
                if fv is not None and fr is not None and job_out[r]["Ir"] - fr > 0:
                    out[f"{v}/{r} (minus startup)"] = (
                        (job_out[v]["Ir"] - fv) / (job_out[r]["Ir"] - fr)
                    )
    return out


def print_table(results):
    floors = results.get("startup_floor")
    if floors:
        print("\n| binary | startup floor (Ir for `-version`) |")
        print("|---|---:|")
        for b, ir in floors.items():
            print(f"| `{b}` | {ir:,} |")
    print("\n| workload | command | Ir (instructions) | deterministic |")
    print("|---|---|---:|---|")
    for name, job in results["jobs"].items():
        for label, entry in job.items():
            if label == "_ratios":
                continue
            if "error" in entry:
                print(f"| {name} | {label} | FAILED | — |")
                continue
            det = "yes" if entry["deterministic"] else f"NO ({entry.get('Ir_spread', 0):.4%})"
            print(f"| {name} | {label} | {entry['Ir']:,} | {det} |")
    print("\n| workload | ratio | value |")
    print("|---|---|---:|")
    for name, job in results["jobs"].items():
        for k, v in job.get("_ratios", {}).items():
            print(f"| {name} | {k} | {v:.2f}x |")


if __name__ == "__main__":
    sys.exit(main())
