# `vaco-core`

Layer 0 (foundation). Depends on `std`, `thiserror`, `tracing` — nothing else, ever. Dev: `proptest`.
Every other crate in the workspace depends on this one, so a change here is a workspace-wide change.

## What it is

The vocabulary every Vaco crate shares: the error taxonomy, exact rational arithmetic, the
timestamp/timebase model, `MediaType`, the ordered dictionary, the escaping grammar, and the CLI
value parsers (`-s 1920x1080`, `-r 30000/1001`, `-t 00:01:30.5`, `-fill_color red@0.5`).

Two rules explain almost every decision in it:

**Exactness.** No timestamp, time base or rate ever passes through `f64`. Rational arithmetic runs in
`i128` and reduces; rescaling multiplies in `i128` and divides exactly once, with the rounding mode
named by the caller. 30000/1001 is not 29.97, and a float error accumulated across a two-hour
timeline is audible desync. The `to_f64` methods exist for display and heuristics and say so in their
own documentation.

**No panics.** This crate is on the path of every byte of untrusted input in the project. A malformed
value is `None`/`Err`, an unrepresentable result is `None` or a documented saturation, and `0/0` and
`1/0` are ordinary inputs that every operation handles.

| Module | Contents |
|---|---|
| `error` | the closed `Error` enum every crate returns, and `Result<T, E = Error>` |
| `rational` | exact `i32/i32` `Rational` — time bases, frame rates, aspect ratios |
| `time` | `Timestamp`, `TimeBase`, `Duration`, `ExactDuration`, `Rounding`, `rescale_rnd` |
| `dict` | insertion-ordered `Dict` for metadata and option maps |
| `escape` | the shared quoting/escaping grammar of the option and filtergraph layers |
| `parse` | the CLI value grammars, plus `Rgba` |
| (root) | `MediaType` |

## How it works

### `Rational` — three classes of value, one type

A `Rational` is a raw `i32/i32` pair that is deliberately **not** kept in lowest terms. `Rational::new`
stores what you give it, because a muxer reproducing a file byte for byte (D5) needs the authored
`1001`, not a helpfully reduced equivalent.

| Class | Shape | Example | Meaning |
|---|---|---|---|
| finite | `den != 0` | `30000/1001` | the number `num / den` |
| infinite | `den == 0`, `num != 0` | `1/0` | ±∞ — a real frame rate that containers store |
| undefined | `0/0` | `Rational::UNDEFINED` | not known; also the `Default` |

**Equality is by value, not by field.** `1/2 == 2/4`, `1/0 == 7/0`, `0/0 != 0/1`. `Hash` hashes the
canonical reduced form, so `Eq` and `Hash` agree. To ask "is this the same literal pair", compare
`num` and `den` yourself.

**Ordering is partial, on purpose.** `PartialOrd` returns `None` when either operand is `UNDEFINED`,
because undefined has no position on the number line — that is exactly why the type has no `Ord`.
Where a total order is required (sorting, a `BTreeMap` key), `cmp_exact` supplies one, ordering
undefined below −∞. Both cross-multiply in `i128`; neither touches a float.

**Overflow.** All arithmetic canonicalises into `i128`, cross-reduces, and converts back. When the
exact result does not fit in `i32/i32`, the operator forms (`*`, `/`, `+`, `-`) fall back to the
closest representable rational and the `checked_*` forms return `None` instead. Nothing panics and
nothing wraps — `i32::MIN / -1` saturates to `i32::MAX / 1` rather than becoming its own negative.

`approximate` and `reduce` share one continued-fraction routine with semiconvergent refinement, so
the answer really is the best rational approximation under the denominator bound rather than merely
a convergent: `approximate(PI, 113) == 355/113`, `approximate(2.6, 1) == 3/1`.

### `Timestamp` — ticks in a base, never seconds

`Timestamp(Option<i64>)`: `None` models an absent timestamp, which is genuinely common in real media.
A sentinel value would get compared, printed and arithmetic'd by accident; `None` cannot be.

`rescale(from, to, rounding)` computes `ticks × from.num × to.den ÷ (from.den × to.num)`. Every
operand is at most 63 bits, so the product is formed in `i128` where it cannot overflow, and the
single division at the end is the only place precision is lost. That is why `Rounding` is a required
argument and not a default.

