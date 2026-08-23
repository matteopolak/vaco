#!/usr/bin/env python3
"""Create placeholders for anything a Cargo.toml declares but does not have.

Two gaps, both of which fail manifest PARSING — and manifest parsing is
workspace-wide, so every `cargo` command in the tree fails for every agent until
the gap closes, not just the owning crate's.

1. A `[[bench]]`/`[[test]]`/`[[bin]]` whose file does not exist.
2. **A crate with a `Cargo.toml` and no crate root at all.** This is the one the
   script missed for its first eight occurrences: an agent creates the manifest
   before `src/lib.rs`, and the whole workspace stops. It cost one agent about
   25 minutes while five others were running, and the script cheerfully reported
   "nothing missing" throughout — which is worse than not existing, because it
   sends you looking somewhere else.

Agents open both routinely. The brief template says to create the file first;
this exists because the rule is easy to forget under a deadline and the blast
radius is everyone.

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


LIB_BODY = '''//! PLACEHOLDER, created to unblock workspace manifest parsing.
//!
//! `{manifest}` exists with no crate root, which fails manifest parsing for the
//! WHOLE workspace -- every agent\'s cargo command, not just this crate\'s.
//!
//! The owning agent should replace this wholesale. Next time create the file
//! BEFORE declaring it; see the agent brief template.
'''


def crate_root(dirpath: str, text: str) -> None:
    """Give a manifest a crate root if it declares none and has none.

    A manifest may name its own path with `[lib] path = ...` or `[[bin]] path`;
    only the default locations are handled, because a custom path is a
    deliberate act and the agent that wrote it knows where the file goes.
    """
    if "[lib]" in text or "[[bin]]" in text or "[workspace]" in text:
        # `[lib]`/`[[bin]]` are handled by the target loop below, and a
        # workspace root is not a crate.
        if "[lib]" not in text:
            return
    for candidate in ("src/lib.rs", "src/main.rs"):
        if os.path.exists(os.path.join(dirpath, candidate)):
            return None
    target = os.path.join(dirpath, "src", "lib.rs")
    os.makedirs(os.path.dirname(target), exist_ok=True)
    with open(target, "w") as f:
        f.write(LIB_BODY.format(manifest=os.path.relpath(target, ROOT)))
    return target


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
        root = crate_root(dirpath, text)
        if root:
            made.append(os.path.relpath(root, ROOT))
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
