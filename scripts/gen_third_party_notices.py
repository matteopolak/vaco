#!/usr/bin/env python3
"""Generate THIRD_PARTY_LICENSES.html (QA-10, #182).

The shipped attribution file has two inputs, not one:

1. Every Cargo dependency actually linked into the release binaries --
   `cargo about generate --format json` over about.toml's allow-list.
2. Every permissively-licensed reference implementation a crate was
   *translated from* under D7's Tier-A rule (see AGENT-CONSTRAINTS.md) but
   never linked as a Cargo dependency, so (1) cannot see it -- recorded in
   provenance/third-party-notices.toml.

Skipping (2) is the actual legal gap this issue exists to close: MIT, BSD,
ISC, Apache and FTL all require attribution in a redistributed binary
whether the covered code arrived via `[dependencies]` or via a from-scratch
translation checked into our own crate.

Usage:
    python3 scripts/gen_third_party_notices.py [--check] [-o OUTPUT]

--check does not write OUTPUT. It runs the coverage scan (are there any
provenance/*.toml sources that look Tier-A and permissively licensed but
are not cross-referenced from third-party-notices.toml?) and exits non-zero
if it finds one, or if `cargo about` itself fails (e.g. an unrecognised
licence -- see deny.toml's twin allow-list). Wire it into CI/`just ci`
next to `cargo deny check licenses`; both must pass for the same reason.
"""

from __future__ import annotations

import argparse
import html
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
NOTICES_TOML = ROOT / "provenance" / "third-party-notices.toml"
PROVENANCE_DIR = ROOT / "provenance"
DEFAULT_OUTPUT = ROOT / "THIRD_PARTY_LICENSES.html"

# Patterns that mean "this provenance entry describes code under a licence
# that carries a redistribution/attribution duty", as opposed to a bare
# specification/RFC citation (which carries no such duty -- reading and
# implementing from a spec is not copying its expression) or an observed
# ffmpeg-binary behaviour (probing a binary's behaviour is not redistributing
# its code). Deliberately broad and reviewed by a human each time one fires,
# not a legal classifier: see the docstring's "Skipping (2)" paragraph for
# why a false negative here is the expensive direction to be wrong in.
#
# Kept as regex, not plain substrings, after a real miss: a provenance entry
# landed the same day as this script (provenance/sources.toml's
# libvpx-vp8-encoder, "libvpx (BSD-3-Clause)") in the hyphenated SPDX-id
# spelling, which no plain-substring form here matched. The bare `\bBSD\b`
# and `\bMIT\b` fallbacks are deliberately wide for the same reason --
# checked against the whole provenance/ corpus at the time they were added
# and produced no false positive that wasn't already covered by a more
# specific pattern above it.
LICENCE_KEYWORDS = (
    r"Apache License",
    r"Apache-2\.0",
    r"BSD License",
    r"BSD-\d-Clause",
    r"\d-clause BSD",
    r"Simplified BSD",
    r"\bBSD\b",
    r"MIT License",
    r"\bMIT\b",
    r"ISC License",
    r"zlib license",
)
LICENCE_KEYWORD_RES = [re.compile(pat, re.IGNORECASE) for pat in LICENCE_KEYWORDS]
# `[[source]]` blocks name themselves with `id = "..."`; `[[table]]` blocks
# (which don't declare a source, only cite one) use `source = "..."`
# instead. Match either so a licence-keyword hit inside a table-only file
# (vaco-codec-opus.toml has no [[source]] blocks at all) still resolves to
# the source id it is actually citing, rather than reporting "no id found".
SOURCE_ID_RE = re.compile(r'^\s*(?:id|source)\s*=\s*"([^"]+)"', re.MULTILINE)
SOURCE_BLOCK_RE = re.compile(r"^\[\[source\]\]\s*$", re.MULTILINE)