| Mode | Rounds |
|---|---|
| `Zero` | towards zero |
| `Infinity` | away from zero |
| `Down` | towards −∞ (floor) |
| `Up` | towards +∞ (ceiling) |
| `NearestAwayFromZero` | to nearest, ties away from zero — the default, and what presentation timestamps want |

Failure is explicit rather than guessed: an absent timestamp, an undefined/infinite base, or a `to`
base with a zero numerator all yield `Timestamp::NONE`. A result that leaves `i64` saturates in
`rescale` and is `None` in `checked_rescale` — a muxer needs to tell "plausible but wrong" from
"cannot be represented". `rescale_rnd(a, b, c, rounding)` is the same primitive for things that are
not timestamps.

`compare(self_base, other, other_base)` orders two timestamps in different bases by cross-multiplying
in `i128`. It does not convert either side, so no rounding decision is hidden inside the answer.

### Duration units at API boundaries

`Duration` stores reduced rational seconds, and `ExactDuration` is a compatibility
alias for that same type. `Duration::from_ticks` and `Timestamp::to_duration`
retain native values such as 1024/44100 and 1001/30000 exactly. Comparisons and
checked addition/subtraction do not choose an intermediate clock.

Use `from_micros` for interfaces that explicitly use microseconds. Integer output
boundaries choose rounding through `checked_micros` or `to_ticks_rounding`;
`to_ticks` defaults to nearest, ties away from zero. The legacy `as_micros`
display accessor rounds the same way and saturates at the `i64` limits. The
compatibility `to_duration(rounding)` explicitly returns a microsecond-rounded
value; it is not needed when passing durations between media components.

Keep rational fields private and normalize every constructor: canonical zero is
0/1 and the denominator is positive. Exact arithmetic refuses an intermediate or
result outside `i128`. Widened rescaling can divide products that exceed `u128`
without wrapping. No configuration or new dependency is required.

### `Dict`

A `Vec<(Box<str>, Box<str>)>` with a linear scan. Media metadata dictionaries have single-digit entry
counts; a hash map costs more in allocation and hashing than the scan saves, and it would lose the
insertion order muxers depend on for byte-identical output. Multi-key and suffix-matching semantics
fall out of an ordered vector; they do not fall out of a `HashMap`.

`DictFlags` mirrors the `AV_DICT_*` bits one for one — `match_case`, `ignore_suffix`,
`dont_overwrite`, `append`, `multikey` — because option strings written against the reference CLI
rely on the behaviours they select. `DictFlags::exact()` is the everywhere-default.

### `escape`

One level of the grammar has three ways to write a character: bare, backslash-escaped (`\:`), or
inside single quotes (`'a:b'`). A literal `'` closes the quote, escapes itself, and reopens:
`'a'\''b'` decodes to `a'b`.

Levels nest, because `escape` always escapes the backslash itself: N escapes need exactly N
unescapes, and that identity is proptested. Research 05 §5.2 names the three levels the CLI stacks —
option value, filter description, invoking shell.

**`split_raw` and `split_once_raw` return pieces that are still escaped.** That ordering is what
makes nesting work: an outer level finds its own separators without being confused by an inner
level's, and each piece is unescaped exactly once on the way down. Unescaping first and splitting
after is the bug this API exists to prevent.

## Grammar reference

These are a compatibility contract, taken from `planning/research/05-fftools-cli.md` §5.6. Every
parser returns `Option`, rejects trailing junk, and has a `format_*` counterpart that it inverts
exactly.

