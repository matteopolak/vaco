# `vaco-opts` and `vaco-opts-derive`

Layer 0 (foundation). Depends on `vaco-core`, `thiserror`, `smallvec`. The derive depends on
`syn`/`quote`/`proc-macro2`. Dev: `proptest`, `insta`.

## What it is

The `AVOption`/`AVClass` equivalent, and the reflection layer the whole CLI rests on. Every
configurable component — demuxer, muxer, decoder, encoder, filter, protocol, scaler, resampler —
declares its options once with `#[derive(Options)]` and gets parsing, validation, serialisation,
help data and runtime mutation for free.

Two crates, because a derive macro and the crate it serves are meaningless apart:
`vaco-opts` (runtime) and `vaco-opts-derive` (proc macro), re-exported so consumers only ever name
`vaco-opts`.

### Why not `clap`

FFmpeg's command line is not a fixed set of flags. Every codec, format and filter contributes its
own private options at runtime, and `-h filter=scale` has to enumerate them **without instantiating
anything**. So the primary object here is not a parser but a `&'static Schema` reachable from a
*type* via `HasSchema`, with `schema_of::<T>()` as the entry point. No builder-style argument parser
models that, which is why this crate exists.

## How it works

### The type model

Twenty bases plus one orthogonal array modifier, covering the inventory's 21 `AVOptionType` values
(`FLAG_ARRAY` is a modifier in the C model too):

```rust
pub enum OptBase {
    Flags, Int, Int64, UInt, UInt64, Double, Float, Bool, String, Rational,
    Binary, Dict, Const, ImageSize, PixelFmt, SampleFmt, ChLayout, VideoRate,
    Duration, Color,
}
pub struct ArrayDesc { pub sep: char, pub min_len: u32, pub max_len: u32 }
pub struct OptKind   { pub base: OptBase, pub array: Option<ArrayDesc> }
```

`OptBase::Const` names the kind for introspection; it is never a struct field.

### The four traits

| Trait | Role |
|---|---|
| `OptValueKind` | `const BASE: OptBase`. Read by the derive to fill in `OptionDesc::kind`. |
| `OptValue` | Everything dynamic: `parse_into`, `serialize`, `as_f64`, `eq_dyn`, `clone_box`, `assign_from`. Dyn-compatible. |
| `Options` | The object projection: `schema()`, `slot(id)`, `slot_mut(id)`, `children()`, `defaults()`, `check_range(id)`. Generated. |
| `OptionsExt` | Every generic operation, blanket-implemented for all `Options` and for `dyn Options`. |

`OptValue` is implemented in this crate for `i32/i64/u32/u64/f32/f64/bool/String/Rational/Duration`,
for the newtypes `Binary`, `VideoRate` and `Rgba`, for `(u32, u32)`, for `Dict`, and generically for
`Option<T>` (unset) and `Vec<T>` (array).

**`PixelFmt`, `SampleFmt` and `ChLayout` are *not* implemented here.** That is the F6 dependency
inversion: `vaco-opts` is layer 0 and `vaco-pixfmt` is layer 1, so `vaco-opts` only *names* those
bases and the layer-1 crates supply `impl OptValue for PixelFormat`. Layering stays acyclic and
adding a new option-carrying type never touches this crate. `tests/support/mod.rs` implements three
stand-ins from outside the crate, which is the test that the inversion actually works.

### Type erasure without `unsafe`

FFmpeg computes `(char *)obj + offset` and casts to whatever the table claims. That is the one
pattern safe Rust cannot have, and a naive port reaches for `unsafe`. Instead the derive generates
an indexed `match` returning `&mut dyn OptValue`:

```rust
fn slot_mut(&mut self, id: OptId) -> Option<&mut dyn OptValue> {
    match id.0 {
        0 => Some(&mut self.in_sample_rate as &mut dyn OptValue),
        1 => Some(&mut self.out_sample_rate as &mut dyn OptValue),
        _ => None,
    }
}
```

One jump table, the same generic machinery, and the type mismatch made impossible.
`set_typed::<i32>` recovers full speed through `Any::downcast_mut`, and option setting is a
configuration-time operation anyway.

### The unit mechanism

Named constants are a property of the **unit**, not of the option. Several options may share one
unit (`scaler` and `scaler_sub` both use `sws_scaler`), and `-h` groups the constants under each
option that references it. `Schema::consts_for_unit(unit)` scans the schema's options for that unit
name and yields their `ConstDesc`s, reproducing the C mechanism's shape exactly.

Two macros produce units:

