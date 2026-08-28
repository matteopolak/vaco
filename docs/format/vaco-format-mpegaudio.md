# `vaco-format-mpegaudio`

Layer 4. MPEG-1/2/2.5 Layer I/II/III frame headers, plus the Xing/Info,
VBRI and LAME extension tags carried in a stream's first frame.

This is **not** a demuxer or a codec. `vaco-demux-mpegaudio` and
`vaco-mux-mpegaudio` use it to find frame boundaries, report duration and
build a VBR header; `vaco-codec-mpegaudio` uses the same `MpegAudioHeader` to
configure decode. One definition of the bit-rate/sample-rate tables and the
Xing/LAME byte layout, shared by all three, rather than three copies drifting
apart.

---

## What it is

| Module | Contents |
|---|---|
| `header` | the 4-byte frame header: version, layer, bit rate/sample rate tables, frame length, side-info length |
| `xing` | the `Xing`/`Info` VBR header: frame count, byte count, seek TOC, quality, and the LAME extension when present |
| `lame` | the LAME/Lavc extension: encoder id string and the encoder delay/padding fields gapless playback needs |
| `vbri` | the Fraunhofer VBRI header (a fixed frame offset, unlike Xing) |

Written from ISO/IEC 11172-3 (MPEG-1 audio) and ISO/IEC 13818-3 (the
MPEG-2/2.5 low-sample-rate extension) for the frame header and bit rate
tables. The Xing/Info/LAME byte layout has no ISO standard behind it at all —
it is a de facto format defined by what encoders write — so `xing.rs` and
`lame.rs` are measured against a real `ffmpeg -c:a libmp3lame` file rather
than transcribed from a document; every offset in `lame.rs` was checked
against that file's actual bytes before being written down, and the module
doc says so.

`vbri.rs` is the one exception: no VBRI-writing encoder was available to
generate a fixture (Fraunhofer's own encoders are effectively extinct), so it
is transcribed from the format's public documentation instead. See
`TECH-DEBT.md` for what a future agent should check if a real VBRI file
turns up.

---

## How it works

### The frame header validates, it does not just sniff

`MpegAudioHeader::parse` rejects a reserved version, reserved layer, the
forbidden (`1111`) bit-rate value, and a reserved sample-rate field — a
syntactically legal header, not merely eleven matching sync bits. This
matters most for probing (see `vaco-demux-mpegaudio`), where the sync
pattern alone is common enough in non-audio data that stricter parsing is
the whole defense.

### Free-format frames have no stated length

When the bit-rate field is `0000` the frame's byte length is not
recoverable from the header at all — [`MpegAudioHeader::frame_len`] returns
`None`, and a caller has to measure the distance to the next sync itself.

### The Xing/LAME byte layout is positional, not self-describing

The LAME extension has no magic bytes of its own; it starts wherever the
Xing/Info header's four optional fields (frames, bytes, TOC, quality) end,
and is only assumed present when **all four** flags are set — the
configuration every LAME-family encoder actually writes. A partial flag set
is some other encoder's plain Xing/Info tag with nothing after it.

---

## Fidelity: what was measured, and what was not

| Field | How confirmed |
|---|---|
| Frame length formula (all three layers, MPEG-1 and the low-sample-rate extension) | `header.rs` unit tests reproduce known real-world frame sizes (417 bytes at 128 kbps/44100 Hz Layer III), and independently against two real `ffmpeg`-encoded files during development |
| Xing tag position, flag bits, field order | Measured against a real VBR file's hex dump |
| LAME extension field order, encoder-delay/padding bit packing | Measured against the same file; an earlier version of the delay/padding byte offset was wrong by 3 bytes and a synthetic-only test could not catch it — see the commit history for `lame.rs` |
| VBRI header layout | **Not measured** — transcribed from public documentation only |
| Xing TOC seek-table contents | Parsed but not independently verified against a known-correct table |

## How to change it

- Add a field to the LAME extension: extend `LameTag` and `lame::parse`;
  the offsets are all relative to the extension's own start, listed in the
  module doc.
- A new sample rate or bit-rate table entry cannot happen — the nine sample
  rates and per-layer bit-rate tables are exhaustive per the two ISO
  documents.

## Configuration

None — this crate has no options of its own; it is pure parsing.

## Dependencies

`vaco-core`, `vaco-bitstream`, `vaco-limits`. No `vaco-format-id3` (ID3
handling lives in the demuxer, not here) and no `vaco-codec-*` crate
(D14.1) — the decoder depends on this crate, not the other way round.
