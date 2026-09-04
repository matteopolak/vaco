#!/usr/bin/env python3
"""Plan or explicitly apply the crates.io publication manifest migration.

The plan comes from Cargo's resolved normal/build graph. It therefore has one
source of truth for three related changes: exact versions on internal path
dependencies, publication of reachable members, and `publish = false` for every
other workspace member. The default is read-only JSON; `--apply` is explicit
because this tool will eventually touch many manifests.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from typing import Any


REPOSITORY = Path(__file__).resolve().parents[1]


def cargo_metadata(workspace_root: Path) -> dict[str, Any]:
    """Read the locked dependency graph without compiling or requiring sccache."""
    env = os.environ.copy()
    env["RUSTC_WRAPPER"] = ""
    completed = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked"],
        cwd=workspace_root,
        check=False,
        capture_output=True,
        text=True,
        env=env,
    )
    if completed.returncode != 0:
        sys.stderr.write(completed.stderr)
        raise SystemExit("cargo metadata failed")
    return json.loads(completed.stdout)


def normal_or_build(dependency: dict[str, Any]) -> bool:
    """Select package edges that ship, excluding all dev-only dependencies."""
    return any(
        dependency_kind.get("kind") in (None, "build")
        for dependency_kind in dependency.get("dep_kinds", [])
    )


def closure_for_roots(metadata: dict[str, Any], root_names: list[str]) -> tuple[
    dict[str, dict[str, Any]], set[str]
]:
    """Return packages and the resolved normal/build closure of selected roots."""
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    root_ids = [
        package["id"]
        for package in packages.values()
        if package["name"] in root_names
    ]
    found = {packages[package_id]["name"] for package_id in root_ids}
    missing = sorted(set(root_names) - found)
    if missing:
        raise ValueError(f"missing selected package roots: {', '.join(missing)}")

    closure: set[str] = set()
    pending = list(root_ids)
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
    return packages, closure


def relative_manifest(package: dict[str, Any], workspace_root: Path) -> str:
    """Return a workspace-relative manifest path suitable for a reviewable plan."""
    manifest = Path(package["manifest_path"]).resolve()
    try:
        return manifest.relative_to(workspace_root).as_posix()
    except ValueError as error:
        raise ValueError(f"package manifest is outside workspace root: {manifest}") from error


def path_target(
    packages: dict[str, dict[str, Any]], dependency: dict[str, Any]
) -> dict[str, Any] | None:
    """Find the metadata package selected by one local Cargo path dependency."""
    path = dependency.get("path")
    if path is None:
        return None
    dependency_path = Path(path).resolve()
    return next(
        (
            candidate
            for candidate in packages.values()
            if Path(candidate["manifest_path"]).parent.resolve() == dependency_path
        ),
        None,
    )


def build_plan(
    metadata: dict[str, Any], root_names: list[str], workspace_root: Path
) -> dict[str, Any]:
    """Create stable operations from the resolved graph without reading manifests."""
    packages, closure = closure_for_roots(metadata, root_names)
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    workspace = set(metadata["workspace_members"])
    internal = {
        package_id
        for package_id in closure & workspace
        if Path(packages[package_id]["manifest_path"]).resolve().is_relative_to(workspace_root)
    }
    operations: list[dict[str, Any]] = []

    for package_id in internal:
        package = packages[package_id]
        manifest = relative_manifest(package, workspace_root)
        active_targets = {
            dependency["pkg"]
            for dependency in nodes[package_id].get("deps", [])
            if normal_or_build(dependency)
        }
        for dependency in package["dependencies"]:
            if dependency.get("kind") not in (None, "build") or dependency.get("path") is None:
                continue
            target = path_target(packages, dependency)
            if target is None or target["id"] not in active_targets:
                continue
            if target["id"] not in internal:
                raise ValueError(
                    f"{package['name']} has a shipped path dependency outside the closure: "
                    f"{dependency['name']}"
                )
            expected_version = f"={target['version']}"
            if dependency["req"] != expected_version:
                operations.append(
                    {
                        "kind": "set-path-version",
                        "manifest": manifest,
                        "package": package["name"],
                        "dependency": dependency["name"],
                        "expected_version": expected_version,
                    }
                )
        if package["publish"] is not None:
            operations.append(
                {
                    "kind": "set-publish",
                    "manifest": manifest,
                    "package": package["name"],
                    "publish": True,
                }
            )

    for package_id in workspace - internal:
        package = packages[package_id]
        if package["publish"] is None:
            operations.append(
                {
                    "kind": "set-publish",
                    "manifest": relative_manifest(package, workspace_root),
                    "package": package["name"],
                    "publish": False,
                }
            )

    operations.sort(
        key=lambda operation: (
            operation["manifest"],
            operation["kind"],
            operation.get("dependency", ""),
        )
    )
    internal_names = sorted(packages[package_id]["name"] for package_id in internal)
    return {
        "schema": 1,
        "roots": sorted(set(root_names)),
        "closure": {
            "internal_packages": internal_names,
            "internal_count": len(internal),
            "external_count": len(closure - internal),
            "total_count": len(closure),
        },
        "operations": operations,
    }


def replace_path_version(contents: str, operation: dict[str, Any]) -> str:
    """Set one inline dependency's version while preserving its other fields."""
    dependency = re.escape(operation["dependency"])
    pattern = re.compile(
        rf"^(?P<head>\s*{dependency}\s*=\s*\{{)(?P<body>[^\n}}]*)(?P<tail>\}}\s*)$",
        re.MULTILINE,
    )

    def replacement(match: re.Match[str]) -> str:
        body = match.group("body")
        version = f'version = "{operation["expected_version"]}"'
        if re.search(r"\bversion\s*=\s*\"[^\"]*\"", body):
            body = re.sub(r"\bversion\s*=\s*\"[^\"]*\"", version, body, count=1)
        else:
            separator = "" if not body.strip() else ", "
            body = f"{body.rstrip()}{separator}{version}"
        return f"{match.group('head')}{body}{match.group('tail')}"

    rewritten, replacements = pattern.subn(replacement, contents, count=1)
    if replacements != 1:
        raise ValueError(
            f"could not find one inline dependency entry for {operation['dependency']} "
            f"in {operation['manifest']}"
        )
    return rewritten


