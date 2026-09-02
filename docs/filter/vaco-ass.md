# vaco-ass

## What it is

ASS/SSA subtitle script parsing and override-tag interpretation
(GitHub #487/FT-5.2, #488/FT-5.3). It stops short of drawing anything:
the crate's output is [`plan::EventPlan`], a renderer-agnostic
description of styled text runs, position and clip, still in the
script's own `PlayResX`/`PlayResY` coordinate space. `vaco-filter-subtitle`
scales an `EventPlan` to real frame pixels and drives
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
- `plan::plan_event` — interprets the tokenized tags against the
  event's [`style::Style`] into an [`plan::EventPlan`].
- `color::` — the `&HAABBGGRR` colour/alpha literal parser the other
  three modules share.

`plan::plan_event` implements the *static* tag set in full: `\b \i \u
\s \fn \fs \fscx \fscy \fsp \frz \fr \bord \xbord \ybord \shad \xshad
\yshad \blur \be \c \1c \2c \3c \4c \alpha \1a \2a \3a \4a \an \a \pos
\org \clip \r`. A second group is recognised but not animated — applied
as a coarse, static approximation rather than a silent drop:
`\t(...)` applies its last argument's tags immediately; `\move` uses
its start point as a static `\pos`; `\fad`/`\fade` are parsed and
ignored (full opacity for the whole event); `\k`/`\kf`/`\ko`/`\K`
karaoke tags are parsed and ignored (no highlight sweep); `\p<n>`
vector drawing suppresses its own text run rather than leaking raw
drawing syntax; `\frx`/`\fry`/`\fax`/`\fay` (3-D rotation/shear) are
parsed and ignored, only `\frz`/`\fr` (2-D) is applied; `\org` is
stored but inert without `\frx`/`\fry`. See `plan.rs`'s own doc for the
full, current list — it is the authority, not this file.

## How to change it

- A new static tag: add a case in `plan::plan_event` and extend
  [`plan::ResolvedStyle`]/[`plan::TextRun`] if it needs a new field on
  the plan.
- Animating a tag currently applied statically (`\t`, `\move`, `\fad`):
  `EventPlan` would need a time-varying representation instead of a
  single resolved value — a renderer-shape change, not a one-line fix.
  Do it in `plan.rs` and update this doc's gap list in the same commit.
- A new override tag the tokenizer has never seen: `tags::tokenize`
  already passes any `name`/`arg` pair through uninterpreted, so only
  `plan.rs` needs a new match arm.

## Configuration

None — this crate takes a script's bytes and returns a `Result`; no
flags, env vars or constants gate its behaviour.

## Dependencies

In actual use: only `vaco-core` (`Rgba` for colour, `Duration` for
timing). `Cargo.toml` also declares `vaco-limits`, `vaco-color`,
`vaco-pixfmt`, `vaco-frame`, `vaco-format-subtitle`, `vaco-filter-draw`
and `vaco-filter-text`, but no current source file in this crate
imports any of them — check with `cargo machete`-style dead-dependency
tooling before assuming a given one is load-bearing. Driving
`vaco-filter-text::TextRenderer` to actually rasterise an `EventPlan`
is `vaco-filter-subtitle`'s job, one layer up, not this crate's.
