#!/usr/bin/env python3
"""Run the Vaco PGO workload and validate the resulting profile.

The module intentionally uses only Python's standard library.  The macOS
system Python used for local gates is currently 3.9, so a small TOML reader is
kept as a fallback until the repository's minimum Python version grows a
``tomllib`` dependency.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import re
import shlex
import subprocess
import sys
import time
from pathlib import Path
from typing import Dict, List, Optional


REQUIRED_GROUPS = {
    "decode",
    "demux",
    "transcode",
    "scale",
    "resample",
    "encode",
    "seek",
    "cli",
}
WORKLOAD_KEYS = {"name", "group", "component", "binary", "args", "weight", "timeout"}
_ENV_RE = re.compile(r"\$(?:\{[^}]+\}|[A-Za-z_][A-Za-z0-9_]*)")


class ManifestError(ValueError):
    """The workload manifest is malformed or violates a profile policy."""


def _fallback_toml(text: str) -> dict:
    """Read the deliberately small profile manifest shape on Python 3.9."""
    out: dict = {}
    current = None
    for lineno, raw in enumerate(text.splitlines(), 1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        if line == "[[workload]]":
            current = {}
            out.setdefault("workload", []).append(current)
            continue
        if "=" not in line:
            raise ManifestError(f"line {lineno}: expected key = value")
        key, value = [part.strip() for part in line.split("=", 1)]
        if current is not None and key in WORKLOAD_KEYS:
            target = current
        elif current is not None and key not in WORKLOAD_KEYS:
            raise ManifestError(f"line {lineno}: unknown workload key {key!r}")
        else:
            target = out
        target[key] = _fallback_value(value, lineno)
    return out


def _fallback_value(value: str, lineno: int):
    if value in {"true", "false"}:
        return value == "true"
    if value.startswith("[") and value.endswith("]"):
        body = value[1:-1].strip()
        if not body:
            return []
        return [_fallback_value(part.strip(), lineno) for part in body.split(",")]
    if (value.startswith('"') and value.endswith('"')) or (
        value.startswith("'") and value.endswith("'")
    ):
        return value[1:-1]
    try:
        return float(value) if "." in value else int(value)
    except ValueError as exc:
        raise ManifestError(f"line {lineno}: unsupported TOML value {value!r}") from exc


def load_manifest_text(text: str) -> dict:
    """Parse and structurally validate a workload manifest."""
    try:
        import tomllib  # type: ignore[import-not-found]
    except ImportError:
        manifest = _fallback_toml(text)
    else:
        try:
            manifest = tomllib.loads(text)
        except (ValueError, TypeError) as exc:
            raise ManifestError(f"invalid TOML: {exc}") from exc

    if manifest.get("version") != 1:
        raise ManifestError("version must be 1")
    workloads = manifest.get("workload")
    if not isinstance(workloads, list) or not workloads:
        raise ManifestError("manifest must contain at least one [[workload]]")
    names = set()
    for index, workload in enumerate(workloads):
        if not isinstance(workload, dict):
            raise ManifestError(f"workload {index} is not a table")
        unknown = set(workload) - WORKLOAD_KEYS
        if unknown:
            raise ManifestError(f"workload {index}: unknown keys {sorted(unknown)}")
        missing = {"name", "group", "component", "binary", "args"} - set(workload)
        if missing:
            raise ManifestError(f"workload {index}: missing keys {sorted(missing)}")
        name = workload["name"]
        if not isinstance(name, str) or not name or name in names:
            raise ManifestError(f"workload {index}: name must be non-empty and unique")
        names.add(name)
        if not isinstance(workload["args"], list) or not all(
            isinstance(arg, str) for arg in workload["args"]
        ):
            raise ManifestError(f"workload {name}: args must be an array of strings")
        if not isinstance(workload["group"], str) or not workload["group"]:
            raise ManifestError(f"workload {name}: group must be non-empty")
        if not isinstance(workload["component"], str) or not workload["component"]:
            raise ManifestError(f"workload {name}: component must be non-empty")
        weight = workload.get("weight", 1)
        if not isinstance(weight, (int, float)) or weight <= 0:
            raise ManifestError(f"workload {name}: weight must be positive")
    share = manifest.get("max_component_share", 0.15)
    if not isinstance(share, (int, float)) or not 0 < share <= 1:
        raise ManifestError("max_component_share must be in (0, 1]")
    runtime = manifest.get("max_runtime_minutes", 25)
    if not isinstance(runtime, (int, float)) or runtime <= 0:
        raise ManifestError("max_runtime_minutes must be positive")
    return manifest


def validate_manifest(manifest: dict, require_groups: bool = False) -> list[str]:
    """Return policy diagnostics; an empty list means the manifest is valid."""
    groups = {entry["group"] for entry in manifest["workload"]}
    missing = sorted(REQUIRED_GROUPS - groups) if require_groups else []
    if missing:
        return [f"missing representative workload groups: {', '.join(missing)}"]
    return []


def resolve_command(workload: dict, environment: Dict[str, str], binary_dir: Path) -> List[str]:
    """Resolve one manifest row to an argv without invoking a shell."""
    binary = workload["binary"]
    if not isinstance(binary, str) or not binary:
        raise ManifestError(f"workload {workload['name']}: binary must be non-empty")
    env = os.environ.copy()
    env.update(environment)
    resolved_binary = Path(binary)
    if not resolved_binary.is_absolute():
        resolved_binary = binary_dir / resolved_binary
    argv = [str(resolved_binary)]
    for arg in workload["args"]:
        value = _expand_vars(str(arg), env)
        if _ENV_RE.search(value):
            raise ManifestError(f"workload {workload['name']}: unresolved variable in {arg!r}")
        argv.append(value)
    # Keep expansion deterministic for callers that pass a custom environment.
    del env
    return argv


def _expand_vars(value: str, environment: Dict[str, str]) -> str:
    """Expand variables using the runner environment, not just the parent shell."""
    def replace(match: re.Match) -> str:
        token = match.group(0)
        name = token[2:-1] if token.startswith("${") else token[1:]
        return environment.get(name, token)

    return _ENV_RE.sub(replace, value)


def run_manifest(
    manifest_path: Path,
    binary_dir: Path,
    environment: dict[str, str],
    selected_group: Optional[str] = None,
    dry_run: bool = False,
) -> int:
    """Run selected workloads, enforcing the manifest's total runtime budget."""
    manifest = load_manifest_text(manifest_path.read_text(encoding="utf-8"))
    diagnostics = validate_manifest(manifest, require_groups=True)
    if diagnostics:
        raise ManifestError("; ".join(diagnostics))
    fixture = environment.get("VACO_PROFILE_FIXTURES")
    if fixture:
        os.environ["VACO_PROFILE_FIXTURES"] = fixture
    deadline = time.monotonic() + float(manifest.get("max_runtime_minutes", 25)) * 60
    for workload in manifest["workload"]:
        if selected_group and workload["group"] != selected_group:
            continue
        argv = resolve_command(workload, environment, binary_dir)
        if dry_run:
            print(f"{workload['name']}: {shlex.join(argv)}")
            continue
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise ManifestError("workload exceeded max_runtime_minutes")
        timeout = min(float(workload.get("timeout", remaining)), remaining)
        print(f"== {workload['name']} ({workload['group']})", file=sys.stderr)
        try:
            completed = subprocess.run(
                argv,
                check=False,
                timeout=timeout,
                stdout=subprocess.DEVNULL,
            )
        except subprocess.TimeoutExpired as exc:
            raise ManifestError(f"workload {workload['name']} timed out after {timeout:.1f}s") from exc
        if completed.returncode:
            raise ManifestError(f"workload {workload['name']} exited {completed.returncode}")
    return 0


