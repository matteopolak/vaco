#!/usr/bin/env python3
"""Interleaved macOS CPU-counter comparisons through Instruments/xctrace.

This driver is the Apple-silicon counterpart to ``perf-hwcycles.py``.  It
launches each command through an Instruments CPU Counters template, asks
``xctrace`` for its process-aggregate table, and compares the named ``cycles``
and retired-``instructions`` metrics over alternating rounds.  CPU and wall
seconds are reported alongside the PMU values, never converted into cycles.

The supplied template must expose ``CounterMetricAggregatedForProcess`` and
explicitly name both counters.  Instruments' stock "CPU Bottlenecks" setup is
not an instruction-counting setup: its numbered columns are bottleneck classes.
The parser rejects those names rather than guessing which slot means retired
instructions.

Usage:
    python3 scripts/perf-xctrace-counters.py spec.json --out counters.json \
        --template /path/to/Instruction-Counts.tracetemplate \
        --jobs '^h264_decode_sd' --cmds '^(vaco|ffmpeg_t1|candidate)$'

Pass ``--samply-dir`` to save one separate diagnostic profile per selected
command after measurement.  Samply recordings are intentionally excluded from
counter ratios because profiling changes the program being measured.
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
import xml.etree.ElementTree as ET
from pathlib import Path


PROCESS_SCHEMA = "CounterMetricAggregatedForProcess"
REQUIRED_COUNTERS = ("cycles", "instructions")


def median(values):
    """Return a median without a third-party dependency."""
    ordered = sorted(values)
    if not ordered:
        return None
    middle = len(ordered) // 2
    if len(ordered) % 2:
        return ordered[middle]
    return (ordered[middle - 1] + ordered[middle]) / 2


def validate_rounds(rounds):
    """Enforce the repository's minimum interleaved-round protocol."""
    if rounds < 10:
        raise ValueError("at least 10 measured rounds are required")


def rotating_order(labels, round_number):
    """Rotate command start position so no label always follows the same load."""
    offset = round_number % len(labels)
    return labels[offset:] + labels[:offset]


def _local_name(value):
    return value.rsplit("}", 1)[-1]


def _number(value):
    if value is None:
        return None
    value = value.strip().replace(",", "")
    if not value:
        return None
    try:
        number = float(value)
    except ValueError:
        return None
    return int(number) if number.is_integer() else number


def _counter_name(element):
    """Find a counter's human-readable name in common xctrace XML forms."""
    for key in ("name", "metric", "label", "title", "event"):
        value = element.attrib.get(key)
        if value:
            return value
    for child in element:
        if _local_name(child.tag).casefold() not in {"name", "metric-name", "label", "title", "event"}:
            continue
        for key in ("name", "value", "label"):
            value = child.attrib.get(key)
            if value:
                return value
        if child.text and child.text.strip():
            return child.text.strip()
    name = _local_name(element.tag)
    if name.casefold() not in {"counter", "metric", "value", "column", "cell"}:
        return name
    return ""


def _canonical_counter(name):
    normalized = re.sub(r"[^a-z0-9]+", " ", name.casefold()).strip()
    words = set(normalized.split())
    if "cycle" in words or "cycles" in words:
        return "cycles"
    if "instruction" not in words and "instructions" not in words:
        return None
    # These are CPU Bottlenecks categories, not retired instructions.
    if "bottleneck" in words or "delivery" in words or "processing" in words:
        return None
    if (
        normalized in {"instruction", "instructions"}
        or "retired" in words
        or "executed" in words
        or "count" in words
    ):
        return "instructions"
    return None


def _element_value(element):
    for key in ("value", "count", "total", "sum", "number"):
        number = _number(element.attrib.get(key))
        if number is not None:
            return number
    number = _number(element.text)
    if number is not None:
        return number
    for child in element:
        if _local_name(child.tag).casefold() not in {"value", "count", "total", "sum", "number"}:
            continue
        number = _element_value(child)
        if number is not None:
            return number
    return None


def _process_text(row):
    for element in row.iter():
        if _local_name(element.tag).casefold() != "process":
            continue
        values = [
            element.attrib.get(key, "")
            for key in ("name", "process", "path", "fmt", "pid", "id")
        ]
        values.append(element.text or "")
        rendered = " ".join(value.strip() for value in values if value.strip())
        if rendered:
            return rendered
    return ""


