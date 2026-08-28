# `vaco-codec-theora`

Layer 4. Theora video decode, native, keyframes only.

## What it is

Xiph.Org's Theora codec (an evolution of the donated, patent-unencumbered
VP3), decoded per `Vaco-Spec-Ref: theora-spec-20170603` (the Theora
Specification, Xiph.Org Foundation, June 3 2017 — the only normative Theora
document; VP3 itself has no separate written spec). This crate decodes
`FTYPE == 0` (intra/keyframe) frames only and returns
`vaco_core::Error::Unsupported` for `FTYPE == 1` (inter/delta) frames rather
than attempting motion compensation — see "A known gap, honestly" below.

## How it works

`src/ident.rs` parses the identification header (frame/picture geometry,
pixel format). `src/setup.rs` parses the setup header's framing (loop filter
limits, quantization parameters, DCT token Huffman tables), delegating the
quantization-matrix math to `src/quant.rs` and the Huffman-tree-from-bitstream
construction to `src/huffman.rs`. `src/blocks.rs` builds the coded-order
geometry — super blocks in raster order, blocks inside each super block via a
4x4 Hilbert curve (section 2.3) — as a table rather than recomputing it per
access. `src/idct.rs` is the exact integerized inverse DCT the spec mandates
bit-for-bit. `src/tokens.rs` decodes EOB run tokens and coefficient tokens
(section 7.7). `src/frame.rs` is the per-frame pipeline tying all of this
together: frame header, block-level qi decode, DCT coefficient decode, DC
prediction inversion, dequantize + IDCT reconstruction, and the loop filter.
`src/decoder.rs` implements `vaco_codec_core::Decoder` directly (not through
the `SendReceive`/`Machine` wrapper, matching `vaco-codec-vorbis`'s shape):
`set_extradata` unpacks the Xiph-laced identification/comment/setup headers a
container hands this crate, and `send_packet`/`receive_frame` decode one
picture per keyframe packet.

### Scope: intra decode only

Because only keyframes are decoded, several general (inter-capable)
procedures the spec describes collapse to a fixed case here, documented in
`src/frame.rs`'s module doc in detail:

- Coded block flags (7.3) and macro block coding modes (7.4): skipped —
  every block is coded and every macro block is `INTRA` in a keyframe.
- Motion vectors (7.5): never decoded.
- DC prediction (7.8): only the "no reference frame" class exists, so
  `LASTDC` needs one slot per plane instead of three.
- Reconstruction (7.9.4): the predictor is always the constant-128 intra
  predictor, and `qti` is always 0.
- Loop filter (7.10.3): only the left/bottom edge of every block is ever
  filtered — provably correct for an all-keyframe decode, not an
  approximation, because the spec's right/top-edge pass only fires when the
  neighbor on that side is *uncoded*, which never happens here.

## A known gap, honestly

No genuine Theora *encoder* was available in this environment (`ffmpeg
-codecs` here lists Theora decode only), so unlike this tree's other
from-scratch codecs, this crate has not been diffed against an independent
decoder's output on real encoded content — the D6/D17 "measure against a
reference implementation" methodology this tree otherwise leans on. What
verification exists instead: every formula in `idct`, `tokens`, and
`frame`'s reconstruction/loop-filter path is transcribed directly from the
spec's own numbered steps (not measured/reconstructed), the coded-order
Hilbert curve and the quantization-matrix interpolation formula are checked
digit-for-digit against the spec's own worked numeric examples, and the
`theora_decode` fuzz target exercises the full pipeline structurally. The one
exception is the loop filter limit table's decode procedure (section 6.4.1),
which is **missing from the published spec PDF itself** — confirmed by
rendering the actual page images, not just a text-extraction glitch — and
was reconstructed by analogy with the very next, textually-complete section;
see `src/setup.rs`'s module doc for the full account. Inter-frame decode is
simply not implemented (a scope decision, not a residual bug): a stream with
delta frames decodes every keyframe correctly and then returns a clean,
typed `Error::Unsupported` on the first delta frame rather than repeating a
stale picture or guessing. A real `.ogv` file with independently-verified
decoded frames is the first thing worth throwing at this crate before
trusting it in production; this crate should be treated as spec-conformant
by construction, not as cross-verified the way this tree's other codecs are.

## How to change it

Inter-frame support is the natural next step and would need: motion vector
decode (section 7.5, a new module), macro block coding mode decode (currently
hardcoded to `INTRA` — see `src/frame.rs`'s doc-listed simplifications),
reference-frame buffering across packets (`TheoraDecoder` would need to hold
the previous and golden reference pictures), and the whole/half-pixel
predictors (sections 7.9.1.2/7.9.1.3) before `frame::decode_frame_payload`
could stop rejecting `FTYPE == 1`. A new pixel format or subsampling case
starts in `ident::PixelFormat` and `blocks::FrameGeom::build`, which are the
only places that know how a pixel format maps onto chroma block-grid
dimensions.

## Configuration

`vaco_limits::Limits` bounds the coded frame the same way every other
decoder in this tree does: `TheoraDecoder::set_extradata` checks the
identification header's `FMBW`/`FMBH` against the budget before building any
block-indexed table from them, and every per-block allocation in `frame.rs`
goes through the same budget via `Budget::alloc`. A frame-decode loop's cost
is additionally bounded by `Budget::consume_fuel`, since the coefficient
decode and reconstruction loops are `O(blocks)` or worse in attacker-facing
dimensions.

## Registration

`vaco-component.toml` registers `DECODER_THEORA` under
`vaco_codec_core::CodecId::Theora` (already present in `vaco-codec-core`
before this crate existed), feature `codec-theora`, default build (GREEN —
Theora was designed from the ground up as royalty-free, derived from the
donated patent-unencumbered VP3, and is not listed as encumbered in
`planning/research/07-legal-patents-licensing.md`). `vaco-demux-ogg`'s
`OggCodec::Theora` branch was updated to attach `CodecId::Theora` to the
`CodecParameters` it builds — before this crate existed, that branch built
`CodecParameters::new(MediaType::Video)` with no codec attached at all,
which is the same one-line "the container never told any decoder what
codec this is" gap already found and fixed for JPEG-LS's `.jls` pipe in
`vaco-demux-image2`.

## Dependencies

`vaco-codec-core` (the decode protocol), `vaco-bitstream` (the MSB-first bit
reader, which matches Theora's own bitpacking convention, section 5, exactly
— no crate-local reader was needed), `vaco-frame`/`vaco-pixfmt` (the decoded
picture), `vaco-packet` (encoded packets), `vaco-limits` (allocation bounds).

## Testing

Unit tests cover: the identification header against the spec's own 240x48
worked example; the coded-order Hilbert curve against that same example's
printed block-index table, plus a full raster/coded round-trip over an
arbitrary grid; the 1D/2D inverse DCT's DC-only case; the Huffman
binary-tree parser (round trip, a single 0-bit-code entry, and an oversized
"comb" tree that must flag malformed rather than panic); the coefficient
token decoder for several representative tokens; the loop filter's response
function at its band edges; and `Setup`/`Ident`/extradata error paths
(truncated, garbage, or out-of-order input must fail cleanly, never panic).
A `theora_decode` fuzz target decodes arbitrary bytes as a keyframe packet
against a small fixed, valid, hand-built extradata blob, run twice through
the same decoder instance to exercise state carried across packets.
