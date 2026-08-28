#!/usr/bin/env python3
"""Differential harness for vaco-codec-subtitle-bitmap.

Builds one hand-constructed fixture per format (DVB, PGS, VobSub/SPU),
decodes it two ways -- this crate's own `examples/decode_dump` binary, and
ffmpeg's reference decoder via PyAV -- and diffs the resulting rect geometry
and raw palette-index pixels. Run with `python3 compare.py` from anywhere
(paths are resolved relative to this file); requires `ffmpeg`/`libavcodec`
and the `av` (PyAV) Python package.
"""

from __future__ import annotations

import struct
import subprocess
import sys
from pathlib import Path

import av

HERE = Path(__file__).resolve().parent
CRATE = HERE.parent.parent


def cargo_run(mode: str, *args: str) -> str:
    cmd = [
        "cargo",
        "run",
        "-q",
        "-p",
        "vaco-codec-subtitle-bitmap",
        "--example",
        "decode_dump",
        "--",
        mode,
        *args,
    ]
    result = subprocess.run(cmd, cwd=CRATE, capture_output=True, text=True, check=True)
    return result.stdout


def parse_dump(text: str):
    """`decode_dump`'s own text format -> list of (x, y, w, h, indices)."""
    lines = text.strip().splitlines()
    rects = []
    i = 0
    while i < len(lines):
        if lines[i].startswith("RECT"):
            _, x, y, w, h = lines[i].split()
            indices = [int(v) for v in lines[i + 1].split()] if i + 1 < len(lines) else []
            rects.append((int(x), int(y), int(w), int(h), indices))
            i += 2
        else:
            i += 1
    return rects


def seg_dvb(kind: int, page_id: int, payload: bytes) -> bytes:
    return bytes([0x0F, kind]) + struct.pack(">H", page_id) + struct.pack(">H", len(payload)) + payload


def build_dvb_fixture() -> bytes:
    page_payload = bytes([5, 0x08, 0, 0]) + struct.pack(">H", 10) + struct.pack(">H", 10)
    page = seg_dvb(0x10, 1, page_payload)

    region_payload = (
        bytes([0, 0x08])
        + struct.pack(">H", 4)
        + struct.pack(">H", 4)
        + bytes([0x24, 0x00, 0x00, 0x04])
        + struct.pack(">H", 1)
        + bytes([0x00, 0x00, 0x00, 0x00])
    )
    region = seg_dvb(0x11, 1, region_payload)

    clut_payload = bytes([0, 0x00, 1, 0x81, 255, 128, 128, 255])
    clut = seg_dvb(0x12, 1, clut_payload)

    line = bytes([0x10, 0x55, 0x00, 0xF0])
    field = line + line
    obj_payload = (
        struct.pack(">H", 1)
        + bytes([0x00])
        + struct.pack(">H", len(field))
        + struct.pack(">H", len(field))
        + field
        + field
    )
    obj = seg_dvb(0x13, 1, obj_payload)

    end = seg_dvb(0x80, 1, b"")
    return page + region + clut + obj + end


def seg_pgs(kind: int, pts: int, payload: bytes) -> bytes:
    return b"PG" + struct.pack(">I", pts) + struct.pack(">I", 0) + bytes([kind]) + struct.pack(">H", len(payload)) + payload


def build_pgs_fixture() -> bytes:
    pcs_payload = (
        struct.pack(">H", 1920)
        + struct.pack(">H", 1080)
        + bytes([0x10])
        + struct.pack(">H", 0)
        + bytes([0x80, 0x00, 0])
        + bytes([1])
        + struct.pack(">H", 1)
        + bytes([0, 0x00])
        + struct.pack(">H", 5)
        + struct.pack(">H", 5)
    )
    pcs = seg_pgs(0x16, 90_000, pcs_payload)

    pds_payload = bytes([0, 0, 1, 255, 128, 128, 255])
    pds = seg_pgs(0x14, 90_000, pds_payload)

    rle = bytes([1, 1, 0, 0, 1, 1])
    data_len = 4 + len(rle)
    ods_payload = (
        struct.pack(">H", 1)
        + bytes([0, 0xC0])
        + bytes([0, (data_len >> 8) & 0xFF, data_len & 0xFF])
        + struct.pack(">H", 2)
        + struct.pack(">H", 2)
        + rle
    )
    ods = seg_pgs(0x15, 90_000, ods_payload)

    end = seg_pgs(0x80, 90_000, b"")
    return pcs + pds + ods + end


