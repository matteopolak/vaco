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

`g726` and `g726le` provide decoder-only support for the
four-bit, 32 kbit/s ITU-T mode. `AdpcmG722Decoder`/`Encoder` and both G.726
encoders deliberately return `Error::Unsupported` rather than guess at an
algorithm or rate that is not implemented.

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
The published SWF `ADPCMPACKET` record has one initial sample plus exactly
4095 codes (per channel), so a duration-less packet decodes to 4096 samples
per channel. When a caller supplies a non-zero `Packet::duration`, the
decoder converts it to an explicit count; this preserves a shorter final
event-sound block whose container-level `SoundSampleCount` trims the encoded
packet. The encoder emits that same fixed-size packet for every input up to
4096 samples per channel, repeating each channel's final sample as deterministic
padding while retaining the real count in `Packet::duration`; larger frames
are rejected because this one-frame/one-packet API does not split them.
Only mono and stereo layouts are valid for SWF ADPCM. An explicit zero-channel
or multichannel layout is rejected instead of being silently normalized to
mono; the generic decoder configuration path's zero-channel “not yet known”
value remains untouched and therefore keeps its normal mono default.
An encoder layout supplied through `with_audio_params` or extradata is also
validated and must match the input frame; an encoder with no explicit layout
continues to derive mono or stereo directly from each frame.

`g726.rs` implements the integer two-pole/six-zero predictor, inverse
quantizer, fast/slow scale-factor adaptation and tone/transition reset defined
by ITU-T G.726 clauses 3-4. Its output uses Annex A's uniform 14-bit PCM path,
including the corrected `SR = 57344` limiter boundary, scaled to signed 16-bit
PCM. The state is continuous across packets and `flush` restores the Table 6
reset values. At 32 kbit/s every byte decodes to exactly two mono 8 kHz
samples: `g726` consumes the high nibble first, while `g726le`
consumes the low nibble first.

The fixed-point path is checked against the original unrestricted-use Sun
G.72x implementation. That implementation reproduced all 16,384 published
mu-law outputs for Appendix II's reset-state `I32` vector; applying Annex A's
uniform-output limiter to the same reconstructed samples supplies the linear
PCM oracle used here. The short encoded fixtures come from ffmpeg 9.0.1 and
therefore independently exercise both raw nibble packings. ffmpeg's own
linear decoder is not the arithmetic oracle: in this measurement it diverged
from the standard/Sun result on 1,760 of 2,400 samples (first at sample 6,
maximum absolute difference 1,024), while retaining the exact sample count.
On the full Appendix-II input, it differed on 9,027 of 16,384 samples (first
at sample 4,567, maximum absolute difference 65,532 where the decoders chose
opposite saturation endpoints). These figures describe ffmpeg 9.0.1 on the
fixtures committed here; they are not a compatibility tolerance.

## Bugs found and fixed

`tests/oracle_ffmpeg.rs` (added together with these fixes) is this crate's
first real-fixture regression test for these four codecs; before it, each
had only a self-round-trip test — this crate's own encoder feeding this
crate's own decoder, which cannot catch either side sharing the same wrong
assumption. It found four real bugs, all now fixed:

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

- **`adpcm_swf` under-counted full packets.** The byte-length estimator
  subtracted seven possible padding bits before dividing, turning the
  2,051-byte mono packet in the real ffmpeg fixture into 4,095 samples.
  SWF v19 defines that packet as 4,095 codes after its initial sample, so
  the decoder now emits the required 4,096 samples when no explicit packet
  duration is present. A non-zero duration remains authoritative for a
  caller-provided partial count.

- **`adpcm_swf` encoded non-conformant partial packets.** The encoder used to
  stop after the input's final sample, even though SWF v19 fixes every packet
  at 4,096 samples per channel. It now pads short input by repeating each
  channel's final sample, keeps the original sample count in the packet
  duration, and rejects input too large for one packet rather than emitting a
  variable-length block.

## Known gaps

- **G.726 encoding and rates other than 32 kbit/s.** The public codec IDs and
  existing raw demuxers do not carry a bitrate parameter, so the decoder
  accepts only the unambiguous four-bit mode. Both encoder descriptors remain
  unregistered and direct construction returns a named unsupported error.

- **G.722.** The current module is a structurally different stand-in rather
  than the ITU-T QMF/predictor, so neither direction is registered.

- **Container wiring for partial event sounds.** SWF's `DefineSound` tag
  carries the total `SoundSampleCount` separately from its ADPCM packets.
  The codec honors a non-zero `Packet::duration` when a container passes that
  count through, but the SWF demuxer must perform that mapping before a
  shorter final event-sound packet can be trimmed. A duration-less packet is
  decoded according to the format-defined 4,096-sample packet size.

## How to change it

Each codec owns its pure algorithm and framing functions in its own module;
`lib.rs` adapts them to `SendReceive`. Changes to G.726 arithmetic must retain
the fixed-width masks in the named clause-4 blocks and be tested against both
the full Appendix-II `I32` input and ffmpeg-generated encoded fixtures, using
the Sun/Annex-A PCM goldens rather than ffmpeg's divergent PCM. Changes to
wire-format assumptions in `ima.rs`/`ms.rs`/`swf.rs` likewise need the real
fixtures in `tests/oracle_ffmpeg.rs`, not only self-round-trips.

## Configuration

There are no runtime options. The `g726` and `g726le` identities
always mean mono 8 kHz, four-bit / 32 kbit/s G.726; their suffix distinguishes
nibble packing, not arithmetic.

## Dependencies

`vaco-chlayout`/`vaco-frame`/`vaco-packet`/`vaco-sampfmt`/`vaco-limits` (the
`SendReceive` data model). `tests/oracle_ffmpeg.rs` has no crate dependency
beyond the workspace's own types. Its container and short G.726 payload
fixtures were produced by real ffmpeg 9.0.1 and are read with this file's own
minimal RIFF/ISO-BMFF/FLV tag walkers (deliberately not a real
container-parsing dependency — see that file's own comments).

The G.726 implementation is derived from ITU-T G.726 (12/1990), Annex A
(11/1994), and Corrigendum 1 (05/2005). Appendix II's official `I32` sequence
is used as input evidence. The original Sun G.72x implementation is a
secondary, unrestricted-use conformance reference; its exact match to
Appendix II validates its state machine before Annex A's published output
limiter is applied for the committed linear PCM goldens.
