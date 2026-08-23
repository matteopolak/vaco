# `vaco-format-adaptive`

Layer 4. FM-43–FM-46's shared library: what HLS and DASH genuinely have in
common, and nothing else.

## What it is

Not a container crate — it registers nothing and has no `DemuxerDesc`/
`MuxerDesc`. It is the library `vaco-demux-hls`, `vaco-mux-hls`,
`vaco-demux-dash` and `vaco-mux-dash` all depend on for the five things that
turned out to be genuinely shared once the playlist/manifest syntax was
stripped away:

1. **Owned protocol access** (`access::RemoteAccess`, `write_access::WriteAccess`) —
   opening a segment, a sub-manifest, or an output file by URL, kept alive
   across many calls under one whitelist. Originally written inside
   `vaco-demux-hls`/`vaco-mux-hls`; moved here once `vaco-demux-dash`/
   `vaco-mux-dash` needed the identical shape.
2. **The segment-timeline model** (`timeline`) — DASH's `SegmentTimeline`
   `S@t/@d/@r` run-length encoding, expanded into a plain list of
   `(start, duration)` pairs. HLS has no run-length encoding to expand (each
   `EXTINF` is already one segment), so `vaco-demux-hls` builds the same
   `SegmentTiming` shape directly rather than calling this module — the value
   here is the *type* both crates converge on, not a shared parser.
3. **Variant/representation selection** (`variant`) — HLS's
   `EXT-X-STREAM-INF` and DASH's `Representation` are the same idea (a
   bitrate/resolution/codec tuple); `select_variant` is the bandwidth-capped
   selection rule both use.
4. **The byte-range segment reader** (`byterange`) — HLS's
   `EXT-X-BYTERANGE` and DASH's `indexRange`/`SegmentBase` both address a
   sub-range of one file. `BoundedSource` wraps an opened `MediaSource` down
   to `[offset, offset+length)`, position zero, so a nested demuxer sees an
   ordinary small file.
5. **The nested-demuxer/muxer seam** (`provider`) — `SegmentDemuxerProvider`/
   `SegmentMuxerProvider`, structured exactly like
   `vaco_format_core::ParserProvider`.
6. **Relative URL resolution and wall-clock parsing** (`url`, `walltime`) —
   every segment/sub-manifest reference is commonly relative to the manifest
   that named it, and every "what time is this" field
   (`EXT-X-PROGRAM-DATE-TIME`, `availabilityStartTime`, `publishTime`) is
   ISO 8601.

What is **not** here: any `EXT-X-` tag spelling, any MPD element name. Those
have no syntax in common and live in the four concrete crates.

## How it works

### `timeline::expand`

Turns `TimelineEntry { t, d, r }` runs into `SegmentTiming { start,
duration }`. The part worth reading before touching it: `r == -1` (or any
negative `r`, read leniently) means "repeat until the next entry's `@t`, or
until `period_end`" — a count not stated in the element itself. A trailing
open `-1` with no `period_end` (a live manifest) produces **zero** further
segments rather than a guess. Bounded by `MAX_SEGMENTS` (2^20) and the
caller's `vaco_limits::Budget` fuel, because `<S t="0" d="1"
r="18446744073709551615"/>` is under 40 bytes of XML and states 2^64
segments — this is the DoS the brief calls out by name.

`compact` is `expand`'s approximate inverse (collapses a `SegmentTiming` run
back into `@r`-repeated entries), used by the round-trip proptest and by a
DASH muxer wanting to write a compact `SegmentTimeline` instead of one `<S>`
per segment.

### `provider`: why this crate cannot depend on `vaco-registry`

The brief that commissioned this crate said to "reach [MPEG-TS/fMP4
demuxers] through `vaco-registry`, not by depending on them directly." Taken
literally that is impossible: `vaco-registry` has an **optional path
dependency on every registered component crate**, including
`vaco-demux-hls` and friends once they register themselves (deliverable 5).
If `vaco-demux-hls` also depended on `vaco-registry`, the graph would have a
cycle — `vaco-registry → vaco-demux-hls → vaco-registry` — the moment both
sides exist, which Cargo refuses regardless of feature-gating.

The actual mechanism, and the one this crate provides, mirrors
`ParserProvider` exactly: `SegmentDemuxerProvider`/`SegmentMuxerProvider` are
traits defined here (below `vaco-registry` in the layer graph), and a
concrete registry-backed implementation (`vaco_registry::demuxer_by_name`
wired to the two hints) has to live in `vaco-registry` itself, which already
depends downward on every concrete format crate and has no cycle problem.
**That implementation does not exist yet** — see the top-level report from
the wave that added this crate. `NoSegmentDemuxers`/`NoSegmentMuxers` are the
safe defaults every unit test uses in the meantime, and each of the four
concrete crates uses a **dev-dependency** on the real MPEG-TS demuxer/muxer
to test end-to-end behaviour without shipping that dependency.

### `walltime`: a clock-free ISO 8601

`parse_iso8601_datetime`/`format_iso8601_datetime` implement Howard
Hinnant's `days_from_civil`/`civil_from_days` by hand (no date/time crate is
declared in `[workspace.dependencies]`). Parsing a timestamp *string* is pure
arithmetic; only `WallClock::now()` touches an actual clock, and it goes
through `vaco_time::unix_nanos()` — never `std::time::SystemTime::now()`,
which panics on `wasm32-unknown-unknown`. `cargo xtask time-gate` checks
this crate for exactly that mistake.

## How to change it

- A change to `timeline::expand`'s `@r=-1` handling is the highest-risk edit
  in the crate: it is checked by a proptest (`compact_of_expand_reproduces_the_same_segments`)
  and by an explicit "bounded by the next entry's `@t`" unit test — keep both
  green.
- `url::resolve` is deliberately not RFC 3986: it implements the four cases
  real playlists/manifests use (absolute, protocol-relative, absolute-path,
  relative-with-`.`/`..`). Do not "upgrade" it to a general URL parser without
  checking `split_url` in `vaco-protocol-core` first — the two have
  different jobs (resolving an address vs. dispatching a scheme) and should
  probably stay separate.
- Adding a third adaptive-streaming format (low-latency HLS parts, CMAF
  chunks) is the reason to grow this crate further; until then, resist
  putting anything HLS- or DASH-specific here.

## Configuration

None of its own. `timeline::expand` takes a `vaco_limits::Budget` from its
caller.

## Dependencies

`vaco-core`, `vaco-time`, `vaco-limits`, `vaco-io`, `vaco-opts`,
`vaco-protocol-core` (rule W2: never a concrete protocol crate — for
`access`/`write_access`), `vaco-format-core`, `vaco-codec-core`.
Deliberately not `vaco-registry` (see above). Dev-only: `vaco-protocol-file`
(local-file fixtures for `access`/`write_access`'s own tests).
