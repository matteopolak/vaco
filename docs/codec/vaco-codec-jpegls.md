# `vaco-codec-jpegls`

Layer 4. JPEG-LS (ITU-T T.87 / ISO/IEC 14495-1) lossless still-image decode
and encode.

## What it is

The LOCO-I predictive/context-modeling codec: a MED (median edge detector)
predictor, 365 adaptive contexts with bias cancellation, Golomb-power-of-2
entropy coding, and a run-length "flat region" mode. Only the lossless case
(`NEAR = 0`) is implemented; a near-lossless (`NEAR > 0`) scan is rejected
with `Error::Unsupported` rather than decoded wrong.

Written from the algorithm's own paper (Weinberger, Seroussi, Sapiro,
"The LOCO-I Lossless Image Compression Algorithm", HP Labs HPL-98-193 —
`Vaco-Spec-Ref: locoi-hpl98-193`), not the paywalled ISO/ITU text. The paper
documents the regular (non-run) mode in full; the run-mode adaptation table
and the run-interruption sample's exact mapping/sign formulas are not given
numerically there, and were instead measured against `ffmpeg -c:v jpegls`'s
own decode (D17/D6) — see `src/context.rs`'s and `src/codec.rs`'s module
docs for exactly what that process found, including three real bugs it
caught (a context-reset ordering error, and two run-interruption sign/mapping
details).

## How it works

`src/bits.rs` implements JPEG-LS's own bit-stuffing (a single `0` bit after
every literal `0xFF` byte — not JPEG's `0xFF 0x00` byte-stuffing).
`src/golomb.rs` is the Golomb-power-of-2 code with the length-limited escape,
parameterized by `qmax` because a run-interruption sample gets a *shorter*
escape budget than a regular sample (`Lmax - g - 1` bits, where `g` is the
just-completed run segment's own parameter). `src/context.rs` holds the
365-context table (gradient quantization, MED prediction, bias/`C`
correction, the periodic `N`-halving reset) and the run-mode adaptation
index. `src/marker.rs` parses/writes `SOI`/`SOF55`/`LSE`/`SOS`/`EOI`.
`src/codec.rs`'s `decode`/`encode` tie these into the per-row, per-sample
loop, dispatching to regular-mode or run-mode per sample and to
run-interruption coding when a run breaks.

8-bit, 1-component (non-interleaved) and 3-component (line-interleaved RGB)
scans are covered — the two shapes `ffmpeg -c:v jpegls` itself produces.
Sample-interleaved scans are rejected (`Error::Unsupported`): this crate's
run-mode state is tracked per row, not per interleaved sample-within-a-row.

## A known gap, honestly

Verified bit-exact against `ffmpeg -c:v jpegls`'s own decode on solid
fields, sharp two-tone transitions (both run-interruption sign cases, many
Golomb parameters and several escape codes), vertical and diagonal
gradients, uniform noise, and three-component RGB. Against `ffmpeg`'s own
`testsrc`/`gradients` test patterns — busier, multi-directional
photographic-like content — a handful of individual pixels still disagree
in a few spots (typically off by 1-2, not an escalating desync at that
point), and on one fixture decoding eventually runs out of input entirely.
This is a real, open defect in some rarely-hit formula detail this crate's
synthetic corpus does not exercise (most likely context reset, run
interruption's mapping selection, or the escape length limit, since those
are exactly the three places already found wrong once each) — not a
rounding tolerance. It is not hidden: most divergence paths run out of
input into a clean `Error`, but a few individual pixels differ with no
error raised at all, which is worth knowing before trusting a clean decode
of unfamiliar input as byte-exact.

## How to change it

A new interleave mode or component-count case starts in `src/codec.rs`'s
`decode`/`encode`, which are the only places that know how `(x, y,
component)` maps onto scan order. A context-modeling change belongs in
`src/context.rs`; a bitstream-format change (a new marker, a different
`LSE` payload) belongs in `src/marker.rs`.

## Configuration

`vaco_limits::Limits` bounds the decoded frame: width/height come from the
attacker-controlled `SOF55`, validated by `vaco_frame::Frame::alloc_video`
before a sample is decoded. Component count is capped at 3 (`Nf` is checked
to be 1 or 3) rather than trusting the 8-bit field directly.

## Dependencies

`vaco-codec-core` (the send/receive protocol), `vaco-frame`/`vaco-pixfmt`/
`vaco-pool` (the decoded picture), `vaco-packet` (the encoded bytes),
`vaco-limits` (allocation bounds).

## Registration

`vaco-component.toml` registers `JPEGLS_DECODER`/`JPEGLS_ENCODER` under
`vaco_codec_core::CodecId::JpegLs` (added to `vaco-codec-core` alongside
this crate), feature `codec-jpegls`, default build. `vaco-demux-image2`'s
`jpegls_pipe` splitter (`.jls`, non-interleaved-or-line-interleaved framing)
was updated from `codec = None` to `Some(CodecId::JpegLs)` so a `.jls` file
actually reaches this decoder — that one-line gap is what kept `.jls` from
resolving to any decoder at all before this crate existed.

## Testing

Unit tests cover the bit-stuffing round trip, Golomb encode/decode at every
`k` (including the escape path), context quantization/symmetry, and full
image round trips (flat, gradient, noise, varying-length runs, single
pixel, three-component RGB) through this crate's own encoder and decoder.
A `jpegls_decode` fuzz target decodes arbitrary bytes and additionally
asserts the stronger, genuinely-lossless property that `decode(encode(f))
== f` for arbitrary pixel data — see "A known gap, honestly" above for why
that property matters here specifically.