* `#[derive(OptEnum)]` on a fieldless enum emits `impl OptEnumConsts` (the `CONSTS` slice),
  `TryFrom<i64>` and `impl OptValue` (parse = look the string up in `ctx.consts`, map the `i64` back
  to a variant; serialize = the reverse).
* `opt_flags!` emits a `u64` newtype with `const` members, `empty/bits/contains/union/difference/
  insert/remove`, `OptEnumConsts`, and an `OptValue` impl implementing the `+flag-flag`
  accumulate/remove grammar over `ctx.consts`.

`bitflags` cannot do this job: our flag types must carry a per-flag name, help string and unit so
`-h` can print them, and adopting it would mean declaring every flag twice.

### Ranges

Two representations, deliberately:

* **`Options::check_range(id)`** — generated, typed, exact. This is the authoritative check.
* **`OptionDesc::range: Option<OptRangeDisplay>`** — an `f64` pair used *only* to render `-h full`'s
  `(from … to …)` text.

FFmpeg stores `min`/`max` as `double`, which silently loses precision above 2^53 and mis-validates
`int64` options. Keeping the check typed removes that bug class; keeping the `f64` pair keeps the
help text identical. `tests/basics.rs::int64_range_is_exact_above_two_to_the_53` pins it.

`RangeCheckable` is implemented for every integer and float, for `Duration`, and — mapping
element-wise — for `Option<T>` and `Vec<T>`.

### Strings in, strings out

`set_from_string(s, kv, pairs)` parses the filter/protocol/muxer grammar. Values before the first
`=` are positional and map to **declaration order**; a positional after a named one is an error.
`serialize(SerializeFlags)` is its inverse. The pair is *specified* by round-trip and tested that
way — `serialise_then_parse_is_identity` runs 512 arbitrary instances covering every base, with
`skip_defaults` on and off.

Escaping is three-tier (bare / `\x` / `'…'`) and lives in `escape`. `escape` always escapes the
backslash itself, so levels nest: an array element is escaped for its separator, and the whole value
is escaped again for `:` and `=`. `split_raw` splits *without* unescaping so each level unescapes
exactly once on the way down.

### Rollback

`set_str` snapshots the slot with `clone_box`, parses, then runs `check_range`. If either fails it
restores from the snapshot. That is what makes "a rejected value leaves the object unmodified" an
invariant rather than a hope; `range_invariance` asserts it as a property.

## The attribute grammar

### Struct level — `#[options(…)]`

| Key | Meaning |
|---|---|
| `name = "…"` | **Required.** The class name, e.g. `"SwrContext"`. |
| `help = "…"` | One-line class description. Lands in `Schema::class_help`. |
| `no_default` | Suppress the generated `impl Default` for a type that writes its own. |

### Field level — `#[opt(…)]`

Every field must carry `#[opt(…)]`, `#[opt(child)]` or `#[opt(skip)]`. A field with no attribute is
a compile error, so a new field can never silently become invisible to the option system.

| Key | Meaning |
|---|---|
| `name = "…"` | Primary option name. Defaults to the field name. |
| `alias = "…"` | Additional accepted spelling. **Repeatable.** |
| `help = "…"` | **Required** for every option field. |
| `default = <expr>` | Const expression of the field type. Also feeds the generated `impl Default`. |
| `default_repr = "…"` | Override the rendered default in `-h full`. See the note below. |
| `range = <a>..=<b>` | Typed inclusive range. Emits both the exact check and the `f64` display pair. Must be `..=`. |
| `unit = "…"` | Groups named constants; makes the option accept const names. |
| `consts = <expr>` | A `&'static [ConstDesc]` to use instead of the field type's `OptEnumConsts`. Requires `unit`. |
| `flags(a, b, …)` | See below. |
| `array(sep = '\|', min_len = 0, max_len = 64)` | Marks the field a `Vec<T>` array option. Required on any `Vec` field; rejected on any non-`Vec`. |
| `child` | The field is itself an `Options`; its schema becomes a child. Exclusive with every other key. |
| `skip` | Not an option. Exclusive with every other key. |

`flags(…)` accepts: `encoding`, `decoding`, `filtering`, `video`, `audio`, `subtitle`, `export`,
`readonly`, `bsf`, `runtime`, `deprecated`, `child_consts`, plus the shorthand `param` =
`encoding + decoding`.

### Enum level — `#[derive(OptEnum)]`

| Attribute | Meaning |
|---|---|
| `#[opt_enum(unit = "…", base = "int")]` | **`unit` required.** `base` is one of `int`, `int64`, `uint`, `uint64`, `flags`, `double`, `float`; default `int`. |
| `#[opt_const(name = "…", help = "…")]` | Per variant. `name` defaults to the lower-cased variant ident. |

