# `vaco-demux-raw`

Layer 4. Raw / headerless elementary-stream demuxers: 50 registrations across
five families (PCM, raw video, bitstream-with-sync-pattern, and two
syncframe-driven streaming demuxers: `ac3`/`eac3` and `aac`). `s337m` moved to
`vaco-format-spdif::S337M_DEMUXER`; the `S337M`/`DEMUXER_S337M` consts stay in
`bitstream.rs`, unregistered. Companion crate: `vaco-mux-raw` (the write
side, FM-26b).

This page's family table and counts had drifted (it still said 47/48 and
omitted `ac3`/`eac3` after those landed); both are corrected here, alongside
adding `aac`. If you find another stale number in this file, fix it in the
same commit rather than treating it as this file's steady state — it is
reference material, not a diary, per `CLAUDE.md`.

---

## What it is

A raw format has no container: the file *is* the elementary stream. `-f h264
in.h264`, `-f s16le in.pcm`, `-f rawvideo -s 1920x1080 -pix_fmt yuv420p
in.yuv`. Geometry, sample rate and frame rate come from options, never from
the file — except `yuv4mpegpipe`, which carries its own text header.

| Module | Family | Count |
|---|---|---|
| `pcm` | Linear PCM: `alaw` … `vidc` | 21 |
| `rawvideo` | `rawvideo`, `bitpacked`, `v210`, `v210x` | 4 |
| `y4m` | `yuv4mpegpipe` (self-describing) | 1 |
| `bitstream` | `h264`, `hevc`, `av1`/`obu`, and 17 more (`s337m` unregistered — see above) | 21 |
| `ac3` | `ac3`, `eac3`: own streaming demuxer, syncframe length read from the header | 2 |
| `aac` | `aac`: bare ADTS, same shape as `ac3` but parsed through `vaco-parse-aac::adts::AdtsHeader` rather than a second copy of its tables | 1 |
| `obu` | AV1 OBU leb128 framing, used by `bitstream` | helper |
| `startcode` | Annex-B/MPEG `00 00 01` scanning, used by `bitstream` | helper |

21 + 4 + 1 + 21 + 2 + 1 = 50. The first 48 (PCM/raw-video/`yuv4mpegpipe`/
bitstream, `s337m` still registered) matched FM-26a and `ffmpeg -demuxers`'
count for this family; `s337m` moving to `vaco-format-spdif` dropped it to 47,
`ac3`/`eac3` brought it to 49, and `aac` — added to close a probe-detection
gap, see below — brings it to 50.

**Why `aac` was added.** Before it existed, a bare ADTS `.aac` file had no
demuxer claiming it at all. `cdgraphics`'s probe (`vaco-format-misc::cdg`)
counts 24-byte-aligned chunks whose command byte's low six bits happen to
equal `0x09`; on compressed AAC data that fires by chance roughly one byte in
64, often enough over a multi-kilobyte prefix to clear its own threshold while
every genuinely-registered candidate scored zero — so a real `.aac` file was
reported as `format_name=cdgraphics`. Not a case of `cdgraphics` scoring too
high or ADTS scoring too low: ADTS was never tried. `aac.rs` closes the gap
the same way `ac3.rs` does for its formats — see that module's own doc comment
for the full measurement (probe score convention, the `time_base=1/28224000`
constant measured off `ffprobe`, and the one disclosed timestamp divergence).

**Measurement method.** All names, long names, extensions and default option
values in this crate were captured directly, not transcribed from a plan or
recalled from memory:

```sh
LC_ALL=C ffmpeg -hide_banner -demuxers
LC_ALL=C ffmpeg -hide_banner -h demuxer=<name>   # per format, for extensions and options
```

against the pinned reference, ffmpeg 8.1 (Homebrew build on macOS, confirmed
`ffmpeg version 8.1`). Packet framing, timestamp, and duration behaviour were
then measured on real encoded files (`ffmpeg -f lavfi -i testsrc=... -f
<name> t.<ext>` piped through `ffprobe -show_streams -show_packets`) —
not derived from the option tables alone. Every module doc comment in this
crate states exactly which numbers were measured this way.

---

## How it works

