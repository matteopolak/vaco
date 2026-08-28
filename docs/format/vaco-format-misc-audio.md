# `vaco-format-misc-audio`

Layer 4. Demux-only: `wv`, `tta`, `amr`/`amrnb`/`amrwb`, `adx`,
`nistsphere`, `pvf`, `g723_1`, `sbc`, and the headerless ITU-T/3GPP2
speech-codec tail (`gsm`, `sln`, `dfpwm`, `g722`, `g726`, `g726le`, `g728`,
`g729`, `aptx`, `aptx_hd`) — twenty registered demuxers in one crate
(FM-58). These are containers: the job is finding frame/block boundaries and
reporting stream parameters, not decoding audio.

---

## What it is

| Module | Formats | Framing |
|---|---|---|
| `wavpack` | `wv` | 32-byte little-endian block header states the next block's byte offset directly (`ckSize + 8`); no decode needed to walk the chain |
| `tta` | `tta` | fixed 22-byte header + a seek table of per-frame byte lengths |
| `amr` | `amr`, `amrnb`, `amrwb` | one TOC byte per frame; its 4-bit mode field indexes a fixed payload-size table |
| `adx` | `adx` | fixed-field header states `block_size` and `copyright_offset` (→ header length); data is then constant-size ADPCM blocks |
| `nistsphere` | `sph`, `nist` | ASCII `key -type value` header of a stated total length, then raw PCM |
| `pvf` | `pvf` | one text line (`channels rate bits`), then raw big-endian PCM |
| `g723` | `g723_1` | headerless; each frame's first two bits select one of four fixed sizes (24/20/4/1 bytes) |
| `sbc` | `sbc` | headerless; each frame's own header states blocks/subbands/bitpool, which a published formula turns into the frame's exact byte length |
| `rawcodec` | `gsm`, `sln`, `dfpwm`, `g722`, `g726`, `g726le`, `g728`, `g729`, `aptx`, `aptx_hd` | headerless, constant bytes-per-block : frames-per-block ratio, fixed by the codec's own bitrate |
| `block` | shared | `BlockDemuxer` — the fixed-ratio block engine `adx`, `nistsphere`, `pvf` and every `rawcodec` entry reduce to |

### Deliberately not in this crate

**#621 (legacy compressed-audio containers):** only `wv` and `tta` landed
with real confidence. `ape` (Monkey's Audio) and `mpc`/`mpc8` (Musepack SV7/
SV8) were not attempted — this build's `ffmpeg` has no encoder for any of
the three, and `true-audio.com`'s WavPack-adjacent "format description" page
turned out to be domain-squatted (redirects to unrelated content), so
getting their several header-version layouts right would have meant more
spec study than this pass's time budget allowed. `shn` (Shorten) is a
continuous Rice-coded bitstream with no separable frame boundary at the
container level at all — demuxing it without decoding is not really
possible, which is a different kind of gap from "not yet done". `tak`,
`rka`, `osq`, `wavarc`, `bonk` were not attempted.

**#622 (T3 tail):** `codec2`/`codec2raw` were skipped — this build's
`ffmpeg` has neither the codec nor a documented default mode, and the
container needs one to know its frame size. `hcom`, `epaf`, `mmf`, `qcp`,
`vqf`, `oma`, `dsf`, `avr`, `aea` were not attempted; `mmf` specifically
turned out to nest a further `Atsq`/`Awa` sub-chunk structure once measured,
past what this pass's time budget covered.

**#620 (tracker/module and chiptune-adjacent):** the tracker half (IT, XM,
S3M, MOD, and the rest of the family the reference reaches through
`libopenmpt`) is recorded as a D10 exclusion — see
`docs/why-some-formats-are-not-included.md`. The chiptune-adjacent game
containers (`bfstm`, `brstm`, `binka`, `genh`, `hca`, `msf`, `xa`, `vag`,
`svag`, `xvag`, `xwma`, `fsb`) are ordinary containers and fair game, but
none of them has an `ffmpeg` encoder in this build to differentially test
against, and this pass did not want to hand-build fixtures for a header
layout recalled rather than measured or freshly fetched. Not started, not
excluded.

---

## How it works

### `block::BlockDemuxer` — the shared engine

