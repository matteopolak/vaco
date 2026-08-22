# `vaco-color`

## What it is

The colour vocabulary of ITU-T H.273 — primaries, transfer characteristics,
matrix coefficients, range and chroma siting — together with the numbers those
names stand for: primary chromaticities, the RGB↔XYZ derivation, the
R'G'B'↔Y'CbCr matrices, the transfer functions and their inverses, and the
narrow/full-range quantisation levels.

Vocabulary and coefficients live in one crate deliberately. Keeping them apart
is how a project ends up with two BT.709 matrices that disagree in the fourth
decimal, and colour bugs of that size are invisible in review and obvious on a
gradient.

Nothing here allocates. Every query is a table index or a handful of
floating-point operations, and every entry point returns `Option` rather than
panicking — the inputs are bitstream bytes and command-line strings.

## How it works

### Layout

```
src/lib.rs         crate docs, `ColorInfo`, the five enums (the frozen surface)
src/primaries.rs   chromaticity table, RGB<->XYZ, chroma-derived Kr/Kb
src/transfer.rs    H.273 Table 3 curves: `encode` / `decode`
src/matrix.rs      H.273 Table 4: `luma_coefficients`, `rgb_to_ycbcr`
src/range.rs       `Levels`: offset / scale / min / max per bit depth
src/chroma.rs      siting names and 4:2:0 sample offsets
src/tests.rs       unit tests plus swept invariant tests
```

The enums stay in `lib.rs` because that is where Phase 0 froze them; the `impl`
blocks live beside the data they need. Public paths are all at the crate root.

### Discriminants are the specification's code points

`ColorPrimaries`, `TransferCharacteristic` and `MatrixCoefficients` are
`#[repr(u8)]` with H.273's own numbering, so `to_u8` is a cast and a demuxer can
compare against the spec table directly. `ColorRange` and `ChromaLocation` are
not — H.273 has no enumeration for either (range is a one-bit
`video_full_range_flag`), so their `to_u8`/`from_u8` speak the *reference tool's*
`AVColorRange` / `AVChromaLocation` numbering, which is what the CLI and the
container mappings use. `ChromaLocation::from_h264_loc_type` handles the
off-by-one against H.264's own numbering.

`from_u8` returns `None` for reserved and unassigned code points. A bitstream may
legally carry one; a demuxer that needs to round-trip the raw byte keeps the
byte, because this crate models only values the specification has assigned.

### Two name tables, and why they must stay two (D17)

The reference tool spells several values one way as a command-line option and a
different way in `ffprobe -show_streams` output. Verified against ffmpeg 8.1 by
writing each code point into an H.264 VUI with the `h264_metadata` bitstream
filter and probing it back:

| Code point | `name()` — printed | `from_name()` — accepted |
|---|---|---|
| transfer 4 | `bt470m` | `gamma22` |
| transfer 5 | `bt470bg` | `gamma28` |
| matrix 0 | `gbr` | `rgb` |
| primaries 22 | `ebu3213` | `jedec-p22`, `ebu3213` |

`-color_trc bt470m` and `-colorspace gbr` are **rejected** by the reference, so
they are rejected here. The two tables are therefore not inverses, and
`output_names_and_option_names_diverge_exactly_where_the_reference_does` fails if
anyone merges them.

A second asymmetry: the unspecified value prints `unknown` for `color_range`,
`color_space`, `color_transfer` and `color_primaries`, but `unspecified` for
`chroma_location`. That is the reference's inconsistency and it is reproduced,
per D17 — these strings are observable in `-show_streams`, which D6 requires to
be byte-identical.

### Coefficients: derived where the spec derives, literal where it states

H.273 Table 4 gives literal `Kr`/`Kb` decimals for code points 1, 4, 5, 6, 7, 9
and 10. Those literals are what `MatrixCoefficients::luma_coefficients` returns,
because they are what every encoder and decoder in the world uses. They are close
to but not equal to the value derived from the matching primaries — BT.709
derives 0.212639 against the standard's 0.2126 — and substituting the derived one
would put us a bit or two away from everyone else at 10-bit depth.

The derivation exists anyway, in `Chromaticity::luma_coefficients`, for two
reasons: H.273 *requires* it for the chroma-derived code points 12 and 13, and it
is the best available cross-check on the chromaticity table. Its shape:

- A chromaticity `(x, y)` fixes a primary only up to scale, so its XYZ is
  `S · (x/y, 1, (1−x−y)/y)`.
