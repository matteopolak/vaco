# `vaco-format-nut`

## What it is

NUT, ffmpeg/mplayer's own fully-specified container (the frozen 2008-02-02
spec). Demux + mux, one crate, registered as `nut` (extension `nut`, no
MIME type — measured against `ffmpeg -h demuxer=nut`/`-h muxer=nut`).

NUT is the one format in this crate's sibling batch (MPJPEG, S/PDIF, SWF)
where byte-identical output was worth chasing: every field's wire encoding
is written down, not reverse-engineered from a muxer's behaviour. What is
*not* specified is which muxer heuristics a particular encoder chooses —
the exact frame-code table packing, which frames get elision headers, and
`back_ptr` placement are left open, and this crate's own muxer does not
attempt to match ffmpeg's choices there (see `header.rs`/`mux.rs` module
docs for exactly what and why).

## How it works

### Varint coding (`vlc.rs`)

`v` (unsigned) is base-128, most-significant-group-first, continuation bit
`0x80`; `s` (signed) is `v` shifted into a zigzag-like form; `vb` is a `v`
length prefix followed by that many raw bytes, routed through `Budget`
since the length is attacker-controlled. `read_v`/`read_s` work against
either an in-memory `Cursor` (every non-frame packet, whose `forward_ptr`
gives the whole length up front) or a live `IoContext` (`frame` packets,
whose length is not known until the frame header itself has been decoded).

One real bug, found against a real file rather than by inspection: the
crate's own `MAX_VLC_BYTES` cap started at 9, copied from an unrelated
fixed-byte-count reader in the specification's own sample code. A real
`ffmpeg -f nut` file's `match_time_delta` field legitimately needs 10
groups to encode a near-maximal 64-bit value, so the cap is 18 (10 groups
for a full 64-bit value, plus the spec's own stated allowance of up to 8
stuffing bytes per field).

### `main_header`'s frame-code table

`header.rs::read_frame_code_table` implements the specification's
`for(i=0;i<256;)` construction loop exactly, including its automatic
`flags['N'] = FLAG_INVALID` (every table has this, whether a muxer intends
it or not). This crate's own muxer writes a three-batch table (code 0
invalid, code 1 the one generic code every real frame uses, everything else
invalid) rather than the reference's compact multi-hundred-code table —
reproducing that would mean reverse-engineering an unspecified muxer
heuristic, not implementing the format, and the specification only
requires *a* valid table.

### Packet framing (`demux.rs`)

`forward_ptr` spans the packet's payload *and* its trailing 4-byte CRC-32
checksum together — the checksum is the last 4 bytes of that span, not 4
bytes after it (this was a real bug: the first implementation read
`forward_ptr` bytes as payload, then separately skipped 4 more, double
counting and misaligning every packet after the first). `main_header` and
every `stream_header` are decoded fully; `syncpoint` is decoded far enough
to reset each stream's `last_pts` (its `back_ptr` is parsed and discarded —
this demuxer reads sequentially and does not seek); `info`/`index`/unknown
startcodes are skipped by their own `forward_ptr`, per the spec's own
forward-compatibility rule.

A `main_header` this crate measured against a real `ffmpeg -f nut` 8.1
file ends immediately after its elision headers, with nothing left for the
`main_flags` field the specification's own text places right after them —
confirmed two independent ways (the next packet's startcode position via
`forward_ptr`, and `crc32_nut` matching the trailing checksum over exactly
that content). Since a `v`-coded value cannot occupy zero bytes, the
reference encoder evidently omits the field here; this parser follows that
measured behaviour and defaults `main_flags` to 0 when nothing remains,
rather than erroring against the reference binary's own output.

### Timestamps

`coded_pts` reconstructs to a full pts via the spec's lsb/full split
(`mask = (1<<msb_pts_shift)-1`); `dts` comes from a `decode_delay`-sized
reorder buffer, exactly the specification's own `get_dts` sample algorithm;
`convert_ts` between time bases uses `i128` arithmetic rather than the
spec's manual 64-bit split (verified equivalent by round-trip tests) —
that split exists only to dodge needing a wider-than-64-bit type in C.

### CRC-32

