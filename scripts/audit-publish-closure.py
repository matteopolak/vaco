#!/usr/bin/env python3
"""Audit the crates.io closure for the installable Vaco package.

Cargo resolves local `path` dependencies before a package is published, while
crates.io resolves their version requirements. This gate derives the actual
normal/build graph and rejects the gap between those two worlds: a reachable
unpublished member, a non-exact internal path version, or a publishable
workspace member outside the selected roots.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]


def cargo_metadata() -> dict[str, object]:
    """Read Cargo's locked graph without compiling or requiring sccache."""
    env = os.environ.copy()
    env["RUSTC_WRAPPER"] = ""
    completed = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked"],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
        env=env,
    )
    if completed.returncode != 0:
        sys.stderr.write(completed.stderr)
        raise SystemExit("cargo metadata failed")
    return json.loads(completed.stdout)


def package_publishable(package: dict[str, object]) -> bool:
    """Return Cargo's effective publish flag from its normalized metadata."""
    return package["publish"] is None


def normal_or_build(dep: dict[str, object]) -> bool:
    """Keep only edges shipped by the package, never dev-only test support."""
    kinds = dep.get("dep_kinds", [])
    return any(kind.get("kind") in (None, "build") for kind in kinds)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root",
        action="append",
        required=True,
        help="package name included in the published distribution; repeatable",
    )
    parser.add_argument(
        "--metadata",
        type=Path,
        help="read a saved cargo metadata JSON document instead of invoking Cargo",
    )
    args = parser.parse_args()

    if args.metadata is None:
        metadata = cargo_metadata()
    else:
        with args.metadata.open(encoding="utf-8") as file:
            metadata = json.load(file)

    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    roots = [package for package in packages.values() if package["name"] in args.root]
    found = {package["name"] for package in roots}
    missing_roots = sorted(set(args.root) - found)
    if missing_roots:
        print(f"missing selected package roots: {', '.join(missing_roots)}", file=sys.stderr)
        return 2

    closure: set[str] = set()
    pending = [package["id"] for package in roots]
    while pending:
        package_id = pending.pop()
        if package_id in closure:
            continue
        closure.add(package_id)
        pending.extend(
            dependency["pkg"]
            for dependency in nodes[package_id].get("deps", [])
            if normal_or_build(dependency)
        )

    workspace = set(metadata["workspace_members"])
    internal = {
        package_id
        for package_id in closure
        if package_id in workspace and Path(packages[package_id]["manifest_path"]).is_relative_to(ROOT)
    }
    failures: list[str] = []
    for package_id in sorted(internal, key=lambda item: packages[item]["name"]):
        package = packages[package_id]
        if not package_publishable(package):
            failures.append(f"reachable package is publish = false: {package['name']}")

        for dependency in package["dependencies"]:
            if dependency.get("kind") not in (None, "build") or dependency.get("path") is None:
                continue
            target = next(
                (
                    candidate
                    for candidate in packages.values()
                    if Path(candidate["manifest_path"]).parent == Path(dependency["path"])
                ),
                None,
            )
            if target is None or target["id"] not in internal:
                failures.append(
                    f"{package['name']} has a shipped path dependency outside the closure: "
                    f"{dependency['name']}"
                )
                continue
            expected = f"={target['version']}"
            if dependency["req"] != expected:
                failures.append(
                    f"{package['name']} -> {target['name']} requires {dependency['req']!r}, "
                    f"expected {expected!r}"
                )

    for package_id in sorted(workspace - internal, key=lambda item: packages[item]["name"]):
        package = packages[package_id]
        if package_publishable(package):
            failures.append(f"out-of-closure package is publishable: {package['name']}")

    external = len(closure - internal)
    print(
        f"publish closure: {len(internal)} internal, {external} external, "
        f"{len(closure)} total normal/build packages"
    )
    if failures:
        print("publish-closure audit failed:", file=sys.stderr)
        for failure in failures:
            print(f"- {failure}", file=sys.stderr)
        return 1
    print("publish-closure audit passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
