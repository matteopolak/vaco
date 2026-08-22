# `vaco-sampfmt`

## What it is

The audio counterpart of `vaco-pixfmt`: twelve sample formats — six sample types
(`u8`, `s16`, `s32`, `s64`, `flt`, `dbl`) crossed with packed and planar storage
— and the metadata every audio component asks about them. Sample width,
planar-ness, the CLI-facing name, the numeric kind, the planar/packed pairing,
and the buffer arithmetic for a frame.

It is metadata only. There is no conversion code and no "best format for this
codec" scoring; those need signal-processing judgement and belong in
`vaco-resample`.

## How it works

Unlike `vaco-pixfmt`, nothing here is generated. Twelve variants is small enough
that a `match` per property is both the clearest form and the fastest: every
accessor is a `const fn`, so a call on a compile-time-known format folds to an
immediate inside a monomorphised resampling kernel, and a call on a dynamic
format is a jump table. The test `accessors_are_const` uses them in `const`
items, so losing that property is a compile error rather than a silent
regression.

### `ALL` is not the enum's declaration order

The one piece of real subtlety. `ffmpeg -sample_fmts` lists:

```
u8 s16 s32 flt dbl u8p s16p s32p fltp dblp s64 s64p
```

— the two 64-bit integer formats **last**, after the planar float ones. That
order is observable output (a `-sample_fmts` listing prints one row per format in
it), so `SampleFmt::ALL` reproduces it, and every listing and every iteration
that reaches a user must go through `ALL`. The enum itself is declared in the
tidy order, because its discriminants are ours and are not observable anywhere.

### Buffer arithmetic

`plane_count`, `plane_size` and `buffer_size` are the audio equivalent of
`vaco-pixfmt`'s `plane_layout`. A packed format has one plane holding
`channels × samples × width`; a planar format has one plane per channel, each
holding `samples × width`. Both operands come from a container header in
practice, so the multiplication is **checked**, not saturating — on a 64-bit
target `usize::MAX == u64::MAX`, and a saturated product compares equal to the
cap and passes the very bound check it was supposed to fail. That was a real bug
caught by a test, not a hypothetical.

## How to change it

- **Adding a format.** Add the variant, then extend every `match` — the compiler
  finds them all, since none has a wildcard arm — and add it to `ALL` **in the
  position the reference lists it**, not in the tidy position.
  `matches_the_reference_listing` will fail until the two agree.
- **Adding a property.** Add a `const fn` with an exhaustive `match`. Resist
  `_ =>` arms: exhaustiveness is what stops a new format from silently inheriting
  a wrong default.
- **Do not add `none` to `from_name`.** The reference accepts it, but one level
  up, in the option layer, where the target is a nullable field. Our equivalent
  is `Option<SampleFmt>` and the `None` spelling belongs with it, in `vaco-opts`.
- The parser is exact-match, case-sensitive and does not trim. See the `D17` note
  on `from_name` before "fixing" any of that.

## Provenance

Names and depths are **interface facts** (D7/D9): a command line written against
the reference has to mean the same thing here, and `ffprobe`'s `sample_fmt` field
has to spell them identically. They were recorded by probing the shipped binary,
never by reading its source.

### How the table was obtained

```
$ ffmpeg -hide_banner -sample_fmts
name   depth
u8        8
s16      16
s32      32
flt      32
dbl      64
u8p       8
s16p     16
s32p     32
fltp     32
dblp     64
s64      64
s64p     64
```

That transcript is `REFERENCE_TABLE` in `src/tests.rs`, verbatim, and
`matches_the_reference_listing` asserts our `ALL` reproduces both columns and the
order.

### How the parser's edges were established

`av_get_sample_fmt`'s behaviour was probed through `-sample_fmt`, which reaches
it directly:

```
$ ffmpeg ... -sample_fmt s16     # accepted
$ ffmpeg ... -sample_fmt S16     # Invalid sample format 'S16'
$ ffmpeg ... -sample_fmt ' s16'  # Invalid sample format ' s16'
$ ffmpeg ... -sample_fmt 's16 '  # Invalid sample format 's16 '
$ ffmpeg ... -sample_fmt none    # Invalid sample format 'none'
$ ffmpeg ... -sample_fmt 1       # Invalid sample format '1'
```

Note the trap: probing through `-af aformat=sample_fmts=...` instead makes `s16 `
and ` s16` *look* accepted. That is the filter's list splitter trimming its
elements, not the format parser, and taking it at face value would have made our
parser wrongly permissive.

To re-verify against a newer reference build, re-run the two commands above and
compare with `REFERENCE_TABLE` and `unknown_names_are_rejected_not_guessed`.

## Testing

`cargo test -p vaco-sampfmt`. Sixteen tests: the reference-listing comparison,
exhaustive per-format invariant loops (name round-trip, planar/packed bijection,
width/depth agreement), hand-computed buffer arithmetic, the overflow case, and
four proptest properties covering name round-tripping, arbitrary text, and the
`buffer_size = plane_size × plane_count` identity.

The fuzz target is `fuzz/fuzz_targets/sampfmt_parse.rs`:

```
cargo +nightly fuzz run sampfmt_parse --features sampfmt --fuzz-dir fuzz -- -max_total_time=60
```

It drives the name parser with arbitrary text and the buffer arithmetic with
arbitrary `(format, channels, samples)`, checking the size against a `u128`
recomputation — so a wrap is a finding rather than an under-allocation.

## Configuration

None. No features, no environment variables, no runtime configuration.

## Dependencies

| crate | why |
|---|---|
| `vaco-core` | `Error`, for `from_name` and `buffer_size` |
| `proptest` (dev) | the round-trip and arithmetic properties |

`vaco-frame` and `vaco-frame`'s pool depend on this crate; `vaco-resample` and
every audio codec will.

## Known gaps

- **No `vaco_opts::OptValue` impl.** `vaco-opts` reserves `OptBase::SampleFmt`
  for this crate to fill, but the frozen manifest has no `vaco-opts` dependency
  and `vaco-pixfmt` has not filled its equivalent either. Both should land in the
  same change, once the layer-1-implements-layer-0-trait direction is confirmed.
