# `vaco-demux-hls`

Layer 4. FM-43. HTTP Live Streaming demuxer (RFC 8216).

## What it is

Parses HLS master and media playlists, selects a variant, and reads segments
one at a time through a nested MPEG-TS or fragmented-MP4 demuxer, presenting
the whole thing as one `vaco_format_core::Demuxer`.

## How it works

`HlsDemuxer::open` reads the whole playlist (`vaco_format_adaptive::read_all_bounded`,
capped at `MAX_MANIFEST_BYTES` = 16 MiB) and detects master vs. media by the
presence of `#EXT-X-STREAM-INF`. A master playlist's variants and renditions
come from `master::parse`; `vaco_format_adaptive::select_variant` picks one
(highest bandwidth under `HlsOptions::max_bandwidth`, or highest overall) and
its media playlist is fetched and parsed in turn. `media::parse` builds the
segment list, threading forward whatever RFC 8216 says applies "until
changed": the current `#EXT-X-MAP`, the current `#EXT-X-KEY`, and the byte
offset an omitted `#EXT-X-BYTERANGE` continues from.

Segments are opened one at a time via `segments: &dyn SegmentDemuxerProvider`
— MPEG-TS when the segment carries no `#EXT-X-MAP`, fMP4 when it does. Every
URL a playlist names — the chosen variant, each segment, each `#EXT-X-MAP`
init segment — is opened through `access: RemoteAccess`, re-exported from
`vaco-format-adaptive` (originally written here, moved once
`vaco-demux-dash` needed the identical "keep the capability alive across
many `read_packet` calls" shape). `RemoteAccess::for_remote_manifest`
is the constructor a caller fetching the top-level playlist over the network
must use (rule W3: excludes `file` from the default grant); `unrestricted`
is for a URL the user typed directly and for tests.

### Continuous timestamps across `#EXT-X-DISCONTINUITY`

Each nested demuxer restarts near whatever timestamps its own segment
happens to carry; RFC 8216 promises continuity only *within* one
discontinuity-delimited run. `HlsDemuxer` tracks, per stream index, the last
emitted `dts` and the last observed positive `dts` delta between consecutive
packets (an estimated "frame interval"). On the first packet following a
discontinuity, it computes an offset so the new run continues exactly from
`last_dts + interval` — no repeated or skipped tick at the boundary.

**Why not `Packet::duration`?** The obvious-looking fix is `end = last_dts +
duration`. A raw MPEG-TS packet carries no explicit duration for
`vaco-demux-mpegts` to report back (`Packet::duration` reads zero in
practice), so that produced an exact duplicate timestamp at every
discontinuity boundary — caught by `tests/demux.rs`'s
`discontinuity_produces_a_continuous_timeline_not_a_backwards_jump`, which is
why the interval is estimated from observed deltas instead.

`#EXTINF` decimal seconds are parsed as exact base-10 rationals. Playlist
duration and timestamp seeking add and compare those values directly, so a
seven-decimal segment such as `0.0000001` is not rounded away through a
microsecond intermediate.

**Known gap**: if a discontinuity genuinely changes the stream count or
order (a real encoding-profile change, which is exactly what
`#EXT-X-DISCONTINUITY` is *for*), this crate keeps the stream list from the
first segment and maps by index — a later segment's differently-ordered or
additional streams are either mismatched or dropped. Not exercised by the
test suite; recorded here rather than silently accepted.

### `#EXT-X-KEY`: detected, never decrypted

`key::KeyInfo` parses `METHOD`/`URI`/`IV`/`KEYFORMAT` fully and surfaces them
(`KeyInfo::metadata_entries`). `open_next_segment` fails the read the moment
a keyed segment is reached, with `KeyInfo::unsupported_error()` naming the
method (`AES-128` vs. `SAMPLE-AES`/`SAMPLE-AES-CTR`) — never a generic parse
failure. Neither AES-128-CBC nor Apple's SAMPLE-AES scheme is implemented.

### Live playlists: parsed, not polled

`MediaPlaylist::is_live()` (no `#EXT-X-ENDLIST`) is reported correctly, but
this crate does **not** reload. `read_packet` reaches `Eof` once the
segments a playlist named at open time are exhausted. `-live_start_index`,
`-max_reload`, `-m3u8_hold_counters` are not implemented. Live behaviour is
inherently wall-clock-driven and explicitly outside this project's
byte-exact corpus (plan 13 §1b); this is a breadth-phase gap, not an
oversight.

### A real interface gap: `DemuxerDesc::open` cannot carry a base URL

The frozen `open: fn(Box<dyn MediaSource>, &dyn ParserProvider) -> ...`
signature has no filename and no protocol registry — both of which HLS
genuinely needs (to resolve relative segment URIs, and to fetch a chosen
variant or any segment beyond the bytes already in hand). `DEMUXER`'s
registered `open_demuxer` degrades gracefully: it parses whatever playlist
text it was handed with `access: None`, which works for a self-contained
media playlist and fails informatively (`Error::Unsupported`) the moment a
master playlist needs a variant fetched, or any segment needs reading. It
also cannot forward the caller's `&dyn ParserProvider` (borrowed for one
call; `HlsDemuxer` needs an owned provider across many later calls), so it
uses `NoParsers` instead — a demuxer opened this way never gets
bitstream-parsed `profile`/`pix_fmt`. `HlsDemuxer::open` (the richer,
intended entry point for a real caller) has neither limitation.

## How to change it

- Add a tag by extending `master::parse`/`media::parse`; both already
  tolerate unrecognised tags (RFC 8216 §4.1), so a gap is silent unless a
  test catches it.
- The attribute-list grammar (`NAME=VALUE`, quoted values may contain
  commas) is centralised in `attrs.rs`; every tag with parameters should use
  it rather than a bespoke `split(',')`.
- `RemoteAccess` is the one place that talks to `vaco-protocol-core`; keep
  it that way (rule W2 — no `vaco-demux-*` crate may depend on a concrete
  protocol crate).

## Configuration

`HlsOptions { max_bandwidth: Option<u64> }`. Fuller CLI option parity
(`-live_start_index`, `-allowed_extensions`, `-seg_max_retry`) is deferred —
see "Live playlists" above.

## Dependencies

`vaco-format-adaptive` (timeline/byte-range/provider/URL/wallclock model),
`vaco-protocol-core` (never a concrete protocol crate), `vaco-format-core`,
`vaco-io`, `vaco-limits`, `vaco-packet`, `vaco-codec-core`, `vaco-opts`
(`Dict` for a protocol `open` call). Dev-only:
`vaco-demux-mpegts`/`vaco-mux-mpegts` (the integration test's real segment
provider) and `vaco-protocol-file` (local-file fixtures) — never a
production dependency.
