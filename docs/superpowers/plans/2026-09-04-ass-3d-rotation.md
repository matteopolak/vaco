# ASS 3-D Rotation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render static ASS `\frx` and `\fry` with bounded perspective projection while preserving `\frz` and `\org`.

**Architecture:** Store all three Euler angles in `ResolvedStyle`. Convert them once into an X→Y→Z planar homography, determine a budget-checked output mask, and inverse-map destination pixel centers through that homography.

**Tech Stack:** Rust 2024, `vaco-ass`, `vaco-filter-subtitle`, `vaco-filter-text`, `vaco-limits`, ffmpeg-full/libass black-box fixtures.

---

### Task 1: Parser state

**Files:**
- Modify: `crates/filter/vaco-ass/src/plan.rs`

- [ ] Add a failing test whose first run has `angle_x = 30` and `angle_y = -45`, then whose `\r`-reset run has both angles at zero.
- [ ] Run `cargo test -p vaco-ass frx_and_fry_are_resolved_and_reset` and confirm it fails because the fields do not exist.
- [ ] Add `angle_x` and `angle_y` to `ResolvedStyle`, initialize them to zero, and parse `frx`/`fry` with the same malformed-value retention used by `frz`.
- [ ] Re-run the focused test and confirm it passes.

### Task 2: Projective mask geometry

**Files:**
- Modify: `crates/filter/vaco-filter-subtitle/src/ass_filter.rs`

- [ ] Add failing synthetic tests for positive X/Y direction, inverse round-trip, Z-only compatibility, and camera-plane-crossing bounds.
- [ ] Run the focused tests and confirm they fail because the projective transform is absent.
- [ ] Build coefficients for source-relative `(x,y)`:

```text
a = cos(z)*cos(y)
b = cos(z)*sin(y)*sin(x) + sin(z)*cos(x)
c = -sin(z)*cos(y)
d = -sin(z)*sin(y)*sin(x) + cos(z)*cos(x)
e = sin(y)
f = -cos(y)*sin(x)
U = F*(a*x + b*y)/(F + e*x + f*y)
V = F*(c*x + d*y)/(F + e*x + f*y)
```

- [ ] Inverse-map destination centers by solving the two linear equations induced by `U` and `V`; reject non-finite, singular, outside-mask, or behind-camera samples.
- [ ] Use projected corners for normal bounds and frame bounds when the corner denominators straddle the near plane. Allocate only through `AlphaMask::blank`.
- [ ] Replace the Z-only call with the unified projector and re-run the focused tests.

### Task 3: Real rendering oracles

**Files:**
- Create: `crates/filter/vaco-filter-subtitle/tests/data/frx60.ass`
- Create: `crates/filter/vaco-filter-subtitle/tests/data/fry60.ass`
- Create: `crates/filter/vaco-filter-subtitle/tests/data/frx60-org.ass`
- Modify: `crates/filter/vaco-filter-subtitle/src/ass_filter.rs`

- [ ] Commit the exact 320x240 `TILT` fixtures measured by the black box.
- [ ] Assert centered `\frx60` keeps width and halves height, centered `\fry60` halves width and keeps height, and moving the X-rotation origin down produces the measured perspective shift and shrink.
- [ ] Run the three real-render tests and tune only for independent font-metric tolerance, never rotation direction or displacement.

### Task 4: Documentation, provenance, and verification

**Files:**
- Modify: `docs/filter/vaco-ass.md`
- Modify: `docs/filter/vaco-filter-subtitle.md`
- Modify: `provenance/sources.toml`

- [ ] Record the public rotation/origin clauses, the 312.5-pixel black-box calibration, exact crop measurements, transform order, near-plane behavior, and the remaining #488 scope.
- [ ] Run Rustfmt, scoped tests, strict Clippy, the SSIM oracle, `provenance-check`, `comment-check`, and `dup-check` with a private target at `-j2`.
- [ ] Commit through a private index with clean-room trailers, atomically advance `main`, push, and add measured evidence to #488 without closing it.
