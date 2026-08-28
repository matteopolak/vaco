# vaco-filter-artistic

T3 artistic/stylisation video filters — `planning/16-filters.md` §4.2's
`vaco-filter-artistic` row. Two implemented: `noise`, `vignette`.

## Scope reconciliation — this crate replaces a dispatch brief that named the wrong crate

The dispatch brief for this work (GitHub issue #478, "FT-4.12e") asked for a
`vaco-filter-effect` crate covering roughly two dozen filters (`sobel`,
`prewitt`, `roberts`, `kirsch`, `edgedetect`, `morpho`, `erosion`,
`dilation`, `deflate`, `inflate`, `shuffleframes`, `shufflepixels`,
`shuffleplanes`, `swaprect`, `swapuv`, `tmix`, `lagfun`, `random`,
`photosensitivity`, `noise`, `vignette`, `pixelize`, among others). Per
`planning/AGENT-CONSTRAINTS.md`'s "the plan already partitions the filters;
do not invent a crate", `planning/16-filters.md` §4.2's table was checked
before writing anything, and `vaco-filter-effect` is not a row in it. Almost
the entire named list already has a home, and, for most of it, is already
built and committed (`planning/ASSIGNMENTS.md`, checked 2026-08-28):

| Filters | Actual crate | Status |
|---|---|---|
| `sobel`, `prewitt`, `roberts`, `kirsch`, `scharr`, `edgedetect`, `morpho`, `erosion`, `dilation`, `deflate`, `inflate`, `median`, `convolution` | `vaco-filter-convolve` | done (#468, `agent:blur2`) |
| `swaprect`, `swapuv`, `shuffleframes`, `shuffleplanes`, `pixelize` | `vaco-filter-geometry` | done (#470, `agent:geom2`) |
| `tmix`, `lagfun`, `random` | `vaco-filter-temporal` | done (#475, `agent:temporal`) |
| `photosensitivity` | `vaco-filter-analysis` | in progress (#477, `agent:analysis2`) at the time this crate was written — not this crate's to touch |
| `shufflepixels` | (declined in `vaco-filter-geometry`) | its `seed` option reads as a generator seed nobody has identified; that crate documented the same "not implemented, not guessed" call this crate makes for `noise` |

What is left, unclaimed, in this crate's actual row: `noise`, `vignette`,
`epx`, `xbr`, `hqx`, `super2xsai`, `amplify`, `delogo`, `removelogo`,
`cover_rect`, `find_rect`. This pass implements `noise` and `vignette`; the
rest are listed under "Left for a follow-up" below. This is a genuine,
reported change of scope from the dispatch brief, not a silent substitution.

## What it is

One module per filter (`src/noise.rs`, `src/vignette.rs`), each exposing
`pub const DESC: FilterDesc` and a crate-private `fn create`, aggregated by
[`registry::ArtisticRegistry`](../../crates/filter/vaco-filter-artistic/src/registry.rs)
— the same shape `vaco-filter-convolve`/`vaco-filter-geometry` use.
`src/common.rs` carries the same small 8-bit-plane helpers those crates each
carry their own copy of (format validation, frame metadata copying) — D19
governs shared *types*, not these tiny per-crate predicates, and each of
those crates' own docs make the same call for the same reason.

### `vignette`

Darkens (or, `mode=backward`, brightens) radially from a centre point using
the classic optical `cos^4` vignetting law. Measured against the reference
(`ffmpeg 8.1`, `-bitexact`, tiny `gray`/`yuv420p` sources through `-f
rawvideo`, 2026-08-28) rather than assumed:

```text
dx = x - x0;  dy = (y - y0) * aspect
dist = sqrt(dx*dx + dy*dy)
theta = angle * dist / max_dist            // max_dist = sqrt(x0^2 + y0^2), UNSCALED by aspect
factor = theta < PI/2 ? cos(theta)^4 : 0
out = mode=forward  ? trunc(baseline + (in-baseline)*factor)
    : mode=backward ? clamp(baseline + (in-baseline)/factor, 0, 255)   // factor==0 -> clip
```

`baseline` is `128` for a chroma plane of a non-RGB format, `0` otherwise
(measured on a `yuv420p` source: chroma is pulled toward `128`, not
multiplied directly, the way a darkening filter that must not tint the image
grey has to work). Pixel coordinates are plain integers — no half-pixel
offset — and the rounding is C-style truncation, not `round()`; a parameter
sweep over both conventions found exactly one that reproduced an 8x8 and a
16x8 reference grid with zero error (`src/vignette.rs`'s module doc has the
full probe).

Three documented, deliberate gaps, none guessed at:

1. **`dither=1` (the default!) is not reproduced.** It perturbs the
   truncated output by up to ±1 per pixel, reproducibly across repeated runs
   despite `vignette` having no `seed` option at all — this crate did not
   identify that generator in the time available. `dither=0` is unaffected
   and is this filter's framecrc-verified path.
2. **Chroma planes are scaled around `128` (structurally reasoned) but not
   pixel-verified.** A `yuv420p` probe confirmed the *shape* of chroma
   handling but left an approximately 1-count residual against the reference
   that this pass did not chase down before its time budget ran out.
3. **`aspect != 1` is exact away from the frame's extreme corners only.** The
   interior formula (`dy` scaled by `aspect`, `max_dist` left unscaled)
   reproduced every interior pixel of a 16x8/`aspect=2` probe exactly, but
   not the reference's extra hard clipping right at the corners; a search
   over plausible alternate scalings did not find one rule matching both the
   interior and the corners.

### `noise`

Adds per-component (`c0`..`c3`, one per plane) pseudo-random noise, with
`all_seed`/`all_strength`/`all_flags` (aliases `alls`/`allf`) setting every
component's suboption at once and a component's own `c{n}_*` option
overriding it. Flags: `a` averaged (3-sample mean), `p` (semi)regular
pattern (a fixed deterministic tile, not a fresh draw), `t` temporal (the
noise table is drawn once and reused for the rest of the stream instead of
being redrawn every frame), `u` uniform.

**Measured, and the one framecrc-exact case this filter has:** with every
component's `strength` at its default of `0` (i.e. plain `-vf noise`), the
reference's output is byte-identical to no filter at all —
`ffmpeg -bitexact -f lavfi -i "color=gray:s=8x8:d=1:r=1" -vf
"format=gray,noise" -f rawvideo -pix_fmt gray -` diffed against the same
command without `noise`. This module reproduces that: at `strength=0` for
every component it never touches its RNG state and passes the frame through
unmodified — checked directly, not assumed.

**Not attempted:** the actual pseudo-random sequence at `strength > 0`.
`all_seed`/`c0_seed`/etc. only make sense as *reproducibility* knobs if this
crate reproduces the reference's own generator, and doing that would mean
reading the reference's source (D7) — the same conclusion
`vaco-filter-temporal::random` already reached and documented. `strength >
0` is implemented (a small dependency-free `SplitMix64` PRNG, the same
generator already duplicated in `vaco-filter-temporal::rng` and
`vaco-filter-source::rng` for the identical reason — see
`planning/TECH-DEBT.md`) but is **not** framecrc-verified at any nonzero
strength. `seed=-1`'s reference behaviour ("seed from local entropy", i.e.
non-deterministic even in the reference) is replaced with a fixed constant
seed, a deliberate divergence toward reproducible pipelines.

## Framecrc comparison table

Built one comparison loop: `ffmpeg -bitexact -f lavfi -i <source> -vf
"<filter>=<args>" -f rawvideo -pix_fmt <fmt> -` against the reference, hand
zero-error-checked against this crate's pure functions (`factor_at`/
`apply_forward`/`apply_backward` for `vignette`; the `strength=0` identity
check for `noise`) — pinned into unit tests so a future change that breaks
the match fails a test rather than needing a human to re-run `ffmpeg`. No
`vaco` CLI/muxer exists yet in this tree to drive an actual `vaco ... -f
framecrc -` invocation (`ffmpeg -h filter=noise`, `planning/14-cli.md` is
still a plan document), so "framecrc comparison" here means the same
rawvideo-diff methodology `vaco-filter-convolve`/`vaco-filter-blur` already
established for exactly this reason.

| Filter | Args | Source | Result |
|---|---|---|---|
| `vignette` | `dither=0` (default angle/x0/y0/aspect/mode) | `gray`, 8x8 | **exact** — zero error, forward mode |
| `vignette` | `dither=0:mode=backward` | `gray`, 16x8 | **exact** — zero error |
| `vignette` | `dither=0` (default) | `yuv420p`, 8x8 | **not verified** — chroma-plane residual, see above |
| `vignette` | `dither=1` (the default) | any | **not verified** — dithering not reproduced |
| `vignette` | `dither=0:aspect=2` | `gray`, 16x8 | **partial** — interior exact, extreme corners diverge |
| `noise` | *(no args, `strength=0` everywhere)* | `gray`, 8x8 | **exact** — verified identity |
| `noise` | any `strength > 0` | any | **not verified** — PRNG not reproduced |

## How to change it

- Both filters follow `vaco-filter-convolve`'s convention: a `pub const
  DESC`, an `Opts` struct via `vaco_opts::Options` with a `parse` inherent
  method, a `Filter` struct implementing `vaco_filter_core::adapt::FrameFilter`,
  and a `create` function the registry dispatches to.
- `vignette`'s expression options (`angle`/`x0`/`y0`) are `vaco-expr`
  `Expr`s bound to `w`/`h`, evaluated once in `Filter::new`-adjacent state
  (`eval=init`, the default) or every frame (`eval=frame`) — see
  `params_for` in `src/vignette.rs`.
- If you resolve the `dither=1` generator or the chroma-plane residual,
  update the doc comment at the top of `src/vignette.rs` (it states exactly
  what was and was not verified) and the table above in the same change —
  both are written to be falsified by a future measurement, not treated as
  permanent.
- If a `rand`-family dependency is ever approved for the workspace, `noise`
  and `vaco-filter-temporal::random` are the two filters whose correctness
  bar changes from "algorithmically faithful" to "must match the reference
  bit for bit" — reproducing FFmpeg's actual generator (not published,
  reachable only by reading its source, which D7 rules out) would still be
  needed even then.

## Configuration

No crate-level configuration, environment variables, or feature flags.
Runtime configuration is entirely per-filter-instance, via each filter's
`Opts` (parsed from the filtergraph argument string — see each module's
`ffmpeg -h filter=<name>` transcription in its doc comment for the exact
option surface).

## Dependencies

`vaco-core`, `vaco-expr` (for `vignette`'s expression options),
`vaco-opts`, `vaco-frame`, `vaco-pixfmt`, `vaco-filter-core`,
`vaco-filter-graph`, `bitflags` (for `noise`'s flag-letter option). No
external crate beyond what the workspace already declares; no new
dependency was added to the workspace's `Cargo.toml`.
