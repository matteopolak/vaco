#!/usr/bin/env python3
"""Refuse a crates.io release when any exact closure name is unavailable.

The check consumes `plan-publish-migration.py` JSON so the release preflight
does not keep a second list of internal crates. A missing crate name is
available; an existing one is permitted only when its public crates.io owner
list contains the explicitly configured expected owner. Network and registry
errors fail closed before Cargo receives any publish request.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass, field
import json
from pathlib import Path
import time
from typing import Any, Callable
from urllib.error import HTTPError, URLError
from urllib.parse import quote
from urllib.request import Request, urlopen


Fetch = Callable[[str], tuple[int, dict[str, Any]]]


@dataclass
class NameReport:
    """Classify exact package names before the separate publish gate proceeds."""

    available: list[str] = field(default_factory=list)
    owned: list[str] = field(default_factory=list)
    conflicts: dict[str, list[str]] = field(default_factory=dict)
    errors: dict[str, str] = field(default_factory=dict)
    unchecked: list[str] = field(default_factory=list)


def owner_names(response: dict[str, Any]) -> list[str]:
    """Read user and team names from crates.io's public owners response."""
    names: list[str] = []
    for owner in response.get("users", []) + response.get("teams", []):
        name = owner.get("login") or owner.get("name")
        if isinstance(name, str):
            names.append(name)
    return sorted(set(names))


def check_names(names: list[str], expected_owner: str, fetch: Fetch) -> NameReport:
    """Classify each exact package name using crate and owner endpoints."""
    report = NameReport()
    pending = sorted(set(names))
    for index, name in enumerate(pending):
        try:
            status, _ = fetch(f"/crates/{quote(name, safe='')}")
        except RuntimeError as error:
            report.errors[name] = str(error)
            report.unchecked = pending[index + 1 :]
            break
            continue
        if status == 404:
            report.available.append(name)
            continue
        if status != 200:
            report.errors[name] = f"crate lookup returned HTTP {status}"
            continue
        try:
            owner_status, owners = fetch(f"/crates/{quote(name, safe='')}/owners")
        except RuntimeError as error:
            report.errors[name] = str(error)
            report.unchecked = pending[index + 1 :]
            break
            continue
        if owner_status != 200:
            report.errors[name] = f"owner lookup returned HTTP {owner_status}"
            continue
        names_for_crate = owner_names(owners)
        if expected_owner in names_for_crate:
            report.owned.append(name)
        else:
            report.conflicts[name] = names_for_crate
    return report


def crates_io_fetcher(base_url: str, delay_seconds: float) -> Fetch:
    """Create a bounded crates.io JSON reader that turns transport failures into errors."""
    normalized_base = base_url.rstrip("/")

    def fetch(path: str) -> tuple[int, dict[str, Any]]:
        if delay_seconds:
            time.sleep(delay_seconds)
        request = Request(
            f"{normalized_base}{path}",
            headers={"Accept": "application/json", "User-Agent": "vaco-release-preflight"},
        )
        try:
            with urlopen(request, timeout=20) as response:  # noqa: S310 - fixed HTTPS default.
                return response.status, json.load(response)
        except HTTPError as error:
            if error.code == 429:
                retry_after = error.headers.get("Retry-After", "an unspecified interval")
                raise RuntimeError(
                    f"crates.io throttled the preflight (HTTP 429; retry after {retry_after})"
                ) from error
            if error.code == 404:
                return 404, {}
            return error.code, {}
        except URLError as error:
            raise RuntimeError(f"registry request failed: {error.reason}") from error

    return fetch


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plan", required=True, type=Path, help="migration plan JSON")
    parser.add_argument(
        "--include-name",
        action="append",
        default=[],
        help="additional planned package name not yet represented in Cargo metadata",
    )
    parser.add_argument("--offset", type=int, default=0, help="zero-based name offset for paced batches")
    parser.add_argument("--limit", type=int, help="maximum names to check in this batch")
    parser.add_argument(
        "--expected-owner",
        required=True,
        help="crates.io user or team that may already own a closure package",
    )
    parser.add_argument(
        "--delay-seconds",
        type=float,
        default=0.25,
        help="minimum delay before each crates.io request; default: 0.25",
    )
    parser.add_argument(
        "--evidence-out",
        type=Path,
        help="write machine-readable report JSON before returning",
    )
    parser.add_argument(
        "--base-url",
        default="https://crates.io/api/v1",
        help="crates.io API base URL; test-only local endpoints are supported",
    )
    args = parser.parse_args()
    with args.plan.open(encoding="utf-8") as file:
        plan = json.load(file)
    names = plan.get("closure", {}).get("internal_packages")
    if not isinstance(names, list) or not all(isinstance(name, str) for name in names):
        parser.error("plan must contain closure.internal_packages as a string list")

    if args.delay_seconds < 0:
        parser.error("--delay-seconds must not be negative")
    names = sorted(set(names) | set(args.include_name))
    if args.offset < 0 or args.limit is not None and args.limit < 1:
        parser.error("--offset must be non-negative and --limit must be positive")
    names = names[args.offset : None if args.limit is None else args.offset + args.limit]
    report = check_names(names, args.expected_owner, crates_io_fetcher(args.base_url, args.delay_seconds))
    if args.evidence_out is not None:
        args.evidence_out.write_text(
            json.dumps(
                {
                    "expected_owner": args.expected_owner,
                    "names": sorted(set(names)),
                    "available": report.available,
                    "owned": report.owned,
                    "conflicts": report.conflicts,
                    "errors": report.errors,
                    "unchecked": report.unchecked,
                },
                indent=2,
                sort_keys=True,
            )
            + "\n",
            encoding="utf-8",
        )
    for name in report.available:
        print(f"{name}: available")
    for name in report.owned:
        print(f"{name}: owned by {args.expected_owner}")
    if report.errors:
        for name, error in sorted(report.errors.items()):
            print(f"{name}: {error}", file=sys.stderr)
        if report.unchecked:
            print(
                "preflight stopped before remaining names; wait for the registry throttle to clear and rerun the complete check",
                file=sys.stderr,
            )
        return 2
    if report.conflicts:
        for name, owners in sorted(report.conflicts.items()):
            owner_text = ", ".join(owners) if owners else "no public owners returned"
            print(f"{name}: owned by {owner_text}", file=sys.stderr)
        return 1
    print(f"crates.io name preflight passed for {len(names)} package names")
    return 0


if __name__ == "__main__":
    import sys

    raise SystemExit(main())