def parse_notices(path: Path) -> list[dict]:
    """Parse third-party-notices.toml's `[[notice]]` array of tables.

    Deliberately not a general TOML parser -- xtask's own precedent
    (AGENT-CONSTRAINTS.md: "deliberately dependency-free") plus this
    script needing to run with the macOS system Python (3.9, no
    `tomllib`) on a bare checkout with no `pip install` step. The file's
    shape is fixed and owned by this same script's docstring, so a
    regex-per-field reader is a fair trade against a real parser dependency.
    """
    text = path.read_text(encoding="utf-8")
    notices = []
    for block in text.split("[[notice]]")[1:]:
        entry: dict = {}
        # Triple-quoted multi-line strings first, so their content (which
        # may itself contain `key = "..."`-shaped lines) never confuses the
        # single-line patterns below.
        for key, val in re.findall(r'^(\w+)\s*=\s*"""(.*?)"""', block, re.S | re.M):
            entry[key] = val.strip()
        block = re.sub(r'^(\w+)\s*=\s*""".*?"""', "", block, flags=re.S | re.M)
        for key, val in re.findall(r"^(\w+)\s*=\s*\[(.*?)\]", block, re.M):
            entry[key] = [v.strip().strip('"') for v in val.split(",") if v.strip()]
        for key, val in re.findall(r'^(\w+)\s*=\s*"([^"]*)"\s*$', block, re.M):
            entry.setdefault(key, val)
        if entry:
            notices.append(entry)
    return notices


def resolve_license_text(notice: dict) -> str:
    if "license_text" in notice:
        return notice["license_text"]
    rel = notice.get("license_text_file")
    if not rel:
        return "(no licence text recorded -- fix provenance/third-party-notices.toml)"
    return (ROOT / rel).read_text(encoding="utf-8")


