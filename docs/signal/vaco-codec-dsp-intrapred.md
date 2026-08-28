# `vaco-codec-dsp-intrapred`

Layer 3. Generic DC/planar/angular-projection intra prediction primitives
shared across codec families (D-09, issue #126).

## What it is

H.264, VP8 and VP9 already ship their own complete, locally-implemented
intra predictors in this tree (`vaco-codec-h264::intra`,
`vaco-codec-vp8::predict`, `vaco-codec-vp9::predict`) and are not touched
here. HEVC's 35-mode angular scheme and AV1's directional-prediction scheme
share underlying arithmetic that this crate implements once:

- `dc_predict` — the average of the available top/left reference samples
  (every format's DC mode, verbatim: an average is not an authorial
  choice).
- `planar_predict` — HEVC §8.4.4.2.4's exact bilinear-corner formula.
- `angular_project` — the "project along a signed angle (1/32-sample
  units) and linearly interpolate between the two nearest reference
  samples" core both HEVC §8.4.4.2.6 and AV1 §7.11.2.4 parameterise the
  same way.

## How it works

`angular_project` computes one row (or, by the caller transposing its
inputs/outputs, one column) at a time: `step = (pos + 1) * angle`, split
via `div_euclid`/`rem_euclid` (floor division, so it is correct for
negative angles too) into an integer offset and a 5-bit fractional weight.
Zero angle degenerates to an exact copy with no interpolation, which is
tested directly rather than assumed.

**Read the crate root doc's confidence-level section before trusting this
byte-exact against a real decoder.** `angular_project` was checked against
the properties the arithmetic must have (zero-angle copies exactly, a
linear reference ramp interpolates exactly, the midpoint weight is an exact
average) — tier 1 self-consistency in the project's own three-tier sense,
not a line-by-line check against a primary ITU-T/AOMedia edition. A caller
wiring this into HEVC or AV1 should do that check before relying on
byte-exact output.

## How to change it

- A format's mode-to-angle table, reference-sample smoothing/filtering
  (HEVC's strong intra smoothing) and chroma cross-component prediction
  belong in that format's own crate — deliberately out of scope here, and
  listed as such in the crate root doc.
- If you verify `angular_project`'s indexing against a primary
  specification edition, update the crate root doc's confidence-level
  section to say so — that section exists specifically so the next reader
  does not have to guess which check was actually done.

## Configuration

None — pure functions, no state, no allocation.

## Dependencies

None beyond the standard library. No current caller in this tree yet
(HEVC and AV1 decode are not implemented here); `vaco-codec-dsp-lpc` and
`vaco-codec-dsp-fmtconvert` are in the same position for the same reason
— this batch builds the shared primitive ahead of the codec that needs it,
per the epic's own "do early" framing.