def _component_from_symbol(symbol: str, known_components: Optional[set[str]] = None) -> str | None:
    if known_components:
        normalized = symbol.lower().replace("-", "_")
        matches = [
            (normalized.find(component.replace("-", "_")), component)
            for component in known_components
            if component.replace("-", "_") in normalized
        ]
        if matches:
            return min(matches)[1]
    match = re.search(r"(vaco(?:[_-][a-z0-9]+)+)", symbol.lower())
    return match.group(1).replace("_", "-") if match else None


def parse_profdata_dump(
    text: str, known_components: Optional[set[str]] = None
) -> dict[str, int]:
    """Aggregate function counts by Vaco crate from ``llvm-profdata show``."""
    counts: dict[str, int] = {}
    current: str | None = None
    saw_function_count = False
    fallback_blocks = 0

    def flush_current() -> None:
        nonlocal current, saw_function_count, fallback_blocks
        if current and not saw_function_count:
            counts[current] = counts.get(current, 0) + fallback_blocks
        current = None
        saw_function_count = False
        fallback_blocks = 0

    for raw in text.splitlines():
        line = raw.strip()
        if not line or line in {"Counters:", "Functions:"}:
            continue
        # `llvm-profdata show --all-functions --counts` puts the execution
        # count on a `Function count:` line inside the preceding symbol's
        # block.  Older LLVM versions and hand-written fixtures may instead
        # put a bare count on the following line, so retain that form too.
        function = re.match(r"(.+?)(?:\s*\((\d+)\s+counts?\))?:$", line)
        if function:
            flush_current()
            candidate = _component_from_symbol(function.group(1), known_components)
            current = candidate
            inline = function.group(2)
            if candidate and inline is not None:
                counts[candidate] = counts.get(candidate, 0) + int(inline)
                saw_function_count = True
            continue
        execution = re.match(r"Function count:\s*(\d+)$", line)
        if current and execution:
            counts[current] = counts.get(current, 0) + int(execution.group(1))
            saw_function_count = True
            continue
        blocks = re.match(r"Block counts:\s*\[([^]]*)\]$", line)
        if current and blocks:
            fallback_blocks += sum(
                int(value) for value in blocks.group(1).split(",") if value.strip()
            )
            continue
        if current and line.isdigit():
            counts[current] = counts.get(current, 0) + int(line)
            current = None
            saw_function_count = False
            fallback_blocks = 0
    flush_current()
    return counts