def _matches_process(rendered, process_name):
    wanted = Path(process_name).name.casefold()
    haystack = rendered.casefold()
    return wanted == haystack or re.search(rf"(?<![a-z0-9_.-]){re.escape(wanted)}(?![a-z0-9_.-])", haystack) is not None


def parse_process_counters(xml_text, process_name):
    """Extract named process-aggregate counters from one xctrace XML export.

    The table is already an Instruments process aggregation.  If an export has
    repeated rows for a process (for example, a hierarchy plus its leaf), the
    greatest monotonic value is retained; a falling value is rejected because it
    would turn a sampled/partial export into a fabricated total.
    """
    try:
        root = ET.fromstring(xml_text)
    except ET.ParseError as exc:
        raise ValueError(f"invalid xctrace XML: {exc}") from exc

    counters = {}
    rows = 0
    seen_process = False
    for row in root.iter():
        if _local_name(row.tag).casefold() != "row":
            continue
        rendered_process = _process_text(row)
        if not _matches_process(rendered_process, process_name):
            continue
        seen_process = True
        row_counters = {}
        for element in row.iter():
            counter = _canonical_counter(_counter_name(element))
            value = _element_value(element)
            if counter is not None and value is not None:
                row_counters[counter] = value
        if row_counters:
            rows += 1
        for counter, value in row_counters.items():
            previous = counters.get(counter)
            if previous is not None and value < previous:
                raise ValueError(
                    f"{counter} falls from {previous} to {value} for {process_name}; "
                    "export a process-aggregate table, not sampled core rows"
                )
            counters[counter] = value

    if not seen_process:
        raise ValueError(f"xctrace export contains no process named {process_name!r}")
    missing = [counter for counter in REQUIRED_COUNTERS if counter not in counters]
    if missing:
        raise ValueError(
            f"xctrace export for {process_name!r} is missing {', '.join(missing)}; "
            "use a saved CPU Counters counting-mode template with explicitly named "
            "cycles and retired instructions, not CPU Bottlenecks"
        )
    return {"process": process_name, "counters": counters, "sample_rows": rows}


def paired_ratios(runs_by_label):
    """Return per-round Vaco/reference and candidate/Vaco metric ratios."""
    output = {}
    labels = list(runs_by_label)
    vacos = [label for label in labels if label.startswith("vaco")]
    references = [label for label in labels if label.startswith(("ffmpeg", "ffprobe"))]
    candidates = [label for label in labels if label.startswith("candidate")]

    pairs = [(vaco, reference) for vaco in vacos for reference in references]
    pairs.extend((candidate, vaco) for candidate in candidates for vaco in vacos)
    for numerator_label, denominator_label in pairs:
        numerator_by_round = {run["round"]: run for run in runs_by_label[numerator_label]}
        denominator_by_round = {run["round"]: run for run in runs_by_label[denominator_label]}
        paired = [
            (numerator_by_round[round_number], denominator_by_round[round_number])
            for round_number in sorted(numerator_by_round.keys() & denominator_by_round.keys())
        ]
        metrics = {}
        for metric in (*REQUIRED_COUNTERS, "cpu_seconds", "wall_seconds"):
            values = []
            for numerator, denominator in paired:
                numerator_value = numerator.get("counters", {}).get(metric, numerator.get(metric))
                denominator_value = denominator.get("counters", {}).get(metric, denominator.get(metric))
                if numerator_value is not None and denominator_value:
                    values.append(numerator_value / denominator_value)
            if values:
                metrics[metric] = {
                    "n": len(values),
                    "median": median(values),
                    "min": min(values),
                    "max": max(values),
                    "all": values,
                    "wins": sum(value < 1.0 for value in values),
                }
        output[f"{numerator_label}/{denominator_label}"] = metrics
    return output


def summarize_runs(runs_by_label):
    """Summarize counter, CPU, and wall values by command label."""
    result = {}
    for label, runs in runs_by_label.items():
        series = {}
        for run in runs:
            for name, value in run.get("counters", {}).items():
                series.setdefault(name, []).append(value)
            for name in ("cpu_seconds", "wall_seconds"):
                if name in run:
                    series.setdefault(name, []).append(run[name])
        result[label] = {
            name: {"n": len(values), "median": median(values), "min": min(values),
                   "max": max(values), "all": values}
            for name, values in sorted(series.items())
        }
    return result