- Write `M₀` for the matrix of those unit vectors. The white-point constraint
  `M₀ · S = W` fixes the three scales; `S = M₀⁻¹ · W`.
- `M₀ · diag(S)` is the RGB→XYZ matrix, and its middle row is `(Kr, Kg, Kb)`,
  because every unit vector's Y component is 1.

`derived_luma_agrees_with_the_stated_constants` checks the two agree to 5e-5 for
BT.709 and BT.2020, and `bt601_luma_is_not_derived_from_its_primaries` asserts
that BT.601's do **not** — they are historical, inherited from the 1953 NTSC
primaries and kept when the primaries changed. That test exists to stop someone
"fixing" the table by deriving them.

The Y'CbCr matrix itself is built from `(Kr, Kb)` at the point of use rather than
stored pre-multiplied, and both directions are written out algebraically rather
than inverted numerically — a numeric inverse leaves the four structural zeros at
~1e-17, and a kernel that multiplies by almost-zero pays for it every pixel.

### What has no matrix, and why

`rgb_to_ycbcr` returns `None` for: `Unspecified`; the constant-luminance pair
(`Bt2020Cl`, `ChromaDerivedCl`), where `Y'c` comes from *linear* luminance
through the transfer function and the chroma channels use different scale factors
above and below zero, so no linear form exists; `Smpte2085`, `Ictcp` and `IptC2`,
which are defined on X'Y'Z' or on LMS after a nonlinearity rather than on R'G'B';
and `YCgCoRe`/`YCgCoRo`, which are reversible integer lifting schemes that widen
the chroma channels by a bit. `Identity` and `YCgCo` do have exact matrices and
return them.

`luma_coefficients` is more permissive than `rgb_to_ycbcr`: constant-luminance
systems still have a defined `(Kr, Kb)`, and the YCgCo family's luma is
`(R + 2G + B)/4`, i.e. `Kr = Kb = 0.25`. Do **not** feed a YCgCo `(Kr, Kb)` into
a generic matrix builder — its chroma axes are not Cb/Cr. `rgb_to_ycbcr` knows
that; a caller reconstructing the matrix by hand would not.

### Transfer functions

`encode` is H.273's own direction, `V = f(L)`, and `decode` is its inverse. The
argument is normalised so `1.0` is the reference peak of whichever quantity that
row is written in — 10000 cd/m² for PQ, the 48/52.37 reference for ST 428-1,
nominal peak white elsewhere. Both return `None` only for `Unspecified`.

Three things worth knowing before changing anything here:

- **The piecewise curves are genuinely discontinuous.** H.273 prints α and β
  rounded, so BT.709's linear segment ends at 0.081 while its power segment
  starts at 0.0812479. `decode`'s branch thresholds are chosen so that
  `decode(encode(l)) == l`; `encode(decode(v))` has a gap of that width and
  always will. Round-trip error is bounded at ~6e-7 (worst case BT.2020 12-bit)
  and the tests assert 1e-6.
- **PQ's `encode(0.0)` is not zero.** It is `c1^m ≈ 7.3e-7`. Clamping it to zero
  breaks the inverse near black.
- **Negative light never produces NaN.** `f64::powf` returns NaN for a negative
  base with a fractional exponent, and a slightly-negative sample is routine
  after a chroma upsample. The pure-power rows use an odd extension
  (`powf_signed`), which keeps them monotonic and exactly invertible. The
  extended-gamut rows (11, 12) define their own negative lobes and use those.

### Quantisation

`Levels { offset, scale, min, max }` states the contract as
`code = clamp(round(offset + scale · E), min, max)`, with `E` in `0..=1` for luma
and R'G'B' and `-0.5..=0.5` for chroma. Both `offset` and `scale` are exact
integers at every depth, so a fixed-point kernel can fold them into its own
coefficients without a rounding step of its own.

`min`/`max` are `0`/`2^depth − 1` in **both** ranges. H.273's `Clip1` clips to the
whole code range, not to 16..235: the footroom and headroom of a narrow-range
signal are legal values carrying real picture information, and clipping them at
the quantiser shows up as crushed blacks and clipped highlights.

`ColorRange::Unspecified` quantises as `Limited`. That is the defaulting every
decoder applies, and returning `None` would only push the same decision onto
every caller.

## How to change it

