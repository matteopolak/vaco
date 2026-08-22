#!/usr/bin/env python3
"""Create placeholder files for declared-but-missing bench/test/bin targets.

A `[[bench]]`/`[[test]]`/`[[bin]]` whose file does not exist fails manifest
PARSING, and manifest parsing is workspace-wide: every `cargo` command in the
tree fails for every agent until the gap closes. Agents open that gap routinely,
by writing the manifest entry before writing the file.

Deliberately plain Python with no cargo involvement, because when this is needed
`cargo run -p xtask` is precisely what does not work.

Safe to run at any time: it only ever creates files that do not exist, and the
owning agent overwrites them.
"""

import os
import re

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
KINDS = {"bench": "benches", "test": "tests", "bin": "src/bin"}

BODY = '''//! PLACEHOLDER, created to unblock workspace manifest parsing.
//!
//! `{manifest}` declared `[[{kind}]] name = "{name}"` before this file existed.
//! That fails manifest parsing for the WHOLE workspace, so every agent's cargo
//! command fails until the gap closes -- not just this crate's.
//!
//! The owning agent should replace this wholesale. Next time create the file
//! BEFORE declaring it; see the agent brief template.

fn main() {{
{body}}}
'''


def main() -> None:
    made = []
    for dirpath, dirnames, filenames in os.walk(os.path.join(ROOT, "crates")):
        dirnames[:] = [d for d in dirnames if d not in ("target", "src", "tests", "benches")]
        if "Cargo.toml" not in filenames:
            continue
        manifest = os.path.join(dirpath, "Cargo.toml")
        try:
            text = open(manifest).read()
        except OSError:
            continue
        for kind, subdir in KINDS.items():
            for block in text.split(f"[[{kind}]]")[1:]:
                block = block.split("[[")[0].split("\n[")[0]
                m = re.search(r'name\s*=\s*"([^"]+)"', block)
                if not m:
                    continue
                name = m.group(1)
                path = re.search(r'path\s*=\s*"([^"]+)"', block)
                target = (
                    os.path.join(dirpath, path.group(1))
                    if path
                    else os.path.join(dirpath, subdir, f"{name}.rs")
                )
                if os.path.exists(target) or os.path.exists(
                    os.path.join(dirpath, subdir, name, "main.rs")
                ):
                    continue
                os.makedirs(os.path.dirname(target), exist_ok=True)
                harness_false = "harness" in block and "false" in block
                body = "    divan::main();\n" if (kind == "bench" and harness_false) else ""
                with open(target, "w") as f:
                    f.write(
                        BODY.format(
                            manifest=os.path.relpath(manifest, ROOT),
                            kind=kind,
                            name=name,
                            body=body,
                        )
                    )
                made.append(os.path.relpath(target, ROOT))

    for p in made:
        print(f"created placeholder: {p}")
    print(f"{len(made)} placeholder(s) created" if made else "nothing missing")


if __name__ == "__main__":
    main()
