# `vaco-codec-opus`

Opus decode (RFC 6716, as amended by RFC 8251): range decoder, CELT, SILK,
hybrid mode, and the packet-framing glue multistream/surround needs.
Issues #313 (C-22, range decoder + framing), #314 (C-23, CELT), #315 (C-24,
SILK) and #316 (C-25, hybrid/multistream/PLC/FEC/integration).

## What it is

A from-scratch Opus decoder built directly against RFC 6716's Appendix A
reference C implementation (a complete, separately-licensed embedded
decoder/encoder, extracted locally for this work — see `provenance/sources.toml`'s
`rfc-6716`/`rfc-8251` entries) and RFC 6716's own prose. It sits on top of
`vaco-parse-opus`, which already owned packet/TOC framing, channel mapping
and the `OpusHead`/`OpusTags` records; this crate does not duplicate that —
`OpusDecoder` (`decoder.rs`) drives `vaco-parse-opus::OpusPacket` and adds
the actual bitstream decode: the range coder, CELT, SILK and their hybrid
combination.

## How it works

- `range.rs` — the range decoder shared by SILK and CELT within one Opus
  frame (`RangeDecoder`), plus the Laplace model SILK's coarse-energy-alike
  paths use.
- `celt/` — `range.rs`'s consumer for the transform-coded half: static
  tables (`tables.rs`), the bit allocator (`rate.rs`), PVQ combinatorics and
  spreading/stereo DSP (`pvq.rs`), the band-energy and shape decode
  (`bands.rs`, `energy.rs`) and the top-level per-frame driver, IMDCT and
  de-emphasis (`mod.rs`).
- `silk/` — the speech half: static tables (`tables.rs`), NLSF decode and
  conversion to LPC coefficients (`nlsf.rs`), the per-subframe gain/LTP/
  excitation/LPC-synthesis pipeline (`decode.rs`), stereo mid/side handling
  and multi-subframe orchestration (`mod.rs`), and an original (not from the
  reference) windowed-sinc upsampler (`resample.rs`) for SILK's internal
  rate to the shared 48 kHz output.
- `decoder.rs` — `OpusDecoder`/`StreamDecoder`: per-stream CELT+SILK state,
  hybrid combination (SILK for bands below the 8 kHz crossover, CELT from
  band 17 up, RFC 6716 §4.5), and channel mixing for multistream layouts.

Both CELT and SILK carry their own module-doc explaining the numeric
convention each uses (CELT: RFC 6716's own float-build convention, where
almost all of the reference's Q-format fixed-point macros collapse to plain
`f32` arithmetic; SILK: everything carried in "PCM-adjacent" scale,
`[-32768, 32767]`-ish, normalized to `[-1, 1]` once at final output — SILK's
own reference has no float build to borrow this simplification from, so it
was re-derived by hand from the fixed-point Q-format arithmetic).

## Known gaps (disclosed, not silently patched)

**Decode correlation against a reference decoder (ffmpeg/libopus) is low.**
This is the most important thing to know before relying on this crate.
Differential testing against `ffmpeg`-encoded real Opus streams (CELT-only
mono/stereo, SILK-only mono, hybrid mono) shows:

- The decoder never panics and never desyncs the range coder — every frame
  in every test stream decodes to completion, sample counts match what the
  bitstream declares, and clippy's `unwrap_used`/`expect_used`/`panic`/
  `indexing_slicing` lints (crate-level `#[allow]`s aside — see below) are
  clean.
- CELT-only output has plausible RMS energy (within ~20% of the reference)
  after this pass's IMDCT/overlap-add fix (see below), but sample-domain
  correlation against the reference stays low (roughly 0.03–0.06, scanning
  a wide range of sample offsets) — energy is approximately right, shape
  is not. `celt/pvq.rs`'s combinatorial pulse-vector decode
  (`decode_pulses`/`cwrsi`/`ncwrs_urow`) was spot-checked line-by-line
  against `cwrs.c` and matches; the remaining defect is most likely in
  `bands.rs`'s `quant_band` (the band-recursive theta-split/energy-mix
  driver, the single most complex function in the crate) or in the bit
  allocator's exact allocation curve, neither of which got the same
  line-by-line re-check in the time available.
- SILK-only output is under-scaled relative to the reference (~10x quieter)
  and similarly uncorrelated in shape.
- Hybrid output has a severe, unexplained amplitude blow-up (tens of times
  louder than the reference) not present in the standalone SILK or CELT
  paths — a bug specific to the hybrid combination or to the SILK
  configuration hybrid mode forces (wideband, internally), not chased down
  in the time available.

Three real, confirmed bugs were found and fixed this pass:

1. A panic in `celt/pvq.rs`'s `stereo_merge` (`copy_from_slice` on
   mismatched-length slices) — hit by any CELT stereo frame with unequal
   mid/side lengths.