- **Adding a code point** the specification later assigns: add the variant to the
  enum in `lib.rs` (all three are `#[non_exhaustive]`, so this is additive), then
  `from_u8`, `all()`, `name()`, `from_name()`, and whichever of `chromaticity` /
  `encode`+`decode` / `luma_coefficients` applies. `all_lists_are_complete_and_ordered`
  and the exhaustive `match`es make an omission a compile error or a test failure.
- **Never** add a name to `name()` or `from_name()` from a specification alone.
  Probe the reference first — the two tables have already diverged four times.
  The recipe: write the value into an H.264 VUI and read it back.
  ```sh
  ffmpeg -f lavfi -i testsrc=size=64x64:rate=1 -frames:v 1 -pix_fmt yuv420p \
         -c:v libx264 -f h264 -y base.h264
  ffmpeg -i base.h264 -c copy -bsf:v h264_metadata=colour_primaries=22 \
         -f h264 -y o.h264 </dev/null
  ffprobe -show_entries stream=color_primaries -of default=nw=1:nk=1 o.h264
  ```
  Note the `</dev/null`: without it `ffmpeg` eats the rest of a shell loop.
- **Never** replace a Table 4 literal with the derived value. Read the
  "Coefficients" section above and the two tests named there first.
- **Gotcha — `#[repr(u8)]` casts.** `ColorRange` and `ChromaLocation` are *not*
  `repr(u8)` and their `to_u8` is a `match`, because their numbering is the
  reference's rather than the specification's. Adding a variant in the middle of
  either changes no code point, which is the point.
- **Gotcha — `Chromaticity::rgb_to_xyz` cannot handle ST 428-1.** Two of its
  "primaries" have `y = 0`, so `x/y` is undefined. `ColorPrimaries::rgb_to_xyz`
  special-cases it to the identity, which is what it is by definition. A new
  degenerate primary set would need the same treatment.

## Configuration

None. No features, no environment variables, no build script. `SUPPORTED_DEPTHS`
(8..=32) is the only tunable constant, and its upper bound is where `scale` stops
fitting in a `u32`.

## Dependencies

**None.** `[dependencies]` is empty, and that is a fact about the layering worth
seeing rather than hiding. The Phase 0 manifest declared `vaco-core` for the
shared `Error` taxonomy, but nothing here uses it: every entry point returns
`Option` rather than `Result`, because the crate sits on the scaler's hot path
and `Error::Option` allocates two `String`s. The option layer above translates a
`None` into a diagnostic, where it knows the option name. The edge was removed
rather than left dangling.

`proptest` is a dev-dependency, and the only one.

Consumers: `vaco-frame`, `vaco-codec-core` (`CodecParameters`),
`vaco-filter-core` (link properties) and `vaco-scale`, which is the crate the
coefficients exist for.

## Testing

`src/tests.rs` is split by whether a domain can be enumerated, and the split is
deliberate:

- **Exhaustive** (hand-written loops): all 256 code-point bytes, all 17 × 12
  matrix/primary pairs, all 65 bit depths, every printed name, every prefix and
  suffix of every transfer name, and the eight corners of the RGB cube. Sampling
  any of these would be strictly weaker than enumerating them.
- **`proptest`**: arbitrary text against `from_name`, and the continuous
  floating-point ranges — `encode`/`decode` inversion, monotonicity, finiteness
  over the extended domain, the Y'CbCr and XYZ round trips, and the `Y ∈ 0..=1`,
  `|Cb|,|Cr| ≤ 0.5` bound a kernel sizes its intermediate precision on. Shrinking
  is the whole reason: "the round trip fails somewhere in `0..=1` for BT.2020
  12-bit" is not a bug report, and "it fails at 0.0181" is.

One property is deliberately *not* asserted: that a proper prefix of a name never
resolves to the same variant. `log` is a declared alias of `log100`, so it does.
The assertions that do catch a leaking `starts_with` or `trim` are `"{name}x"`,
`" {name}"` and `"{name} "`, all of which must return `None`, and none of which
has an alias exception.

## Fuzzing

`fuzz/fuzz_targets/color_signalling.rs`, feature `color`. It drives both
untrusted entry points — names as UTF-8, code points as bytes — and the
arithmetic behind them: shifts by a bitstream-supplied bit depth, and divisions
by quantities derived from bitstream-supplied primaries. The fuzz profile enables
`overflow-checks`, so a wrap is a finding rather than a silent wrong answer.

```sh
cargo +nightly fuzz run color_signalling --features color -- -max_total_time=60
```

Last run: `exit=0 execs=#9102605`, `fuzz/artifacts/` empty before and after.
