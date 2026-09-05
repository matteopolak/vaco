#!/usr/bin/env python3
"""Verify the installable ``vaco`` package without compiling or publishing it.

The package-list check is intentionally separate from the publication closure
audit.  The closure answers *which* packages must be released; this gate
answers whether the public root contains the files and targets promised to
users.  ``cargo package --list`` is read-only with respect to the registry and
uses Cargo's own include/exclude and package metadata resolution.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import sys
from typing import Any, Sequence


ROOT = Path(__file__).resolve().parents[1]
PACKAGE_NAME = "vaco"
EXPECTED_LICENSE = "GPL-3.0-or-later"
EXPECTED_REPOSITORY = "https://github.com/matteopolak/vaco"
EXPECTED_BINARIES = ("vvmpeg", "vvprobe")
REQUIRED_FILES = frozenset(
    {
        "Cargo.toml",
        "README.md",
        "src/lib.rs",
        "src/bin/vvmpeg.rs",
        "src/bin/vvprobe.rs",
    }
)
FORBIDDEN_FILES = frozenset({"src/bin/vaco.rs", "src/bin/vaco-probe.rs"})


def package_files(output: str) -> frozenset[str]:
    """Extract relative package paths from ``cargo package --list`` output."""
    paths: set[str] = set()
    for raw_line in output.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("Packaging ") or line.startswith(" Verifying "):
            continue
        # Cargo may prefix diagnostics with ``warning:``; they are not package
        # paths and must not accidentally satisfy a required-file check.
        if line.startswith(("warning:", "error:", "Updating ", "Locking ")):
            continue
        if " " in line and not line.startswith(("Cargo.toml", "README.md", "src/")):
            continue
        if line == "Cargo.toml" or line == "README.md" or line.startswith("src/"):
            paths.add(line)
    return frozenset(paths)


def target_names(metadata: dict[str, Any]) -> tuple[str, ...]:
    """Return binary target names from one ``cargo metadata`` package record."""
    packages = metadata.get("packages", [])
    package = next((item for item in packages if item.get("name") == PACKAGE_NAME), None)
    if package is None:
        raise ValueError(f"metadata does not contain package {PACKAGE_NAME!r}")
    return tuple(
        target["name"]
        for target in package.get("targets", [])
        if "bin" in target.get("kind", [])
    )


def verify(
    files: Sequence[str], binaries: Sequence[str], package_name: str = PACKAGE_NAME
) -> list[str]:
    """Return actionable violations for one package-list and target set."""
    observed = frozenset(files)
    failures: list[str] = []
    if package_name != PACKAGE_NAME:
        failures.append(f"expected package {PACKAGE_NAME!r}, got {package_name!r}")
    missing = sorted(REQUIRED_FILES - observed)
    if missing:
        failures.append(f"package is missing required files: {', '.join(missing)}")
    forbidden = sorted(FORBIDDEN_FILES & observed)
    if forbidden:
        failures.append(f"package contains legacy binary sources: {', '.join(forbidden)}")
    unexpected = sorted(observed - REQUIRED_FILES)
    if unexpected:
        failures.append(f"package contains unexpected files: {', '.join(unexpected)}")
    expected = tuple(EXPECTED_BINARIES)
    actual = tuple(binaries)
    if actual != expected:
        failures.append(
            f"binary targets are {', '.join(actual) or '(none)'}; "
            f"expected {', '.join(expected)}"
        )
    return failures


def verify_metadata(package: dict[str, Any]) -> list[str]:
    """Check the public package fields Cargo will expose on crates.io."""
    failures: list[str] = []
    if package.get("name") != PACKAGE_NAME:
        failures.append(f"expected package {PACKAGE_NAME!r}, got {package.get('name')!r}")
    if package.get("license") != EXPECTED_LICENSE:
        failures.append(
            f"license is {package.get('license')!r}; expected {EXPECTED_LICENSE!r}"
        )
    if package.get("readme") in (None, ""):
        failures.append("package has no README configured")
    if package.get("repository") != EXPECTED_REPOSITORY:
        failures.append(
            f"repository is {package.get('repository')!r}; expected {EXPECTED_REPOSITORY!r}"
        )
    if package.get("publish") not in (None,):
        failures.append("package is marked publish = false")
    return failures


def run_cargo(manifest: Path) -> tuple[str, dict[str, Any]]:
    """Run metadata and package-list probes, never ``cargo publish``."""
    command = [
        "cargo",
        "package",
        "--list",
        "--locked",
        "--allow-dirty",
        "--no-verify",
        "--manifest-path",
        str(manifest),
    ]
    listed = subprocess.run(command, cwd=ROOT, check=False, capture_output=True, text=True)
    if listed.returncode != 0:
        sys.stderr.write(listed.stderr)
        raise RuntimeError(f"{' '.join(command)} failed with exit code {listed.returncode}")
    metadata_command = [
        "cargo",
        "metadata",
        "--format-version",
        "1",
        "--no-deps",
        "--locked",
        "--manifest-path",
        str(manifest),
    ]
    metadata_result = subprocess.run(
        metadata_command, cwd=ROOT, check=False, capture_output=True, text=True
    )
    if metadata_result.returncode != 0:
        sys.stderr.write(metadata_result.stderr)
        raise RuntimeError(
            f"{' '.join(metadata_command)} failed with exit code {metadata_result.returncode}"
        )
    return listed.stdout, json.loads(metadata_result.stdout)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--manifest-path",
        type=Path,
        default=ROOT / "crates/app/vaco/Cargo.toml",
        help="facade manifest to inspect (default: crates/app/vaco/Cargo.toml)",
    )
    args = parser.parse_args()
    try:
        output, metadata = run_cargo(args.manifest_path.resolve())
        package = next(
            item for item in metadata["packages"] if item.get("name") == PACKAGE_NAME
        )
        failures = verify_metadata(package)
        failures.extend(verify(package_files(output), target_names(metadata), package["name"]))
    except (KeyError, StopIteration, RuntimeError, ValueError, json.JSONDecodeError) as error:
        print(f"crates.io package verification failed: {error}", file=sys.stderr)
        return 2
    if failures:
        print("crates.io package verification failed:", file=sys.stderr)
        for failure in failures:
            print(f"- {failure}", file=sys.stderr)
        return 1
    print(
        f"{PACKAGE_NAME} package verification passed: "
        f"{len(package_files(output))} listed files; "
        f"binaries={','.join(EXPECTED_BINARIES)}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