The enum must be fieldless and should derive `Debug, Clone, Copy, PartialEq`. Explicit
discriminants are honoured.

### `Option<T>` and the tri-state bool

`Option<T>` means "unset" for every base, and is the idiomatic way to express FFmpeg's magic
`-1`/`INT_MIN` defaults (F4). `None` serialises as the empty string, **except** when the inner base
is `Bool`, where it serialises as `auto` — `auto` is genuinely distinct for options like
`src_range`, and putting it in the type rather than in a `-1` convention makes the two cases
distinguishable to the compiler.

A plain `bool` field accepts only the boolean spellings (`true/false/1/0/on/off/yes/no/
enable/disable`); `Option<bool>` additionally accepts `auto`.

## How to change it

### Adding an option to an existing component

Add a field with `#[opt(…)]`. Nothing else changes — the `OptId`s are indices assigned in
declaration order, so **inserting a field in the middle renumbers everything after it**. That is
fine internally (ids never leave the process) but it *does* change positional-argument order, which
is a CLI compatibility surface. Append rather than insert unless you mean to change the order.

### Adding a new option base

1. Add the variant to `OptBase` and to `OptBase::ALL`, and give it a name in `OptBase::name()` —
   that string is the `-h full` type column and is an interface fact.
2. Implement `OptValueKind` and `OptValue` for the carrier type. If the carrier lives in a layer-1
   crate, implement it *there*, not here.
3. Extend `tests/support/mod.rs::AllKinds` with a field of that type. The test
   `every_base_except_const_appears_in_the_reference_object` fails until you do, and the round-trip
   property then covers the new base for free.

### Implementing `OptValue` for your own type

```rust
impl vaco_opts::OptValueKind for PixelFormat {
    const BASE: vaco_opts::OptBase = vaco_opts::OptBase::PixelFmt;
}

impl vaco_opts::OptValue for PixelFormat {
    fn parse_into(&mut self, s: &str, ctx: &ParseCtx<'_>) -> Result<(), OptError> { … }
    fn serialize(&self, out: &mut String, _ctx: &SerCtx<'_>) { … }
    vaco_opts::impl_opt_value_common!(PixelFormat);   // the other six methods
}
```

`impl_opt_value_common!` needs `Self: Clone + PartialEq + Debug + Send + Sync + 'static`.

### Gotchas

* **`default_repr` is best effort at expansion time.** The macro can render a *literal* default
  (`0`, `1.5`, `true`, `"x"`, `None`) but it cannot evaluate `SampleFormat::None` or
  `SwrFlags::empty()`. Pass `default_repr = "…"` for those, or — better, for anything that has to be
  exactly right — call `OptionsExt::default_repr(name)`, which walks a real `defaults()` instance
  and is always correct.
* **`Vec<T>` needs `array(…)`.** The separator has no safe default at the schema level, so the macro
  refuses to guess.
* **Child option names must not collide with the parent's.** Lookup finds the parent first, and
  `serialize` emits both levels into one flat string.
* **`readonly` options are omitted from `serialize`**, matching `av_opt_serialize`. They therefore
  do not round-trip; that is intended, since `set_str` rejects them anyway.
* **`Option<String>` cannot distinguish `None` from `Some("")`** on the wire. Neither can the C
  model — the grammar has no way to write it.
* **An array of exactly one empty-string element** is likewise indistinguishable from an empty
  array. Both are excluded from the round-trip property, with the reason recorded in the
  `prop_filter`.
* **Duplicate dictionary keys collapse on parse.** `a=1:a=2` means `a=2`, by the grammar.
* **`parse::color` does not implement `random`.** It needs an RNG, which this crate has no business
  owning.
* **The macro emits `::vaco_opts::…` paths.** A consumer that renames the dependency will not
  compile. There is no `crate = "…"` escape hatch yet because nothing needs one.

## Testing

| File | What it covers |
|---|---|
| `tests/basics.rs` | Every base: valid, boundary, invalid, empty. Flag `+a-b` accumulation, const lookup and case sensitivity, aliases, ranges, positional arguments, escaping, children, dict application, typed access, runtime gating, help entries, introspection. |
| `tests/roundtrip.rs` | The property suite: `set_from_string(serialize(x)) == x` with `skip_defaults` on and off, serialisation stability, range invariance and no-partial-application, dict partitioning, escape/split round trips, the flag grammar, and a no-panic property over arbitrary input. |
| `tests/snapshot.rs` | The rendered schema, pinned with `insta`. |
| `tests/worked_example.rs` | Plan 11 §6.4's `SwrContext` table, transcribed, as an executable check that the plan's declaration compiles and behaves. |
| `vaco-opts-derive/src/tests.rs` | The compile-fail suite (see below). |