def parse_time_l(text):
    """Parse the target's BSD ``time -l`` real/user/sys line.

    The target is wrapped in ``/usr/bin/time`` inside the trace so these are
    target-and-child CPU seconds, not the CPU time Instruments uses to collect
    its own samples.
    """
    values = {}
    for name, value in re.findall(r"(?m)([0-9]+(?:\.[0-9]+)?)\s+(real|user|sys)\b", text):
        values[value] = float(name)
    missing = {"real", "user", "sys"} - values.keys()
    if missing:
        raise ValueError(f"BSD time output is missing {', '.join(sorted(missing))}")
    return {
        "cpu_seconds": values["user"] + values["sys"],
        "wall_seconds": values["real"],
    }


def _run(command, *, capture_output=True):
    return subprocess.run(command, stdout=subprocess.PIPE if capture_output else None,
                          stderr=subprocess.PIPE, check=False)


def _missing_process_schema_error(schemas):
    """Explain why an Instruments trace cannot support a process-total ratio."""
    if "counters-profile" in schemas:
        return (
            "CPU Counters exported sampled per-core counters-profile rows, not "
            f"{PROCESS_SCHEMA}. Their values reset when a target migrates between "
            "cores, so summing them would not be a process counter total. Use a "
            "template/export that exposes a named process aggregate; do not use this "
            "sampled trace for Vaco:ffmpeg ratios."
        )
    listed = ", ".join(sorted(schema for schema in schemas if schema)) or "none"
    return (
        f"CPU Counters template did not expose {PROCESS_SCHEMA}; found: {listed}. "
        "Use a saved counting-mode CPU Counters template."
    )


def _find_process_schema(xctrace, trace_path):
    toc = _run([xctrace, "export", "--input", str(trace_path), "--toc"])
    if toc.returncode != 0:
        raise RuntimeError(toc.stderr.decode("utf-8", "replace")[-2000:])
    try:
        root = ET.fromstring(toc.stdout)
    except ET.ParseError as exc:
        raise RuntimeError(f"xctrace --toc returned invalid XML: {exc}") from exc
    schemas = {
        element.attrib.get("schema", "")
        for element in root.iter()
        if "schema" in element.attrib
    }
    if PROCESS_SCHEMA not in schemas:
        raise RuntimeError(_missing_process_schema_error(schemas))


def _export_process_table(xctrace, trace_path):
    _find_process_schema(xctrace, trace_path)
    xpath = f'/trace-toc/run/data/table[@schema="{PROCESS_SCHEMA}"]'
    exported = _run([xctrace, "export", "--input", str(trace_path), "--xpath", xpath])
    if exported.returncode != 0:
        raise RuntimeError(exported.stderr.decode("utf-8", "replace")[-2000:])
    return exported.stdout.decode("utf-8", "replace")


def run_one(argv, label, xctrace, template, workdir):
    """Run one process under xctrace and collect its PMU, CPU, and wall metrics."""
    trace_path = Path(workdir) / f"{label}.trace"
    time_path = Path(workdir) / f"{label}.time"
    if time_path.exists():
        time_path.unlink()
    command = [
        xctrace, "record", "--template", str(template), "--output", str(trace_path),
        "--target-stdout", os.devnull, "--launch", "--", "/usr/bin/time", "-l", "-o",
        str(time_path),
    ] + list(argv)
    completed = _run(command)
    if completed.returncode != 0:
        message = completed.stderr.decode("utf-8", "replace")[-2000:]
        raise RuntimeError(f"xctrace exited {completed.returncode} for {argv}: {message}")
    try:
        parsed = parse_process_counters(_export_process_table(xctrace, trace_path), Path(argv[0]).name)
    finally:
        shutil.rmtree(trace_path, ignore_errors=True)
    if not time_path.exists():
        raise RuntimeError(f"BSD time wrote no output for {argv}")
    parsed.update(parse_time_l(time_path.read_text(encoding="utf-8", errors="replace")))
    return parsed


def _profile(argv, label, samply, output_dir):
    output = Path(output_dir) / f"{label}.json.gz"
    completed = _run([samply, "record", "--rate", "4000", "--save-only", "-o", str(output), "--"] + list(argv))
    if completed.returncode != 0:
        raise RuntimeError(completed.stderr.decode("utf-8", "replace")[-2000:])
    return str(output)


