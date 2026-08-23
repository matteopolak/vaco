# `vaco-mux-dash`

Layer 4. FM-46. MPEG-DASH muxer.

## What it is

Segments each input stream into its own fMP4 representation, writes the
MPD, and optionally a companion HLS playlist set over the same segment
files.

## How it works

Unlike `vaco-mux-hls`, which multiplexes every stream into one shared
MPEG-TS segment per rotation, DASH puts each elementary stream in its own
`Representation`, so [`DashMuxer`] segments each stream **independently**:
every added stream gets its own `RepresentationState`, its own
keyframe-triggered rotation (at `seg_duration` seconds), and its own
`init-stream<N>.m4s`/`chunk-stream<N>-<number>.m4s` files.
`-adaptation_sets` only changes how representations are *grouped* in the
MPD (`adaptation_sets::parse_adaptation_sets` handles the
`"id=0,streams=0,1 id=1,streams=2"` syntax — note that `streams=`'s own list
is comma-joined *inside* a comma-joined clause, so it cannot be parsed by a
naive whole-clause `split(',')`); it does not change segmentation.

Every representation's actual segment boundaries are recorded in
microseconds (this crate's fixed `@timescale`) and rendered as a compact
`SegmentTimeline` via `vaco_format_adaptive::timeline::compact` — the
mux-side use of the same function `vaco-demux-dash`'s round-trip properties
exercise from the read side.

`-hls_playlist` writes a companion `master.m3u8` plus one
`media_<RepresentationID>.m3u8` per representation, naming the same segment
files DASH just wrote — a second index over one set of bytes, not a second
encode.

### The registered entry point cannot really mux DASH

The same gap `vaco-mux-hls` documents: `MuxerDesc::open` has one sink, no
filename, no protocol write access. `MUXER`'s registered `open_muxer`
writes that one sink once, as the MPD, and fails every `write_packet`.
`DashMuxer::new` is the real entry point.

## What is deferred

- **`-single_file`**: parsed and stored, has no effect. Needs the same
  `CountingSink`-before-the-muxer trick `vaco-mux-hls` uses, wired for N
  independent representations rather than one shared segment stream.
- **`-streaming`**: parsed and stored, has no effect. True per-frame
  fragmentation needs `SegmentMuxerProvider` to expose a
  fragment-per-packet mode, which the trait does not have.
- **`-use_template 0` (`SegmentList` output) is refused outright** at
  `write_header` with a clear error, rather than silently falling back to
  `SegmentTemplate`.

## Configuration

`DashMuxOptions` — names, types and defaults measured against `ffmpeg -h
muxer=dash` (ffmpeg 8.1): `seg_duration` (default `5`), `use_template`
(default `true`), `use_timeline` (default `true`), `adaptation_sets`,
`window_size` (default `0` = unlimited), `hls_playlist` (default `false`),
`hls_master_name` (default `"master.m3u8"`), `single_file`, `streaming`
(both parsed, inert — see above).

## Dependencies

`vaco-format-adaptive`, `vaco-protocol-core` (never a concrete protocol
crate), `vaco-format-core`, `vaco-io`, `vaco-limits`, `vaco-packet`,
`vaco-codec-core`. Dev-only: `vaco-demux-dash`, `vaco-demux-mp4`/
`vaco-mux-mp4`, `vaco-protocol-file`.