### PCM (`pcm.rs`) — one engine, 21 registrations

Measured on `s16le`: every packet is a fixed **1024-byte** read
(`PCM_PACKET_SIZE`), timestamped by the running sample count
(`bytes_consumed / (container_bytes * channels)`), `time_base = 1 /
sample_rate`, every packet flagged `KEY`. A 100000-byte file at 8000 Hz mono
produced 97 full 1024-byte packets and one 672-byte trailer — `PcmDemuxer`
reproduces exactly that split.

Only six of the 21 formats have a declared extension (`al`, `ul`, `sb`, `sw`,
`ub`, `uw`); the other fifteen can only be opened with `-f <name>` — measured
by probing a headerless file with no matching extension and observing the
reference refuse it ("Invalid data found").

### Raw video (`rawvideo.rs`) — `rawvideo`, `bitpacked`, `v210`, `v210x`

`rawvideo`'s frame size is `PixFmt::plane_layout(width, height, align=1).total`
— exact, byte-aligned. `bitpacked` reuses the same engine, which is *not*
verified against the reference for genuinely sub-byte-packed formats (see
"What is measured versus assumed"). `v210`/`v210x` use the publicly documented
SMPTE 292M/424M packing convention (6 pixels → 16 bytes, rows padded to a
48-pixel/128-byte boundary) — a format-dictated formula from a published
standard, not FFmpeg's expression of it (D7), but not independently measured
here.

Every packet: `pts` = frame index, `dts == pts`, `duration = 1` tick,
`time_base = 1/framerate`, flagged `KEY`. Opening with `width == 0 || height
== 0` is a hard error, matching the reference's "Picture size 0x0 is invalid".

### `yuv4mpegpipe` (`y4m.rs`) — the one self-describing member

Parses the `YUV4MPEG2 W… H… F…:… …` header line and one `FRAME\n` marker per
picture. Measured: `probe_score` is **100** for the exact 9-byte magic
(stronger than the "magic, nothing further checked" convention row of 90 —
the whole header line functions as the self-consistency check).
`time_base = 1/F` from the header itself, unlike the bitstream family below.

### Bitstream (`bitstream.rs`) — 22 registrations, one timestamp rule, five framings

Measured on `h264` (`ffmpeg -f lavfi ... -c:v libx264 -f h264 t.h264`):

* `time_base` is **always `1/1_200_000`**, independent of the declared
  `-framerate` — confirmed identically on `mjpeg` (forced with `-f mjpeg`, to
  rule out `jpeg_pipe` hijacking the auto-detected format — see below).
* **Every packet has `pts = N/A` and `dts = N/A`.** Only `duration` is
  populated, and it collapses to plain `1/framerate` seconds regardless of the
  internal tick base — so this crate stamps `duration_from_rate(framerate)`
  directly rather than reproducing the tick arithmetic.
* `data` (and, by inference, every format with no `-framerate` option: `bit`,
  `loas`) has **no duration at all** and is chunked into flat
  1024-byte reads (`raw_packet_size`, default 1024) — measured directly on
  `data`.

Framing dispatches on `Framing`:

| Framing | Formats | How |
|---|---|---|
| `StartCode3` | `h264`\*, `hevc`\*, `vvc`, `m4v`, `mpegvideo`, `cavsvideo`, `avs2`, `avs3`, `vc1`, `evc` | Split at every `00 00 01`, a convention shared by all these public specs (`startcode.rs`) |
| `Obu` | `av1`\*, `obu`\* | AV1 OBU leb128 framing, split into temporal units at `OBU_TEMPORAL_DELIMITER` (`obu.rs`) |
| `Marker` | `mjpeg` (SOI/EOI `FFD8`/`FFD9`), `mjpeg_2000` (SOC/EOC `FF4F`/`FFD9`) | Scan for the start marker, then the next end marker |
| `Dirac` | `dirac` | SMPTE 2042 parse-info header's `next_parse_offset` field |
| `FixedBlock` | `h261`, `h263`, `dnxhd`, `bit`, `data`, `loas` | Flat 1024-byte reads, no structure at all |