def check_profile(
    manifest: dict,
    profile_counts: Dict[str, int],
    required_components: Optional[List[str]] = None,
) -> dict:
    """Check raw-profile coverage and manifest weighting.

    Counts are crate-level because LLVM's textual dump does not retain the
    registry descriptor that selected a function.  The manifest's explicit
    required list is therefore the stable mapping from the release workload to
    components; adding a component requires adding its crate to that list.

    LLVM's counters measure how much work a path performed, not how much it
    was intentionally represented in the training corpus. For example, an
    entropy decoder naturally has far more counter increments than argument
    parsing. The anti-overfit policy therefore applies to explicit manifest
    weights, while the profile itself proves every required component ran.
    """
    required = required_components
    if required is None:
        required = manifest.get("required_components", [])
    required = sorted(set(required))
    missing = [name for name in required if profile_counts.get(name, 0) <= 0]
    total_counts = sum(max(0, value) for value in profile_counts.values())
    limit = float(manifest.get("max_component_share", 0.15))
    weights: dict[str, float] = {}
    for workload in manifest["workload"]:
        component = workload["component"]
        weights[component] = weights.get(component, 0.0) + float(workload.get("weight", 1))
    total_weight = sum(weights.values())
    shares = {
        name: value / total_weight
        for name, value in sorted(weights.items())
        if total_weight > 0
    }
    overweight = sorted(name for name, share in shares.items() if share > limit)
    return {
        "required_components": required,
        "missing_components": missing,
        "overweight_components": overweight,
        "total_counts": total_counts,
        "total_weight": total_weight,
        "shares": shares,
        "ok": not missing and not overweight and total_counts > 0,
    }


def manifest_sha256(path: Path) -> str:
    """Return the content hash pinned alongside a published profile."""
    return hashlib.sha256(path.read_bytes()).hexdigest()


def check_lock(lock_path: Path, manifest_path: Path) -> None:
    """Ensure the lockfile's manifest hash matches the workload on disk."""
    text = lock_path.read_text(encoding="utf-8")
    match = re.search(r'^manifest_sha256\s*=\s*"([0-9a-f]{64})"\s*$', text, re.MULTILINE)
    if not match:
        raise ManifestError(f"{lock_path}: missing manifest_sha256")
    actual = manifest_sha256(manifest_path)
    if match.group(1) != actual:
        raise ManifestError(
            f"{lock_path}: manifest hash {match.group(1)} does not match {manifest_path} ({actual})"
        )


def _main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    run = sub.add_parser("run", help="run profile/workload.toml")
    run.add_argument("manifest", type=Path)
    run.add_argument("--binary-dir", type=Path, default=Path("target/release"))
    run.add_argument("--fixtures", type=Path, default=None)
    run.add_argument("--group", default=None)
    run.add_argument("--dry-run", action="store_true")
    validate = sub.add_parser("validate", help="validate manifest shape and representative groups")
    validate.add_argument("manifest", type=Path)
    check = sub.add_parser("check-profile", help="check llvm-profdata text dump")
    check.add_argument("manifest", type=Path)
    check.add_argument("dump", type=Path)
    lock = sub.add_parser("check-lock", help="check profile lockfile manifest hash")
    lock.add_argument("lockfile", type=Path)
    lock.add_argument("manifest", type=Path)
    args = parser.parse_args()
    try:
        if args.command == "run":
            env = {}
            if args.fixtures:
                env["VACO_PROFILE_FIXTURES"] = str(args.fixtures.resolve())
            return run_manifest(args.manifest, args.binary_dir, env, args.group, args.dry_run)
        if args.command == "check-lock":
            check_lock(args.lockfile, args.manifest)
            print(f"lock valid: {args.manifest}")
            return 0
        manifest = load_manifest_text(args.manifest.read_text(encoding="utf-8"))
        if args.command == "validate":
            diagnostics = validate_manifest(manifest, require_groups=True)
            if diagnostics:
                raise ManifestError("; ".join(diagnostics))
            print(f"manifest valid: {len(manifest['workload'])} workloads")
            return 0
        known = set(manifest.get("required_components", []))
        known.update(entry["component"] for entry in manifest["workload"])
        profile_counts = parse_profdata_dump(args.dump.read_text(encoding="utf-8"), known)
        report = check_profile(manifest, profile_counts)
        print(report)
        if not report["ok"]:
            return 1
        return 0
    except (OSError, ManifestError) as exc:
        print(f"pgo: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(_main())