def run_cargo_about() -> dict:
    proc = subprocess.run(
        ["cargo", "about", "generate", "--format", "json"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr)
        raise SystemExit(
            "cargo-about failed -- a dependency's licence is not on "
            "about.toml's accepted list (keep this in sync with deny.toml's "
            "[licenses] allow list)"
        )
    return json.loads(proc.stdout)


def scan_provenance_gaps(covered_ids: set[str]) -> list[str]:
    """Grep provenance/*.toml for a licence keyword whose nearest
    `[[source]] id` is not in `covered_ids`. Heuristic and deliberately
    over-inclusive: a false positive costs one line of review, a false
    negative is an uncredited redistribution."""
    gaps = []
    for path in sorted(PROVENANCE_DIR.glob("*.toml")):
        if path == NOTICES_TOML:
            continue
        text = path.read_text(encoding="utf-8")
        lines = text.splitlines()
        source_positions = [
            (m.start(), m.group(1)) for m in SOURCE_ID_RE.finditer(text)
        ]
        for lineno, line in enumerate(lines):
            matches = [rx.pattern for rx in LICENCE_KEYWORD_RES if rx.search(line)]
            if not matches:
                continue
            offset = sum(len(l) + 1 for l in lines[:lineno])
            after = [pos for pos in source_positions if pos[0] >= offset]
            before = [pos for pos in source_positions if pos[0] < offset]
            # A comment line describes whatever block comes AFTER it (the
            # style throughout provenance/: a prose paragraph immediately
            # above a `[[source]]` block explains that block). A match
            # inside a field's own value (e.g. `where = "..."`) belongs to
            # the block it is textually INSIDE, i.e. the nearest one
            # BEFORE it. These need different directions -- a comment
            # match resolved backward would credit the previous,
            # unrelated block instead.
            if line.strip().startswith("#"):
                nearest_id = after[0][1] if after else (before[-1][1] if before else None)
            else:
                nearest_id = before[-1][1] if before else (after[0][1] if after else None)
            if nearest_id is None or nearest_id not in covered_ids:
                gaps.append(
                    f"{path.relative_to(ROOT)}:{lineno + 1}: matched "
                    f"{matches[0]!r}, nearest source id = {nearest_id!r} -- "
                    f"not in provenance/third-party-notices.toml"
                )
    return gaps


def render_html(deps: dict, notices: list[dict]) -> str:
    parts = [
        "<!doctype html><meta charset=\"utf-8\">",
        "<title>Vaco Third-Party Licences</title>",
        "<style>body{font-family:system-ui,sans-serif;max-width:900px;"
        "margin:2rem auto;line-height:1.5}pre{white-space:pre-wrap;"
        "background:#f5f5f5;padding:1rem;border-radius:4px;max-height:20rem;"
        "overflow-y:auto}h2{border-bottom:1px solid #ccc;padding-bottom:.3rem}"
        "summary{cursor:pointer;font-weight:600}</style>",
        "<h1>Vaco Third-Party Licences</h1>",
        "<p>Generated by <code>scripts/gen_third_party_notices.py</code>. "
        "Do not edit by hand.</p>",
        "<h2>Part 1: Bundled dependencies</h2>",
        "<p>Every crate compiled into the release binaries, grouped by "
        "licence (source: <code>cargo about</code> over "
        "<code>about.toml</code>'s allow-list).</p>",
    ]
    for lic in deps["licenses"]:
        crates = ", ".join(
            f"{u['crate']['name']} {u['crate']['version']}" for u in lic["used_by"]
        )
        parts.append(f"<details><summary>{html.escape(lic['name'])} "
                      f"({len(lic['used_by'])} crates)</summary>")
        parts.append(f"<p>{html.escape(crates)}</p>")
        parts.append(f"<pre>{html.escape(lic['text'])}</pre></details>")

    parts.append("<h2>Part 2: Reference implementations consulted</h2>")
    parts.append(
        "<p>Code in this repository translated or transcribed from a "
        "permissively-licensed reference implementation under D7's Tier-A "
        "rule, but never linked as a Cargo dependency -- so Part 1's scan "
        "cannot see it. Source: <code>provenance/third-party-notices.toml</code>, "
        "cross-referenced against <code>provenance/</code>.</p>"
    )
    for notice in notices:
        crates = ", ".join(notice.get("crates", []))
        parts.append(
            f"<details open><summary>{html.escape(notice['name'])} "
            f"({html.escape(notice.get('license', '?'))}, used by {html.escape(crates)})"
            f"</summary>"
        )
        if notice.get("note"):
            parts.append(f"<p>{html.escape(notice['note'])}</p>")
        parts.append(f"<pre>{html.escape(resolve_license_text(notice))}</pre></details>")

    return "\n".join(parts) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--check", action="store_true",
                     help="run the coverage scan only; do not write output")
    ap.add_argument("-o", "--output", type=Path, default=DEFAULT_OUTPUT)
    args = ap.parse_args()

    notices = parse_notices(NOTICES_TOML)
    covered_ids = {sid for n in notices for sid in n.get("sources", [])}

    gaps = scan_provenance_gaps(covered_ids)
    if gaps:
        sys.stderr.write("gen_third_party_notices: coverage gap(s) found:\n")
        for gap in gaps:
            sys.stderr.write(f"  {gap}\n")
        if args.check:
            return 1
        sys.stderr.write(
            "(continuing to generate anyway; re-run with --check to make "
            "this fail)\n"
        )

    if args.check:
        # Still worth confirming cargo-about itself resolves cleanly even
        # in --check mode, since that is the other half of "coverage".
        run_cargo_about()
        print(f"gen_third_party_notices --check: ok, {len(notices)} Tier-A "
              f"notice(s), no coverage gaps")
        return 0

    deps = run_cargo_about()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(render_html(deps, notices), encoding="utf-8")
    # `-o` may be a path outside ROOT entirely (e.g. scripts/package-release.sh
    # writes into dist/<version>/<triple>/), so resolve before displaying
    # rather than assuming it is ROOT-relative.
    shown = args.output.resolve()
    try:
        shown = shown.relative_to(ROOT)
    except ValueError:
        pass
    print(f"wrote {shown} "
          f"({len(deps['licenses'])} dependency licence group(s), "
          f"{len(notices)} Tier-A notice(s))")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
