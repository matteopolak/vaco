# `vaco-format-gxf`

Layer 4. FM-55a (epic FM-55, shared with `vaco-format-imf` for ownership
reasons only — the two share nothing technically). GXF (General eXchange
Format, SMPTE 360-2009, formerly SMPTE 360M) demuxer and muxer. Registered
as `gxf`.

Written from the published SMPTE 360-2009 standard — a document SMPTE
itself distributes free of charge as a "Stable"/"Archived" engineering
document at `https://pub.smpte.org/pub/st360/st0360-2009_stable2016.pdf`
(confirmed genuinely public, not a leak or an ffmpeg-source mirror) —
clean-room (D7/D15), cross-checked against real files `ffmpeg 8.1` both
reads and writes on this machine (D6/D17): `ffmpeg -demuxers`/`-muxers`
both list `gxf`, confirmed rather than assumed (unlike this project's IMF
work, which had no reference at all).

---

## What it is

| Module | Contents |
|---|---|
| `packet` | The 16-byte packet header every GXF packet shares (leader/type/length/trailer), and the `MAP`/`MEDIA`/`FLT`/`UMF`/`EOS` type constants |
| `map` | The `MAP` packet: material data section + per-track description section, both a run of `tag(1) len(1) value(len)` items; `parse`/`encode` are exact inverses of each other |
| `media` | The 16-byte media packet preamble: media type, track number, media/timeline field numbers, media-type-dependent field info (MPEG picture coding/structure decoded; everything else raw) |
| `demux` | `GxfDemuxer`: reads `MAP` once, skips `FLT`/`UMF`, turns `MEDIA` packets into `Packet`s |
| `mux` | `GxfMuxer`: buffers packets, writes a full simple-clip stream (`MAP`+minimal `UMF`+`MEDIA`+`EOS`) in `write_trailer` |

---

## How it works

### One shared field-number timeline

Every track — video, audio, time code — is addressed on a single virtual
timeline counted in video *fields* (clause 4.6/4.26/7.4.2.1.3), not
per-track ticks. A 25 fps PAL file's field clock runs at 50 Hz regardless
of which track a packet belongs to; `demux.rs::derive_field_rate` finds
this once (from whichever track states a recognised Table 6 frame-rate
code) and gives every `Stream` the same `time_base` — the reciprocal of
twice the frame rate, since GXF field numbers advance by 2 per frame even
for progressive storage (confirmed against the real fixture: ten 25 fps
frames land at field numbers `0, 2, 4, ..., 18`).

### The `MAP` packet is the source of truth; `FLT`/`UMF` are read only well enough to skip

`GxfDemuxer::open` requires the stream's first packet to be a `MAP`
(clause 5.1's own requirement) and parses it fully: material data (media
file name, first/last/mark-in/mark-out fields, estimated size) and one
`TrackDescription` per track (media type, media file name, and — for MPEG
tracks — the `Ipg`/`Ppi`/`Bpiop`/`Cf`/`Cg`/... auxiliary parameter string).
`read_packet` skips any `FLT`/`UMF`/repeated-`MAP`/reserved packet type it
meets — clause 7.3's own text is why this is not a shortcut: "MAP packets
shall have priority" over the UMF for any value that could differ, and a
simple clip's `MAP` packet already states everything this crate surfaces.

### Audio packets are a fixed size, and GXF's own muxer over-reports validity

Every audio `MEDIA` packet carries exactly 32,768 sample words (clause
7.4.2.3), however the input arrived. `GxfMuxer` buffers a stream's whole
PCM byte range and chunks it into 32,768-sample packets in
`write_trailer`, using `Annex B`'s own relationship (inverted) to compute
each packet's field number:
`ceil(packet_index * 32768 * field_rate.num / (48000 * field_rate.den))` —
checked against the real fixture's own three audio packets, which land at
field numbers 0, 35 and 69 for a 50 fields/sec file.

A genuinely short trailing chunk is zero-padded to the full 32,768
samples. The naive, spec-literal choice would be to state the honest
shorter valid-sample count in the preamble's `field_info` (clause
7.4.2.1.4 allows it) — measured directly against real `ffmpeg` and found
wrong for interop: `ffmpeg -f gxf` always declares `field_info` as `00 00
80 00` (32,768, i.e. *fully valid*) even for its own genuinely-partial
final packet, and real `ffmpeg`'s own `gxf` demuxer *trims the reported
packet* to whatever a shorter declared count says. Stating the honest
count therefore made a real reference reader visibly diverge from the
reference file's own shape; `GxfMuxer` matches the measured convention
(always claim full validity) instead. See `planning/TECH-DEBT.md` for the
full before/after account.

