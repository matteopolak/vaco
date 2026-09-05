#!/usr/bin/env python3
"""Generate CDEF goldens using unmodified BSD-2-Clause dav1d C function bodies.

The temporary compatibility prelude supplies dav1d's integer helpers and public
types. No reference implementation is incorporated into the Rust decoder.
"""
import argparse
import pathlib
import subprocess
import tempfile
import urllib.request

REVISION = "aa09a630ef57ee7d9482ffb7ef355a903dbb5302"
URL = f"https://raw.githubusercontent.com/videolan/dav1d/{REVISION}/src/cdef_tmpl.c"

PRELUDE = r"""
#include <assert.h>
#include <stdint.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#define NOINLINE
#define HIGHBD_DECL_SUFFIX , const int bitdepth_max
#define HIGHBD_TAIL_SUFFIX , bitdepth_max
#define PXSTRIDE(s) ((s) / 2)
typedef uint16_t pixel;
enum CdefEdgeFlags { CDEF_HAVE_LEFT=1, CDEF_HAVE_RIGHT=2,
                     CDEF_HAVE_TOP=4, CDEF_HAVE_BOTTOM=8 };
static int imin(int a,int b) { return a < b ? a : b; }
static int imax(int a,int b) { return a > b ? a : b; }
static unsigned umin(unsigned a,unsigned b) { return a < b ? a : b; }
static int iclip(int x,int a,int b) { return imin(imax(x,a),b); }
static int apply_sign(int v,int sign) { return sign < 0 ? -v : v; }
static int ulog2(unsigned x) { return 31 - __builtin_clz(x); }
static int bitdepth_from_max(int x) { return ulog2(x) + 1; }
/* AV1 specification 7.15.3, row stride 12, two wraparound entries per end. */
static const int8_t dav1d_cdef_directions[12][2] = {
  {12,24},{12,23},{-11,-22},{1,-10},{1,2},{1,14},
  {13,26},{12,25},{12,24},{12,23},{-11,-22},{1,-10}
};
"""

DRIVER = r"""
static void put16(unsigned v) { putchar(v & 255); putchar((v >> 8) & 255); }
static void put32(unsigned v) { put16(v & 65535); put16(v >> 16); }
int main(void) {
  unsigned id = 0;
  const int secondary[] = {0,1,2,4};
  for (int bd=8; bd<=12; bd+=2)
  for (int damping=2; damping<=6; damping++)
  for (int dir=0; dir<8; dir++)
  for (int pri=0; pri<16; pri++)
  for (int sec=0; sec<4; sec++,id++) {
    uint16_t src[144], left[8][2];
    unsigned state = 0x9e3779b9u ^ id;
    for (int i=0; i<144; i++) {
      state ^= state << 13; state ^= state >> 17; state ^= state << 5;
      src[i] = (((id & 1) ? (96 + state % 33) : (state & 255)) << (bd-8)) |
               ((state >> 8) & ((1u << (bd-8))-1));
    }
    const int shape = id % 3;
    const int w = shape == 0 ? 8 : 4;
    const int h = shape == 2 ? 4 : 8;
    const int edges = (id / 3) % 16;
    uint16_t *dst = src + 26;
    unsigned variance;
    int direction = cdef_find_dir_c(dst, 24, &variance, (1 << bd)-1);
    for (int y=0; y<h; y++)
      for (int x=0; x<2; x++) left[y][x] = src[(y+2)*12+x];
    if (pri || secondary[sec])
      cdef_filter_block_c(dst, 24, left, src+2, src+(h+2)*12+2,
        pri << (bd-8), secondary[sec] << (bd-8), dir, damping+bd-8,
        w,h,edges,(1<<bd)-1);
    put32(direction); put32(variance);
    for (int y=0; y<8; y++)
      for (int x=0; x<8; x++) put16(y<h && x<w ? dst[y*12+x] : 0);
  }
  return ferror(stdout) ? 1 : 0;
}
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=pathlib.Path)
    args = parser.parse_args()
    reference = urllib.request.urlopen(URL, timeout=30).read().decode()
    # Keep license and function bodies verbatim; omit build-system includes,
    # platform dispatch, and the initializer (none participate in the oracle).
    reference = reference.split("#if HAVE_ASM", 1)[0]
    reference = "\n".join(line for line in reference.splitlines()
                          if not line.startswith("#include"))
    with tempfile.TemporaryDirectory(prefix="vaco-cdef-oracle-") as directory:
        scratch = pathlib.Path(directory)
        source = scratch / "oracle.c"
        source.write_text(PRELUDE + reference + DRIVER)
        binary = scratch / "oracle"
        subprocess.run(["cc", "-O2", "-std=c11", str(source), "-o", str(binary)], check=True)
        result = subprocess.run([str(binary)], check=True, capture_output=True)
    expected = 3 * 5 * 8 * 16 * 4 * 136
    if len(result.stdout) != expected:
        raise RuntimeError(f"Expected {expected} bytes, received {len(result.stdout)}")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_bytes(result.stdout)
    print(f"{len(result.stdout)} bytes, 7680 cases, dav1d {REVISION}")


if __name__ == "__main__":
    main()