| Function | Accepted | Canonical output |
|---|---|---|
| `parse::image_size` | `WxH`, `WXH`, or an abbreviation (53 of them: `ntsc`, `pal`, `qcif`, `hd1080`, `4kdci`, `uhd4320`, …) | `format_image_size` → `WxH` |
| `parse::video_rate` | `num/den`, `num:den`, an integer, a decimal, or `ntsc` (30000/1001), `pal` (25/1), `qntsc`, `qpal`, `sntsc`, `spal`, `film` (24/1), `ntsc-film` (24000/1001) | `format_rational` → `num/den` |
| `parse::rational` | `num/den`, `num:den`, an integer, or a decimal (approximated, `max_den` 10⁶). `1/0` and negatives parse — §5.6 is explicit that filtering them is the caller's job | `format_rational` → `num/den` |
| `parse::duration` | `[-][HH:]MM:SS[.m…]` (at most three colon columns, only the last fractional) or `[-]S+[.m…][s\|ms\|us]`; a bare number is seconds. Result is microseconds | `format_duration` → `[-]S.ffffff`; `format_duration_clock` → `[-]HH:MM:SS.ffffff` |
| `parse::color` | `#RRGGBB[AA]`, `0xRRGGBB[AA]`, one of 147 X11/SVG names (case-insensitive), `random`, any of those with `@alpha` where alpha is a float in `0.0..=1.0` or `0xNN` | `format_color` → `0xRRGGBBAA` |
| `parse::boolean` | `1`/`true`/`on`/`yes`/`enable`/`enabled` and their negatives | `format_boolean` → `true`/`false` |
| `parse::binary` | an even number of hex digits, either case | `format_binary` → lower-case hex |

Notes:

- `duration` scales its fractional part by repeated integer division rather than through `f64`, so
  `0.1` is exactly 100 000 µs and never 99 999.
- `i64::MIN` microseconds is the one `Duration` that does not round-trip: `format_duration` prints a
  magnitude with no positive counterpart. It still parses and formats without panicking.
- Six-decimal duration text is a rounded display boundary. It is not an exact
  serialization of arbitrary native media clocks; preserve the rational value
  or native ticks when the value must round-trip.
- `color("random")` draws fresh RGB with alpha 255 on every call and is therefore the one input that
  does not round-trip. It uses a SplitMix64 counter seeded from the wall clock — decorative, with no
  cryptographic or statistical claim attached, and deliberately not exposed as a general RNG.
- The colour table carries both the `gray` and `grey` spellings of the seven names SVG defines twice.

## How to change it

- **Adding a parser.** Add the parse function, the `format_*` inverse, and a `*_names()` accessor if
  it has a table. Then add it to `parsers_roundtrip` and `parsers_never_panic` in `tests/text.rs`.
  A parser without both is not finished.
- **Touching `Rational` arithmetic.** Do the work in `canonicalise`/`from_canonical`/`approx_canonical`
  rather than in the operator impls; the operators are three lines each precisely so there is one
  place where overflow is handled. Run the boundary unit tests (`reduce_at_i32_min`,
  `multiplication_does_not_overflow_at_the_boundaries`, `negation_at_the_double_i32_min`) before the
  proptests — they are faster and they are where the bugs are.
- **The `i32::MIN` traps.** `-i32::MIN` wraps. `Rational::neg_value` moves the sign to the denominator
  when the numerator is `i32::MIN`, and spells out the answer when *both* fields are `i32::MIN` (the
  value is exactly 1, and flipping either field maps it back onto itself). Proptest found that one;
  it will find the next one too.
- **Touching rounding.** `tests/time.rs` carries a reference implementation that derives all five
  modes from the *floor* quotient, where `muldiv_rnd` truncates towards zero and adjusts. Keep the two
  derivations different — a reference that mirrors the implementation only tests that the code equals
  itself.
- **Changing `Rational`'s equality.** Don't, without reading the note above: `PartialEq`, `Hash`,
  `PartialOrd` and `cmp_exact` are one interlocking set, and `clippy::derived_hash_with_manual_eq`
  exists because breaking that set is easy.
- **Adding a dependency.** You cannot. This crate's whole point is that it is the bottom of the
  graph. `num-rational` was assessed and rejected: it is generic over integer types and allocates for
  `BigInt`, where this needs exactly `i32/i32` with `i128` intermediates, `Option`-returning
  arithmetic and a `Display` that matches the CLI.

## Testing

`cargo test -p vaco-core` runs three files:

| File | Covers |
|---|---|
| `tests/rational.rs` | boundary units at `i32::MIN`/`MAX`, plus properties: `reduced` preserves value and is coprime and idempotent; `cmp_exact` matches an independently derived reference, is total and transitive; `PartialOrd` is `None` exactly for undefined and never disagrees with `==`; `Hash` agrees with `Eq`; `checked_*` are exact; the operators are total; `approximate` respects its bound and recovers exact ratios |
| `tests/time.rs` | all five rounding modes against an independent floor-derived reference across the `i64` extremes; the rescale error never exceeds the mode's stated bound; round trips stay within one tick of the coarser grid; cross-base comparison matches exact rational arithmetic and is antisymmetric; two hours of NTSC frames survive a round trip through the microsecond base unchanged |
| `tests/text.rs` | escaping round-trips in all three modes and nests to arbitrary depth; `split(join(parts)) == parts`; `Dict` round-trips through its string form; every parser inverts its formatter; nothing panics on arbitrary input |

