# `vaco-format-mpjpeg`

## What it is

MPJPEG (MIME multipart JPEG), the "motion JPEG over HTTP" wire format: both
demuxer and muxer, one crate, because the format is thin enough not to need
splitting. Registers as `mpjpeg`, extension `mjpg`. Not a container in the
box/EBML/pack sense — a byte stream of repeated MIME multipart parts, one
JPEG picture per part, produced by e.g. IP cameras and `ffmpeg -f mpjpeg`.

## How it works

### Wire format

Measured against `ffmpeg -f mpjpeg` (8.1):

```text
--ffmpeg\r\n
Content-type: image/jpeg\r\n
Content-length: 2164\r\n
\r\n
<2164 bytes of JPEG>\r\n
--ffmpeg\r\n
...
--ffmpeg\r\n            <- write_trailer: one more boundary, nothing else
```

`--ffmpeg` is the boundary tag (`-boundary_tag`, default `"ffmpeg"`). The
header block is exactly two lines, in that order and that capitalisation —
`Content-type` (not `Content-Type`) then `Content-length` — followed by a
blank line. There is no closing `--boundary--`: this format is for streams
with no defined end, so `write_trailer` just emits one more boundary line.

### Demuxing

`demux::MpjpegDemuxer::read_part` reads a boundary line, then header lines
until a blank line, extracting `Content-length` case-insensitively (matching
HTTP header-name comparison, not the reference's exact casing — a real
producer's casing is not something this crate controls). That length is
attacker-controlled input read straight from the stream, so it goes through
`vaco_limits::Budget` via `Packet::alloc` before anything is allocated for
it, exactly like every other declared length in this workspace's demuxers.
Header/boundary line scanning is bounded by a fixed `MAX_HEADER_LINE` (4096
bytes) on top of `IoContext::peek`'s own `max_probe_bytes` cap, so a stream
that never produces a newline fails fast rather than growing the peek window
to its limit.

Width/height are read from the first JPEG payload's own SOF marker (a plain
byte scan over already-allocated, already-budgeted bytes — not a new
allocation and not a JPEG parser). `pix_fmt`, chroma subsampling and every
other JPEG-internal property are not read: there is no JPEG parser anywhere
in this workspace to reuse, and writing a component-subsampling-to-`PixFmt`
mapper is out of scope for a container work package. Every packet gets an
assumed `time_base=1/25`, `pts` counting up by one per frame and
`duration=40ms` — measured: `ffprobe` reports exactly that
(`r_frame_rate=25/1`, `time_base=1/25`) for an MPJPEG stream regardless of
how it was produced, because the format states no real frame rate anywhere
and this is the reference's own fixed fallback, not a computed value.

`-strict_mime_boundary` (reference default `false`) is modelled as
`MpjpegDemuxer::strict_mime_boundary`: non-strict mode does not check that a
later boundary line repeats the first one's tag. What is **not** modelled is
recovering a part with no `Content-length` header at all — the only samples
this crate can generate always send it, and guessing a JPEG's length by
scanning for an `0xFFD9` (EOI) marker is unsafe in general (entropy-coded
scan data may contain that byte pair legitimately), so a missing
`Content-length` is `Error::InvalidData` rather than a best-effort scan.

### Muxing

`mux::MpjpegMuxer::write_packet` writes exactly the layout above for one
video packet; `write_header` writes nothing; `write_trailer` writes the
final boundary line. `with_boundary_tag` mirrors `-boundary_tag`.

## How to change it

* **A missing `Content-length` fallback**: would need a verified-safe
  JPEG-EOI scan (handling `0xFFD9` appearing inside scan data via marker
  segment tracking) before it could ship; see "Demuxing" for why none exists
  today.
* **`pix_fmt`/chroma reporting**: needs a JPEG SOF component-table reader
  (sampling factors -> 4:2:0/4:2:2/4:4:4), which does not exist in this
  workspace yet. `sof_dimensions` in `demux.rs` is the place a fuller SOF
  reader would replace.
* **A real frame rate**: MPJPEG carries none; `ASSUMED_FRAME_RATE` in
  `demux.rs` is the reference's own fixed fallback (25/1), not a per-file
  fact — do not try to derive one from inter-packet arrival timing, since a
  file replayed from disk (not a live capture) has no meaningful arrival
  timing to derive from.

## Configuration

| Item | Default | Meaning |
|---|---|---|
| `MpjpegDemuxer::strict_mime_boundary` | `false` | Mirrors `-strict_mime_boundary` |
| `MpjpegMuxer::with_boundary_tag` | `"ffmpeg"` | Mirrors `-boundary_tag` |
| `demux::ASSUMED_FRAME_RATE` | `25/1` | Measured reference fallback, see above |
| `demux::MAX_HEADER_LINE` | 4096 bytes | Local bound on top of `max_probe_bytes` |

## Dependencies

`vaco-core`, `vaco-io`, `vaco-limits`, `vaco-packet`, `vaco-format-core`,
`vaco-codec-core` (for `CodecId::Jpeg`, the reference's `mjpeg`). No
`vaco-parse-*` dependency: `open_demuxer` accepts a `&dyn ParserProvider` to
satisfy `DemuxerDesc::open`'s frozen signature and does not call it, because
MPJPEG carries no in-band codec configuration beyond the JPEG bytes
themselves.

## What was and was not measured

Verified directly against real `ffmpeg 8.1 -f mpjpeg` output (2026-08-27),
embedded as `tests/fixtures/sample.mjpg` (five real 32x32 JPEG frames):

* `Content-type`/`Content-length` header text, capitalisation and order.
* The trailing bare `\r\n` after each payload, and the trailer's single
  extra boundary line with no `--` suffix.
* Width/height via the JPEG SOF scan.
* **Full remux round trip is byte-identical**: demuxing the reference's own
  file and remuxing it with this crate's muxer (default boundary tag,
  matching the fixture) reproduces the input exactly —
  `tests/reference_files.rs::remuxing_a_real_sample_reproduces_it_byte_for_byte`.
  Falsified: temporarily writing `Content-Type` (wrong case) broke that test
  before the fix was restored.

**Not measured, and known to be absent, not merely approximate**:

* `pix_fmt`/chroma subsampling — not read; see "How to change it".
* Behaviour with no `Content-length` header — refused, not scanned around.
* `-strict_mime_boundary=true`'s exact rejection message text (only that it
  rejects a changed tag was checked, not phrased against a specific
  reference error string — MPJPEG has no error-string contract to match in
  the first place, since the reference's own errors are not part of any
  documented output format).
