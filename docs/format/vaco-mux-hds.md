# `vaco-mux-hds`

Layer 4. FM-57. Adobe HTTP Dynamic Streaming (HDS) muxer.

## What it is

Writes an HDS asset: an `index.f4m` client manifest (XML) plus, per bitrate
("quality level"), one `stream<N>.abst` bootstrap box and a sequence of
`stream<N>Seg1-Frag<M>` fragment files. A quality level bundles video and
audio together into one interleaved stream — unlike Smooth Streaming, HDS
has no per-elementary-stream file. Supports H.264 video and AAC audio, the
two codecs `ffmpeg`'s own `hds` muxer supports.

## How it works

No demuxer exists for this format anywhere — in this project or in
`ffmpeg` itself — so every fact below is measured against two real
`ffmpeg -f hds` reference trees (one quality level/two fragments, and two
quality levels/one fragment each; `provenance/sources.toml`'s
`ffmpeg-hds-mux-probe` entry), not a round trip.

- **Fragments are not ISOBMFF.** A `stream<N>Seg1-Frag<M>` file is one bare
  `mdat` box (`vaco_format_isom::build::bx`) wrapping a sequence of classic
  FLV tags (`flv.rs`) — audio and video interleaved in arrival order,
  exactly the shape a `.flv` file's own body has. **None of
  `vaco-format-isom::writer`'s ISOBMFF fragment writers
  (`mfhd`/`tfhd`/`trun`/`traf`/`moof`) transfer here** — only its generic,
  format-agnostic `build::{bx, fullbx}` box-header helpers do (reused for
  `mdat` here and for `abst`/`asrt`/`afrt` in `bootstrap.rs`). This is the
  central answer to "how much of the ISOBMFF machinery transfers": less
  than hoped, because HDS's own fragment body predates ISOBMFF fragments
  entirely.
- **Every fragment restates both tracks' sequence headers**: measured
  directly — the second fragment of a two-fragment reference opens with a
  fresh AVC sequence header, then a fresh AAC sequence header, both
  timestamped at the fragment's own start, before any real sample.
  `SmoothStreamingMuxer`'s CodecPrivateData-lives-only-in-the-Manifest
  design does not apply here; `ensure_fragment_started` in `lib.rs`
  reproduces the restatement.
- **Fragmentation** is gated on the video track's keyframes once
  `min_frag_duration_us` (default 10s, measured against `ffmpeg -h
  muxer=hds` — different from Smooth Streaming's 5s) is met, applied to the
  whole interleaved quality-level stream; a video-less quality level
  flushes purely on accumulated duration.
- **`abst`** (`bootstrap.rs`) is the addressing scheme — `asrt`'s single
  segment (this crate never produces a second one; `-window_size`/
  `-extra_window_size` are a live-streaming concern not implemented here)
  and `afrt`'s per-fragment `(firstFragment, firstFragmentTimestamp,
  fragmentDuration)` triples, all in a fixed 1000-tick (millisecond)
  timescale — **not** Smooth Streaming's 10,000,000-tick HNS. Every field
  measured byte-by-byte against the reference; `bootstrap::tests` includes
  a full byte-for-byte match of a real 122-byte reference `abst`.
- **`Manifest`** (`manifest.rs`) matches the reference's `manifest`/
  `bootstrapInfo`/`media` shape, including the `<media>` element's own
  base64-encoded `onMetaData` AMF0 blob (`amf0.rs`) — twelve fixed keys,
  `videodatarate`/`audiodatarate` in **kibibit/s** (`bits_per_second /
  1024.0`, a different unit from the `Manifest`'s own decimal-kbit/s
  `bitrate` attribute), `stereo` always `false` regardless of real channel
  count (a measured FLV/AAC convention).
- **No directory-creation gap here.** Contrast `vaco-mux-smoothstreaming`:
  every file HDS writes sits flat in
  the manifest's own directory — measured directly with a two-quality-level
  reference tree (`stream0.abst`/`stream1.abst`, no per-quality
  subdirectory). Gap 27 does not apply to this crate.

## What is deferred

- **Re-framing Annex-B H.264 / ADTS-framed AAC**: not done. This crate
  requires `VideoParameters::nal_length_size == Some(4)` (already-`avcC`-
  framed samples) and raw, ADTS-free AAC access units — the same
  convention every other MP4-family muxer in this workspace already
  relies on — and refuses anything else with a clear error.
- **Playback through a real Flash/HDS client**: not verifiable on this
  machine. Issue #618's own Acc criterion is only "the manifest and
  fragment set match the reference's structure", which this crate's tests
  check end to end against real `file:` output.
- **A second segment / live sliding window** (`-window_size`,
  `-extra_window_size`): not implemented. Every fragment lands in `asrt`
  segment 1.

## Configuration

`HdsMuxOptions` — currently `min_frag_duration_us` (default `10_000_000`,
matching `ffmpeg -h muxer=hds`'s own default).

## Dependencies

`vaco-format-adaptive` (`WriteAccess`, relative-URL `resolve`),
`vaco-format-isom` (`build::{bx, fullbx}` only — its ISOBMFF fragment
writers do not apply to this format), `vaco-format-core`, `vaco-io`,
`vaco-codec-core`, `vaco-packet`, `vaco-core`, `vaco-limits`,
`vaco-protocol-core`. No dependency on any `vaco-parse-*` crate, and no new
external dependency for base64 (D10) — `base64.rs` hand-rolls RFC 4648
encoding, the same convention `vaco-protocol-http`, `vaco-protocol-local`
and others already use. Dev-only: `vaco-chlayout`, `vaco-protocol-file`,
`tempfile`.
