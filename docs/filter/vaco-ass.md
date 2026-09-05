# vaco-ass

## What it is

ASS/SSA subtitle script parsing and override-tag interpretation
(GitHub #487/FT-5.2, #488/FT-5.3). It stops short of drawing anything:
the crate's output is [`plan::EventPlan`], a renderer-agnostic
description of styled text runs, position, rotation origin and clip,
still in the script's own `PlayResX`/`PlayResY` coordinate space.
`vaco-filter-subtitle` scales an `EventPlan` to real frame pixels and drives
`vaco_filter_text::TextRenderer`; nothing in this crate touches a pixel.

Built from the informally-published ASS/SSA format documentation and by
comparing this crate's own parse against real `.ass` files — no libass
source was read.

## How it works

Four stages, one module each:

- `script::parse` — reads `[Script Info]`, `[V4+ Styles]`/`[V4 Styles]`
  and `[Events]` into a [`script::Script`].
- `tags::tokenize` — splits one event's `Text` field into literal-text
  and `{...}` tag-block [`tags::Item`]s, without interpreting any tag.
- `plan::plan_event_at` — interprets the tokenized tags against the
  event's [`style::Style`] into a point-in-time [`plan::EventPlan`]. The
  compatibility `plan_event` wrapper evaluates at the event start.
- `color::` — the `&HAABBGGRR` colour/alpha literal parser the other
  three modules share.

`plan::plan_event_at` implements the static tag set in full: `\b \i \u
\s \fn \fs \fscx \fscy \fsp \frx \fry \frz \fr \bord \xbord \ybord \shad \xshad
\yshad \blur \be \c \1c \2c \3c \4c \alpha \1a \2a \3a \4a \an \a \pos
\org \clip \r`. It resolves `\k`/`\K`/`\kf`/`\ko` into event-relative
centisecond intervals on text runs, carrying secondary-fill and instant,
sweep, or delayed-outline state to the renderer. `\p<n>` payloads are kept
as drawing runs with their coordinate divisor and `\pbo` baseline offset. It
also evaluates the four legal `\t` forms at the
requested frame time: `\t(tags)`, `\t(accel,tags)`,
`\t(t1,t2,tags)`, and `\t(t1,t2,accel,tags)`. Times are milliseconds
relative to the event start; progress is clamped and raised to the
positive finite acceleration exponent. Numeric style fields, colours,
alpha, and X/Y/Z rotation interpolate without retaining an animation
list. A zero-length interval steps at its end, while invalid acceleration
leaves the current style unchanged. Nested `\t` and line-level tags inside
a transform are ignored deliberately, keeping evaluation non-recursive and
placement unchanged.

A second group is now resolved at the requested event time: `\move` linearly
interpolates between its endpoints (with optional event-relative `t1,t2` and
clamping outside that interval), while `\fad` and the two-segment `\fade`
forms adjust all four rendered colour alphas. `\fax`/`\fay` shear is parsed
and ignored. `\frx`/`\fry`/
`\frz`/`\fr` carry static X/Y/Z angles on each run and `\org` carries the
optional line rotation origin; `vaco-filter-subtitle` projects them when
it rasterises the plan. See
`plan.rs`'s own doc for the full, current list — it is the authority,
not this file.

## How to change it

- A new static tag: add a case in `plan::plan_event_at` and extend
  [`plan::ResolvedStyle`]/[`plan::TextRun`] if it needs a new field on
  the plan.
- Movement and fades belong in `plan_event_at`, where the event-relative
  timestamp and duration are available. Keep their state bounded to one
  motion/fade value; do not retain a per-frame animation list.
- Rotation geometry belongs in `vaco-filter-subtitle`; this crate keeps
  `\frx`/`\fry`/`\frz` and `\org` in script coordinates so the renderer
  can scale the pivot and camera distance exactly once.
- Add a `\t`-animatable run-style field in `plan.rs`'s bounded
  `apply_transform_style_tag` and `interpolate_style` pair. Placement and
  animated clipping need their own point-in-time line state; do not make
  nested transforms recursive.
- A new override tag the tokenizer has never seen: `tags::tokenize`
  already passes any `name`/`arg` pair through uninterpreted, so only
  `plan.rs` needs a new match arm.

## Configuration

None — this crate takes a script's bytes and returns a `Result`; no
flags, env vars or constants gate its behaviour.

## Dependencies

In actual use: only `vaco-core` (`Rgba` for colour, `Duration` for
timing). The transform timing formula, three rotation directions, and
`\org` semantics follow Aegisub's published ASS override-tag documentation
and were cross-checked against ffmpeg-full 9.0.1 with libass 0.17.5 as a
black box. For the exact 320x240 Arial 48 `TILT` transform fixture, the
black-box visible bounds change from `88x31` before the interval to `76x76`
at its midpoint and `31x88` after it. `Cargo.toml` also
declares `vaco-limits`, `vaco-color`, `vaco-pixfmt`, `vaco-frame`,
`vaco-format-subtitle`, `vaco-filter-draw` and `vaco-filter-text`, but
no current source file in this crate imports any of them — check with
`cargo machete`-style dead-dependency tooling before assuming a given one is load-bearing. Driving
`vaco-filter-text::TextRenderer` to actually rasterise an `EventPlan`
is `vaco-filter-subtitle`'s job, one layer up, not this crate's.
