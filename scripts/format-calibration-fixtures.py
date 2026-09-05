#!/usr/bin/env python3
"""Create the byte-level inputs needed by format calibration experiments."""

from __future__ import annotations

import argparse
import struct
from pathlib import Path


def vint(value: int, length: int | None = None) -> bytes:
    if length is None:
        length = next(n for n in range(1, 9) if value < (1 << (7 * n)) - 1)
    if not 1 <= length <= 8 or value >= (1 << (7 * length)) - 1:
        raise ValueError("EBML value does not fit")
    return (value | (1 << (7 * length))).to_bytes(length, "big")


def ebml_id(value: int) -> bytes:
    return value.to_bytes(max(1, (value.bit_length() + 7) // 8), "big")


def ebml_element(identifier: int, payload: bytes) -> bytes:
    return ebml_id(identifier) + vint(len(payload)) + payload


def read_ebml_header(data: bytes, offset: int, *, size: bool) -> tuple[int, int]:
    first = data[offset]
    mask = 0x80
    length = 1
    while length <= 8 and not first & mask:
        mask >>= 1
        length += 1
    if length > 8 or offset + length > len(data):
        raise ValueError("invalid EBML variable-length integer")
    raw = int.from_bytes(data[offset : offset + length], "big")
    return (raw & ((1 << (7 * length)) - 1) if size else raw), length


def ebml_children(data: bytes, start: int, end: int):
    offset = start
    while offset < end:
        identifier, id_len = read_ebml_header(data, offset, size=False)
        payload_size, size_len = read_ebml_header(data, offset + id_len, size=True)
        payload = offset + id_len + size_len
        box_end = payload + payload_size
        if box_end > end:
            raise ValueError("EBML child exceeds its parent")
        yield identifier, offset, payload, box_end
        offset = box_end


def make_k1(source: Path, output: Path) -> None:
    data = source.read_bytes()
    top = list(ebml_children(data, 0, len(data)))
    segment = next(item for item in top if item[0] == 0x18538067)
    cluster = next(
        item
        for item in ebml_children(data, segment[2], segment[3])
        if item[0] == 0x1F43B675
    )
    groups = [
        item
        for item in ebml_children(data, cluster[2], cluster[3])
        if item[0] == 0xA0
    ][:2]
    if len(groups) != 2:
        raise ValueError("source needs two BlockGroups")

    frames: list[bytes] = []
    header = b""
    for group in groups:
        children = list(ebml_children(data, group[2], group[3]))
        block = next(item for item in children if item[0] == 0xA1)
        payload = data[block[2] : block[3]]
        _, track_len = read_ebml_header(payload, 0, size=False)
        if len(payload) < track_len + 3 or payload[track_len + 2] & 0x06:
            raise ValueError("source Block is already laced or truncated")
        if not header:
            header = payload[: track_len + 3]
        frames.append(payload[track_len + 3 :])

    laced_header = bytearray(header)
    laced_header[-1] |= 0x06
    lace = bytes(laced_header) + b"\x01" + vint(len(frames[0])) + b"".join(frames)
    replacement = ebml_element(0xA0, ebml_element(0xA1, lace))
    old_length = groups[1][3] - groups[0][1]
    gap = old_length - len(replacement)
    for size_len in range(1, 9):
        payload_len = gap - 1 - size_len
        if payload_len >= 0 and payload_len < (1 << (7 * size_len)) - 1:
            replacement += b"\xec" + vint(payload_len, size_len) + bytes(payload_len)
            break
    if len(replacement) != old_length:
        raise ValueError("could not pad laced replacement to the original size")
    output.write_bytes(data[: groups[0][1]] + replacement + data[groups[1][3] :])


def mp4_boxes(data: bytes, start: int, end: int):
    offset = start
    while offset + 8 <= end:
        size = int.from_bytes(data[offset : offset + 4], "big")
        header = 8
        if size == 1:
            size = int.from_bytes(data[offset + 8 : offset + 16], "big")
            header = 16
        elif size == 0:
            size = end - offset
        if size < header or offset + size > end:
            raise ValueError("invalid ISO BMFF box")
        yield data[offset + 4 : offset + 8], offset, offset + header, offset + size
        offset += size


MP4_CONTAINERS = {b"moov", b"trak", b"mdia", b"minf", b"stbl"}


def map_mp4(data: bytes, wanted: bytes, transform, *, occurrence: int = 0) -> bytes:
    seen = 0

    def visit(region: bytes) -> bytes:
        nonlocal seen
        result = bytearray()
        for kind, offset, payload, end in mp4_boxes(region, 0, len(region)):
            box = region[offset:end]
            header_len = payload - offset
            body = region[payload:end]
            if kind in MP4_CONTAINERS:
                body = visit(body)
                box = len(body + bytes(header_len)).to_bytes(4, "big") + kind + body
            if kind == wanted:
                if seen == occurrence:
                    box = transform(box)
                seen += 1
            result.extend(box)
        return bytes(result)

    result = visit(data)
    if seen <= occurrence:
        raise ValueError(f"missing {wanted.decode('ascii')} box")
    return result


def make_m2_cslg(source: Path, output: Path, shift: int) -> None:
    def add_cslg(stbl: bytes) -> bytes:
        body = stbl[8:]
        fields = b"".join(struct.pack(">q", value) for value in (shift, shift, 0, 0, 0))
        cslg = struct.pack(">I4sB3s", 12 + len(fields), b"cslg", 1, b"\0\0\0") + fields
        children = list(mp4_boxes(body, 0, len(body)))
        ctts = next(item for item in children if item[0] == b"ctts")
        at = ctts[3]
        changed = body[:at] + cslg + body[at:]
        return struct.pack(">I4s", len(changed) + 8, b"stbl") + changed

    output.write_bytes(map_mp4(source.read_bytes(), b"stbl", add_cslg))


def make_m4_conflict(source: Path, output: Path) -> None:
    data = source.read_bytes()
    chpl = next(item for item in mp4_boxes(data, 0, len(data)) if item[0] == b"moov")
    moov_children = list(mp4_boxes(data, chpl[2], chpl[3]))
    udta = next(item for item in moov_children if item[0] == b"udta")
    chapter_box = next(item for item in mp4_boxes(data, udta[2], udta[3]) if item[0] == b"chpl")
    payload = bytearray(data[chapter_box[2] : chapter_box[3]])
    for old, new in ((b"Alpha", b"NeroA"), (b"Beta", b"Nero")):
        position = payload.find(old)
        if position < 0:
            raise ValueError(f"missing chpl title {old!r}")
        payload[position : position + len(old)] = new
    output.write_bytes(data[: chapter_box[2]] + payload + data[chapter_box[3] :])


def make_m6(avc: Path, hevc: Path, output: Path, description_index: int) -> None:
    def first_box(data: bytes, kind: bytes) -> bytes:
        found: list[bytes] = []

        def capture(box: bytes) -> bytes:
            found.append(box)
            return box

        map_mp4(data, kind, capture)
        return found[0]

    hvc_stsd = first_box(hevc.read_bytes(), b"stsd")
    hvc_entries = hvc_stsd[16:]

    def extend_stsd(box: bytes) -> bytes:
        count = int.from_bytes(box[12:16], "big")
        body = box[8:12] + (count + 1).to_bytes(4, "big") + box[16:] + hvc_entries
        return struct.pack(">I4s", len(body) + 8, b"stsd") + body

    changed = map_mp4(avc.read_bytes(), b"stsd", extend_stsd)

    def select_description(box: bytes) -> bytes:
        body = bytearray(box[8:])
        count = int.from_bytes(body[4:8], "big")
        if count < 1:
            raise ValueError("stsc has no entries")
        body[16:20] = description_index.to_bytes(4, "big")
        return struct.pack(">I4s", len(body) + 8, b"stsc") + body

    result = map_mp4(changed, b"stsc", select_description)
    stsd = first_box(result, b"stsd")
    entry_types = [box[0] for box in mp4_boxes(stsd, 16, len(stsd))]
    if entry_types != [b"avc1", b"hvc1"]:
        raise ValueError(f"unexpected mixed stsd entries: {entry_types!r}")
    stsc = first_box(result, b"stsc")
    if int.from_bytes(stsc[24:28], "big") != description_index:
        raise ValueError("stsc did not select the requested description")
    output.write_bytes(result)


def main() -> None:
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)
    k1 = sub.add_parser("k1")
    k1.add_argument("source", type=Path)
    k1.add_argument("output", type=Path)
    m2 = sub.add_parser("m2-cslg")
    m2.add_argument("source", type=Path)
    m2.add_argument("output", type=Path)
    m2.add_argument("shift", type=int)
    m4 = sub.add_parser("m4")
    m4.add_argument("source", type=Path)
    m4.add_argument("output", type=Path)
    m6 = sub.add_parser("m6")
    m6.add_argument("avc", type=Path)
    m6.add_argument("hevc", type=Path)
    m6.add_argument("output", type=Path)
    m6.add_argument("description_index", type=int, choices=(1, 2))
    args = parser.parse_args()
    if args.command == "k1":
        make_k1(args.source, args.output)
    elif args.command == "m2-cslg":
        make_m2_cslg(args.source, args.output, args.shift)
    elif args.command == "m4":
        make_m4_conflict(args.source, args.output)
    else:
        make_m6(args.avc, args.hevc, args.output, args.description_index)


if __name__ == "__main__":
    main()
