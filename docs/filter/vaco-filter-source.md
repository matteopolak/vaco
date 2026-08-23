# vaco-filter-source

Video test-pattern and procedural generator sources (FT-4.12a, GitHub #474):
plan 16 §4.3's `vaco-filter-source` row.

## What it is

Fifteen source filters, registered here: `allrgb`, `allyuv`, `cellauto`,
`colorchart`, `colorspectrum`, `gradients`, `life`, `mandelbrot`, `perlin`,
`rgbtestsrc`, `sierpinski`, `smptebars`, `smptehdbars`, `yuvtestsrc`,
`zoneplate`.

Five names in the plan's row are **not** registered here:

- `nullsrc`, `color`, `nullsink` — already shipped by `vaco-filter-plumbing`
  (FT-4.3, GitHub #467).
- `pal100bars`, `pal75bars` — already shipped by `vaco-filter-video-source`
  (FT-4.4, GitHub epic #54).

Re-registering any of those five would collide with an existing
`[[component]]` row for the same `ctor` — `cargo xtask dup-check` exists to
catch exactly that.

Two more names in the row are not implemented at all: `testsrc` and
`testsrc2`. See "What is not implemented" below.

## How it works

One module per filter (or, for `smptebars`/`smptehdbars`, one shared
`bars.rs` module with `sd`/`hd` submodules — mirroring
`vaco-filter-video-source::bars`'s `pal100`/`pal75` shape), each exposing
`pub const DESC: FilterDesc` and a crate-private `create`, dispatched by
`registry::GeneratorRegistry`. Every filter is a `SourceFilter` wrapped in
`vaco_filter_core::adapt::Sourced`, same as every sibling source crate.

`rng.rs` holds one `SplitMix64` generator (Vigna, public domain), shared by
every filter whose options include a `seed`. It is **not** an attempt to
reproduce the reference's `av_lfg` bit stream — see that module's doc, and
`vaco-filter-temporal::rng`'s identical precedent — so anywhere this crate
resolves a `seed`, only reproducibility (same seed -> same output) is
guaranteed, not the reference's specific sequence.

Colour parsing reuses `vaco_core::parse::color` directly (the same function
`vaco-filter-plumbing::color` uses) rather than a second colour parser.
`vaco-filter-draw`, the plan's dedicated colour/fill crate, does not exist
yet in this tree; this crate does not need its plane-correct fill machinery
because every generator here writes a pixel format it chooses directly,
rather than compositing onto an existing frame.

## Per-generator exactness

Every claim below states the independent check used, not just "matches a
probe" — see each module's doc comment for the full derivation.

| Filter | Status | Independent check |
|---|---|---|
| `allrgb` | **Exact** | Closed-form bit-splitting of `(x, y)`; verified as a bijection onto all 2^24 RGB triples, not just sampled points. |
| `allyuv` | **Exact** | Closed-form derived from measurements, confirmed on two held-out points not used in the derivation; verified as a bijection onto all 2^24 (Y,U,V) triples. |
| `yuvtestsrc` | **Exact** | Closed-form 3-band gradient; formula checked at multiple widths/heights. |
| `rgbtestsrc` | **Exact** for `complement=false` (the default). `complement=true` is accepted but has no effect — the pixels it changes were not localised (see `rgbtestsrc.rs`'s doc). |
| `colorspectrum` | **Exact** to 3+ decimal digits | Hue wheel uses `smoothstep`, not linear interpolation, confirmed by two distinguishing sample points (`0.352`, `0.896`) that a linear ramp would give `0.4`/`0.8` for. All three `type` blend modes checked against measured references. |
| `colorchart` | **Exact**, both presets | `preset=reference`'s 24 patches cross-checked against the independently published X-Rite/BabelColor ColorChecker sRGB values, not just the one probe that produced them. `preset=skintones` is measured only (no independent public source found). |
| `zoneplate` | **Exact at the default (all-`k*`=0)**; algorithmically faithful, not calibrated for non-zero coefficients. The reference's phase-per-coefficient scale did not resolve to a clean constant from black-box probing (see `zoneplate.rs`'s doc) — likely due to a fixed-point `precision`-bit LUT this crate does not reproduce. |
| `sierpinski` | Carpet: **exact static structure** (the classic membership test, verified via a self-similarity property, not just point matching); the reference's frame-to-frame zoom animation is **not** reproduced (every frame renders the same static carpet). Triangle: algorithmically faithful chaos game, seed not calibrated to the reference. |
| `mandelbrot` | Escape-time recurrence **exact and independently checkable** (the origin never escapes; `\|c\| > 2` always escapes within ~1 iteration). Colour palette is **not calibrated** to the reference's gradient — see `mandelbrot.rs`'s doc. |
| `perlin` | Algorithmically faithful (published fade/lerp/gradient construction, fractal summation across octaves). **Not** calibrated: the permutation table for `random_mode=ken` is this crate's own seeded shuffle, not Perlin's 1985 table (hand-transcribing 256 magic numbers was judged riskier than admitting the gap) — see `perlin.rs`'s doc. |
| `cellauto` | Rule application **exact and closed-form** (Wolfram's own elementary-CA definition; checked against two well-known rules, 30 and 110, from the rule *number* alone). Explicit `pattern`/`filename` paths are exact. The random-fill default (no pattern given) is algorithmically faithful, not bit-exact, and this crate's `320x240` default frame size is its own choice — `-h` states none. |
| `life` | Rule evaluation **exact and closed-form** (checked against the textbook blinker oscillator and a stable block, properties of Life itself). Random-fill default is not bit-exact, same as `cellauto`. `mold`/`mold_color` are accepted but not implemented. |
| `gradients` | The piecewise-linear colour blend is **exact** given explicit colours and endpoints. `-1` ("auto") endpoints, `"random"` colours, animation (`speed`) and the `radial`/`circular`/`spiral`/`square` distance metrics are this crate's own choices, not measured. |
| `smptebars` | **Exact at the measured 320x240 default** (every bar/PLUGE segment's Y/Cb/Cr recorded from a direct probe). Segment boundaries at other sizes are proportionally scaled from that measurement, not independently re-measured. |
| `smptehdbars` | Same as `smptebars`: **exact at 320x240**, including the linear luma ramp row (closed-form pair-index formula, confirmed against four consecutive measured samples), proportionally scaled elsewhere. |

## What is not implemented

- **`testsrc`** — the reference overlays a rendered timestamp using its own
  bitmap glyph table, which needs a font/glyph rasteriser this crate does
  not have. Same reason `vaco-filter-video-source` left it out.
- **`testsrc2`** — has no text-related option at all (confirmed via
  `-h filter=testsrc2`), so no rasteriser is needed, but its animated
  moving-checker pattern did not resolve to a formula this crate could
  verify with confidence in the time available. Given that this project's
  own conformance work leans on `testsrc2` as an oracle for other filters
  (see the crate's closing report on GitHub #474), shipping a guessed
  pattern under that name was judged worse than not shipping it at all —
  the same call `vaco-filter-video-source` made for `smptebars` in FT-4.4,
  before this crate measured that pattern successfully.

## How to change it

- To add exactness to `zoneplate`, `mandelbrot`'s palette, or `perlin`'s
  `ken` mode: these all need either resolving the reference's fixed-point
  arithmetic (LUT precision, palette gradient stops) or the exact 1985
  permutation table, none of which a black-box probe alone was enough to
  pin down here.
- To animate `sierpinski`'s zoom or `gradients`' `speed` rotation: both
  need per-frame state threaded through `produce`, which the current
  `Source` structs do not carry (they render the same static frame every
  call). Add a `frame_index`-derived transform to the per-pixel formula.
- `smptebars`/`smptehdbars` at non-320-wide sizes: re-run the module's
  probe recipe at a second width and confirm the proportional-scaling
  assumption, or replace it with a second measured table if it does not
  hold.

## Configuration

No environment variables or external configuration. Every filter's options
are documented in its own module's doc comment and mirror `ffmpeg -h
filter=<name>` exactly (names, aliases, defaults).

## Dependencies

`vaco-core` (colour parsing, `Duration`/`Rational`/`Timestamp`), `vaco-opts`
(the options derive), `vaco-frame`/`vaco-pixfmt` (frame allocation and pixel
formats), `vaco-filter-core` (the `Filter`/`SourceFilter`/`Sourced`
adapters and format negotiation), `vaco-filter-graph` (the `FilterRegistry`
trait this crate implements). No `vaco-expr` or `vaco-tx` dependency was
needed — nothing in this crate's row evaluates a user expression or a
transform (`gradients`' colours and geometry are plain numeric options, not
expressions).
