#!/usr/bin/env python3
"""Differential comparison of this crate's decoders against the reference.

One loop, one table. For every fixture below: build the exact packet payload
the reference's decoder would receive, run it through both this crate
(`examples/decode_one.rs`) and `ffmpeg -f ass -`, and diff the resulting ASS
dialogue text.

Usage:
    python3 tests/differential.py <path-to-decode_one-binary>

Not a cargo test: it shells out to `ffmpeg`, which is a developer tool and not
a build dependency, so the hermetic checks live in the crate's own unit tests
and this drives the measurement that those tests encode.

`text` and `ttml` have no reference decoder path reachable from a file
(`ffmpeg -demuxers` lists neither a `text` nor a `ttml` demuxer, and
`-decoders` has no `ttml` row at all), so they are reported as hand-built
rather than silently counted as passes.
"""

import pathlib
import re
import subprocess
import sys
import tempfile

FFMPEG = ["ffmpeg", "-hide_banner", "-loglevel", "error", "-bitexact"]

SRT_FIXTURES = {
    "srt-basic": "Hello <i>world</i>\nsecond line",
    "srt-bold-font": '<b>Bold</b> and <font color="#00ff00">green</font>',
    "srt-underline-strike": "<u>u</u> <s>s</s> <b><i>bi</i></b>",
    "srt-font-attrs": '<font size="24" face="Times">sz</font>',
    "srt-passthrough": "{\\an8}already ass {braces}",
    "srt-unclosed": "unclosed <i>italic",
    "srt-unknown-tag": "<unknown>tag</unknown> & amp",
    "srt-entity-not-decoded": "a &amp; b &lt;c&gt;",
    "srt-named-colour": '<font color="red">r</font>',
    "srt-nonascii": "ééé <i>ital</i> end",
}

VTT_FIXTURES = {
    "vtt-basic": "<i>it</i> <b>bo</b> <u>un</u>",
    "vtt-voice": "<v Roger>Hi there",
    "vtt-class": "<c.yellow>classy</c> plain",
    "vtt-entities": "&amp; &lt;esc&gt;",
    "vtt-ruby": "<ruby>base<rt>anno</rt></ruby> plain",
    "vtt-multiline": "line one\nline two",
    "vtt-nbsp": "a&nbsp;b",
    "vtt-numeric-ref": "&#65;&#x42;",
}

ASS_FIXTURES = {
    "ass-tags": "{\\i1}hi{\\i0} there, with, commas",
    "ass-plain": "plain",
}

MOVTEXT_FIXTURES = {
    "movtext-italic": "Hello <i>world</i>\nsecond line",
    "movtext-bold": "<b>Bold</b> and green",
    "movtext-nonascii": "ééé <i>ital</i> end",
    "movtext-astral": "\U0001f600 <i>ital</i> end",
    "movtext-plain": "no styling here",
}


def srt_file(path, cues):
    with open(path, "w", encoding="utf-8") as fh:
        for i, text in enumerate(cues, start=1):
            s, e = i * 10, i * 10 + 5
            fh.write(f"{i}\n00:00:{s:02d},000 --> 00:00:{e:02d},000\n{text}\n\n")


def vtt_file(path, cues):
    with open(path, "w", encoding="utf-8") as fh:
        fh.write("WEBVTT\n\n")
        for i, text in enumerate(cues, start=1):
            s, e = i * 10, i * 10 + 5
            fh.write(f"{i}\n00:00:{s:02d}.000 --> 00:00:{e:02d}.000\n{text}\n\n")


def ass_file(path, cues):
    with open(path, "w", encoding="utf-8") as fh:
        fh.write("[Script Info]\nScriptType: v4.00+\n\n[V4+ Styles]\n")
        fh.write(
            "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, "
            "OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, "
            "ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, "
            "MarginL, MarginR, MarginV, Encoding\n"
        )
        fh.write(
            "Style: Default,Arial,16,&Hffffff,&Hffffff,&H0,&H0,0,0,0,0,100,100,"
            "0,0,1,1,0,2,10,10,10,1\n\n[Events]\n"
        )
        fh.write(
            "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, "
            "Effect, Text\n"
        )
        for i, text in enumerate(cues, start=1):
            s, e = i * 10, i * 10 + 5
            fh.write(
                f"Dialogue: 0,0:00:{s:02d}.00,0:00:{e:02d}.00,Default,,0,0,0,,{text}\n"
            )