Once a format's header (if it has one) is parsed down to `(sample_rate,
channels, bytes_per_block, frames_per_block, data_start, declared_len)`,
`BlockDemuxer` does the rest: clamps `declared_len` against the source's own
size, reads ~4096-byte packets rounded down to a whole number of blocks
(discarding a partial trailing block, never emitting a short final block as
if it were whole), and stamps `pts`/`duration` from a running block count.
`adx`, `nistsphere`, `pvf`, and every entry in `rawcodec` are this engine
plus their own header.

### The measured comparison table

Ten of the twenty formats have a real `ffmpeg 8.1`-produced fixture
(`tests/fixtures/`); the rest are headerless codecs this build's `ffmpeg`
cannot encode, or (`g723_1`) can encode but the reference states no
duration for. `tests/differential.rs` opens each fixture through its real
`DemuxerDesc` and checks it against `ffprobe -show_entries
stream=sample_rate,channels -show_entries format=duration`:

| Fixture | `sample_rate` | `channels` | reference duration | notes |
|---|---:|---:|---:|---|
| `wavpack.wv` | 44100 | 2 | 0.300 s | |
| `tta.tta` | 44100 | 2 | 0.300 s | |
| `adx.adx` | 8000 | 1 | 0.304 s | reference `duration_ts` is block ticks (`time_base=1/250`), not samples — see below |
| `g722.g722` | 16000 | 1 | 0.150 s | |
| `g726.g726` | 8000 | 1 | 0.300 s | reference `time_base=1/90000` (a generic raw-audio fallback), not samples |
| `g726le.g726le` | 8000 | 1 | 0.300 s | as `g726` |
| `aptx.aptx` | 48000 | 2 | *(reference: N/A)* | this crate estimates a duration from the file size; the reference's raw `aptx` demuxer declines to |
| `aptx_hd.aptx_hd` | 48000 | 2 | *(reference: N/A)* | as `aptx` |
| `sbc.sbc` | 16000 | 1 | *(reference: N/A)* | self-delimited, no declared total |
| `g723_1.g723_1` | 8000 | 1 | *(reference: N/A)* | self-delimited, no declared total |

Two genuinely measured divergences are recorded here rather than "fixed",
because fixing them would mean discarding information rather than
correcting an error:

- **`adx` and `g726`/`g726le` report a duration in the same wall-clock
  seconds as the reference, at a different tick rate.** The reference's
  `duration_ts` is a *ticks* value in whatever `time_base` that format's own
  demuxer picked (block count for `adx`, a generic `1/90000` for the raw
  ADPCM formats); this crate's `time_base` is always `1/sample_rate`. Both
  land on the same duration in seconds — `0.304 s` and `0.3 s` respectively
  — which is the invariant the comparison table checks.
- **`aptx`/`aptx_hd` get a duration where the reference reports `N/A`.**
  Both have a fixed, spec-mandated bytes-per-sample ratio, so this crate's
  `BlockDemuxer` estimates a duration from the file size the same way
  `vaco-format-audio-simple`'s `RawPcmDemuxer` does for headerless PCM; the
  reference's own raw `aptx`/`aptx_hd` demuxer just does not bother.

### Measured field layouts, not recalled ones

`wv`'s block header and `tta`'s header/seek-table were reconstructed by
generating a real fixture with `ffmpeg -c:a wavpack -f wv` / `-c:a tta -f
tta` and reading the bytes back with a script, then cross-checking every
field against `ffprobe`'s own reported `sample_rate`/`channels`/
`duration_ts` — not from `true-audio.com`, which turned out to be
domain-squatted (see "Deliberately not in this crate"). `adx`'s header was
recovered the same way. `amr`'s frame-size table was cross-checked against
IETF RFC 4867's own Table 1 (total speech bits per mode), fetched directly;
the AMR-WB half of that table is the standard 3GPP TS 26.201 byte sizes,
reproduced from the widely-published numbers rather than a freshly fetched
copy of that specific document. `sbc`'s frame-length formula was verified
against a real fixture: the measured header fields (`blocks=16`,
`subbands=8`, `bitpool=60`, mono) give exactly `128` bytes by the formula,
and the fixture is `128 * 37` bytes exactly.

---

## How to change it

- **A new entry in the fixed-ratio `rawcodec` family**: add a
  `RawCodecSpec` const and one `raw_codec_format!` macro invocation. The
  macro generates the probe, the `open` wrapper and the `DemuxerDesc`;
  nothing else needs touching except the `vaco-component.toml` fragment and
  `cargo xtask gen-registry`.
- **Reviving one of the deferred #621/#622 formats**: read "Deliberately
  not in this crate" above first — most of them are blocked on a spec
  source, not on difficulty. `ape`/`mpc`/`mpc8` need a genuinely reachable
  primary specification (`monkeysaudio.com`'s own SDK docs are one
  candidate not yet tried); `codec2`/`codec2raw` need a decision about
  which mode a headerless file is assumed to be.
- **`amr`'s multichannel interleaved variant** (`#!AMR_MC1.0\n` and
  friends): not implemented; only mono narrowband/wideband are.
- **A `Limits` injection point for `open`**: as
  `vaco-format-audio-simple`, none of these `open` functions take a
  `vaco_limits::Limits` override; each opens under a fixed internal
  `Limits::permissive()`.

## Configuration

None. Every format's `open` function takes only the source (or, for the
registered `DemuxerDesc`s, the `ParserProvider` no module here actually
calls — none of these codecs is reached through a bitstream parser).

## Dependencies

`vaco-core`, `vaco-io` (`IoContext`, the byte-order-aware readers every
module is built on), `vaco-limits` (`Budget`), `vaco-packet`,
`vaco-sampfmt`, `vaco-chlayout`, `vaco-format-core` (`Demuxer`,
`DemuxerDesc`, `ProbeData`/`ProbeScore`), and `vaco-codec-core` for
`CodecId` — including seven variants (`WavPack`, `Tta`, `Dfpwm`, `Aptx`,
`AptxHd`, `Sbc`, `AdpcmAdx`) added there by this crate, following the
precedent set by the RTP/FLV/subtitle-text crates of adding to that
hand-maintained enum directly when a new format needs an identity it does
not yet have.
