# `vaco-codec-adpcm`

Layer 4. The standardised ADPCM subset: G.722, G.726/G.726le, MS-ADPCM,
SWF ADPCM, IMA-WAV, IMA-QT.

## What it is

One crate covering every ADPCM variant that has a stable, public bitstream
description independent of any one decoder's source: `g722.rs`/`g726.rs`
(ITU-T recommendations), `ms.rs` (the widely-published `WAVE_FORMAT_ADPCM`
block layout), `swf.rs` (Adobe's SWF file format spec), and `ima.rs` (the
shared IMA/DVI step-table codec, framed two different ways for
`adpcm_ima_wav` and `adpcm_ima_qt`). `lib.rs` wraps each pure codec module in
the `vaco_codec_core::SendReceive` shape every codec in this tree uses.

`AdpcmG722Decoder`/`Encoder` and both `AdpcmG726*` pairs deliberately
`Error::Unsupported` rather than guess at a bitstream this crate does not
implement correctly — see their own `_refuse_rather_than_produce_wrong_output`
tests. Everything below is about the four codecs that do decode/encode.

## How it works

`ima.rs`'s `ImaState` (predictor, step-table index) is shared by both IMA
framings: `decode_wav_block`/`encode_wav_block` for WAV's block header
(4 bytes/channel: predictor, index byte, reserved byte, then round-robin
4-byte nibble groups), and `decode_qt_block`/`encode_qt_block` for
QuickTime's 34-byte `ima4` chunks (2-byte big-endian header + 32 bytes/64
nibbles).

`ms.rs` implements the classic `ADPCMBLOCKHEADER` layout: one coefficient
index byte, one `i16` delta, then the two seed samples, per channel; 4-bit
codes follow, packed two per byte, consumed round-robin across channels.

`swf.rs` is an MSB-first bit-packed format (`BitReader`/`BitWriter`): a
2-bit code-width selector (2/3/4/5-bit codes), then per channel a 16-bit
initial sample and 6-bit initial step index, then the codes themselves.
`AdpcmSwfDecoder::send` has to *estimate* how many samples a block holds
from its byte length, because SWF states this at the container level
(`SoundStreamHead`/`DefineSound`'s own sample-count field) which this
codec-level function is never handed — see "Known gaps" below, this is not
just imprecision, it is a bug with measured evidence.

## Bugs found and fixed

`tests/oracle_ffmpeg.rs` (added together with these fixes) is this crate's
first real-fixture regression test for these four codecs; before it, each
had only a self-round-trip test — this crate's own encoder feeding this
crate's own decoder, which cannot catch either side sharing the same wrong
assumption. It found two real bugs, both now fixed:

- **`adpcm_ima_qt` discarded 7 bits of predictor precision at every chunk
  boundary.** `decode_qt_block` re-derived the running predictor/step index
  from every `ima4` chunk's own 16-bit header, but a real header only
  carries 9 bits of predictor precision (`0xFF80` masks the low 7 bits).
  Re-seeding from it every chunk produced a constant additive bias from the
  second chunk onward (measured: exactly +114 on the real fixture used
  here). Root-caused by brute-force differential replication in Python
  against real `ffmpeg`-decoded reference PCM: no per-chunk header
  reconstruction reproduced `ffmpeg`'s decode, while carrying `ImaState`
  continuously across chunks (trusting the header only for chunk 0)
  matched it bit-for-bit across all 104 chunks of the fixture.
  `AdpcmImaQtDecoder` now carries that state in a `state` field across
  `send` calls (reset on `flush`); `encode_qt_block` was updated to match,
  since its own per-chunk re-estimation was the same wrong assumption the
  old decoder made, just on the encode side.

- **`adpcm_ms`'s first two samples came out transposed.** The real
  `ADPCMBLOCKHEADER` field order is `bPredictor, iDelta, iSamp1 (newer),
  iSamp2 (older)` — `iSamp1` first on the wire. `decode_block` read the two
  fields in the opposite order, which flipped this crate's own
  "`sample1` = newer" convention throughout: `MsState`'s `sample1`/
  `sample2` held the wrong sample, and every block's first two *output*
  samples were reversed relative to a real decoder (measured:
  `ours[0..2] == [ffmpeg_ref[1], ffmpeg_ref[0]]` exactly, everything after
  already correct). `encode_block` wrote its seeds in the matching backwards
  order, so the self-round-trip test could not catch this either. Fixed by
  reading/writing `iSamp1` first, `iSamp2` second.

## Known gaps

- **`AdpcmSwfDecoder`'s sample-count estimate is provably ambiguous in the
  general case**, not merely imprecise. A real fixture (mono, 4-bit codes,
  2051-byte block) is bit-consistent with *both* 4096 and 4097 samples once
  the trailing partial byte is accounted for; the byte length alone cannot
  distinguish them. The current estimator resolves the tie to a third,
  wrong answer (4095). Separately, black-box probing `ffmpeg`'s own
  `adpcm_swf` encoder shows it always emits a *fixed* 4096-sample block
  regardless of input duration/rate/channel count — useful evidence, but
  not a general `adpcm_swf` rule (a conformant file may legally use any
  block size, carried in a container field this codec-level API is never
  given). The real fix is threading a container-supplied sample count
  through to this decoder, not a magic constant; landed as an `#[ignore]`d
  regression test with the measured evidence rather than guessed at.

## How to change it

The four working codecs (`ima_wav`, `ima_qt`, `ms`, `swf`) each own their
pure encode/decode functions in their own module; `lib.rs` only adapts them
to `SendReceive`. Any change to wire-format assumptions in `ima.rs`/`ms.rs`/
`swf.rs` should be re-verified against `tests/oracle_ffmpeg.rs`'s real
fixtures, not just the internal `mod tests` self-round-trips beside each
codec — those catch a change in encode/decode agreement with each other,
not agreement with the real format.

## Configuration

None — no feature flags, no options.

## Dependencies

`vaco-chlayout`/`vaco-frame`/`vaco-packet`/`vaco-sampfmt`/`vaco-limits` (the
`SendReceive` data model). `tests/oracle_ffmpeg.rs` has no crate
dependency beyond the workspace's own types; its fixtures were produced by
real `ffmpeg 9.0.1` and are read with this file's own minimal RIFF/ISO-BMFF/
FLV tag walkers (deliberately not a real container-parsing dependency — see
that file's own comments).