**Outstanding.** Two acceptance criteria from plan 11 §4.6 are not yet met, both because they need
infrastructure this crate does not own:

1. **Fuzz targets.** D6 says a crate that parses input and has no fuzz target is not done.
   `parse_duration`, `parse_color`, `parse_image_size`, `parse_video_rate`, `dict_parse_string` and
   `escape_unescape` belong in `fuzz/fuzz_targets/`, which is a separate workspace outside this
   crate's directory.
2. **Differential validation of the tables.** The colour, frame-size and frame-rate tables are
   internally consistent and derived from the published abbreviation lists, but have not been
   diffed against the reference binary (`ffmpeg -f lavfi -i color=c=<spec>:s=2x2 …` over a generated
   corpus). That needs the pinned reference binary, which arrives with the conformance harness.

## Configuration

None. No environment variables, no features, no build-time switches. The only tunables are arguments:
`Rounding` on every rescale, `max_den` on every approximation, `DictFlags` on every dictionary
operation, and `escape::Mode` on every escape.

## Dependencies

| Crate | Why |
|---|---|
| `std` | everything |
| `thiserror` | error derives (the `Error` `Display` is still hand-written — its text is a compatibility surface) |
| `tracing` | the logging façade the rest of the project emits through |
| `proptest` (dev) | the property tests above |

## Divergences from plan 11 §4

Recorded because contributors will compare the two. In every case the frozen tree won, per the
standing instruction to trust the tree over the plan.

- **`Rational` fields are public and `new` does not reduce.** §4.2 has private fields, a reducing
  `new` that panics on `den == 0`, and a separate `new_raw`. The frozen tree has `pub num`, `pub den`
  and a total `new`; `reduced`/`checked_reduced` are separate. Both `1/0` and `0/0` are valid values,
  so a panicking constructor was never viable.
- **`Ts`/`TimeBase(Rational)` became `Timestamp(Option<i64>)` and `type TimeBase = Rational`.** §4.2
  has `Ts(pub i64)` with a `TimeBase` newtype; the tree makes absence a type-level fact instead.
  `TimeBase::MICROSECONDS` still resolves, as a constant on `Rational`.
- **`Rounding` is a plain enum, not `{ mode, pass_min_max }`.** The saturate-versus-refuse choice is
  made by picking `rescale` or `checked_rescale`, which is a clearer place for it.
- **The `parse::*` functions return `Option`, not `Result`.** This matches the shape `vaco-opts`
  already built against; a rejected value carries no information a `Result` would add, and the caller
  wraps it in its own `OptError` with the option name attached.
- **`rescale_rnd` returns `Option<i64>`, not `Result<i64>`.** The frozen `Error` enum has no
  `Overflow` variant to return.
- **`parse::date` is not implemented.** §4.2 lists it. It needs civil-date arithmetic and a local
  timezone, and it was outside the implementation brief; `-timestamp` will need it.
- **`num` and `log` are not implemented.** §4.2 lists both. `vaco-simd` already worked around the
  absence of `vaco_core::num::clip_u8` by defining `ops::clip_u8` locally, and nothing yet needs the
  `log::Level` ladder. Both are still missing.
- **`Limits`/`try_reserve` live in `vaco-limits`, not here.** §4.4 puts them in this crate; the tree
  has a dedicated layer-0 crate for them, which is the better home.
- **`Error` is the tree's variant set, not §4.2's.** No `ComponentKind`, `Again`, `Experimental`,
  `InputChanged` or `Overflow`; `InvalidData` carries a `&'static str` rather than a struct.

## See also

- `docs/core/vaco-opts.md` — the option system, the largest consumer of `Dict`, `escape` and `parse`.
- `planning/11-foundations.md` §4 — the original specification.
- `planning/research/05-fftools-cli.md` §5.6 — the grammars the parsers implement.
