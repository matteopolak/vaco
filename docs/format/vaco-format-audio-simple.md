# `vaco-format-audio-simple`

Layer 4. `wav`, `w64`, `aiff`, `caf`, `au`, `voc`, `sox`, `ircam`, `rso` —
demux and mux, all nine in one crate (FM-27).

Per `planning/18-formats.md` §3.4.6, one crate carrying nine demuxers and
nine muxers is the right factoring for this family. In practice the shared
structure is narrower than that framing suggests: WAV and W64 are genuinely
thin over `vaco-format-riff`, but AIFF/CAF/AU/VOC/SOX/IRCAM/RSO each parse
their own header from scratch. What is genuinely shared, in the `pcm`
module, is the second half every one of the nine reduces to once its header
is parsed: a single stream of raw interleaved audio, framed into packets and
timestamped from a running sample count.

---

## What it is

| Module | Format | Demux | Mux |
|---|---|---|---|
| `wav` | WAV / RF64 | thin over `vaco-format-riff` | plain `WAVEFORMATEX` PCM |
| `w64` | Sony Wave64 | thin over `vaco-format-riff` (128-bit GUIDs instead of FourCCs) | plain PCM |
| `aiff` | AIFF / AIFF-C | full chunk walk, extended80 sample rate | big-endian integer PCM |
| `caf` | Apple CAF | full chunk walk, native 64-bit signed sizes | PCM, `lpcm` only |
| `au` | Sun/NeXT `.au` | fixed 24-byte header | signed/float/A-law/µ-law PCM |
| `voc` | Creative Voice | block-chain walk (not one contiguous span) | one type-9 block, 16-bit PCM |
| `sox` | SoX native | fixed header, always 32-bit samples | 32-bit signed PCM |
| `ircam` | BICSF/IRCAM | fixed 1024-byte header | 16-bit signed PCM |
| `rso` | Lego Mindstorms RSO | 8-byte header, no public spec (black-box probed) | mono `pcm_u8`/`pcm_s16le`/`pcm_s24le`/`pcm_s32le`/`pcm_f32le`/`pcm_f64le`/A-law/mu-law, verbatim |
| `pcm` | shared | `RawPcmDemuxer`, `sample_fmt_for`, `params` — the data-pointer half every format above reduces to | |
| `extended80` | shared | AIFF's 80-bit IEEE-754 extended-precision sample rate | |