\* When the caller's `ParserProvider` supplies a real parser for the codec
(the real registry does, for `h264`/`hevc`/`av1`; this crate's own tests use
`NoParsers` per D14.1 and so exercise only the fallback), `BitstreamDemuxer`
drives it through `vaco_codec_core::ParserDriver` instead of the structural
scan — this gets correct per-access-unit grouping and correct `KEY` flags
(verified separately: `vaco-parse-h264`'s own tests show it groups
SPS+PPS+IDR into one access unit and flags it `KEY`, matching a direct
measurement of the reference's own packet sizes on the same file: 1641, 172,
83, 31 bytes for four consecutive access units).

Every registration in this module **loads the whole remaining input at
`open`** and computes its packet table (or drives the parser to completion)
once, bounded by the caller's `Limits` — see "How to change it" for why.

### Probing

* PCM/raw-video/`FixedBlock` formats have no magic at all: their `probe`
  functions call `ProbeScore::from_extension` and nothing else — the frozen
  `DemuxerDesc` gives every demuxer a mandatory `probe` field, so "no content
  check" has to be spelled out rather than omitted.
* `Obu`/`Marker`/`Dirac` formats score **51** when structural evidence is
  found (an OBU temporal unit, a JPEG/JP2 marker pair, a `BBCD` magic),
  regardless of extension — measured directly on `obu` (scored 51 with a
  `.bin` extension).
* `Obu`'s structural check (`obu::looks_like_obu_stream`) requires a *second*
  OBU to parse immediately after the first, unless the first already
  consumes the whole probe buffer. One syntactically-plausible header byte
  was not rare enough on its own: an `ffmpeg -f mpegts ... out.m2ts` fixture
  (real H.264/AAC content, BD-style M2TS striping) has `0x0e` immediately
  before its first `0x47` transport-stream sync byte, and `0x0e` passes
  every single-header check with a self-consistent 73-byte span — so `av1`
  scored 51 against `mpegts`'s own correctly-earned 50, the same collision
  shape as the `StartCode3`/`avs2` finding above, found by
  `tests/probe_confusion.rs`'s differential sweep against `ffprobe` rather
  than by inspection. The degenerate-but-legitimate case of a stream that is
  *only* one OBU (a bare temporal delimiter and nothing else) is still
  accepted, since it exhausts the buffer.
* `StartCode3` formats also score **51**, but only when the byte(s)
  immediately after the first `00 00 01` — the start-code *identifier* —
  match what that specific format requires. This is the finding-3 fix: the
  first version only checked that a
  start code was present at offset 0 or 1, which every one of the ten
  `StartCode3` members satisfies on any of the other nine's real content, so
  ties broke alphabetically and `avs2` beat `h264` on an actual H.264
  elementary stream. `bitstream.rs`'s `start_code_identifier` has the full
  measured table; the short version: `h264` and `hevc` are verified against
  their NAL-header parameter-set/AUD types, `mpegvideo` and `m4v` against
  their single-byte sequence/visual-object headers, all four generated with
  `ffmpeg -f lavfi -i testsrc -c:v <codec> -f <rawformat>` and read back with
  `xxd`. The other six (`avs2`, `avs3`, `cavsvideo`, `evc`, `vc1`, `vvc`) have
  no encoder in the `ffmpeg` 8.1 build this was measured against, so they make
  no structural claim at all and fall back to extension-only scoring —
  `tests/probe_matrix.rs` asserts exactly that split, plus that no
  `StartCode3` sibling ever outscores a real sample's true owner.
