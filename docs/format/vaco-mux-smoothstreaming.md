# `vaco-mux-smoothstreaming`

Layer 4. FM-57 (shared with `vaco-mux-hds`/`vaco-mux-whip` in
`planning/18-formats.md`; only this muxer is implemented — see
`planning/TECH-DEBT.md` for HDS/WHIP's status). Microsoft Smooth Streaming
(MS-SSTR) muxer.

## What it is

Writes a Smooth Streaming asset: a `Manifest` (XML) plus, per bitrate
(`QualityLevel`) and per track, a sequence of `Fragments(TYPE=STARTTIME)`
files (ISO-BMFF `moof`+`mdat`) and matching `FragmentInfo(TYPE=STARTTIME)`
files (the same `moof` bytes, no `mdat`). Supports the two codecs `ffmpeg`'s
own `smoothstreaming` muxer supports and this project has encoders for:
H.264 video, AAC audio.

## How it works

No demuxer for this format exists anywhere — in this project or in
`ffmpeg` itself — so every fact below comes from measuring real
`ffmpeg -f smoothstreaming` output directly (two reference trees, 3s and
12s, one and three fragments per track respectively; see
`provenance/sources.toml`'s `ffmpeg-smoothstreaming-mux-probe` entry), not
from a round-trip through this project's own reader.

- **Timescale** is fixed at 10,000,000 ticks/second ("HNS") for every
  track, independent of the track's own sample rate or frame rate.
- **Fragmentation is per-track and independent** (`SmoothStreamingMuxer::
  write_packet`): video flushes at the next keyframe once the accumulated
  duration since the last flush reaches `min_frag_duration_us` (default 5s,
  matching `ffmpeg -h muxer=smoothstreaming`); audio, which has no keyframe
  concept, flushes as soon as the threshold is met, including the sample
  that crossed it. The final fragment of each track is force-flushed at
  `write_trailer` regardless of the threshold.
- **`moof`/`mdat` construction** (`fragment::build_fragment`) reuses
  `vaco-format-isom::writer`'s existing fragment box writers
  (`mfhd`/`tfhd`/`trun`/`traf`/`moof`) rather than re-encoding ISO-BMFF
  boxes from scratch. Measured `trun` flag sets differ by track: video
  carries per-sample flags and a composition-time offset (`0x0000_0f01`);
  audio needs neither, every AAC frame being a sync sample in decode order
  (`0x0000_0301`). The MS-specific `tfxd` `uuid` extension box (UUID
  `6d1d9b05-42d5-44e6-80e2-141daff757b2`, version 1, 64-bit
  `fragment_absolute_time`+`fragment_duration` in HNS ticks) is hand-built —
  no ISO base-spec box carries a fragment's own absolute time.
- **`CodecPrivateData`**: for H.264, Annex-B SPS/PPS unpacked from the
  stream's `avcC` extradata by `avcc::avcc_to_annexb` (a small, local,
  self-contained parser — see that module's docs for why this crate does
  not depend on `vaco-parse-h264` for it, per D14.1); for AAC, the raw
  `AudioSpecificConfig` extradata bytes, hex-encoded directly with no
  unpacking.
- **`Manifest` XML** (`manifest::build_manifest`) reproduces the
  reference's own `<c n="N" d="…">`-carries-only-duration convention,
  deliberately not inventing a `t` (start time) attribute the reference
  never writes — see that module's docs for the resulting, measured
  self-inconsistency between the reference's own `Manifest` (whose `Url`
  template implies a client sums `d` from `t=0`) and its fragment
  filenames (named by the track's true encoder-timeline absolute time).

### The registered entry point cannot really mux Smooth Streaming

The same gap `vaco-mux-dash`/`vaco-mux-hls` document: `MuxerDesc::open` has
one sink, no filename, no protocol write access. `MUXER`'s registered
`open_muxer` writes that one sink once, as the `Manifest`, and fails every
`write_packet`. `SmoothStreamingMuxer::new` is the real entry point, taking
the manifest's own URL plus a `vaco_format_adaptive::WriteAccess`.

## What is deferred

- **`tfrf`** (the look-ahead `uuid` box naming *future* fragments, UUID
  `d4807ef2-ca39-4695-8e54-26cb9e46a79f`): not written. It is a
  live-streaming latency optimisation with no VOD correctness role, and the
  reference's own encoding of it requires a seek-back rewrite of
  already-written files once later fragments are known — disproportionate
  complexity for what it buys a VOD asset. See `planning/TECH-DEBT.md`.
- **Playback through a real Smooth Streaming / Silverlight client**: not
  verifiable on this machine. This crate's bar is structural/self-
  consistency verification against the two measured reference trees.
- **Creating each `QualityLevels(<bitrate>)/` directory**: not done by this
  muxer. `vaco_protocol_core::Protocol` has no directory-creation verb
  (`planning/INTERFACE-GAPS.md` gap 27). A caller driving this muxer against
  a local `file:` output must pre-create every `QualityLevels(<bitrate>)/`
  directory before the first flush for that bitrate — see
  `tests/roundtrip.rs`.

## Configuration

`SmoothStreamingMuxOptions` — currently `min_frag_duration_us` (default
`5_000_000`, matching `ffmpeg -h muxer=smoothstreaming`'s own
`-min_frag_duration` default; HDS's default is 10s, measured separately).

## Dependencies

`vaco-format-adaptive` (`WriteAccess`, relative-URL `resolve`),
`vaco-format-isom` (`writer`/`build` box construction only, never its
demux/parsing surface), `vaco-format-core`, `vaco-io`, `vaco-codec-core`,
`vaco-packet`, `vaco-core`, `vaco-limits`, `vaco-protocol-core`. No
dependency on any `vaco-parse-*` crate (D14.1). Dev-only: `vaco-chlayout`,
`vaco-protocol-file`, `tempfile`.
