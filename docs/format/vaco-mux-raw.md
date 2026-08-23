# `vaco-mux-raw`

Layer 4. Raw / headerless elementary-stream muxers: 40 registrations. Sibling
crate to `vaco-demux-raw` (FM-26a, the read side); this crate is FM-26b.

---

## What it is

Muxing a raw format is close to trivial. 39 of the 40 registrations write
**nothing but the packet payload, back to back** — no header, no trailer, no
framing at all. The reference's own `rawenc.c` is exactly `write_packet:
avio_write(pb, pkt->data, pkt->size)`, confirmed by inspecting a real encode
(`ffmpeg -f lavfi ... -f h264 t.h264` starts with the encoder's own Annex-B
bytes, nothing prepended). This is the whole reason FM-26b's effort budget
(2.5 person-weeks) is a fraction of FM-26a's (3.5): there is barely anything
to implement.

| Module | Registrations |
|---|---|
| `raw` | 21 PCM formats, `rawvideo`, and 17 bitstream formats — 39 verbatim writers, one `RawMuxer` type |
| `y4m` | `yuv4mpegpipe` — the one format with a real header and a per-frame marker |

39 + 1 = 40, matching FM-26b and the muxer half of `ffmpeg -muxers`'
raw/elementary-stream family, captured the same way as the demux side:

```sh
LC_ALL=C ffmpeg -hide_banner -muxers
LC_ALL=C ffmpeg -hide_banner -h muxer=<name>
```

against ffmpeg 8.1.

### This is a *different* 40 than the demuxer's 48

Measured directly, not assumed symmetric:

* `mpegvideo`, `mjpeg_2000`, `bitpacked`, `v210`, `v210x`, `s337m`, `loas`
  have **no muxer at all** in the reference — `ffmpeg -h muxer=mpegvideo`
  etc. name no such muxer.
* `mpeg1video`/`mpeg2video` **do** exist as muxers, but the reference files
  them under its legacy/misc muxer set (`planning/research/03-libavformat.md`
  §3.9), not its raw-elementary-stream one (§3.7, the 40-muxer table this
  crate reproduces exactly). They are out of this crate's scope by the same
  measurement that put `aac`/`ac3`/`mp3`/… out of `vaco-demux-raw`'s scope —
  see that crate's docs.

---

## How it works

### `RawMuxer` (`raw.rs`) — one type, 39 registrations

```rust
fn write_packet(&mut self, packet: &Packet) -> Result<()> {
    self.out.write(packet.payload())
}
```

`write_header` and `write_trailer` do nothing observable. `add_stream`
accepts exactly one call — a headerless dump has nowhere to multiplex a
second stream — and rejects a second with `Error::Unsupported`.

**Long names, extensions and default codecs are the muxer's own**, captured
separately from the demuxer's fragment because several genuinely differ:

| Name | Demux long_name | Mux long_name |
|---|---|---|
| `avs3` | `raw AVS3-P2/IEEE1857.10` | `AVS3-P2/IEEE1857.10` (no `raw` prefix) |
| `cavsvideo` | `raw Chinese AVS (Audio Video Standard)` | `raw Chinese AVS (Audio Video Standard) video` |
| `evc` | `EVC Annex B` | `raw EVC video` |
| `vc1` | `raw VC-1` | `raw VC-1 video` |

Extensions diverge too: `h264` demuxes `h26l`/`264`/`avc` besides `h264` but
only ever muxes `.h264`/`.264`; `cavsvideo` demuxes `.avs` but muxes `.cavs`;
`dirac` (`.drc`,`.vc2`), `dnxhd` (`.dnxhd`,`.dnxhr`) and `h263` (`.h263`) have
mux extensions and no demux ones at all; `rawvideo` demuxes
`yuv,cif,qcif,rgb` but muxes only `yuv,rgb`. Each is transcribed from its own
`ffmpeg -h muxer=<name>` capture, not derived from the demuxer's table.

### `Yuv4MpegMuxer` (`y4m.rs`) — the one exception

Writes `YUV4MPEG2 W<w> H<h> F<n>:<d> Ip A0:0 C<tag>\n` once, at
`write_header`, using the geometry `add_stream` was given (buffered until
then, since header fields need the whole stream declaration). Every packet
becomes `FRAME\n` + the payload verbatim.

---

## How to change it

* **Add a verbatim registration**: one `raw_reg!(...)` invocation in `raw.rs`
  plus one row in `RAW_MUXERS`.
* **A format needs a real header after all** (discovered a muxer isn't
  actually byte-for-byte payload passthrough): give it its own type the way
  `Yuv4MpegMuxer` does, rather than adding a special case to `RawMuxer` —
  `RawMuxer`'s whole value is that it has no branches.
* **`query_codec`/`check_bitstream`/`init` are all left at their trait
  defaults** (`Supported`, `Keep`, no-op). A raw muxer that should refuse an
  incompatible codec (the reference restricts `h264` mux to H.264-tagged
  packets, for instance) would override `query_codec`; not done here for the
  same reason `default_video`/`default_audio` are mostly `None` — see
  "Interface gaps".

---

## Configuration

None. Every registration writes whatever `add_stream`/`write_packet` are
given; there is no private option surface on the mux side (contrast
`vaco-demux-raw`, where sample rate/geometry/frame rate are option-driven on
the read side because the file itself states nothing).

---

## Interface gaps (reported, not worked around)

Both gaps below are the mux-side half of exactly what `vaco-demux-raw`
reports; recorded independently because the evidence (the `-h muxer=`
capture) is independent too.

1. **`CodecId` has 16 variants** and this crate needs a dozen more
   (`Rawvideo`, per-subtype PCM tags, `Vc1`, `Mpeg4`, `Vvc`, `Evc`, `Avs2`,
   `Avs3`, `Cavs`, `Dirac`, `Dnxhd`, `H261`, `H263`). `MuxerDesc::default_video`/
   `default_audio` is `None` for every registration where the real codec has
   no variant — measured against the reference's own `Default video/audio
   codec:` line for each muxer (e.g. `bit`'s default audio codec is `g729`,
   which has no `CodecId` either).
2. **No mux-side probing surface exists to test against** — muxers are
   selected by name or extension only, so there is nothing analogous to
   `vaco-demux-raw`'s `ProbeScore` gap to report here.

---

## Dependencies

* `vaco-core`, `vaco-io`, `vaco-limits`, `vaco-packet` — standard layer 0–2
  primitives.
* `vaco-pixfmt` — pixel-format-to-Y4M-colorspace-tag mapping only.
* `vaco-format-core` (layer 3b) — `Muxer`, `MuxerDesc`.
* `vaco-codec-core` (layer 3a) — `CodecId`, `CodecParameters`. No
  `vaco-parse-*`/`vaco-codec-<name>` dependency (D14.1): this crate never
  needs a parser, since it never inspects packet contents.
