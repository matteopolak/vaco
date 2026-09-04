# ASS 3-D Rotation Design

## What it is

This slice completes static ASS `\frx` and `\fry` rendering while preserving the existing `\frz`/`\fr` behavior. It does not add animation interpolation, karaoke, drawings, or shear.

## How it works

`vaco-ass` records X-, Y-, and Z-axis angles on each resolved text run. `vaco-filter-subtitle` rasterizes a line once, then maps its alpha mask through one projective homography around the explicit `\org` point or the aligned line anchor.

The plane is rotated in X→Y→Z order. Positive X rotation sends the top edge into the screen; positive Y rotation sends the right edge into the screen; positive Z remains counterclockwise in screen coordinates. Projection uses the ASS-compatible camera distance of 312.5 script pixels, scaled by the frame's Y scale. This reproduces the reference's centered cosine foreshortening and its shifted-origin perspective.

The renderer inverse-maps destination pixel centers and bilinearly samples source coverage. For an ordinary projection, transformed corners define the output rectangle. If the source plane crosses the camera plane, the output rectangle falls back to the video frame. Every output allocation still goes through `TextRenderer`'s budget; samples whose corresponding source point is behind the camera are transparent.

## How to change it

Change tag state in `crates/filter/vaco-ass/src/plan.rs` and projection geometry in `crates/filter/vaco-filter-subtitle/src/ass_filter.rs`. Keep all three rotations in the same transform so ordering cannot drift. A new transform must have a parser/reset regression, a synthetic direction regression, and a real black-box geometry fixture.

## Configuration

There are no flags or environment variables. The 312.5 script-pixel camera distance is a compatibility constant, converted to frame pixels once with `PlayResY` scaling.

## Dependencies

Semantics follow Aegisub's published ASS override-tag documentation. Geometry fixtures are measured with ffmpeg-full 9.0.1 and libass 0.17.5 as a black box. Allocation uses `vaco-limits`; rasterization and mask compositing use `vaco-filter-text`.
