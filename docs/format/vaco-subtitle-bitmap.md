# `vaco-subtitle-bitmap`

Layer 4. Bitmap subtitle demuxers and muxers (issue #611, FM-52): `dvbsub`,
`dvbtxt`, `sup` (Blu-ray PGS), `vobsub` (DVD subpicture).

---

## What it is

`ffmpeg -demuxers | grep -iE 'sub|sup|vob|dvb|pgs'` (8.1) names four demuxers
in this family beyond the text-subtitle ones `vaco-subtitle-text` already
covers — matching FM-52's list exactly:

| | Demux | Mux | `CodecId` | Extensions registered |
|---|---|---|---|---|
| `dvbsub` | yes | no (reference has none) | `DvbSubtitle` | none (reference names none either) |
| `dvbtxt` | yes | no | `DvbTeletext` | none |
| `sup` | yes | yes | `HdmvPgsSubtitle` | `sup` |
| `vobsub` | yes | no | `DvdSubtitle` | `idx` |

`ffmpeg -muxers` over the same filter names exactly **one** muxer here:
`sup`. `dvd`/`vob` also match that grep, but they are MPEG-PS *container*
muxers — the transport `vobsub`'s codec rides in, not a `vobsub`-format
muxer — and out of scope. All four `CodecId` variants already existed in
`vaco-codec-core` before this crate; there was no gap to report, unlike
`vaco-subtitle-text`'s family.

## The demuxer/decoder line — one decision per format, not one for the crate

`crates/format/` recovers packets and their timing; `crates/codec/` (a later
wave) decodes a packet into pixels. Where exactly that line falls is
genuinely different per format, and each is a separate design call:

- **`dvbsub`/`dvbtxt` are, standalone, blind raw chunk readers, and that is
  measured, not assumed.** `ffmpeg -h demuxer=dvbsub` and `-h
  demuxer=dvbtxt` both print `"generic raw demuxer AVOptions: -raw_packet_size"`
  — the exact same option every other headerless elementary-stream demuxer in
  the reference has (`h264`, `aac`, …). In a real broadcast, DVB subtitles
  and teletext arrive inside MPEG-TS PES packets, which already carry framing
  and timing — a different crate's scope entirely
  (`crates/format/vaco-demux-mpegts`). As a **standalone** elementary stream
  with no PES wrapper, there is genuinely nothing for a demuxer to frame on
  beyond "here are the bytes", so `DvbSubDemuxer`/`DvbTxtDemuxer` do exactly
  that: fixed 1024-byte packets (`RAW_PACKET_SIZE`, the reference's own
  default), `FormatFlags::NOTIMESTAMPS`.

  The real EN 300 743/EN 300 706 structure — `dvbsub::segments`,
  `dvbtxt::teletext` — is implemented anyway, and is real, tested, fuzzed
  code. It is used by each format's `probe()` (which is *allowed* to be
  stricter than the reference's own generic-raw probe — detection and
  demuxing ask different questions, see `planning/AGENT-CONSTRAINTS.md`) and
  is exposed for a future decoder. It is deliberately **not** used for
  packetisation, because doing that would deviate from the measured
  reference behaviour for no benefit this crate could verify.

- **`sup` (PGS) has its own real container-level framing, no fallback.**
  There is no "raw" fallback in the reference for `sup`; segment structure
  (`"PG"` magic, 90 kHz PTS/DTS, type, size) is genuinely how the format
  frames itself, so `PgsDemuxer` reads segments directly. One packet per
  **segment**, not per display set — waiting for an `END` segment before
  emitting anything would stall forever on a stream truncated mid-composition.
  A packet's payload is the segment's bytes verbatim (header included), so
  the `sup` **muxer** is pure concatenation and the demux→mux round trip is
  byte-identical (see `sup::tests::mux_writes_packet_payloads_verbatim`).

- **`vobsub` splits down the middle of its own two files.** The `.idx` is
  plain text — parsing it (`vobsub::idx`) is entirely this crate's job, no
  different from parsing an `.ini` file. The `.sub` payload is MPEG-PS
  `private_stream_1` (DVD subpicture sub-ids `0x20..=0x3F` —
  `vaco_demux_mpegps::substream::classify`), which `vaco-demux-mpegps`
  already demuxes correctly; `VobSubDemuxer::open_pair` uses
  `vaco_demux_mpegps::MpegPsDemuxer` rather than re-deriving PES framing.

## A real, structural registry-seam gap (`vobsub`)

