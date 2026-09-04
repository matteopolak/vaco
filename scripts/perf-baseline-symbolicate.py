#!/usr/bin/env python3
"""Symbolicate a samply profile.json.gz against a dSYM using llvm-symbolizer,
recovering full inline chains, and aggregate self time by each chain's outermost
physically-emitted frame or its innermost inlined frame.

Usage: symbolicate.py <profile.json.gz> <dsym_path> <lib_name>
                      [--vmaddr 0x100000000] [--innermost]

Prints:
  - fraction of samples whose leaf frame is inside <lib_name>
  - of those, fraction llvm-symbolizer resolved to a real name (vs "??")
  - top 20 functions by self time (outermost physically-emitted frame)
"""
import gzip
import json
import subprocess
import sys
import argparse
from collections import Counter


def load_profile(path):
    with gzip.open(path) as f:
        return json.load(f)


def aggregate_counts(leaf_addrs, addr_to_chain, innermost=False):
    """Aggregate address samples by one end of each inline chain."""
    counts = Counter()
    for addr, count in leaf_addrs.items():
        chain = addr_to_chain.get(addr, [])
        if not chain:
            counts["<unresolved>"] += count
            continue
        frame = chain[0] if innermost else chain[-1]
        name = frame.split(" at ")[0].strip()
        prefix = "(inlined by)"
        if name.startswith(prefix):
            name = name[len(prefix):].strip()
        counts[name] += count
    return counts


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("profile")
    ap.add_argument("dsym", help="path to the dSYM's inner DWARF binary, e.g. foo.dSYM/Contents/Resources/DWARF/foo")
    ap.add_argument("lib_name")
    ap.add_argument("--vmaddr", default="0x100000000")
    ap.add_argument("--top", type=int, default=20)
    ap.add_argument("--innermost", action="store_true",
                    help="attribute each sample to the first (most inlined) frame")
    args = ap.parse_args()

    d = load_profile(args.profile)
    lib_idx = None
    for i, lib in enumerate(d["libs"]):
        if lib["name"] == args.lib_name:
            lib_idx = i
            break
    if lib_idx is None:
        print("lib not found:", args.lib_name, file=sys.stderr)
        sys.exit(1)

    total_samples = 0
    in_lib_samples = 0
    leaf_addrs = Counter()  # address (int) -> sample count, only for in-lib leaf frames

    for th in d["threads"]:
        strs = th["stringArray"]
        frameTable = th["frameTable"]
        funcTable = th["funcTable"]
        resourceTable = th["resourceTable"]
        stackTable = th["stackTable"]
        samples = th["samples"]

        # stack index -> frame index
        stack_frame = stackTable["frame"]

        n = samples["length"]
        total_samples += n
        for si in range(n):
            stack_idx = samples["stack"][si]
            if stack_idx is None:
                continue
            frame_idx = stack_frame[stack_idx]
            func_idx = frameTable["func"][frame_idx]
            resource_idx = funcTable["resource"][func_idx]
            if resource_idx is None or resource_idx < 0:
                continue
            resource_lib = resourceTable["lib"][resource_idx]
            if resource_lib != lib_idx:
                continue
            in_lib_samples += 1
            addr = frameTable["address"][frame_idx]
            leaf_addrs[addr] += 1

    print(f"total samples: {total_samples}", file=sys.stderr)
    print(f"leaf-in-{args.lib_name} samples: {in_lib_samples} ({100*in_lib_samples/total_samples:.1f}%)", file=sys.stderr)

    vmaddr = int(args.vmaddr, 16)
    unique_addrs = sorted(leaf_addrs.keys())
    print(f"unique leaf addresses: {len(unique_addrs)}", file=sys.stderr)

    # Feed all addresses to llvm-symbolizer in one batch (module-relative offset + vmaddr).
    addr_lines = [hex(a + vmaddr) for a in unique_addrs]
    proc = subprocess.run(
        ["llvm-symbolizer", f"--obj={args.dsym}", "--inlines", "-f", "-C", "-p"],
        input="\n".join(addr_lines) + "\n",
        capture_output=True, text=True,
    )
    if proc.returncode != 0:
        print("llvm-symbolizer failed:", proc.stderr, file=sys.stderr)
        sys.exit(1)

    # Output format per address: one or more lines of "func at file:line", blank line separates addresses.
    blocks = proc.stdout.strip("\n").split("\n\n")
    if len(blocks) != len(unique_addrs):
        print(f"WARNING: block count {len(blocks)} != address count {len(unique_addrs)}", file=sys.stderr)

    addr_to_chain = {}
    resolved = 0
    for addr, block in zip(unique_addrs, blocks):
        lines = [l for l in block.split("\n") if l.strip()]
        chain = []
        for l in lines:
            chain.append(l.strip())
        if chain and not chain[0].startswith("??"):
            resolved += 1
        addr_to_chain[addr] = chain

    print(f"resolved (non-'??') addresses: {resolved}/{len(unique_addrs)} ({100*resolved/len(unique_addrs) if unique_addrs else 0:.1f}%)", file=sys.stderr)

    counts = aggregate_counts(leaf_addrs, addr_to_chain, args.innermost)
    total_in_lib = sum(counts.values())
    mode = "innermost inlined frame" if args.innermost else "outermost physically-emitted frame"
    print(f"\nTop {args.top} by self time ({mode}), of {total_in_lib} in-lib samples:", file=sys.stderr)
    results = []
    for name, count in counts.most_common(args.top):
        pct = 100 * count / total_in_lib
        results.append({"function": name, "self_pct": round(pct, 2), "samples": count})
        print(f"{pct:6.2f}%  {count:6d}  {name}")

    print(json.dumps({
        "total_samples": total_samples,
        "in_lib_samples": in_lib_samples,
        "in_lib_fraction": in_lib_samples / total_samples if total_samples else 0,
        "unique_addrs": len(unique_addrs),
        "resolved_addrs": resolved,
        "resolved_fraction": resolved / len(unique_addrs) if unique_addrs else 0,
        "frame_mode": "innermost" if args.innermost else "outermost",
        "top": results,
    }, indent=2))


if __name__ == "__main__":
    main()