NUT's checksum is `vaco_hash::crc32_nut`: poly `0x04C11DB7`, init `0`,
non-reflected, no final XOR — distinct from the ordinary reflected
`CRC-32/ISO-HDLC` `vaco_hash::crc32` already provides, and from
`CRC-32/MPEG-2` (which shares the same polynomial but initialises to
`0xFFFFFFFF`). Added to `vaco-hash` rather than duplicated here, so it
stays the one place this workspace defines a CRC table (see that crate's
own tests for the derivation, verified against a real main-header
checksum).

## How to change it

* **Frame-code table compactness**: if byte-identical muxer output is ever
  required, `mux.rs` would need to replicate ffmpeg's specific table-packing
  heuristic — currently out of scope; see `header.rs`'s module docs.
* **`decode_delay`**: bounded to 256 in `StreamHeader::parse` — a defensive
  cap of this crate's own choosing, not a spec limit (real codecs use 0-2).
  Fuzzing found an unbounded value reaching `demux.rs`'s
  `vec![pts; decode_delay]` reorder-buffer allocation directly, bypassing
  `Budget`, and triggering a 100+ GiB allocation attempt from a 488-byte
  input. If a future codec genuinely needs more than 256, raise the cap
  deliberately rather than removing it.
* **Seeking**: `NutDemuxer::seek` is unimplemented — `back_ptr` is read and
  discarded, and index packets are structurally skipped, not parsed. Both
  would need real work to support backward seeking.
* **Elision headers**: fully supported on read (prepended to a frame ≤4096
  bytes when `header_idx` points at a non-empty entry); `NutMuxer` never
  writes any (`elision_headers = [[]]` always) — a real muxer-side gap
  documented in `mux.rs`, not silently wrong.

## Configuration

No crate-specific options; `NutDemuxer::open`/`open_with_limits` and
`NutMuxer::new` are the whole interface.

| Constant | Value | Meaning |
|---|---|---|
| `vlc::MAX_VLC_BYTES` | 18 | Longest a `v`/`s` encoding may run — see above |
| `demux::MAX_HEADER_PACKET` | 64 MiB | Bound on a non-frame packet's `forward_ptr`, checked before the `Budget`-backed allocation it sizes |
| `demux::MAX_FRAME_SIZE` | 256 MiB | Same bound, for one frame's `data_size` |
| `header::MAX_VLC_BYTES` (via `decode_delay` check) | 256 | Defensive cap — see "How to change it" |
| `mux::NUT_VERSION` | 3 | The only version ffmpeg 8.1 writes |
| `mux::MAX_DISTANCE` | 32768 | The spec's own recommended syncpoint spacing ceiling |

## Dependencies

`vaco-core`, `vaco-io`, `vaco-limits`, `vaco-packet`, `vaco-format-core`,
`vaco-codec-core`, `vaco-chlayout`, `vaco-hash` (`crc32_nut` — this crate's
reason for depending on it; see D11 on CRC ownership).

## What was and was not measured

Verified against `tests/fixtures/sample.nut`, a real `ffmpeg -f lavfi
... -c:v mpeg4 -g 10 -c:a mp3 -bitexact` capture (2026-08-27; `ffprobe`
reports 25 video packets, 43 audio packets — distinguishing input: two
streams, more than one syncpoint's worth of frames each, a real MPEG-4
`codec_specific_data` blob, elision headers this crate's own muxer never
writes but this file does):

* `main_header`'s every field, byte-by-byte, including the frame-code table
  construction algorithm and the CRC-32 variant.
* `stream_header` for both streams (fourcc, dimensions, sample rate).
* `forward_ptr`/checksum framing (the double-counting bug above).
* Every video packet reconstructs a real `0x000001` MPEG start code via
  elision-header prepending (checked directly against the payload bytes).
* pts is non-decreasing and dts never exceeds pts across the whole file.
* A round trip through this crate's own muxer and demuxer (two streams,
  mixed keyframe/non-keyframe, `msb_pts_shift=0`'s degenerate always-full
  `pts+1` encoding).

**Not measured, and known to be absent, not merely approximate**:

* Byte-identical remux against a real ffmpeg-written file — not attempted;
  this crate's muxer writes a structurally valid but differently-packed
  frame-code table (see "How to change it").
* Seeking, `back_ptr` computation, and real index/info packet parsing
  (structurally skipped, not read).
* Elision-header writing (read-only support today).