### The compile-fail suite

Plan 11 §6.9 asks for `trybuild`. It is **not** in `[workspace.dependencies]` and D10 makes every
adoption a reviewed decision, so this crate did not add one. The hand-rolled equivalent drives
`gen_options::expand` / `gen_enum::expand` directly and asserts the message each rejection produces:
missing `help`, `range` on a `String`, `unit` on a `Color`, duplicate names and aliases, `array` on
a non-`Vec`, a `Vec` without `array`, unknown keys and unknown flag names, `skip`/`child` combined
with other keys, generics, non-structs, and every `OptEnum` rejection. Twenty-nine cases, running in
milliseconds. If `trybuild` is ever adopted, keep these — asserting on the message is a superset of
what a `.stderr` file pins.

### Still missing

* **A fuzz target.** D6 wants `opts_set_from_string` and `opts_flags_parse` under `cargo-fuzz`. The
  `fuzz/` directory is excluded from the workspace and is not this crate's to create. The property
  `arbitrary_input_never_panics` covers the same ground weakly, on every `cargo test`.
* **The `-h full` differential harness**, plan 11 §6.9's highest-value oracle. It belongs in
  `vaco-cli-core`. The snapshot in `tests/snapshot.rs` renders exactly the facts it will diff.

## Configuration

None. There are no environment variables, no feature flags and no build-time configuration; a crate
whose entire job is to model configuration would be an odd place to read any.

## Dependencies

| Crate | Why |
|---|---|
| `vaco-core` | `Rational` and `Duration` as option value types. |
| `thiserror` | `OptError`'s `Display`. |
| `smallvec` | Small-vector storage on the resolution path. |
| `syn` / `quote` / `proc-macro2` | The derive. `syn` needs `full` (for `Expr` in `default = …`) and `extra-traits`. |
| `proptest`, `insta` (dev) | The property and snapshot suites. |

`darling` was considered for the attribute grammar and rejected on D10 Gate 3 (shallow) and on model
grounds: the grammar has a repeatable `alias`, a `flags(…)` list, a typed `range` and a `default`
const expression, and hand-parsing with `syn` is ~450 lines we would rather own than bend `darling`
around. No media-specific capability is at stake anywhere in this crate.

## Divergences from plan 11 §6

Recorded so contributors comparing the two are not surprised. Also listed in the crate-level docs.

| Plan says | Reality | Why |
|---|---|---|
| `Dict`, `escape`, `parse::*`, `Rgba` live in `vaco-core` (§4.2) | They live in `vaco-opts` | `vaco-core` is still a stub without them and this crate cannot be written without them. The APIs match §4.2's shape; delete and re-export when it catches up. |
| `OptValue { const BASE: OptBase where Self: Sized; }` | Split into `OptValueKind` | An associated const makes a trait non-dyn-compatible, and the `where Self: Sized` escape hatch is unstable on 1.97 ("generic const items", rust#113521). |
| `Options::children() -> &[&dyn Options]` | `-> Vec<&dyn Options>` | No struct can implement the plan's signature — it would have to return a reference to a temporary. |
| Range check in a separate `rt::RangeCheck` trait | `Options::check_range` | `set_str` only ever holds `&mut dyn Options` and could not reach a non-supertrait method. |
| `ParseCtx { consts, unit, range, array }` | Plus `name` | Without it an `OptValue` impl cannot build an `OptError`. |
| `Schema { class_name, options, children }` | Plus `class_help` | `#[options(help = "…")]` had nowhere to land. |
| `OptBase::Rational` and `VideoRate` both carry `vaco_core::Rational`; `Binary` is `Vec<u8>` | `VideoRate` and `Binary` are newtypes | A type can carry only one `OptValue` impl, and `Vec<u8>` would overlap the blanket array impl. |
| `rt::array(b'|', …)` | `rt::array('|', …)` | `ArrayDesc::sep` is a `char`. |
| — | `default_repr` and `consts` attribute keys are new | See the gotchas above. |
| — | Fields require an explicit `#[opt(…)]` | The plan's example never shows an unannotated field; making it an error is the safer reading. |

Nothing else in the plan turned out wrong.

## See also

* `planning/11-foundations.md` §6 — the specification.
* `planning/research/01-libavutil-swr-sws.md` §5 — the `AVOption` feature inventory.
* `planning/00-decisions.md` — D2 (`forbid(unsafe_code)`), D6 (fuzzing), D9 (interface names are
  implementable, help *text* is not), D10 (dependency policy).
