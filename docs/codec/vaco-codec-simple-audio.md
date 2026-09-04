# `vaco-codec-simple-audio`

Layer 4. Three small, open-spec audio codecs that share no algorithm with
each other: QOA (Quite OK Audio), RFC 3389 comfort noise, and `DFPWM1a`.
**Two of three are real, registered codecs; DFPWM is not** — see below
before assuming otherwise.

## What it is

Grouped in one crate the way `vaco-codec-image-simple` groups unrelated
trivial image formats: each codec here is too small to earn its own crate,
and none is a variant of another. [`qoa`]/[`comfortnoise`]/[`dfpwm`] each
own their pure encode/decode functions; `src/lib.rs` wraps them in the
`vaco_codec_core::SendReceive` shape every codec in this tree uses.

## How it works

**QOA**: a sign-sign LMS (least mean squares) predictor coded in fixed
20-sample/8-byte slices, transcribed clause-for-clause from "The Quite OK
Audio Format" specification v1.0 (`qoaformat.org`). One QOA *frame* (header
+ per-channel LMS state + up to 256 slices) is this codec's packet unit;
because the LMS state is re-transmitted in full at every frame header, a
frame decodes independently of every other frame, and an over-long input
`Frame` is split into several QOA frames (`Caps::SUBFRAMES`) on encode. The
specification only defines decode; the encoder's per-slice scale-factor
search (try all 16, keep the one with lowest squared error) is this crate's
own design and cannot affect interoperability, since it only ever emits
spec-conformant slices. **Verified against `ffmpeg`'s own QOA decoder**:
encoding a tone with this crate and decoding the resulting file with both
this crate's own decoder and `ffmpeg -i x.qoa` gives byte-identical PCM in
both the stereo and multi-frame (>5120 samples/channel) cases. That claim
used to rest entirely on a one-off manual check with nothing committed to
catch a regression; `tests/oracle_ffmpeg_qoa.rs` now makes it a real,
committed test: this crate's own encoder produces the frame, real `ffmpeg`
and this crate's own decoder both read the identical bytes, and the test
asserts they agree bit-for-bit (QOA decode is fully-specified integer
arithmetic — two correct decoders reading the same bytes cannot disagree)
plus checks SNR against the original source so agreement alone can't mask
both sides being wrong the same way.

**Comfort noise**: RFC 3389 defines the SID payload wire format exactly (a
noise-level byte plus quantised LPC reflection coefficients) but states
plainly that the noise analysis/synthesis algorithm is unspecified and
implementation-defined. Decode converts the reflection coefficients to a
direct-form all-pole filter (the textbook Levinson-Durbin step-up
recursion) and drives it with white noise, then rescales the whole block so
its measured RMS matches the requested `-dBov` level exactly — decoupling
the filter's spectral shaping from the level, rather than depending on the
filter's own incidental gain. Encode reuses `vaco-codec-dsp-lpc`'s
autocorrelation + Levinson-Durbin directly for the reflection coefficients
(no separate conversion needed). Mono only, matching RFC 3389's own scope;
`ffmpeg -h decoder=comfortnoise`/`-h encoder=comfortnoise` independently
confirm mono/`s16` as the real-world convention.

**DFPWM1a is not implemented as a real codec.** This module transcribes the
only public specification available (a CC-BY wiki page written by the
format's original author) exactly, and it is internally consistent — but
black-box testing against `ffmpeg 8.1`'s `dfpwm` decoder found the charge
growth rate does not match real `.dfpwm` streams (a brute-force search over
the formula's free parameters could not reproduce `ffmpeg`'s observed
decode of a simple constant-bit-run fixture). See `src/dfpwm.rs`'s module
doc for the full measurement. `DfpwmDecoder`/`DfpwmEncoder` always return
`Error::Unsupported` and are not registered — the same shape as
`vaco-codec-adpcm`'s `AdpcmG722Decoder`/`AdpcmG726Decoder`.

## How to change it

A new codec this small belongs here as its own module plus a pair of
`SendReceive` wrappers in `src/lib.rs`. Whoever finds the real `DFPWM1a`
recursion next has a real fixture to work from: `src/dfpwm.rs`'s
`own_decode_does_not_match_the_measured_ffmpeg_trace` test records the
actual `ffmpeg`-observed charge sequence for a constant-bit-run input.

## Configuration

`vaco_limits::Limits` bounds every allocation. `comfortnoise::MAX_MODEL_ORDER`
(20) bounds the LPC order a SID payload's attacker-controlled byte length can
drive analysis/synthesis to. QOA needs no external configuration — its frame
header supplies sample rate and channel count directly.

## Dependencies

`vaco-codec-core` (the send/receive protocol and `Machine`), `vaco-frame`/
`vaco-sampfmt`/`vaco-chlayout` (the decoded audio frame), `vaco-packet`,
`vaco-limits`, and `vaco-codec-dsp-lpc` (comfort noise's LPC analysis, reused
rather than re-derived).

## Registration

`vaco-component.toml` registers `QOA_DECODER`/`QOA_ENCODER` under
`CodecId::Qoa` and `COMFORTNOISE_DECODER`/`COMFORTNOISE_ENCODER` under
`CodecId::ComfortNoise`, feature `codec-simple-audio` (on by default).
`DFPWM_DECODER`/`DFPWM_ENCODER` exist as compilable identities but are not
listed in `vaco-component.toml`. Verified through the real `vaco` binary:
`vaco -h decoder=qoa`/`-h decoder=comfortnoise` resolve; `vaco -h
decoder=dfpwm` correctly reports "not recognized".

## Testing

Unit tests cover each codec's own round trip, channel-separated correctness
(a stereo test asserts a silent channel stays silent — the shape of bug that
hides behind an aggregate metric), the `SendReceive` protocol shape, and
`decode` never panicking on arbitrary bytes. `qoa_decode` and
`comfortnoise_parse` fuzz targets exercise both codecs' untrusted-input
surface, the latter also asserting the parser enforces its own documented
model-order bound.
