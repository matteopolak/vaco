# vaco-filter-overlay

Multi-input video combiners — plan 16 §4.2's `vaco-filter-overlay` row,
the real unclaimed remainder GitHub issue #111 (FT-4.11)'s title meant by
"overlay family" (not the literal `overlay` filter, already shipped in
`vaco-filter-video-composite`, #465). Seven implemented: `blend`,
`multiply`, `mix`, `xmedian`, `xfade` (one transition), `displace`,
`remap` (`format=gray` only).

## Scope reconciliation

`vaco-filter-stack` (#111's first half) established that this project's
scoping checks (real option surface against `ffmpeg -h filter=<name>`,
not the plan's or an issue's guessed `Crate(s):` field) catch real
divergences. The eight names plan 16 assigns to this row —
`blend`, `xfade`, `mix`, `multiply`, `xmedian`, `displace`, `remap`,
`feedback` — were all confirmed present in `ffmpeg -hide_banner -filters`
and absent from every other crate, `planning/ASSIGNMENTS.md`, and the
generated registry before any code.

## Multi-input framework fit, checked per filter

`vaco-filter-stack` showed `Paired` (gap 10) and
`Synced`/`FrameSyncFilter` are different, non-interchangeable shapes.
Each filter here was checked against its own measured option surface:

| Filter | Arity | Framesync surface? | Adapter |
|---|---|---|---|
| `blend` | 2 (fixed) | Full (`eof_action`/`shortest`/`repeatlast`/`ts_sync_mode`) | `Synced`/`FrameSyncFilter`, `FsInput::dual` |
| `multiply` | 2 (fixed) | None | `Paired` |
| `mix` | N (`2..=32767`, capped at `pads::MAX`) | None (its own `duration=longest/shortest/first`) | `Synced`/`FrameSyncFilter`, hand-built roles |
| `xmedian` | N (`3..=255`, same cap) | Full | `Synced`/`FrameSyncFilter`, `FsInput::uniform` |
| `xfade` | 2 (fixed) | None (its own `duration`/`offset` timing) | `Synced`/`FrameSyncFilter`, `FsInput::dual` |
| `displace`, `remap` | 3 (fixed, `VVV->V`) | None | `Paired` — no framework gap |
| `feedback` | 2 in, **2 out** | — | `Dual`/`DualFilter` fits the shape (gap 24, closed) — the loop still cannot run (gap 25). |

`feedback` is `VV->VV`. `planning/INTERFACE-GAPS.md` gap 24 (no adapter
for 2-in-2-out) is closed: `vaco-filter-core::adapt` gained
`Dual`/`DualFilter` for exactly this shape, tested end-to-end in
`vaco-filter-core`'s own `tests/graph.rs`. That does not unblock
`feedback` itself: the reference's own use
(`[0][fb]feedback[out][fb]`) loops one output back as the filter's own
next-frame input — a genuine cycle — and `Graph::configure()` requires
`Graph::topological_order()`, which hard-rejects any cycle before a
`Dual`-shaped node's pads are ever negotiated. That is a separate,
harder limitation in the scheduler's negotiation and scheduling, not an
adapter-shape question, filed as `planning/INTERFACE-GAPS.md` gap 25.
`feedback` remains unimplemented in this crate — a `Dual`-wrapped filter
that cannot loop would not be `feedback`, so no partial version is
built here.

## What it is

One module per filter (`src/{blend,multiply,mix,xmedian,xfade}.rs`), each
exposing `pub const DESC: FilterDesc` and a crate-private `fn create`,
aggregated by `registry::OverlayRegistry`. `src/common.rs` carries the
same small 8-bit-plane helpers this whole filter family carries its own
copy of.

### `blend`

18 of the reference's ~30 distinct named blend-mode formulas (`ffmpeg -h
filter=blend`'s `c0_mode`..`c3_mode`, `0..=39`, several aliasing pairs)
are measured and implemented, each pinned against a full `0..=255`
gradient sweep at a fixed second operand:

```text
normal(a,b)    = a
multiply(a,b)  = floor(a*b / 255)
screen(a,b)    = 255 - floor((255-a)*(255-b) / 255)
darken(a,b)    = min(a,b)          lighten(a,b) = max(a,b)
average(a,b)   = floor((a+b) / 2)
difference(a,b)= |a-b|             negation(a,b) = 255 - |255-a-b|
subtract(a,b)  = max(0, a-b)       addition(a,b) = min(255, a+b)
exclusion(a,b) = a + b - floor(2*a*b / 255)
grainmerge(a,b)  (=addition128)    = clamp(a+b-128, 0, 255)
grainextract(a,b)(=difference128)  = clamp(a-b+128, 0, 255)
and/or/xor(a,b) = bitwise, exact
burn(a,b)  = a==0 ? 0 : clamp(255 - round((255-b)*255/a), 0, 255)
dodge(a,b) = a==255 ? 255 : clamp(round(b*255/(255-a)), 0, 255)
```

`burn`/`dodge` are the one pair confirmed to use **round-half-up**, not
the `floor` every fixed-`/255` formula above uses — an exact `.5` tie
inside `burn`'s division (`a=150, b=150` → `178.5`) resolves to `179`,
not `178`. Per-component (`c0`..`c3`) modes and `opacity` are generic
over every mode above: `out = floor(a + opacity*(mode(a,b)-a))`, measured
directly against `multiply` at `opacity=0.5`.

**Not implemented**: `hardlight`, `overlay`, `softlight`, `hardmix`,
`linearlight`, `vividlight`, `pinlight`, `reflect`, `phoenix`,
`extremity`, `freeze`, `glow`, `heat`, `softdifference`, `geometric`,
`harmonic`, `bleach`, `stain`, `interpolate`, `hardoverlay`,
`multiply128`. Raw output curves for all of these were captured (below)
but none was confirmed against a formula this pass could verify at more
than one point. `create` rejects them with a clean error. `c0_expr`/
`all_expr` (arbitrary expressions) are not implemented.

**A second, bounded pass was made at `hardlight`/`vividlight`/
`linearlight`** once `burn`/`dodge` (above) turned up round-half-up —
these three are the natural candidates a published formula would build
from `multiply`/`screen` (threshold at `127`/`128`) or from `burn`/
`dodge` themselves, so the new rounding rule was the most promising lead
available. It did not unlock them. `hardlight` swept at two fixed second
operands (`b=60`, `b=200`) against the standard `a<=127 ->
multiply(b,2a)` / `a>127 -> screen(b,2a-255)` shape shows the *large-a*
end matching a plain `floor(a*2b/255)` exactly (`a=150,200,255` against
`b=60`), while the *small-a* end is consistently one below that
prediction (`a=50,100`) — neither `floor` nor `round`, nor dividing by
`256` instead of `255`, reconciles both ends with one rule. Recorded so
a future attempt does not re-derive and re-reject the same candidates.

#### Raw curves recorded for the unimplemented modes

`a = 0, 50, 100, 150, 200, 255`, `b = 150` fixed, `ffmpeg 8.1`,
`-bitexact`:

| Mode | Curve |
|---|---|
| `hardlight` | `45, 87, 129, 169, 211, 255` |
| `overlay` | `0, 58, 116, 169, 211, 255` |
| `softlight` | `0, 55, 109, 158, 206, 255` |
| `hardmix` | `0, 0, 0, 255, 255, 255` |
| `linearlight` | `0, 0, 94, 194, 255, 255` |
| `vividlight` | `0, 0, 121, 181, 255, 255` |
| `pinlight` | `44, 50, 100, 150, 200, 255` |
| `reflect` | `0, 23, 95, 214, 255, 255` |
| `phoenix` | `105, 155, 205, 255, 205, 150` |
| `extremity` | `105, 55, 5, 45, 95, 150` |
| `freeze` | `0, 0, 95, 182, 235, 255` |
| `glow` | `88, 109, 145, 214, 255, 255` |
| `heat` | `0, 35, 145, 182, 200, 212` |
| `softdifference` | `255, 170, 85, 0, 121, 255` |
| `geometric` | `0, 87, 122, 150, 173, 196` |
| `harmonic` | `0, 75, 120, 150, 171, 188` |
| `bleach` | `105, 55, 5, 211, 161, 106` |
| `stain` | `104, 54, 4, 210, 160, 105` |
| `interpolate` | `81, 93, 124, 162, 195, 209` |
| `hardoverlay` | `0, 58, 117, 182, 255, 255` |
| `multiply128` | `0, 0, 0, 231, 255, 255` |

A future attempt should vary the *second* operand too (this sweep only
varies `a`, `b=150` throughout) before guessing at any of these — several
(the "light" family especially) are almost certainly threshold formulas
whose branch point cannot be located from a single fixed `b`.

### `multiply`

`ffmpeg -h filter=multiply`: `scale` (`0..=9`, default `1`), `offset`
(`-1..=1`, default `0.5`). No framesync surface — built on `Paired`.
**Structural, not confirmed exact**: at `offset=0`, `scale=1`, 4 of 6
gradient points match `round(a*b/255)`, but the other two do not, and not
in a way one consistent rounding rule explains (`a=200` needs `floor`,
`a=100` needs `round` — both cannot be true of a single rule; see the
module's own doc for the full derivation). Most plausibly floating-point
representation error specific to the reference's own operation order.

### `mix`

`ffmpeg -h filter=mix`: `inputs` (`2..=32767`, capped at `pads::MAX`),
`weights` (default `"1 1"`), `scale` (default `0`, meaning "auto:
normalise by the sum of weights" — confirmed, not assumed, via
`scale=1` skipping normalisation entirely), `duration`
(`longest`/`shortest`/`first`, default `longest`).

```text
divisor = scale == 0 ? sum(weights) : scale
out = clamp(round_ties_even(sum(weight_i * value_i) / divisor), 0, 255)
```

Default `weights="1 1"` matches `blend`'s `average` exactly. The rounding
rule is confirmed **round-half-to-even** (not `burn`/`dodge`'s
round-half-up): `weights="3 1"` produces three exact `.5` ties (`37.5`,
`112.5`, `187.5`) that all resolve to their even neighbour. `duration=
longest`/`shortest` map onto `FsInput::uniform`'s built-in shape;
`duration=first` (stop when input `0` ends, regardless of the others) has
no `vaco-filter-framesync` built-in equivalent, so this module builds its
own roles by hand (`uniform(n)` with input `0`'s `after` overridden to
`Stop`).

### `xmedian`

`ffmpeg -h filter=xmedian`: `inputs` (`3..=255`, capped at `pads::MAX`),
`percentile` (default `0.5`), plus the full framesync surface. Built like
`blend` (`FsInput::uniform`, `apply_opts` driving the option truth
table). `percentile=0.5` on an odd input count matches the plain sorted
middle element exactly (`sorted([a, 50, 200])[1]` across a full `a`
gradient). Other percentiles and even input counts (needing a documented
interpolation rule) are not implemented.

### `xfade`

`ffmpeg -h filter=xfade`: `transition` (`58` named values plus `custom`,
default `fade`), `duration`/`offset` (`<duration>`, defaults `1`/`0`).
Only `transition=fade` is implemented:

```text
progress = clamp((pts_seconds - offset) / duration, 0, 1)
fade(a,b,progress) = floor(a + progress*(b-a))
```

Pinned at all 10 frames of a `10fps`, 1-second `black -> white`
transition — every value matches `floor(255 * i/10)` exactly, including
the non-tie fractional frames. The other 57 transitions are each their
own per-pixel geometry formula and were not attempted; `create` rejects
any transition other than `fade`.

### `displace`

`ffmpeg -h filter=displace`: `edge` (`blank`/`smear`/`wrap`/`mirror`,
default `smear`). Three fixed inputs (`source`, `xmap`, `ymap`), no
framesync surface — `Paired` (already proven to generalise past 2 inputs
by `vaco-filter-geometry::mergeplanes` with `input_count()` overridden).

Measured directly: the maps are plain **8-bit `gray`** (no auto-inserted
format conversion — contrast with `remap`, below), `128` is the zero
point, and:

```text
output(x, y) = source(x + (xmap(x,y)-128), y + (ymap(x,y)-128))
```

confirmed with an all-`128` identity probe and `+2`-offset probes on both
axes against a per-pixel-distinct gradient source. `edge` modes, probed
with a deeply out-of-range offset (`-128`):

```text
blank  -> 16 (not 0 -- see remap's BT.709 finding below; the two filters
          share this constant)
smear  -> clamp to the nearest valid coordinate (the default)
wrap   -> coordinate modulo the frame dimension
mirror -> see below -- the two edges do not share one axis
```

`mirror` has a real surprise: probed with small (`+-1`, `+-2`, `+3`)
offsets to keep each case a single bounce off one edge, the *left* edge
reflects around index `0` itself (`resolve(-1)=1`, `resolve(-2)=2`),
while the *right* edge reflects around the half-pixel point `len-0.5`
(`resolve(len)=len-1`, `resolve(len+1)=len-2`, …) — two different axes,
not one symmetric rule. A separately-probed deep offset (`-128`) does
not match either axis extended periodically, so this crate's `Mirror`
clamps beyond one bounce rather than guessing at further periodicity —
confirmed exact only within one frame dimension of the edge.

Not implemented: non-luma planes (copied from the source unchanged;
`blank`'s neutral chroma value was not independently confirmed). RGB
pixel formats. Bit depths above 8.

### `remap`

`ffmpeg -h filter=remap`: `format` (`color`/`gray`, default `color`),
`fill` (default `"black"`). Three fixed inputs, no framesync surface —
`Paired`, the same shape as `displace`. **Only `format=gray` is
implemented** — `format=color` (the reference's own default) is a
materially different, unmeasured code path (does an RGB source need the
same limited-range treatment as the fill colour? does `yuv420p` pass
through unconverted? neither was probed), and `create` rejects it with a
clean error rather than guessing at ffmpeg users' most common case.

Measured directly: the map planes are **16-bit `gray16le`**, discovered
via `ffmpeg -v verbose` auto-inserting a `gray -> gray16le` conversion
ahead of `remap` when fed plain 8-bit maps (its native format — contrast
with `displace`'s plain 8-bit maps), then re-measured with genuine
16-bit map files to avoid the auto-conversion's own scaling. Each map
holds an **absolute source pixel coordinate**, not an offset:

```text
output(x,y) = (xmap in [0,width) and ymap in [0,height))
            ? source(xmap(x,y), ymap(x,y))
            : fill
```

Confirmed with an identity map (`xmap(x,y)=x`, `ymap(x,y)=y`) reproducing
a per-pixel-distinct source exactly, and confirmed that *either* axis
alone out of range triggers `fill` for the whole pixel (a valid `x` with
an invalid `y` still fills), not just both together.

`fill`'s colour, for `format=gray`, is not a plain component average — it
goes through ITU-R BT.709's own published full-range-RGB-to-limited-range
luma formula:

```text
fill_gray = round(16 + 219*(0.2126*r + 0.7152*g + 0.0722*b) / 255)
```

Pinned at four colours: `black` -> `16` and `white` -> `235` exactly (no
fractional part to round — these are BT.709's own limited-range black/
white points, not a coincidence), `red` -> `63`, mid-grey (`128,128,128`)
-> `126`. `16`/`219`/`235` and the luma weights are the published
standard's own numbers (D7 merger doctrine), not this crate's invention.

Not implemented: `format=color`. Map dimensions differing from the
source's own (assumed to match, not enforced). Bit depths above 8 for
the source.

## Framecrc comparison table

| Filter | Args | Source | Result |
|---|---|---|---|
| `blend` | `all_mode=<mode>` for each of the 18 implemented modes | `gray`, `0..=255` gradient vs flat `150` | **exact** — all 18, each pinned at 6 points |
| `blend` | `all_mode=burn`/`dodge` | same, including an exact `.5` tie inside `burn` | **exact** — round-half-up confirmed |
| `blend` | `all_mode=multiply:all_opacity=0.5` | same | **exact** — opacity mixing formula confirmed |
| `multiply` | `offset=0:scale=1` | same gradient | **structural, not exact** — 4 of 6 points match, 2 do not (see above) |
| `mix` | `inputs=2` (default weights/scale) | same | **exact** — matches `blend`'s `average` |
| `mix` | `weights=3 1` | same | **exact** — round-half-to-even confirmed at 3 ties |
| `xmedian` | `inputs=3` (default `percentile=0.5`) | one gradient, two flat inputs | **exact** — sorted-middle confirmed |
| `xfade` | `transition=fade:duration=1:offset=0` | `black`→`white`, `10fps` | **exact** — all 10 transition frames |
| `displace` | `edge=smear` (default), `wrap`, `blank` | per-pixel-distinct gradient, offset probes | **exact** |
| `displace` | `edge=mirror`, small offsets (`+-1`,`+-2`,`+3`) | same | **exact** — within one frame dimension of the boundary |
| `displace` | `edge=mirror`, deep offset (`-128`) | same | **not exact** — clamped, not periodic; recorded, not guessed |
| `remap` | `format=gray`, identity map | per-pixel-distinct gradient | **exact** |
| `remap` | `format=gray:fill=<colour>`, out-of-range map | same, 4 colours | **exact** — BT.709 limited-range luma confirmed |
| `remap` | `format=color` (default) | — | **not attempted** |
| `feedback` | — | — | **not implemented — adapter shape exists (gap 24, closed); the loop cannot run (gap 25)** |

No `vaco` CLI/muxer exists yet to drive an actual `-f framecrc`
invocation; comparisons
are against the reference's raw pixel output and cross-checked against
this crate's own tests.

## How to change it

- `blend`/`xmedian` follow the full-framesync shape (`FsInput::uniform`
  or `dual`, `apply_opts`); `mix` needs hand-built roles for
  `duration=first`; `multiply` uses `Paired`, the fixed-lockstep shape.
  Check `ffmpeg -h filter=<name>` for the actual option surface before
  assuming which one a new filter needs — `vaco-filter-stack`'s and this
  crate's own scoping both turned up filters that looked alike but
  needed different adapters.
- If you add more `blend` modes, this crate has now tried varying the
  second operand for `hardlight`/`vividlight`/`linearlight` specifically
  and still not matched a formula — see that section's "second, bounded
  pass" for the exact candidates already falsified, so as not to repeat
  them. The raw curves for the other 17 are a starting point, not
  evidence of a specific formula, and were only ever swept against one
  fixed second operand.
- `displace`/`remap` are done for their measured cases. If extending
  `remap` to `format=color`, expect a genuinely different code path, not
  a small addition to `format=gray`'s — probe whether the source itself
  needs limited-range treatment before assuming it doesn't.
- `feedback` had one of its two blockers removed: `vaco-filter-core`
  now has a 2-in/2-out adapter (`Dual`/`DualFilter`, gap 24). What is
  still missing is cycle support in the scheduler itself —
  `Graph::configure()`'s negotiation pass rejects any cycle outright, so
  the loop `feedback` needs (`[0][fb]feedback[out][fb]`) cannot be built
  regardless of adapter shape. See `planning/INTERFACE-GAPS.md` gap 25
  before attempting this filter again — it needs scheduler/negotiation
  changes in `vaco-filter-core`, not more work in this crate.

## Configuration

No crate-level configuration, environment variables, or feature flags.
Runtime configuration is entirely per-filter-instance, via each filter's
`Opts` (parsed from the filtergraph argument string).

## Dependencies

`vaco-core`, `vaco-opts`, `vaco-frame`, `vaco-pixfmt`, `vaco-filter-core`,
`vaco-filter-graph`, `vaco-filter-framesync`, `smallvec` (already a
workspace dependency, used by `vaco-filter-geometry::mergeplanes` for the
same `Paired`/`SmallVec<[Frame; 4]>` shape). No new dependency was added
to the workspace's `Cargo.toml`.