Written from: Microsoft/IBM RIFF/WAVE (via `vaco-format-riff`) plus EBU Tech
3306 for `RF64`; Sony's Wave64 specification; Apple's *Audio Interchange
File Format* v1.3 and AIFF-C; Apple's *Core Audio Format Specification 1.0*;
Sun's `.au`/`audio(4)` documentation; the historical Sound Blaster VOC file
format notes; the SoX project's own native-format documentation
(sox.sourceforge.net); the CARL/IRCAM `sfheader` documentation for BICSF.
RSO has no independent public specification and is documented from black-box
observation of `ffmpeg`/`ffprobe` 8.1 only (D6/D7 — recording a shipped
binary's observed behaviour is not copying its expression).

No FFmpeg source was consulted (D7/D15). Every field-layout claim below that
says "measured" came from running `ffmpeg`/`ffprobe` 8.1 against files this
crate's author built with it; the exact commands are in each module's own
doc comments so they can be re-run when the pinned reference version moves
(plan 13 §1b).

---

## How it works

### The shared half: `pcm::RawPcmDemuxer`

Once a format module has parsed its header down to `(sample_rate, channels,
bytes_per_frame, data_start, declared_len)`, `RawPcmDemuxer` does the rest:
clamps `declared_len` against what the source actually holds (`None` means
"read to EOF", the same convention every one of these formats uses somewhere
for a streaming write), reads `TARGET_PACKET_BYTES`-ish blocks rounded down
to a whole number of frames, and stamps each packet's `pts`/`dts` from a
running frame count. Seeking converts a timestamp or byte target into a
frame-aligned byte offset and seeks the source directly — audio PCM has no
keyframe distinction, so `SeekFlags` has nothing else to say.

VOC does **not** use `RawPcmDemuxer`: its audio is not one contiguous byte
span (see below), so it has its own small state machine that shares only
`pcm::params`/`pcm::new_stream` with the rest.

### `pcm::sample_fmt_for`: the "24-bit gap" that turned out not to exist

`vaco-sampfmt::SampleFmt` has no 24-bit variant. Before measuring, this
looked like a real problem for AIFF/AU/CAF's 24-bit PCM. **Measured**
against `ffmpeg`/`ffprobe` 8.1 (`ffmpeg -c:a pcm_s24be -f aiff`, then
`ffprobe -show_streams`): the reference's own *working* sample format for
24-bit container PCM is `sample_fmt=s32`, with `bits_per_raw_sample=24`
stating the real depth separately — not a packed 24-bit type. The same
pattern holds for every bit depth:

```text
              bits_per_coded_sample   sample_fmt   bits_per_raw_sample
int                8                     u8              N/A
int               16                     s16             N/A
int               24                     s32             24
int               32                     s32             N/A
int               64                     s64             N/A
float             32                     flt             N/A
float             64                     dbl             N/A
```

One genuine surprise inside that table: AU's encoding `2` is `codec_name=
pcm_s8` (a *signed* 8-bit codec) but `sample_fmt=u8` — the working format is
unsigned even though the container's codec name says signed. Reproduced
exactly rather than assumed; see `au`'s module docs for the probe.

### Per-format header shapes

- **WAV/W64**: a chunk walk (RIFF FourCCs for WAV, 128-bit GUIDs for W64)
  that finds `fmt `/`data`, reusing `vaco_format_riff::wave::WaveFormatEx`,
  `wave_tags`, and (WAV only) `rf64::Ds64` verbatim. W64's outer GUIDs are
  matched as fixed 16-byte constants rather than a decoded
  tag-plus-shared-suffix, because **measured**, the suffix is not the same
  for every GUID: the outer `riff` GUID's suffix differs from the one
  `wave`/`fmt `/`data` share.
- **AIFF**: plain `AIFF` form means big-endian signed integer PCM at
  `COMM`'s `sampleSize`, full stop — **measured**, `ffmpeg` only switches to
  `AIFC` when the codec needs a `compressionType` (`sowt`, `fl32`, `fl64`,
  `raw `, `alaw`, `ulaw`). `COMM`'s length (18 bytes vs. more) is what a
  reader should branch on, not the form type text.
- **CAF**: `desc`'s `mFormatFlags` bit 0 states float vs. integer directly;
  `mBytesPerPacket` already states the per-frame byte width for uncompressed
  PCM, so it is used instead of re-deriving it. `data`'s size is CAF's own
  signed 64-bit `-1` "unknown, read to EOF" convention.
- **AU**: fixed 24-byte header; the `encoding` field maps to
  `(sample_fmt, bits_per_coded_sample)` via a table **measured** field by
  field (`au` module docs), including the one value (`27` for A-law) that
  contradicts a plausible-looking guess.
- **VOC**: a chain of type-tagged blocks, not one span. `ffmpeg` itself only
  ever writes one type-9 ("new format") header block followed by 16 KiB
  type-2 continuation blocks; the demuxer also understands the legacy type-1
  form and skips types 3–8 (silence/marker/text/loop/extended) as opaque.
  Type 8 (legacy stereo setup) is **not** combined with the type-1 block
  that follows it — a documented, deliberate gap.
- **SOX**: fixed header (`magic, header_size, num_samples, rate, channels,
  comment_size`); `num_samples` counts *individual samples across all
  channels*, not frames, and the sample format is always 32-bit signed —
  SoX's own internal working format never varies with what was encoded.
- **IRCAM**: fixed 1024-byte header regardless of payload size; the
  `encoding` field is two packed 16-bit halves (`mode << 16 | width`) —
  **measured** exhaustively for the eight values `ffmpeg` actually produces,
  not derived from the plausible-looking general scheme.
- **RSO**: an 8-byte big-endian header with two fields whose meaning could
  not be determined by black-box probing (recorded as unknown, not guessed);
  the offset-2 field is a **byte count** of the data that follows, not a
  sample count — measured by encoding exactly 1000 `pcm_s16le` samples (2000
  bytes) and reading `2000` back, not `1000` — and is 16 bits wide, so it is
  useless past 65 535 bytes; this module ignores it for framing and reads to
  EOF instead. `RsoMuxer::add_stream` (`rso::accepts`) writes any of
  `pcm_u8`/`pcm_s16le`/`pcm_s24le`/`pcm_s32le`/`pcm_f32le`/`pcm_f64le`/A-law/
  mu-law verbatim and refuses big-endian PCM and `pcm_s8` — measured directly
  against `-c copy`, not derived from `ffmpeg -h muxer=rso`'s *default* codec
  claim, which names only `pcm_u8` and understates the accepted set. The
  demuxer still always decodes as `pcm_u8` on read, since nothing in the file
  states which of the accepted formats was actually written.
  `probe` never claims content-based confidence — there is no signature, only
  plausible-looking numbers — so RSO is reached by extension or `-f rso`,
  exactly as the reference reaches it.

### Probe scores

Every format's probe score is **measured** against `ffprobe` 8.1's
`format.probe_score` on a plain `ffmpeg -f <fmt>` file with no extension
(the honest test of whether a probe is right, per the brief). Eight of the
nine score `100`; WAV scores `99` (the one exception — no explanation found
beyond the reference's own table, reproduced rather than rationalised);
IRCAM scores `75` (`ProbeScore::CONTENT`); RSO scores `0` (no signature
exists to score on).

---

## How to change it

- **A new format-specific chunk/metadata surface** (WAV `LIST/INFO`, AIFF
  `MARK`/`INST`, CAF `info`/`chan`, …): add a branch to that format's chunk
  walk that decodes it into `Stream::metadata`/`chapters` instead of
  skipping it. None of these are read today — see "What is deferred" below.
- **A new PCM sample width or encoding value for a format that already has a
  table** (`au::encoding_to_format`, `ircam::decode_encoding`,
  `aiff::compression_to_format`): add a match arm backed by a probe command
  in a doc comment and a unit/round-trip test, never from memory alone (plan
  13 §1b — the AU A-law value in this crate is the concrete example of why).
- **A `Limits` injection point for `open`**: none of the nine `Xxx::open`
  functions takes a `vaco_limits::Limits` override the way
  `vaco-demux-mpegts::MpegTsDemuxer::open_with_limits` does; each opens
  under a fixed internal `Limits::permissive()`. Adding one means changing
  each format's `open` signature and its `open_demuxer` registration
  wrapper together.
- **VOC's multi-block mux**: `VocMuxer` only ever writes one type-9 block
  (refusing input over the 3-byte length field's ~16 MiB cap) rather than
  chunking into 16 KiB continuation blocks the way `ffmpeg` does. The
  demuxer already handles the chunked form on read; extending the muxer to
  match is additive.

## Configuration

None as CLI-visible options. Every `Xxx::open` takes `&FormatOptions` for
interface-shape consistency with the rest of `vaco-format-core`, but no
field of it is read yet — see the `Limits` gap above for the one place a
caller-supplied knob is genuinely missing.

## Dependencies

`vaco-core`, `vaco-io` (`IoContext`/`IoWriter`, the byte-order-aware
readers/writers every format module is built on), `vaco-bitstream`
(`ByteReader`, used for the small fixed-size sub-structures: `COMM`, `desc`,
VOC block headers), `vaco-limits` (`Budget`), `vaco-packet`, `vaco-sampfmt`,
`vaco-chlayout`, `vaco-format-core` (the `Demuxer`/`Muxer` traits and
`ProbeData`/`ProbeScore`), `vaco-format-riff` (WAV/W64's `fmt ` payload and
codec-tag tables, reused verbatim — see that crate's own docs), and
`vaco-codec-core` (`CodecId`, the identity layer beneath `vaco-format-core`,
permitted under D14.1).
