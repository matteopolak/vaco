# vaco-filter-video-format

Video format/metadata filters (FT-4.4, GitHub epic #54, format child issue):
`format`, `setsar`, `setdar`, `setfield`, `setrange`, `fps`, `framerate`.

## What it is

Seven filters that change a link's *declared meaning* (pixel format,
aspect ratio, field order, colour range, frame rate) rather than its pixel
content. Each is a module exposing `pub const DESC: FilterDesc` and a
crate-private `create`, dispatched by
[`registry::FormatRegistry`](../../crates/filter/vaco-filter-video-format/src/registry.rs).

## How it works

### `format` declares a constraint; it does not convert

`format`'s `Filter::filter_frame` is a pure passthrough. Its actual job is
the `NodeFormats` it builds at `create` time: `Constraint::OneOf(list)` on
both its input and output pads, tied together. Whatever inserts converters
(the graph builder, outside this crate) does the real work if the upstream
link is not already in the requested set.

### `setsar`/`setdar` never read each other's output

Both are metadata-only: `setsar` overwrites the link's SAR directly;
`setdar` computes `sar = dar * height / width` and overwrites SAR with
*that*. Neither reads whatever SAR was already on the link, which means
chaining them clobbers, it doesn't combine — see the measured table below.

### `fps` — zero-order hold, never blending

One frame is always held one arrival behind. On the next frame's arrival,
the held frame is emitted once per output slot from the last slot produced
up to (but not including) the new frame's slot: duplicated if that span is
more than one slot, silently dropped entirely if the new frame landed on a
slot that has already been produced. `fps.rs`'s doc has the full measured
timeline. `framerate.rs` reuses this exact mechanism under the reference's
different option names — it does **not** implement motion-compensated
blending (that needs `vaco-filter-vdsp`, which this crate does not depend
on), so treat it as "keeps a constant rate," not as a fidelity match.

## The measured edge-case table

| Filter | Case | Measured behaviour (ffmpeg 8.1) |
|---|---|---|
| `setsar` | `setsar=2/1` after `setdar=1/1` | Final SAR = `2/1`, DAR reported as `4/1` — `setdar`'s prior SAR is gone entirely. |
| `setdar` | `setdar=1/1` after `setsar=2/1` | Final SAR = `1/2` (`= 1 * height/width` on a 100×50 input) — `setsar`'s prior value is gone entirely. |
| `setsar`/`setdar` | ratio syntax | Must use `/` (`setsar=16/9`), not `:` (`setsar=16:9`) — `:` is the filtergraph's own argument separator, so `16:9` parses as two positional arguments. |
| `fps` | 25→50 (upsampling) | Every input frame duplicated exactly twice; each output frame's timestamp is the **output grid's own**, not a copy of any input frame's `pts`. |
| `fps` | end of stream, `eof_action=round` (default) | Extrapolates **one more full interval**, using the gap between the last two real frames — not a single final duplicate. Three input frames with gap 2 produced *six* total output frames, not five. |
| `fps` | end of stream, `eof_action=pass` | Emits the held frame exactly once more (not independently measured; inferred from the documented contrast with `round`). |
| `fps` | two input frames land on the same output slot | The earlier one is silently dropped entirely — no error, no partial blend. |
| `format` | `pix_fmts` list | `|`-separated, preference order preserved into `Constraint::OneOf`. |

## How to change it

- Add a filter: follow an existing module's shape, declare it in `lib.rs`,
  add a `[[component]]` row to `vaco-component.toml`, wire the name into
  `registry.rs`, then run `cargo xtask gen-registry`.
- To give `framerate` real motion compensation, it needs
  `vaco-filter-vdsp`'s `scene_sad` and a blend/motion-estimation kernel —
  see `framerate.rs`'s doc comment for exactly what line was drawn and why.
- `fps`'s hold/duplicate/drop logic lives in `Filter::step`/`Filter::eof`,
  deliberately factored out of `FrameFilter::filter_frame`/`flush` so it can
  be unit-tested without a `FilterContext`.

## Configuration

Options are declared with `#[derive(vaco_opts::Options)]` and parsed via
`OptionsExt::set_from_string(args, "=", ":")`. `setsar`/`setdar`'s ratio
parsing goes through `vaco_core::parse::rational`, which accepts an
integer, `int:int` (only reachable when the whole value has no top-level
`:` left to split on — i.e. never from a filtergraph argument, always from
a value embedded another way), or a general `vaco-expr` expression such as
`16/9`. Defaults and option names were captured with
`LC_ALL=C ffmpeg -h filter=<name>` against ffmpeg 8.1.

## Dependencies

`vaco-filter-core`, `vaco-filter-graph`, `vaco-expr` (none of this crate's
own filters use it directly except transitively through
`vaco_core::parse::rational`'s fallback path), `vaco-pixfmt` (`format`'s
name resolution), `vaco-color` (`setrange`'s `ColorRange`), `vaco-frame`,
`vaco-opts`, `vaco-core`.
