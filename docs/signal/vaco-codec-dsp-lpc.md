# `vaco-codec-dsp-lpc`

Layer 3. Linear predictive coding: autocorrelation, Levinson-Durbin
analysis, coefficient quantisation, fixed-point synthesis (D-07, issue
#257).

## What it is

The classic LPC analysis/synthesis pair that a FLAC-style encoder needs:
turn a windowed sample block into predictor coefficients, quantise them to
the small-integer-plus-shift form a bitstream actually stores, and
reconstruct samples from residual + quantised coefficients + history.

FLAC (`vaco-codec-flac`), ALAC (`vaco-codec-alac`) and Opus SILK
(`vaco-codec-opus`) all code audio this way in spirit, but each already
ships its own working, format-specific implementation (FLAC's encoder only
does fixed predictors so far; ALAC's is a proprietary adaptive filter with
no analysis step; SILK's LPC comes from NLSF-quantised coefficients via a
step-up recursion, not autocorrelation). None of them currently call this
crate — see the crate root doc for exactly why each is a genuinely
different algorithm rather than a case of one shared function serving
three formats.

## How it works

- `analysis.rs`: `autocorrelate` computes `r[0..=max_lag]` from a sample
  window; `levinson_durbin` runs the textbook recursion up to `max_order`,
  keeping coefficients, reflection coefficients and residual error *at
  every intermediate order* in one pass, so an encoder can pick the
  cheapest order from the error curve without re-running the recursion.
- `quantize.rs`: turns `f64` coefficients into `i32` + a shared shift,
  choosing the shift that maximises precision usage without overflow and
  carrying each coefficient's rounding error into the next (error-feedback
  quantisation).
- `synthesis.rs`: `predict` computes `(sum(qcoeffs[i] * history[i])) >>
  shift` in `i128` internally (saturating back to `i64`) — the exact
  arithmetic IETF RFC 9639 §9.2.6 defines for FLAC's `LPC` subframe.
  `synthesize` runs it across a full block, given warm-up samples and a
  residual array.

All state fits on the stack: `MAX_ORDER = 32` (FLAC's own cap), so every
buffer is a fixed-size array rather than a `Vec`.

## How to change it

- A new format's analysis needs (windowing, a different quantisation rule)
  belongs in that codec's own crate, not here — this crate stays limited to
  the parts genuinely shared across formats (see the crate root doc's "not
  implemented here, on purpose" list: windowing, NLSF/step-up conversion,
  coefficient stabilisation).
- `predict`/`synthesize` are defined for the full `i32` domain including
  adversarial coefficients and history (an entropy-decoded bitstream is
  attacker-controlled) — any change here must keep the `i128`-accumulator
  and `saturating_add` behaviour; a fuzz regression already caught the i64
  overflow the first draft had.

## Configuration

None — pure functions and a `MAX_ORDER` constant, no state.

## Dependencies

None beyond the standard library. IETF RFC 9639 (FLAC), fetched directly
and registered in `provenance/sources.toml` as `rfc-9639`, backs the
synthesis arithmetic. Autocorrelation and Levinson-Durbin are textbook
signal processing (Levinson 1947 / Durbin 1960) with no single
implementation to derive from.
