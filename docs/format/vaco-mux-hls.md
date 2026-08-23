# `vaco-mux-hls`

Layer 4. FM-44. HLS muxer (RFC 8216).

## What it is

Segments the input into MPEG-TS or fMP4 files (or byte ranges of one file),
writes the media playlist after every rotation, and optionally a master
playlist and an fMP4 init segment.

## How it works

A rotation happens when a reference-stream (index 0) keyframe arrives at or
past `hls_time` seconds since the current segment started. The finishing
segment's nested muxer gets `write_trailer`, its entry (`#EXTINF`, byte
range, `#EXT-X-PROGRAM-DATE-TIME`) is appended to the live window, the
window is trimmed to `hls_list_size` (deleting the dropped file when
`hls_flags delete_segments` is set), and the playlist is rewritten from
scratch via a fresh, truncating `WriteAccess::create` — never a
seek-and-overwrite of a handle that might now be shorter than what it
replaces.

`hls_flags single_file` is a different code path: one nested muxer is opened
once for the whole session (`HlsMuxer::single_file_muxer`), and
`counting::CountingSink` — inserted before the sink is handed to that muxer —
is what lets the byte offset stay visible from outside once the muxer owns
it. **The container header (PAT/PMT) is written exactly once, at segment 0,
and belongs to segment 0's byte range**: reading the position *after*
`write_header` instead of before it excluded those bytes from every range
and opened a gap at the front of the file — caught by
`single_file_segments_are_contiguous_non_overlapping_byte_ranges` in
`tests/roundtrip.rs`, and worth remembering if this code is touched again.

### The registered entry point cannot really mux HLS

`MuxerDesc::open`'s frozen signature is one sink, no filename, no protocol
write access — the mux-side mirror of the gap `vaco-demux-hls` documents on
its read side. `MUXER`'s registered `open_muxer` writes that one sink
exactly once, at `write_trailer`, and fails `write_packet` the moment a
segment file would need to be created. `HlsMuxer::new` — the real entry
point — takes the playlist's own URL string and a `WriteAccess` (re-exported
from `vaco-format-adaptive`, moved there once `vaco-mux-dash` needed the
same shape) instead, and creates every file itself.

## How to change it

- The six `hls_flags` this crate implements — `single_file`, `temp_file`,
  `delete_segments`, `append_list`, `program_date_time`,
  `independent_segments` — are exactly the ones the brief named. **Not
  implemented**: `round_durations`, `discont_start`, `omit_endlist`,
  `split_by_time`, `second_level_segment_index`/`_duration`/`_size`,
  `periodic_rekey`, `iframes_only` (ten of the reference's sixteen).
- `-hls_flags append_list` recovers only the continuing
  `#EXT-X-MEDIA-SEQUENCE` and segment-numbering start from an existing
  playlist (`recover_append_state`), by reading it back and counting
  `#EXTINF:` lines — it does **not** re-load the prior segments' entries
  into the live window, so `hls_list_size` trimming starts fresh. A caller
  that needs the old entries to stay listed across a restart has to seed
  them itself.
- Live/`-hls_flags omit_endlist` semantics, `-hls_init_time`,
  `-hls_delete_threshold`, `-hls_key_info_file`/`-hls_enc*` (AES-128
  encryption of *output* segments — the mux-side mirror of
  `vaco-demux-hls`'s "detect, never decrypt" boundary, except here nothing is
  even detected because encryption was never requested), `-strftime`,
  `-var_stream_map`/`-cc_stream_map` (multiple variants from one input) are
  not implemented. `-master_pl_name` writes a **trivial one-variant** master
  playlist; it does not compute a real multi-rendition ladder.
- `filename::expand`'s template grammar is a single `%d`/`%0Nd` conversion,
  not `strftime`. Do not conflate the two — `-strftime` is a separate,
  unimplemented boolean option on the reference.

## Configuration

`HlsMuxOptions` — names, types and defaults measured against `ffmpeg -h
muxer=hls` (ffmpeg 8.1): `hls_time` (default `2`), `hls_list_size` (default
`5`, `0` = unlimited), `hls_segment_filename`, `hls_flags`,
`hls_playlist_type`, `hls_segment_type` (`mpegts` default, `fmp4`),
`master_pl_name`.

## Dependencies

`vaco-format-adaptive`, `vaco-protocol-core` (never a concrete protocol
crate), `vaco-format-core`, `vaco-io`, `vaco-limits`, `vaco-packet`,
`vaco-codec-core`, `vaco-opts`, `bitflags`. Dev-only:
`vaco-demux-hls`/`vaco-demux-mpegts`/`vaco-mux-mpegts` (the round-trip
test) and `vaco-protocol-file` (local-file fixtures).
