# ASS Transform Animation Design

## What it is

This slice evaluates ASS `\t(...)` transform tags at the timestamp of the
rendered frame. It animates the bounded style state already represented by
`ResolvedStyle`, including X/Y/Z rotation, colours, alpha, size, scale,
spacing, border, shadow and blur. It does not add animated clipping, movement,
fades, karaoke or vector drawings.

## How it works

`vaco-ass` exposes `plan_event_at(script, event, now)` alongside the existing
start-time `plan_event` compatibility wrapper. A transform snapshots the style
at that point in the override stream, applies its supported nested tags to a
target clone, and interpolates the two styles at event-relative time. This
produces a normal point-in-time `EventPlan`; it does not retain a list of
animations or allocate per-frame geometry.

The four legal forms are `\t(tags)`, `\t(accel,tags)`,
`\t(t1,t2,tags)` and `\t(t1,t2,accel,tags)`. Times are milliseconds from the
event start. Omitted times use zero and the event duration. Progress is clamped
to `[0,1]` and raised to the positive finite acceleration exponent. A
zero-length interval steps to the target at its end; an invalid acceleration
leaves the snapshot unchanged. Nested `\t` and line-level tags are ignored
inside a transform, so malformed input cannot recurse or change placement.

Parsing finds the first nested backslash rather than splitting the whole
argument on commas. This preserves commas inside nested tags such as `\clip`,
even though animated clipping itself remains out of scope.

## How to change it

Add an animatable field in `crates/filter/vaco-ass/src/plan.rs` by allowing its
tag in the transform-target matcher and adding it to `interpolate_style`.
Placement and path animation should use a separate bounded representation,
because `EventPlan` stores those values once per event rather than per run.
Keep nested transforms non-recursive.

## Configuration

There are no flags or environment variables. Event time is supplied as
`vaco_core::Duration`; ASS transform times are interpreted as milliseconds.

## Dependencies

Semantics follow Aegisub's published ASS override-tag documentation. The
before/midpoint/after rotation bounds are measured with ffmpeg-full 9.0.1 and
libass 0.17.5 as a black box. Rendering remains in `vaco-filter-subtitle` and
uses the existing bounded mask projection path.