def replace_publish(contents: str, operation: dict[str, Any]) -> str:
    """Set the package publication flag in exactly its `[package]` table."""
    package_match = re.search(
        r"(?ms)^(?P<header>\[package\][^\n]*\n)(?P<body>.*?)(?=^\[[^\n]+\]|\Z)",
        contents,
    )
    if package_match is None:
        raise ValueError(f"missing [package] table in {operation['manifest']}")
    value = str(operation["publish"]).lower()
    body = package_match.group("body")
    publication_pattern = re.compile(r"^\s*publish\s*=\s*[^\n]*$", re.MULTILINE)
    if publication_pattern.search(body):
        body = publication_pattern.sub(f"publish = {value}", body, count=1)
    else:
        body = f"publish = {value}\n{body}"
    return f"{contents[:package_match.start()]}{package_match.group('header')}{body}{contents[package_match.end():]}"


def apply_plan(plan: dict[str, Any], workspace_root: Path) -> None:
    """Apply only reviewed operations, stopping on an unrecognized manifest form."""
    for operation in plan["operations"]:
        manifest = workspace_root / operation["manifest"]
        contents = manifest.read_text(encoding="utf-8")
        if operation["kind"] == "set-path-version":
            rewritten = replace_path_version(contents, operation)
        elif operation["kind"] == "set-publish":
            rewritten = replace_publish(contents, operation)
        else:
            raise ValueError(f"unsupported operation kind: {operation['kind']}")
        manifest.write_text(rewritten, encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", action="append", required=True, help="published root package")
    parser.add_argument("--metadata", type=Path, help="saved cargo metadata JSON")
    parser.add_argument(
        "--workspace-root",
        type=Path,
        default=REPOSITORY,
        help="workspace containing manifests to plan or apply",
    )
    parser.add_argument(
        "--apply",
        action="store_true",
        help="apply the deterministic plan; omitted by default for read-only output",
    )
    args = parser.parse_args()
    workspace_root = args.workspace_root.resolve()
    if args.metadata is None:
        metadata = cargo_metadata(workspace_root)
    else:
        with args.metadata.open(encoding="utf-8") as file:
            metadata = json.load(file)
    try:
        plan = build_plan(metadata, args.root, workspace_root)
        if args.apply:
            apply_plan(plan, workspace_root)
    except ValueError as error:
        print(f"publish migration plan failed: {error}", file=sys.stderr)
        return 2
    print(json.dumps(plan, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