`vaco_format_core::DemuxerDesc::open` is frozen as `fn(Box<dyn MediaSource>,
&dyn ParserProvider) -> Result<Box<dyn Demuxer>>` — one source, no filename,
no options (the same gap `vaco-demux-raw`'s docs describe for its own
option-driven formats). `vobsub` needs **two** files: the `.idx` this
signature is handed, and a sibling `.sub` it has no path, protocol handle, or
options struct to reach from inside `open()`.

So there are two entry points:

- **`DEMUXER`** (`open_demuxer`, what the registry calls): parses the `.idx`
  in full — every track's correct timestamps, the canvas `Rect`, the
  `Palette` — but every packet's payload is **empty**, because the
  compressed subpicture bytes live in a file this entry point cannot open.
  Reported, not worked around, per `planning/AGENT-CONSTRAINTS.md`'s "Scope".
- **`VobSubDemuxer::open_pair(idx_src, sub_src)`** is the real thing, for an
  embedder or a future CLI layer that has both paths open already: it
  correlates each track's `.idx` timestamps with the real (still-compressed)
  packets `MpegPsDemuxer` recovers from the `.sub`, matched ordinally per
  sub-id. `vobsub::tests::open_pair_correlates_idx_timing_with_real_sub_payloads`
  exercises this end to end against a hand-built minimal MPEG-2 program
  stream.

## How it works, format by format

- **`dvbsub.rs`** / **`dvbsub/segments.rs`** — `SYNC_BYTE = 0x0F`;
  `SegmentHeader { kind, page_id, length }`; `iter_segments` walks
  sync-byte-prefixed, length-delimited records, stopping cleanly (not
  erroring) at the first non-conforming position — a truncated tail or PES
  `0xFF` stuffing. `parse_region_composition` and `parse_clut` read only the
  fixed uncompressed header fields (region size; CLUT `Y Cr Cb T` entries),
  never an object-data segment's run-length payload.
- **`dvbtxt.rs`** / **`dvbtxt/teletext.rs`** — `RECORD_LEN = 46`,
  `DATA_UNIT_LENGTH = 0x2C`; `count_valid_records` scans fixed-width records
  for `probe()`. No decoding of the Hamming-coded magazine/row address or
  page content (EN 300 706 §8) — decoder work.
- **`sup.rs`** — `HEADER_LEN = 13`; `SegmentType::{Pds,Ods,Pcs,Wds,End}`;
  `iter_segments` yields `(header, whole_record)` pairs. `PgsDemuxer` reads
  incrementally from `IoContext`, with a bounded resync scan
  (`MAX_RESYNC_BYTES`) if a `"PG"` magic is not where expected.
- **`vobsub.rs`** / **`vobsub/idx.rs`** — `idx::parse` is total (never
  fails; a bad line is skipped). `parse_timestamp`/`format_timestamp` are the
  `HH:MM:SS:mmm` grammar's parse/print pair — proptested for the exact
  round-trip property `planning/AGENT-CONSTRAINTS.md` names as "one" example
  of where this matters. `VobSubDemuxer` eagerly merges every track's entries
  into one globally time-ordered sequence (same shape
  `vaco-subtitle-text::engine::CueDemuxer` uses) and serves/seeks against it.

## Provenance note on `sup`

There is no freely published PGS specification. The segment framing is the
structure independently documented by numerous public, non-`FFmpeg`
write-ups of the format; `ffmpeg`'s own `sup` demuxer/muxer round-trip a
`-c:s copy`d `.sup` unmodified, consistent with this being exactly what is on
disk. `~/repos/FFmpeg` was not opened for this (D7). **`ffmpeg` has no PGS
encoder** (`ffmpeg -h encoder=hdmv_pgs_subtitle` does not exist), so unlike
`dvbsub`'s reference-encodable samples, this crate's `sup` test fixtures are
hand-built from the documented byte layout rather than extracted from a
reference-encoded file — flagged here rather than left implicit.

## Configuration

None. `RAW_PACKET_SIZE` (`dvbsub`/`dvbtxt`, `1024`) mirrors the reference's
`-raw_packet_size` default but is not itself exposed as an option — the same
"registry seam has no options parameter" gap `vaco-demux-raw` reports.

## Dependencies

`vaco-format-core`, `vaco-codec-core`, `vaco-io`, `vaco-limits`,
`vaco-packet`, `vaco-format-subtitle-bitmap` (the shared model), and
`vaco-demux-mpegps` (for `vobsub`'s `.sub` half). The last is a same-layer
format-crate dependency, not a layering violation: D14.1 forbids a format
crate depending on a *codec* crate, not on another format crate —
`vaco-demux-avi` depending on `vaco-format-riff` is the existing precedent
for exactly this shape.