def reference_dialogue(path):
    """The Text field of every Dialogue line the reference decoder produces."""
    out = subprocess.run(
        FFMPEG + ["-i", str(path), "-f", "ass", "-"],
        capture_output=True,
        check=True,
    ).stdout.decode("utf-8", "replace")
    texts = []
    for line in out.splitlines():
        if line.startswith("Dialogue:"):
            body = line[len("Dialogue:") :].lstrip()
            texts.append(body.split(",", 9)[9] if body.count(",") >= 9 else "")
    return texts


def mine(binary, codec, payload):
    with tempfile.NamedTemporaryFile(delete=False) as fh:
        fh.write(payload)
        tmp = fh.name
    try:
        out = subprocess.run(
            [binary, codec, tmp], capture_output=True, check=True
        ).stdout
        return out.decode("utf-8", "replace").rstrip("\n")
    finally:
        pathlib.Path(tmp).unlink(missing_ok=True)


def movtext_samples(raw, mp4):
    """Split a concatenated mov_text dump into samples using packet sizes.

    An earlier version walked the trailing box list to find each boundary and
    over-consumed into the next sample, silently concatenating five fixtures
    into one. The packet sizes are authoritative and already known, so ask for
    them instead of re-deriving them.
    """
    out = subprocess.run(
        ["ffprobe", "-hide_banner", "-loglevel", "error", "-bitexact",
         "-show_entries", "packet=size", "-of", "csv=p=0", str(mp4)],
        capture_output=True, check=True,
    ).stdout.decode()
    sizes = [int(x) for x in out.split() if x.strip().isdigit()]
    samples, at = [], 0
    for size in sizes:
        samples.append(raw[at : at + size])
        at += size
    return samples


def run(binary, workdir):
    rows = []

    def compare(group, names, texts, path, codec, payloads):
        ref = reference_dialogue(path)
        for name, payload, expected in zip(names, payloads, ref):
            got = mine(binary, codec, payload)
            rows.append((group, name, expected, got, expected == got))

    # --- SubRip
    names, texts = list(SRT_FIXTURES), list(SRT_FIXTURES.values())
    p = workdir / "f.srt"
    srt_file(p, texts)
    compare("subrip", names, texts, p, "subrip", [t.encode() for t in texts])

    # --- WebVTT
    names, texts = list(VTT_FIXTURES), list(VTT_FIXTURES.values())
    p = workdir / "f.vtt"
    vtt_file(p, texts)
    compare("webvtt", names, texts, p, "webvtt", [t.encode() for t in texts])

    # --- ASS: the reference's demuxer emits a nine-field chunk per packet.
    names, texts = list(ASS_FIXTURES), list(ASS_FIXTURES.values())
    p = workdir / "f.ass"
    ass_file(p, texts)
    chunks = [
        f"{i},0,Default,,0,0,0,,{t}".encode() for i, t in enumerate(texts)
    ]
    compare("ass", names, texts, p, "ass", chunks)

    # --- mov_text: build an MP4, then read back the real samples.
    names, texts = list(MOVTEXT_FIXTURES), list(MOVTEXT_FIXTURES.values())
    src, mp4, dump = workdir / "m.srt", workdir / "m.mp4", workdir / "m.bin"
    srt_file(src, texts)
    subprocess.run(
        FFMPEG + ["-i", str(src), "-c:s", "mov_text", str(mp4), "-y"], check=True
    )
    subprocess.run(
        FFMPEG + ["-i", str(mp4), "-c:s", "copy", "-f", "data", str(dump), "-y"],
        check=True,
    )
    samples = [s for s in movtext_samples(dump.read_bytes(), mp4) if len(s) > 2]
    ref = reference_dialogue(mp4)
    for name, payload, expected in zip(names, samples, ref):
        got = mine(binary, "mov_text", payload)
        rows.append(("mov_text", name, expected, got, expected == got))

    return rows


def main():
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    binary = sys.argv[1]
    with tempfile.TemporaryDirectory() as td:
        rows = run(binary, pathlib.Path(td))

    width = max(len(r[1]) for r in rows) + 2
    print(f"{'fixture'.ljust(width)}{'match':<8}reference | vaco")
    print("-" * (width + 60))
    failures = 0
    for _group, name, expected, got, ok in rows:
        if not ok:
            failures += 1
        mark = "yes" if ok else "NO"
        detail = expected if ok else f"{expected!r} | {got!r}"
        print(f"{name.ljust(width)}{mark:<8}{detail}")
    print("-" * (width + 60))
    print(f"{len(rows)} fixtures, {len(rows) - failures} match, {failures} differ")
    print("text, ttml: no reference decoder reachable — hand-built, see crate docs")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