def _print_table(results):
    print("\n| workload | ratio | cycles | instructions | CPU s | wall s |", file=sys.stderr)
    print("|---|---|---:|---:|---:|---:|", file=sys.stderr)
    for name, job in results["jobs"].items():
        for ratio, metrics in job["ratios"].items():
            rendered = lambda metric: "—" if metric not in metrics else f"{metrics[metric]['median']:.3f}x"
            print(f"| {name} | {ratio} | {rendered('cycles')} | {rendered('instructions')} | "
                  f"{rendered('cpu_seconds')} | {rendered('wall_seconds')} |", file=sys.stderr)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("spec", help="workload spec emitted by perf-baseline-gen-spec.py")
    parser.add_argument("--out", required=True, help="write results JSON here")
    parser.add_argument("--template", required=True, help="saved CPU Counters counting-mode .tracetemplate")
    parser.add_argument("--jobs", help="regex selecting workload names")
    parser.add_argument("--cmds", help="regex selecting command labels")
    parser.add_argument("--rounds", type=int, default=10, help="measured alternating rounds (minimum 10)")
    parser.add_argument("--warmups", type=int, default=1, help="unmeasured warm-up rounds")
    parser.add_argument("--xctrace", default="/usr/bin/xctrace")
    parser.add_argument("--samply-dir", help="save diagnostic Samply profiles here after measurement")
    parser.add_argument("--samply", default="samply")
    args = parser.parse_args()

    try:
        validate_rounds(args.rounds)
    except ValueError as exc:
        parser.error(str(exc))
    if args.warmups < 0:
        parser.error("--warmups cannot be negative")
    if platform.system() != "Darwin":
        parser.error("xctrace CPU counters require macOS")
    xctrace = shutil.which(args.xctrace)
    if xctrace is None:
        parser.error(f"xctrace was not found at {args.xctrace}")
    template = Path(args.template)
    if not template.exists():
        parser.error(f"CPU Counters template does not exist: {template}")
    if args.samply_dir and shutil.which(args.samply) is None:
        parser.error(f"samply was not found: {args.samply}")

    spec = json.loads(Path(args.spec).read_text(encoding="utf-8"))
    job_pattern = re.compile(args.jobs) if args.jobs else None
    command_pattern = re.compile(args.cmds) if args.cmds else None
    results = {
        "backend": "xctrace-cpu-counters-process-aggregate",
        "counter_contract": "named cycles and retired instructions only",
        "rounds": args.rounds,
        "warmups": args.warmups,
        "template": str(template),
        "host": {"platform": platform.platform(), "machine": platform.machine()},
        "jobs": {},
    }
    failures = 0
    with tempfile.TemporaryDirectory(prefix="vaco-xctrace-") as workdir:
        for job in spec:
            if job_pattern and not job_pattern.search(job["name"]):
                continue
            commands = {label: argv for label, argv in job["cmds"].items()
                        if not command_pattern or command_pattern.search(label)}
            if len(commands) < 2:
                continue
            labels = list(commands)
            runs = {label: [] for label in labels}
            print(f"== {job['name']}", file=sys.stderr)
            for round_number in range(args.warmups + args.rounds):
                order = rotating_order(labels, round_number)
                measured = round_number >= args.warmups
                for label in order:
                    try:
                        record = run_one(commands[label], label, xctrace, template, workdir)
                    except RuntimeError as exc:
                        failures += 1
                        print(f"  FAILED {label}: {exc}", file=sys.stderr)
                        continue
                    if measured:
                        record["round"] = round_number - args.warmups
                        runs[label].append(record)
                        print(f"  round {record['round'] + 1} {label}: cycles="
                              f"{record['counters']['cycles']:,} instructions="
                              f"{record['counters']['instructions']:,}", file=sys.stderr)
            results["jobs"][job["name"]] = {
                "commands": commands,
                "runs": runs,
                "summary": summarize_runs(runs),
                "ratios": paired_ratios(runs),
            }
            Path(args.out).write_text(json.dumps(results, indent=2), encoding="utf-8")

    if args.samply_dir:
        Path(args.samply_dir).mkdir(parents=True, exist_ok=True)
        profiles = {}
        for job in results["jobs"].values():
            for label, argv in job["commands"].items():
                profiles[label] = _profile(argv, label, args.samply, args.samply_dir)
        results["samply_profiles"] = profiles
    Path(args.out).write_text(json.dumps(results, indent=2), encoding="utf-8")
    _print_table(results)
    if failures:
        print(f"{failures} measurement(s) failed and were excluded", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