### `GxfMuxer` buffers, then writes once

`write_header` is a no-op; every packet is buffered in memory and the
whole stream (`MAP`, a minimal `UMF`, every `MEDIA` packet, `EOS`) is
written inside `write_trailer` — the same trade-off
`vaco-mux-mxf::MUXER_OPATOM` makes for clip-wrapped essence, for the
analogous reason: the `MAP` packet's own `EstimatedSizeOfStream`/
`LastFieldOfMaterial` values need the whole clip's size known up front,
and this crate has not built the "placeholder `MAP`, rewrite via
`MediaSink::seek` at the end" streaming version (see "How to change it").

---

## What is measured vs. spec-derived, and the manual `ffmpeg` cross-check

`tests/fixtures/ffmpeg_pal_mpeg2_pcm.gxf` is a real file `ffmpeg -f gxf`
wrote on this machine (`testsrc`/`sine` inputs, `mpeg2video`/`pcm_s16le`,
720x576 @ 25 fps, 2.00s). Every numeric tag `map.rs`/`media.rs` decode was
cross-checked against this file's own bytes before being trusted (see
those modules' own test suites) — not inferred from the Standard's prose
alone.

`GxfMuxer`'s own output was cross-checked manually against real `ffmpeg`
during development (not as an automated test — this workspace's tests
must be reproducible without an external binary installed, the same
posture every other muxer's test suite in this workspace already takes):
`ffprobe` accepted the container structure produced by `GxfMuxer`
end-to-end (correct packet count, sizes, `pts`/`dts` sequence, `MAP`/`UMF`
presence) once the audio `field_info` convention above was matched to what
real `ffmpeg` actually does.

## Scope limits, stated rather than silently absent

- **`GxfMuxer` writes at most one `Mpeg2video` video track and one
  `PcmS16le` audio track.** `add_stream` returns `Error::Unsupported` for
  anything else — a second track of either kind, Motion JPEG, DV, AC-3,
  24-bit PCM, or time code (all of which `GxfDemuxer` already *reads*) —
  and for an MPEG frame rate that is not exactly one of Table 6's eight
  defined values.
- **`GxfMuxer`'s `UMF` packet is minimal, not a full restatement.** It
  declares zero tracks and zero segments — legitimately empty rather than
  wrong, per clause 7.3's own "MAP packets shall have priority" — rather
  than encoding Table 15/16's full per-track/per-media description
  sections a byte-for-byte-complete UMF would need.
- **Video width/height are not stated anywhere in GXF's own metadata**
  (checked directly against the Standard: neither Table 6's track tags
  nor Table 16's UMF media description carry pixel dimensions, only a
  lines-per-frame *code*). `GxfDemuxer` reports the conventional ITU-R
  BT.601 SD width (720) and the 525/625-implied height, and leaves HD
  width/height at `0` rather than guess a common resolution. The real
  value lives only in the elementary stream's own sequence header;
  wiring the D14.1 `ParserProvider` seam (already threaded through `open`
  like every other demuxer, but not yet called) to read one is the fix —
  see `vaco-demux-raw::bitstream`'s `drive_parser` for the pattern.
- **`GxfDemuxer::seek` is not implemented.** The `FLT` packet is this
  format's own named seek aid; no fixture measured this session exercises
  anything past sequential reading.
- **Compound clips are read, not re-timed.** A `MEDIA` packet's own
  `effective_field_number` is used directly as `pts`, correct for a
  simple clip; a compound clip's cut transitions are represented exactly
  as the stream states them.

## How to change it

- **A streaming (rather than buffer-then-write) muxer** needs a
  placeholder `MAP`/`UMF` written first, `MediaSink::seek` back to rewrite
  them once the final field count is known — the pattern
  `vaco-mux-mxf`'s OP1a variant uses for its own header rewrite, not yet
  ported here.
- **A fuller `UMF`** needs Table 15 (one track description per track,
  `Xx`-form track info) and Table 16 (one media description per segment,
  including the MPEG/DV/audio-specific 32-byte tail) actually populated
  from the same `TrackDescription`/buffered-frame data the `MAP` packet
  already has.
- **Real video dimensions** need the `ParserProvider` seam `open` already
  receives, driven against the first video packet's own MPEG sequence
  header.

## Configuration

None yet — no CLI-facing option channel exists for this format.

## Dependencies

`vaco-format-core`, `vaco-io`, `vaco-limits`, `vaco-packet`,
`vaco-codec-core`, `vaco-chlayout`, `vaco-core`. Dev-only: `proptest`.