def build_spu_fixture() -> bytes:
    body = bytearray([0, 0, 0, 0])
    top_offset = len(body)
    body += bytes([0x55, 0x55])
    bottom_offset = len(body)
    body += bytes([0x55, 0x55])
    dcsqta = len(body)
    body += struct.pack(">H", 0)
    body += struct.pack(">H", dcsqta)
    body += bytes([0x01])
    body += bytes([0x03, 0x21, 0x30])
    body += bytes([0x04, 0xFF, 0xFF])
    body += bytes([0x05, 0x00, 0x00, 0x03, 0x00, 0x00, 0x01])
    body += bytes([0x06]) + struct.pack(">H", top_offset) + struct.pack(">H", bottom_offset)
    body += bytes([0xFF])
    size = len(body)
    body[0] = (size >> 8) & 0xFF
    body[1] = size & 0xFF
    body[2] = (dcsqta >> 8) & 0xFF
    body[3] = dcsqta & 0xFF
    return bytes(body)


def oracle_dvb(path: Path):
    container = av.open(str(path), format="dvbsub")
    stream = container.streams.subtitles[0]
    rects = []
    for packet in container.demux(stream):
        for sub in packet.decode():
            rects.append((sub.x, sub.y, sub.width, sub.height, list(bytes(sub.planes[0]))))
    return rects


def oracle_pgs(path: Path):
    container = av.open(str(path))
    stream = container.streams.subtitles[0]
    rects = []
    for packet in container.demux(stream):
        for sub in packet.decode():
            rects.append((sub.x, sub.y, sub.width, sub.height, list(bytes(sub.planes[0]))))
    return rects


def oracle_vobsub(path: Path, palette_csv: str):
    codec = av.CodecContext.create("dvdsub", "r")
    codec.extradata = f"palette: {palette_csv}\n".encode()
    packet = av.Packet(path.read_bytes())
    rects = []
    for sub in codec.decode(packet):
        rects.append((sub.x, sub.y, sub.width, sub.height, list(bytes(sub.planes[0]))))
    return rects


def report(name: str, mine, theirs) -> tuple[bool, str]:
    if len(mine) != len(theirs):
        return False, f"rect count differs: mine={len(mine)} reference={len(theirs)}"
    for (mx, my, mw, mh, midx), (tx, ty, tw, th, tidx) in zip(mine, theirs):
        if (mx, my, mw, mh) != (tx, ty, tw, th):
            return False, f"geometry differs: mine=({mx},{my},{mw},{mh}) reference=({tx},{ty},{tw},{th})"
        if midx != tidx:
            diffs = sum(1 for a, b in zip(midx, tidx) if a != b)
            return False, f"{diffs}/{len(midx)} pixel indices differ"
    return True, "bitmap-identical"


def main() -> int:
    rows = []

    dvb_path = HERE / "dvbsub_manual.dvb"
    dvb_path.write_bytes(build_dvb_fixture())
    mine = parse_dump(cargo_run("dvb", str(dvb_path)))
    mine_rects = [(x, y, w, h, idx) for x, y, w, h, idx in mine]
    theirs = oracle_dvb(dvb_path)
    ok, detail = report("dvb", mine_rects, theirs)
    rows.append(("dvb", len(mine_rects), ok, detail))

    pgs_path = HERE / "pgs_manual.sup"
    pgs_path.write_bytes(build_pgs_fixture())
    mine_out = cargo_run("pgs", str(pgs_path))
    mine = parse_dump(mine_out)
    mine_rects = [(x, y, w, h, idx) for x, y, w, h, idx in mine]
    theirs = oracle_pgs(pgs_path)
    ok, detail = report("pgs", mine_rects, theirs)
    rows.append(("pgs", len(mine_rects), ok, detail))

    spu_path = HERE / "vobsub_manual.spu"
    spu_path.write_bytes(build_spu_fixture())
    palette_csv = "000000,0a141e,ffffff,010203"
    mine_out = cargo_run("vobsub", str(spu_path), palette_csv)
    mine = parse_dump(mine_out)
    mine_rects = [(x, y, w, h, idx) for x, y, w, h, idx in mine]
    theirs = oracle_vobsub(spu_path, palette_csv)
    ok, detail = report("vobsub", mine_rects, theirs)
    rows.append(("vobsub", len(mine_rects), ok, detail))

    print(f"{'format':<10}{'events':<8}{'matches ref':<14}{'detail'}")
    for fmt, events, ok, detail in rows:
        print(f"{fmt:<10}{events:<8}{'yes' if ok else 'no':<14}{detail}")

    return 0 if all(ok for _, _, ok, _ in rows) else 1


if __name__ == "__main__":
    sys.exit(main())
