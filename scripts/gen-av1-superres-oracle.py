#!/usr/bin/env python3
"""Generate AV1 super-resolution goldens from pinned BSD-2-Clause dav1d.

The production Rust code is derived from AV1 specification section 7.16.  This
generator instead extracts dav1d's scalar filter table and `resize_c` function
unchanged, compiles them with a small input/output adapter, and writes its
observed output as a checked-in fixture.
"""

import argparse
import pathlib
import subprocess
import tempfile
import urllib.request

REVISION = "aa09a630ef57ee7d9482ffb7ef355a903dbb5302"
BASE_URL = f"https://raw.githubusercontent.com/videolan/dav1d/{REVISION}/src"
TABLES_URL = f"{BASE_URL}/tables.c"
MC_URL = f"{BASE_URL}/mc_tmpl.c"

PRELUDE = r"""
#include <stdint.h>
#include <stddef.h>
#include <stdlib.h>
#include <stdio.h>
#define ATTR_MCMODEL_SMALL
#define ALIGN(name, alignment) name
#define NOINLINE
#define HIGHBD_DECL_SUFFIX , const int bitdepth_max
#define HIGHBD_TAIL_SUFFIX , bitdepth_max
#define PXSTRIDE(stride) ((stride) / 2)
typedef uint16_t pixel;
static int imin(int a, int b) { return a < b ? a : b; }
static int imax(int a, int b) { return a > b ? a : b; }
static int iclip(int value, int low, int high) {
    return imin(imax(value, low), high);
}
#define iclip_pixel(value) iclip((value), 0, bitdepth_max)
"""


def definition(source: str, marker: str) -> str:
    """Return one complete C definition beginning at marker, including `};`."""
    start = source.index(marker)
    end = source.index("};", start) + 2
    return source[start:end]


def function(source: str, marker: str) -> str:
    """Return one balanced-brace C function beginning at marker."""
    start = source.index(marker)
    brace = source.index("{", start)
    depth = 0
    for index in range(brace, len(source)):
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
            if depth == 0:
                return source[start : index + 1]
    raise ValueError(f"unclosed function starting {marker!r}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=pathlib.Path)
    args = parser.parse_args()
    tables = urllib.request.urlopen(TABLES_URL, timeout=30).read().decode()
    mc = urllib.request.urlopen(MC_URL, timeout=30).read().decode()
    table = definition(tables, "const int8_t ALIGN(dav1d_resize_filter")
    resize = function(mc, "static void resize_c(")
    adapter = pathlib.Path("provenance/vaco-codec-av1-superres-oracle.c").read_text()
    with tempfile.TemporaryDirectory(prefix="vaco-superres-oracle-") as directory:
        scratch = pathlib.Path(directory)
        source = scratch / "oracle.c"
        source.write_text(PRELUDE + "\n" + table + "\n" + resize + "\n" + adapter)
        binary = scratch / "oracle"
        subprocess.run(["cc", "-O2", "-std=c11", str(source), "-o", str(binary)], check=True)
        result = subprocess.run([str(binary)], check=True, capture_output=True)
    if len(result.stdout) == 0:
        raise RuntimeError("dav1d oracle produced no output")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_bytes(result.stdout)
    print(f"{len(result.stdout)} bytes, 18 cases, dav1d {REVISION}")


if __name__ == "__main__":
    main()
