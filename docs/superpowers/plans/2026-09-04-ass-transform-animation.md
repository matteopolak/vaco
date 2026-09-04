# ASS Transform Animation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Evaluate supported ASS `\t(...)` style and rotation transforms at the rendered frame time.

**Architecture:** Add a time-aware event planner which snapshots the current resolved style, computes one bounded transform target, and returns the style interpolated at that timestamp. Keep the existing planner as a start-time wrapper and route the renderer through the new entry point.

**Tech Stack:** Rust 2024, `vaco-ass`, `vaco-filter-subtitle`, `vaco-core::Duration`, ffmpeg-full/libass black-box fixtures.

---

### Task 1: Time-aware transform planning

**Files:**
- Modify: `crates/filter/vaco-ass/src/plan.rs`
- Modify: `crates/filter/vaco-ass/src/lib.rs`

- [ ] Add failing tests for all four legal forms, before/midpoint/after timing, acceleration, zero-duration and invalid acceleration.
- [ ] Add failing tests proving commas in nested tag arguments survive, nested transforms do not recurse, and `\r` resets following runs.
- [ ] Run the focused `vaco-ass` tests and record the RED caused by the missing `plan_event_at` entry point.
- [ ] Add event timing to the cursor, parse the numeric prefix before the first nested backslash, and interpolate only finite supported style fields.
- [ ] Export `plan_event_at`, leave `plan_event` as the start-time wrapper, and rerun the focused tests.

### Task 2: Renderer time routing and real oracle

**Files:**
- Modify: `crates/filter/vaco-filter-subtitle/src/ass_filter.rs`
- Create: `crates/filter/vaco-filter-subtitle/tests/data/t-frz90.ass`

- [ ] Add a failing real-render regression for a 0-to-90-degree transform at its midpoint.
- [ ] Route `render_at` through `plan_event_at(script, event, t)`.
- [ ] Compare the 320x240 Arial 48 `TILT` bounds against the black-box observations: `88x31` before the interval, `76x76` at its midpoint and `31x88` after it.
- [ ] Confirm the existing projective rotation and SSIM regressions remain green.

### Task 3: Documentation, provenance and delivery

**Files:**
- Modify: `docs/filter/vaco-ass.md`
- Modify: `docs/filter/vaco-filter-subtitle.md`
- Modify: `provenance/sources.toml`

- [ ] Document point-in-time planning, legal transform forms, bounded behavior, oracle measurements and remaining animation/karaoke/drawing gaps.
- [ ] Append only the exact Aegisub specification and black-box source entries required by this slice after shared provenance edits settle.
- [ ] Run Rustfmt, scoped tests, strict Clippy, the SSIM oracle, `provenance-check`, `comment-check` and `dup-check` in the private target at `-j2`.
- [ ] Commit through a private index with clean-room trailers, atomically advance `main`, push, and comment measured evidence on #488 without closing it.
