# vaco

A reimplementation of ffmpeg's command-line tools in Rust, written from published
specifications rather than from FFmpeg's source.

`vaco` transcodes, `vaco-probe` inspects. They use the same option names as `ffmpeg`
and `ffprobe`, print the same shapes of output, and are tested by comparing their
bytes against the real thing. Many of those options are not implemented yet — see
[Option coverage](#option-coverage) — and the ones that aren't now fail with a
named error rather than being accepted and ignored.

This is not a drop-in replacement, and it is not close to one yet. About a third of
the container formats are implemented, video decoding runs several times slower than
FFmpeg, and a number of codecs decode only intra frames. The tables below say exactly
what works. If you need to get a job done today, use FFmpeg.

## Why this exists

Three constraints shape the whole tree, and they are the reason it looks the way it
does:

**No `unsafe`.** The workspace sets `unsafe_code = "forbid"`, so every decoder, every
bit reader, and every filter kernel is safe Rust. There is no inline assembly and
there are no unsafe intrinsics. SIMD comes from portable abstractions instead. This
costs real performance — see [Performance](#performance) — and it is the trade the
project is making on purpose.

**Clean-room.** Nothing here is derived from FFmpeg, libav, VLC, GStreamer, mpv, x264
or x265. The implementations come from published standards, academic papers, and
permissively-licensed reference code. The `ffmpeg` binary is used as a black box to
generate test fixtures and to compare output against, which is a different thing from
reading its source. `docs/provenance.md` records what each component was written from.

**Patents are gated, not ignored.** Codecs covered by active patent pools — H.264,
H.265, AAC, VC-1 — sit behind non-default Cargo features. A default build does not
include them. See [Patent-encumbered codecs](#patent-encumbered-codecs).

The correctness bar is byte-exactness. Where a decoder is finished, its output is
compared against FFmpeg's byte for byte on real files, not eyeballed.

## Binaries

| vaco | FFmpeg equivalent | What it does |
|---|---|---|
| `vaco` | `ffmpeg` | Transcode, remux, filter |
| `vaco-probe` | `ffprobe` | Inspect streams, packets, frames |
| — | `ffplay` | No equivalent. Playback is out of scope for now. |

## Building

Requires the pinned nightly toolchain in `rust-toolchain.toml`, which `rustup` picks
up automatically.

```
git clone https://github.com/matteopolak/vaco
cd vaco
cargo build --release
```

The binaries land in `target/release/`. There are no published packages yet.

For a build that includes the patent-encumbered decoders:

```
cargo build --release -p vaco-cli --features vaco-registry/patent-encumbered-h264-decode,vaco-registry/patent-encumbered-hevc-decode,vaco-registry/patent-encumbered-aac-decode
```

## Examples

```
# Remux without re-encoding
vaco -i input.mkv -c copy output.mp4

# Scale and crop
vaco -i input.mp4 -vf "scale=1280:720,crop=1280:600:0:60" output.mp4

# Inspect streams as JSON
vaco-probe -v error -show_streams -of json input.mkv

# List what this build actually registered
vaco -codecs
vaco -filters
vaco -formats
```

A default build has no H.264 or HEVC decoder, so an example reading a typical `.mp4`
needs the patent-encumbered feature line above. `vaco -codecs` tells you what the
build you have can actually open.

## Compared to FFmpeg

Component counts, against `ffmpeg` 9.0.1 on the same machine. Both columns are counted
the same way — the entries each binary lists under `-demuxers`, `-decoders`,
`-filters` and so on — because counting the two sides differently is how the earlier
version of this table came to overstate protocols by nearly twice.

| | vaco | FFmpeg 9.0.1 |
|---|---:|---:|
| Demuxers | 172 | 361 |
| Muxers | 117 | 184 |
| Decoders | 89 | 527 |
| Encoders | 65 | 190 |
| Filters | 327 | 481 |
| Protocols | 14 | 41 |

Two caveats on that table. The decoder and encoder counts include the
patent-encumbered ones, which a default build leaves out. And a large share of the
missing formats are simply unwritten rather than deliberately excluded —
`docs/format-coverage.md` lists every format either side registers, and
`docs/why-some-formats-are-not-included.md` explains the handful that are excluded on
purpose.

### Conformance

A differential suite runs 709 cases against the reference. 288 agree and 421 diverge.

The case count understates the problem and the divergence count overstates it. The
comparison used to stop at a case's first differing line, so a real fix could move
nothing visible; reporting every differing line instead turns those 421 cases into
**13,004 field-level divergences**. Those are not 13,004 independent defects. The
median diverging case has 3, while the mean is 39 — pulled there by the 84 cases that
print one line per packet, where a single wrong timestamp formula surfaces as several
hundred changed lines. A further 79 cases compare whole container files byte for byte
and report one offset, with no breakdown at all.

They concentrate in probe metadata and container remux details — a `start_time`
defaulting to 0 rather than N/A, a raw timebase where the reference normalises one, an
unmapped codec profile — rather than in decoded pixels, which is where the
byte-exactness above is measured.

`planning/CONFORMANCE-FINDINGS.md` has the breakdown and the method for extending it.

### Option coverage

The option tables carry ffmpeg's names, but carrying a name is not implementing it.
Measured across both binaries:

| | `vaco` | `vaco-probe` |
|---|---:|---:|
| Options in the table | 172 | 65 |
| Implemented | 70 | 55 |
| Refused by name | 99 | 10 |
| Accepted as a deliberate no-op | 3 | 0 |

Roughly two in five are implemented. An option that isn't exits with an error naming
it, which is the point: until recently they were accepted and silently ignored, so
`-frames:v 2` gave byte-identical output to omitting it and `-shortest` did not
truncate. Those now refuse rather than lie. `-y`/`-n` and `-ss`/`-t`/`-to` were in
that category too and are now implemented.

The three no-ops are `-qphist`, `-top` and `-stdin`, which ffmpeg also ignores or
which already describe what this build does.

Counts move as work lands, and not always upwards: `vaco-probe`'s implemented count
last went *down*, because an audit found `-c` and `-cpucount` being accepted and
ignored and made them refuse. `cargo run -p xtask -- option-consumption-check` reports
any option that parses and reaches nothing.

### Performance

Measured 2026-09-01 on an Apple silicon laptop, interleaved A/B, medians of six
rounds. `planning/PERF-BASELINE.md` has the full matrix, the machine details, and the
harness.

| Workload | vaco `-threads 1` | `ffmpeg -threads 1` | Ratio |
|---|---:|---:|---:|
| H.264 decode, 1080p | 3.409s | 0.336s | 10.2x slower |
| H.264 decode, 4K | 8.406s | 0.754s | 11.2x slower |
| HEVC decode, 1080p | 2.917s | 0.414s | 7.1x slower |
| HEVC decode, 4K | 6.586s | 0.856s | 7.7x slower |
| Transcode H.264 to FFV1, 1080p | 6.824s | 0.402s | 17.0x slower |
| Remux mkv to mp4, 60s 1080p | 0.014s | 0.027s | **1.9x faster** |
| Probe, H.264 4K | 0.0055s | 0.040s | **7.3x faster** |

Decoding is the slow part, and the gap widens with resolution. Demuxing, remuxing and
probing are competitive or better, because those paths are I/O and bookkeeping rather
than per-pixel arithmetic.

Some of this moves quickly. H.264 gained row threading after this baseline was taken,
and an audio decode path that measured pathologically slow here has since been
rewritten. Treat the file as the living record, not this table.

The gap is not expected to close entirely. FFmpeg's inner loops are hand-written
assembly; these are autovectorised safe Rust.

## What works

Scope notes matter more than checkmarks here. "Intra-only" means keyframes decode and
inter-predicted frames do not, so most real files will not play through. Per-crate
documents under `docs/codec/` carry the exact clause-level scope.

### Video

| Codec | Decode | Encode | Notes |
|---|---|---|---|
| H.264 / AVC | yes | via x264 | Decode is patent-gated. Encode spawns your own `x264` binary |
| H.265 / HEVC | yes | via x265 | I-, P- and B-slices; no tiles or range extensions. Decode is patent-gated. Encode spawns your own `x265` binary |
| MPEG-1 / MPEG-2 | yes | — | |
| VP8 | yes | — | RFC 6386 |
| VP9 | yes | — | Inter prediction, compound reference, profiles 1-3 |
| AV1 | not registered | — | Implemented but not wired into the registry, so no build reaches it |
| Theora | intra only | — | Keyframes only |
| VC-1 / WMV3 | intra only | — | Simple/Main, progressive I-frames. Patent-gated |
| H.261 / H.263 | yes | — | Baseline |
| ProRes | yes | — | Decode only by decision; SMPTE RDD 36. Within ±4 of ffmpeg on 3% of 10-bit samples, an IDCT rounding difference |
| FFV1 | **no** | **no** | Decodes and encodes only against itself — see below |
| Raw / uncompressed | yes | yes | rawvideo, v210, r10k, y41p and friends |

FFV1 is the sharpest example of why this page reports measurements rather than
status. Its encoder and decoder round-trip losslessly *against each other*, and the
crate's test suite is exactly that round-trip, so it passed. Measured against ffmpeg
it fails in both directions: an ffmpeg-written FFV1 file decodes to wrong pixels here
(differing on 99.6% of bytes, from the first byte, on a lossless codec), and ffmpeg
reads our FFV1 output as wrong pixels too. It is registered, so a build will still
accept it. Treat it as broken until that row says otherwise.

### Audio

| Codec | Decode | Encode | Notes |
|---|---|---|---|
| AAC-LC | partly | — | Patent-gated. Output is offset ~1181 samples against ffmpeg (encoder delay not trimmed), with a real residual after aligning |
| MP1 / MP2 / MP3 | partly | — | Layer I/II/III decode, but output is offset ~1303 samples against ffmpeg (encoder delay not trimmed) with a smaller residual on top |
| AC-3 | **no** | — | Known accuracy defect: 99.5% of samples differ from ffmpeg, mean error 6806 of 32768, and it is not a time offset. E-AC-3 non-default |
| Vorbis | yes | yes | Within 1 LSB of ffmpeg. Encode is one fixed low-complexity configuration |
| FLAC | yes | yes | Byte-identical to ffmpeg |
| ALAC | yes | yes | Byte-identical to ffmpeg |
| Opus | not registered | — | Implemented but has unresolved correctness gaps, so it is deliberately not wired up |
| PCM | yes | yes | The whole `pcm_*` family |
| ADPCM | yes | — | IMA-WAV, IMA-QT, MS, SWF. No G.722 or G.726 |
| QOA, comfort noise | yes | yes | DFPWM is implemented but deliberately not registered |

### Images

BMP, GIF, JPEG, JPEG-LS, JPEG XL (decode only), OpenEXR, PCX, PNG and APNG, PNM,
QOI, SGI, TGA, TIFF, WebP, XBM, XWD.

Registered is doing a lot of work in that list. Probing an ffmpeg-written still,
`vaco-probe` reports the correct size and pixel format for PNG, BMP, TIFF and GIF, and
reports `0x0` with `pix_fmt=unknown` for PCX, SGI, PNM, QOI, XBM, XWD and JPEG-LS —
nine of thirteen formats never populate stream parameters at all, and TGA fails to
probe outright. The decode follows the metadata down: a colour P6 PPM comes back as
grey, each pixel carrying the luma of the colour it should have been.

Writing a still has its own bug: an output named `.png` selects the JPEG encoder
rather than PNG, so image output needs an explicit `-c:v`. With one given, PNG, BMP
and TIFF decode identically to ffmpeg.

### Subtitles

SubRip, ASS/SSA, WebVTT, 3GPP timed text, TTML, DVB bitmap, DVD/VobSub, PGS/HDMV,
CEA-608 and CEA-708 closed captions, and EBU Teletext.

### Filters

327 filters are registered, covering scaling and cropping, colour and LUTs, blur and
sharpen, convolution and morphology, deinterlacing, denoising, keying, overlay and
stacking, text and subtitle rendering, audio EQ and dynamics, resampling, analysis and
scopes, and test-pattern sources. `vaco -filters` lists what a given build has.

Coverage inside a family is uneven — some crates implement a subset and say so in
their documentation. `docs/filter/` has the per-crate breakdowns.

## Patent-encumbered codecs

H.264, H.265, AAC and VC-1 decode are behind Cargo features that default to off:

```
patent-encumbered-h264-decode
patent-encumbered-hevc-decode
patent-encumbered-aac-decode
patent-encumbered-vc1-decode
```

A default build registers none of them, and `vaco -codecs` will not list them.
Enabling them is your decision about your own jurisdiction and use.

## Development

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo xtask --help          # repository gates and generators
```

`xtask` owns the checks that keep the tree honest: layering, dependency policy,
patent gating, provenance, and reachability of every registered component. Several
tables in `docs/` and the registry itself are generated, and are marked as such.

`docs/README.md` indexes the per-crate documentation. `planning/` holds decision
records and measured results, including the performance baseline this README quotes.

## License

MIT or Apache-2.0, at your option. See `LICENSE-MIT` and `LICENSE-APACHE`.

Third-party licenses for permissively-licensed reference material consulted during
development are recorded under `LICENSES/`.
