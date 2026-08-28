# `vaco-codec-dsp-ratecontrol` — encoder bitrate control

---

## 1. What it is

D-14 (#146): constant-quality, CBR and VBR bitrate control, driven one
frame at a time via `RateController::next_qscale` / `RateController::report`.

**This crate is a policy, not a bit-exact function**, and is meant to be
judged that way. The repository owner has ruled directly (2026-08-28,
recorded in `AGENT-CONSTRAINTS.md`) that there is no single correct
rate-control curve to reproduce, and that improving on any reference is
explicitly welcome. So: judge this crate (and re-judge any change to it) on
**measured** behaviour — does it reach the target bitrate, does it avoid
visible quantiser oscillation, does it avoid buffer starvation/overflow —
never on whether it matches a specific formula from anywhere else.

## 2. How it works

### 2.1 `qscale`: a codec-agnostic quantisation scale, not a QP index

`next_qscale` returns an `f64` "quantisation scale factor" rather than an
integer in any particular codec's own QP range, because D-14 serves
multiple, not-yet-built encoders (VP8 and VP9 encode, per plan 15 §7.4, are
both blocked on this crate) whose QP-to-quantiser-step relationships are
not comparable. The only contract: `bits ≈ K · complexity / qscale`
approximately and monotonically — a caller maps the returned `qscale` onto
its own codec's nearest QP.

### 2.2 The three pieces of the model

1. **An adaptive complexity constant** (`complexity_k`): an exponential
   moving average of `bits · qscale / complexity` implied by each reported
   frame. This is what lets the controller predict a `qscale` for a new
   frame's complexity without knowing the target codec's actual
   rate-distortion curve in advance — it learns the curve's scale from
   what really happened, one frame at a time.
2. **A virtual buffer**: accumulates `bits_produced − target_bits_per_frame`
   every frame (the same shape MPEG/H.26x-family rate control has used for
   decades, described here from first principles — D7's clean-room rule
   means this was derived from the textbook feedback-control idea, not
   read out of any implementation). Under CBR, `next_qscale` corrects
   toward this buffer's target level in proportion to how far off it is.
3. **A per-frame step clamp** (`MAX_QSCALE_STEP_UP`/`MAX_QSCALE_STEP_DOWN`):
   whatever the model above computes, `qscale` never moves by more than a
   fixed multiplicative step from the previous frame. This is the direct
   mechanism against visible quality pumping — see
   `tests::qscale_never_changes_by_more_than_the_configured_step`.

### 2.3 CBR vs. VBR is a difference in *baseline*, not just in gain

The first implementation of VBR here just used a softer version of CBR's
buffer-feedback gain. It was wrong, and wrong in an instructive way: with a
`prev_qscale`-based baseline, *any* nonzero feedback gain compounds
multiplicatively every frame, so under sustained one-directional content
(a long hard scene) `qscale` grew geometrically without bound — VBR ended
up *quieter* than CBR on hard content, the opposite of the intended
behaviour, and `tests::vbr_respects_its_peak_bitrate_under_sustained_hard_content`
caught it directly (achieved bitrate came out at roughly a third of the
*target*, let alone the peak).

The fix, and the actual design:

- **CBR's baseline already erases complexity**: `complexity_k · complexity
  / target_bits_per_frame` is *defined* to produce roughly `target_bits_per_frame`
  bits regardless of how complex the frame is. Buffer feedback on top of
  that is a secondary correction for the model's own prediction error.
- **VBR's baseline is `prev_qscale`** — quality stays where it settled,
  with **no buffer feedback at all**. Bits then float naturally with
  content: easy content costs less (and those savings are never "clawed
  back"), hard content costs more.
- **VBR's only actual limiter is the peak cap**: `qscale.max(complexity_k
  · complexity / peak_bits_per_frame)`. This is stable rather than
  compounding, because it is recomputed fresh from `complexity_k` (an
  independent per-frame estimate) every call, not from VBR's own previous
  output.

### 2.4 Defending against a caller's own bad config

Every `RateControlConfig` field is exactly the kind of externally-supplied
value (CLI options, an eventual config file) that can arrive malformed:
`NaN`, infinite, or `min_qscale > max_qscale`. `f64::clamp` panics on the
last two. `RateControlConfig::sanitized` (called once, inside
`RateController::new`) repairs all of this before anything downstream reads
it — swapping an inverted range, replacing a non-finite bound with this
crate's own default. Fuzzed directly (`dsp_ratecontrol` target, 8.5M execs
in 31s, no crashes, no artifacts) across the full space of malformed
configs and arbitrary per-frame complexity/bit reports, not just the
hand-picked cases a unit test would think to construct.

## 3. How to change it

- **Any change to the gain constants or baseline formulas must be
  re-validated against `tests`**, not read for plausibility. The VBR
  compounding bug in §2.3 *looked* like a reasonable, smaller version of
  CBR's correction and still ran away; the tests, not code review, are
  what this crate is actually checked against.
- **A lambda-weighted mode decision** (letting rate control bias which
  candidate a motion search or mode decision picks, not just which
  `qscale` to encode at): out of scope here by design — D-13
  (`vaco-codec-dsp-me`) and this crate were assigned to be built
  independently, so a lambda would be composed by the caller from this
  crate's `qscale` rather than added here.
- **Two-pass encoding**: not implemented. A first pass would need to
  accumulate a complexity trace across the whole sequence before encoding;
  this crate's `complexity_k` EMA is a single-pass, causal estimate only.
- **A minimum-bitrate floor** (the mirror of the peak cap, preventing VBR
  from spending arbitrarily little on very easy content): not implemented;
  none of D-14's callers have asked for one yet, and the peak cap's shape
  is the template to copy if one is needed later.

## 4. Configuration

`RateControlConfig`'s fields are the whole surface: `mode`,
`target_bitrate_bps`, `peak_bitrate_bps` (VBR only, defaults to `1.5 ×`
target), `fps`, `vbv_buffer_bits` (defaults to one second of target
bitrate), `min_qscale`/`max_qscale` (default `0.1..=128.0`, a deliberately
wide, codec-agnostic range), `initial_qscale`, `constant_qscale`. No env
vars or feature flags.

## 5. Dependencies

`vaco-core` (`Rational`, for `fps`). No `provenance/` table: the virtual
buffer and complexity-EMA model are original composition of a textbook
feedback-control idea (D7 clean-room), not transcribed from any
specification or reference implementation.

## Measured (simulation — see `tests` for the exact scenarios)

There is no real encoder wired up yet to measure this against actual video
(VP8/VP9 encode are what this crate unblocks), so every number below is
from this crate's own synthetic `bits = true_k · complexity / qscale`
simulation, with per-frame multiplicative noise in `[0.9, 1.1]` so the
controller is never handed the exact answer:

| Scenario | Result |
|---|---|
| CBR, 400 frames, smoothly varying complexity | achieved bitrate within 15% of target |
| `qscale` step, 200 frames of deliberately jumpy complexity | every consecutive step within the configured multiplicative bound (by construction, verified) |
| Scene cut (steady state -> 6× complexity for 120 frames) | buffer never leaves its configured bounds; settles back within 60% of its target level within ≤ 40 frames |
| VBR vs. CBR, 200 frames of easy constant content | VBR bitrate < 90% of CBR's, CBR within 15% of target |
| VBR, 250 frames of sustained hard content (peak = 1.8× target) | achieved bitrate exceeds target by > 10% and stays within 15% of peak |

Re-run `cargo test -p vaco-codec-dsp-ratecontrol` (numbers are asserted, not
just printed) and `cargo bench -p vaco-codec-dsp-ratecontrol` (per-call
cost: ~15ns for `next_qscale` + `report` together, on this machine) before
trusting these on a different machine or after a change — per-issue policy
components are exactly where "it looks right" and "it measures right" most
often diverge.