2. A 65536x gain-scale bug in `silk/decode.rs`'s `gains_dequant` (missing a
   `/65536.0` converting the reference's `Gains_Q16` fixed-point format to
   this crate's real-unit convention) that saturated every SILK sample at
   the PCM clip boundary.
3. A structurally wrong CELT overlap-add: the reference's `clt_mdct_backward`
   (`celt/mdct.c`) computes only `n2 + overlap` output samples through a
   fast N/4-point-FFT path with its own windowing/mirroring convention, not
   a naive `2*n2`-sample IMDCT windowed at both ends the way an earlier
   version of `celt/mod.rs` assumed. The fix reuses `vaco_tx`'s plain full
   IMDCT (verified against `vaco_tx::reference::imdct` by
   `tests/mdct_sanity.rs`) via the identity `f2[idx] = y[n4 + idx]`
   (`f2` being `clt_mdct_backward`'s de-shuffled intermediate, `y` the full
   IMDCT output) — confirmed numerically against a literal transliteration
   of the reference's pointer-walking mirror loops before being adopted,
   and documented in `celt/mod.rs`'s IMDCT block.

None of the three fully closed the correlation gap on its own; each was a
real defect worth fixing regardless, but at least one more structural bug
remains in each of CELT, SILK and the hybrid combination.

**Post-filter (comb filter) is read but not applied.** `celt/mod.rs`'s
module doc covers this: the post-filter's `octave`/`period`/`gain`/`tapset`
fields are decoded (skipping the read would desync every following symbol)
but the 5-tap comb filter itself (RFC 6716 §4.3.7.1) is never run. It mainly
sharpens low-bitrate voiced content; its absence does not desync anything.

**`clippy::indexing_slicing` is disabled crate-wide**, not swept to
bounds-checked (`.get()`) access. This is a disclosed, un-triaged gap
covering the CELT/SILK recursion (roughly 160 sites at the time this was
last counted) — some are almost certainly safe by a same-function
invariant (a loop bound already checked against the same length), others
are not yet reasoned about at all. Sweeping this crate to `.get()`-based
access throughout is unstarted follow-up work, not something this doc can
currently point at specific proven-safe vs. open sites for.

**PLC (packet loss concealment) and DTX/FEC are not implemented.** A missed
or corrupted packet is not synthesized from surrounding context; only
straight-line decode of packets actually received is covered.

**Bit-exactness against the reference decoder was not attempted.** RFC 6716
mandates bit-exact CELT range-coder/PVQ decisions but leaves float-domain
DSP (most of CELT, and this crate's from-scratch SILK re-derivation) as an
implementation choice; this crate uses ordinary `f32` arithmetic throughout
rather than replicating the reference's fixed-point rounding.

## Configuration

No feature flags or environment variables gate behavior (an earlier
`OPUS_DEBUG` environment-variable debug-print hook used during this pass's
investigation was removed before landing). `vaco-component.toml` registers
the `opus` decoder unconditionally (not `encumbered`, per D9 — Opus carries
no patent claims the project's legal review flagged).

## How to change it

- Start from RFC 6716's Appendix A source for anything table- or
  algorithm-shaped; the RFC's own prose is comparatively sparse and mostly
  useful for the framing/mode-selection narrative, not exact constants.
  `provenance/vaco-codec-opus.toml` names which reference source file each
  transcribed table came from.
- The CELT and SILK module docs (`celt/mod.rs`, `silk/decode.rs`) explain
  the numeric-scale conventions in enough detail to avoid re-introducing a
  scale bug like the two fixed this pass; read them before touching gain or
  energy code.
- `tests/mdct_sanity.rs` is a permanent, self-contained regression test
  pinning `vaco_tx`'s FULL_IMDCT behavior against a direct O(n²) reference
  formula — keep it passing if `vaco_tx`'s API or this crate's IMDCT usage
  changes, since the whole CELT overlap-add derivation in `celt/mod.rs`
  depends on that identity holding.
- There is no committed differential-decode test against `ffmpeg` output
  (the ones used during this investigation lived in a scratch directory
  with machine-specific absolute paths and were not suitable to commit).
  Anyone continuing the correlation investigation above should regenerate
  fixtures with `ffmpeg -f lavfi -i <tone-or-noise-source> -c:a libopus
  <file>.ogg` and decode both with `ffmpeg -i <file>.ogg -f f32le
  <file>.ref.f32` and this crate, then compare — a real encoder plus a
  faithful reference decoder round trip is a far stronger signal than
  hand-derived unit tests for a codec this size.

## Dependencies

`vaco-parse-opus` (packet/TOC framing, `OpusHead`/`OpusTags`), `vaco-tx`
(the IMDCT), `vaco-frame`/`vaco-chlayout`/`vaco-sampfmt` (output framing),
`vaco-codec-core` (the `Decoder` trait and registry glue), `vaco-limits`
(allocation budgets). No FFI, no `-sys` crates, no encumbered dependencies.