* `yuv4mpegpipe` scores **100** on its full magic.
* `ac3`/`eac3`/`aac` score **51** once four consecutive syncframes chain
  (each frame's declared length lands exactly on the next frame's sync word),
  **24** at two or three, and **0** below that — measured against `ffprobe`
  on all three formats (see each module's own doc comment). This is
  deliberately not `ProbeScore::repeating`'s `min(100, 25 + 8n)` formula the
  crate-level convention table in `vaco-format-core::probe` documents: the
  reference's own score for these three formats caps at 51 regardless of how
  many further frames chain, and reproducing that exact number is what makes
  `probe_score` byte-identical (D5), which matters more here than following
  the general formula.

---

## What is measured versus assumed — the honesty ledger

| Tier | Formats | Status |
|---|---|---|
| Measured end to end (names, extensions, options, timestamps, framing) | 21 PCM formats, `rawvideo`, `yuv4mpegpipe`, `data` | Exercised by unit tests |
| Names/extensions/options measured; framing spec-derived and unit-tested, not cross-checked against a real encoder; **probe identifier measured against a real encoder's output** (finding 3) | `h264`, `hevc` (fallback path), `av1`, `obu`, `mjpeg`, `mpegvideo`, `m4v` | Exercised by unit tests and `tests/probe_matrix.rs` |
| Names/extensions/options measured; framing is a documented simplification; **no encoder in this `ffmpeg` build to measure a probe identifier against**, so probing is extension-only (finding 3) | `vvc`, `cavsvideo`, `avs2`, `avs3`, `vc1`, `evc`, `mjpeg_2000`, `dirac` | Registered, lightly tested, not verified against a real encoder's exact packet count |
| Names/extensions/options measured; no structural framing attempted at all | `h261`, `h263`, `dnxhd`, `bit`, `loas` | Registered, structurally present, framing is flatly wrong for anything but `data`-style dumps |
| Geometry formula from a public standard, unverified against the reference | `bitpacked`, `v210`, `v210x` | Registered, geometry best-effort |

One format, `s337m`, is deliberately left unregistered here: it now
resolves to `vaco-format-spdif::S337M_DEMUXER`, a real SMPTE 337M parser,
rather than this crate's `Framing::FixedBlock` placeholder. Nothing else in this crate was left unregistered.
Per-registration status is also recorded in each module's doc comment,
which is the version to trust if this table and the code ever disagree.

---

## How to change it

* **Add a PCM format**: one row in `pcm::PCM_FORMATS` plus one
  `pcm_reg!(...)` invocation. The two must agree exactly — a test
  (`every_descriptor_matches_its_spec_row`) checks it.
* **Add a bitstream format**: one `spec!(...)` const, one line in
  `BITSTREAM_FORMATS`, one `bitstream_reg!(...)`. Pick a `Framing` — if the
  format has a real start-code convention, use `StartCode3`; otherwise
  `FixedBlock` is the honest default, not a shortcut.
* **Give a fallback-framed format a real parser**: implement it as
  `vaco-parse-<name>` (a new layer-4 codec crate) and wire it into the real
  `ParserProvider`. **Do not** add a dependency on it here — D14.1 forbids a
  `vaco-format-*` crate depending on a codec crate, and `cargo xtask
  layer-check` enforces it. This crate already asks for a parser by
  `CodecId` (`BitstreamSpec::parser_codec`); a new parser crate just needs
  registering, and every format naming that `CodecId` starts using it with no
  change here.
* **Give the registry path real options**: `DemuxerDesc::open` has no options
  parameter (see "Interface gaps" below). If that signature is ever widened,
  `pcm_reg!`/`rawvideo_reg!`/`bitstream_reg!`'s `open` closures are the only
  place using `*Options::default()` — swap in the caller-supplied value there.
* **Gotcha**: `BitstreamDemuxer` and `Yuv4MpegDemuxer`/`RawVideoDemuxer`/
  `PcmDemuxer` are all fixed-buffer or bounded-fixed-size readers; none of
  them stream a genuinely unbounded input efficiently. `BitstreamDemuxer` in
  particular loads the **whole** remaining input at `open` (bounded by
  `Limits`, not by available RAM) so that both the parser-driven path and the
  five structural framings can share one implementation. A future
  streaming rewrite is straightforward (the framing functions all operate on
  a `&[u8]` slice already) but was not worth the complexity for a v0.1
  breadth pass across 22 formats.

---

## Configuration

None of these formats read `FormatOptions` (the 39-option generic table) —
they are governed entirely by their own private option structs
(`PcmOptions`, `RawVideoOptions`, `BitstreamOptions`), each with the
reference's own measured defaults:

| Struct | Fields | Reference defaults |
|---|---|---|
| `PcmOptions` | `sample_rate`, `layout` | 44100 Hz, mono |
| `RawVideoOptions` | `width`, `height`, `pixel_format`, `framerate`, `stride` | 0×0 (must be set), `yuv420p`, 25 fps, no override |
| `BitstreamOptions` | `framerate` | 25 fps (ignored for `bit`/`data`/`loas`) |

Each `*Demuxer::open`/`open_with_limits` inherent constructor takes the
matching options struct explicitly, for a caller that reaches the type
directly. `DemuxerDesc::open` (the registry path) always uses
`*Options::default()` — see "Interface gaps".

---

## Interface gaps (reported, not worked around)

1. **`DemuxerDesc::open` has no options parameter.** Every format in this
   crate is *defined* by options a real user would set with `-sample_rate`,
   `-ch_layout`, `-video_size`, `-pixel_format`, `-framerate`. Opened through
   the registry, every one of them gets the reference's bare defaults. This
   is the same gap `vaco-demux-mpegts`/`vaco-demux-mp4` already carry for the
   generic `FormatOptions`, generalised to formats whose entire behaviour is
   option-driven.
2. **Decoder dispatch requires typed codec identity.** Y4M reports
   `CodecId::Rawvideo`; the four raw-video formats resolve their names through
   `CodecId::from_name`. Known identities do not leak a `raw_codec_name`
   metadata tag. Keep this assignment when changing these constructors:
   metadata alone does not make a registered decoder reachable from the CLI.
   Remaining bitstream rows with no codec identity retain their exact name
   as metadata, not as an invented decoder mapping. `MPEGVIDEO` still uses
   one static `Mpeg2video` identity for the shared MPEG-1/2 elementary format.
3. **`ProbeScore`'s convention table has no entry matching the measured 51**
   for start-code/OBU/marker evidence — the same situation
   `vaco-demux-mpegts` found and reported for its own measured 50. `51` is
   declared inline in `bitstream::probe_for` rather than added to the shared
   table, which is the smaller, reviewable change; promoting it to a named
   constant in `vaco-format-core::probe` is a plausible follow-up once a
   second crate needs the same number.

## What was wrong in the brief

`planning/18-formats.md` §3.4.9's "Bitstream families" list names ~29 codecs
including `aac`, `ac3`, `eac3`, `dts`, `flac`, `mp3`, `spdif`, `g72x`, `amr`,
`gsm` as part of this crate's scope. **They are not.** Measured against
`planning/research/03-libavformat.md` §2.3 ("Audio containers /
codecs-as-container", 91 demuxers) versus §2.8 ("Raw / elementary-stream
demuxers", **48** demuxers, matching FM-26a's count exactly): those ten
formats are self-delimiting compressed-audio containers filed under §2.3, not
under the headerless §2.8 family this crate implements. The 48-name inventory
in §2.8 is the one this crate reproduces exactly; §3.4.9's prose predates
that correction and double-counts a different crate's scope. This report
follows the measured 48, not the brief's list.

Separately, `planning/CONFORMANCE-FINDINGS.md` finding 3 (twice) and the
finding-3 brief itself say **eleven** `StartCode3` formats share the
tie-breaking bug. Counted directly from `bitstream.rs`: `h264`, `hevc`,
`vvc`, `m4v`, `mpegvideo`, `cavsvideo`, `avs2`, `avs3`, `vc1`, `evc` — **ten**.
The alphabetical tie-break `avs2` winning over `h264` still checks out either
way (`avs2` sorts first among all ten), so the miscount did not change which
bug was found, just its size.

---

## Dependencies

* `vaco-core`, `vaco-io`, `vaco-limits`, `vaco-packet` — the standard layer
  0–2 primitives every demuxer in the workspace uses.
* `vaco-sampfmt`, `vaco-chlayout`, `vaco-pixfmt` — sample format, channel
  layout and pixel format metadata (layer 1 model crates).
* `vaco-format-core` (layer 3b) — `Demuxer`, `DemuxerDesc`, `ProbeData`/
  `ProbeScore`, `ParserProvider`.
* `vaco-codec-core` (layer 3a) — `CodecId`, `CodecParameters`, the `Parser`
  trait and `ParserDriver`. **No `vaco-parse-*` or `vaco-codec-<name>`
  dependency** (D14.1); parsers arrive only through the injected
  `ParserProvider`.
