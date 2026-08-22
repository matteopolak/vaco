# `vaco-pixfmt`

## What it is

The pixel-format enum and its descriptor metadata: 268 formats, each with a plane
count, per-component plane/step/offset/shift/depth, chroma subsampling, average
bits per pixel, and a flag set. Every scaler, filter, decoder, encoder and the
`-pix_fmts` listing reads this table.

It is metadata only. There is no conversion code, no format-compatibility
scoring and no "best format for" logic here — those need colour knowledge and
belong in `vaco-scale`. Keeping this crate pure data is what makes it exhaustively
testable.

**The table is generated.** `crates/model/vaco-pixfmt/src/table.rs` is produced by
`cargo xtask gen-pixfmt` from a declarative family description. Do not edit it.

## How it works

### The generator

```
xtask/src/gen_pixfmt/
  source.rs   the declarative families, exceptions and aliases  <- you edit this
  model.rs    the intermediate `Format` shape and the flag vocabulary
  expand.rs   families -> a flat, ordered list of formats
  mod.rs      emits table.rs, and the --check mode
```

`source.rs` declares ~35 families plus a handful of explicit exceptions. Families
are Rust `const` data rather than TOML or RON for two reasons: `rustc`
type-checks and exhaustiveness-checks a declaration before it can produce a bad
table, and the generator needs no data-format parser (xtask has zero
dependencies, deliberately — it gates the build).

Expansion is mechanical. A `PlanarYuv` family crossed with subsamplings, depths,
alpha and endianness yields 74 formats; `PackedRgb` yields 52; and so on. Nothing
in `expand.rs` special-cases a format name. If a format needs a rule that is not
mechanical, it goes in `Family::Explicit` instead — a family that grows a special
case has stopped paying for itself.

The emitted file contains:

| item | what it is |
|---|---|
| `pub enum PixFmt` | `#[repr(u16)]`, dense discriminants `0..268` in declaration order |
| `DESCRIPTORS: [PixFmtDescriptor; 268]` | index-aligned with the discriminants |
| `ALL: [PixFmt; 268]` | every format, in discriminant order |
| `NAMES_SORTED: [(&str, PixFmt); 274]` | canonical names plus aliases, sorted, for binary search |
| `ENDIAN_SWAP: [Option<PixFmt>; 268]` | the opposite-endianness sibling, precomputed |
| `mod generated_invariants` | nine structural assertions, run over every entry |

The output is piped through `rustfmt` before being written, so the committed file
is `cargo fmt --check` clean. The large tables carry `#[rustfmt::skip]` so their
one-row-per-format layout survives — that layout is the whole point of the diff
being reviewable.

### Why the queries are free

`PixFmt::descriptor` is `&DESCRIPTORS[self as usize]` inside a `const fn`. When the
format is a compile-time constant — which it is inside a monomorphised conversion
kernel — the whole chain `fmt.descriptor().components[1].offset` folds to an
immediate. When it is dynamic, it is one load from a table of a few kilobytes that
stays resident in L2. There is no map, no allocation, and no `Option` in the
descriptor; `PixFmtDescriptor` is `Copy`, so a kernel can take one by value into
registers.

`descriptor`, `name`, `plane_count`, `plane_size`, `min_stride`,
`swap_endianness` and the flag predicates are all `const fn`. The test
`descriptor_folds_at_compile_time` uses them in `const` items, so losing that
property is a compile error rather than a silent regression.

### Component indexing

Components are indexed by **logical channel**, not by memory order:

| index | YUV | RGB |
|---|---|---|
| 0 | Y | R |
| 1 | Cb (U) | G |
| 2 | Cr (V) | B |
| 3 | A | A |

So `gbrp`'s component 0 is R and reports `plane: 2`, because the planes are stored
G, B, R. And `bgr24`'s component 0 is R at `offset: 2`.

Padding channels are not components. `0rgb` has three components and
`bits_per_pixel == 24`; the padding shows up only in `step`, and therefore in
`min_stride`.

### Units

`step` and `offset` are **bytes**, except for a `BITSTREAM` format (`monow`,
`monob`, `rgb4`, `bgr4`) where they are **bits** — a sub-byte packing has no
byte-aligned unit to measure in. `min_stride` handles the difference; callers
that read `step` directly must check the flag.

`shift` is how far right to shift after loading the container, so a component
occupies bits `[shift, shift + depth)`. `p010le` is depth 10 shift 6: ten
significant bits at the top of a 16-bit word.

### Endianness is a flag, not a type

Formats come in BE/LE pairs whose descriptors are identical but for the
`BIG_ENDIAN` flag. `swap_endianness()` is a generated table, and
`generated_invariants::endian_siblings_pair_up` asserts the pairing is an
involution and that siblings differ by nothing else. Duplicating the enum into a
type generic over endianness would double the table for no gain.

### Geometry

`min_stride(width, plane)` is derived as the largest `step x samples-in-this-plane`
over the components living in that plane, which is exactly the span the last
sample of the row reaches. That one rule is correct for planar, biplanar, packed,
sub-byte and irregular layouts alike — see the hand-verified cases in
`min_stride_matches_hand_computed_values`.

`plane_size`/`plane_height` apply vertical decimation to chroma planes only. A
plane counts as chroma when the first component in it is logical channel 1 or 2,
which is derived from the component table rather than stored: alpha and luma
planes are never decimated, and an RGB format has zero decimation so the question
never arises.

`plane_layout(width, height, align)` rounds each plane's stride up to `align`
independently and returns strides, sizes and the total, with every multiplication
and addition checked. Overflow is `Error::LimitExceeded`, never a wrapped size —
an undersized allocation from a wrapped size is the classic safe-Rust media bug.

## How to change it

### Adding a format

1. **Decide whether it belongs to an existing family.** Most do. Adding
   `yuv444p18le` is adding `18` to a `depths` list in `source.rs`. Adding a new
   subsampling to an existing planar family is adding a `Sub` variant and putting
   it in a `subs` list.

2. **If it is a new family**, add a `Family` variant and an expansion function in
   `expand.rs`. Expansion must stay mechanical: no matching on names.

3. **If it is a one-off**, add an `ExplicitDef` to the `Family::Explicit` list with
   the component tuples written out. This is the right answer for anything whose
   layout no family expresses — `uyyvyy411` and `pal8` live there.

4. **Append, do not insert.** Enum discriminants are assigned in declaration
   order. They are ours, never serialised and never a compatibility surface, so
   renumbering is harmless — but it makes the generated diff unreviewable.
   Appending keeps the diff to the lines you actually added.

5. **Regenerate and check:**

   ```
   cargo xtask gen-pixfmt
   cargo test -p vaco-pixfmt
   ```

   The generator asserts structural properties of your *declaration* (unique
   names, plane counts consistent with components, siblings that actually pair).
   `generated_invariants` then asserts properties of the *table*. Between them,
   most mistakes fail loudly at generation time.

6. **If the name has a leading digit** the derived variant is prefixed with `X`
   (`0rgb` -> `X0rgb`). If the mechanical variant name reads badly, add an entry
   to `VARIANT_OVERRIDES` — the format `gray` gets the variant `Gray8` that way.
   The variant identifier and the format name are independent; only the name is
   an interface fact.

7. **Aliases** go either in the `ExplicitDef`'s `aliases` field or in the global
   `ALIASES` table. `from_name` additionally widens an unsuffixed name to the
   host endianness (`gray16` resolves to `gray16le` on x86), so endianness
   shorthands need no alias entries.

### Gotchas

- **Never edit `src/table.rs`.** `cargo xtask gen-pixfmt --check` compares the
  committed file byte-for-byte against a fresh run, so a hand edit cannot land.
- **`bits_per_pixel` is truncated.** 4:2:0 at 9 bits genuinely averages 13.5 and
  the table reports 13. `bits_per_pixel_matches_the_components` recomputes it, so
  a wrong override fails immediately.
- **The `PLANAR` and `ALPHA` flags are derived**, from plane count and component
  count respectively. Do not set them by hand in an `ExplicitDef`; the invariants
  will disagree with you.
- **A `Field` pack lists channels most-significant first; a `Bytes` pack lists
  them in memory order.** They are different orders and mixing them up produces a
  format that is silently byte-reversed. `rgb565` is `Field`; `rgb24` is `Bytes`.
- **`assert!` in the generator is the right tool.** It runs at generation time, in
  a developer's terminal, with the declaration in front of them. The crate itself
  never panics.

### Formats to re-check

Two formats have no public specification and are modelled from the reading of
their names that makes them distinct from their neighbours. Both are flagged in
`source.rs`, and the differential harness (below) is what will settle them:

- **`nv20le`/`nv20be`** — 10-bit 4:2:2 semi-planar. Modelled right-aligned
  (`shift: 0`), which is what distinguishes it from `p210`. If it is actually
  left-aligned it is a duplicate of `p210` and should be re-derived.
- **`v30xle`/`v30xbe`** — modelled as `xv30` with the two padding bits moved to
  the low end of the 32-bit word, per the channel order the name states.

`bayer_*` is modelled as **one** component at the sample depth, which is what is
physically stored: a colour-filter-array mosaic is one sample per pixel and
demosaicing is a filter's job. The reference tool models the same formats as three
components with fractional depths summing to the sample width. The
`bits_per_pixel` agrees either way; `component_count` does not.

### Deliberately deferred

- `padded_bits_per_pixel`. Not user-visible (`-pix_fmts` prints only
  `bits_per_pixel`), and `min_stride`/`plane_layout` already answer every sizing
  question a caller has.
- The **differential extractor** against `ffprobe -show_pixel_formats` and
  `ffmpeg -pix_fmts`, which plan 11 §9.6 makes the primary acceptance criterion
  for this crate. It needs the pinned reference binary from the conformance
  harness, which does not exist yet. It validates essentially the whole crate in
  one automated pass and should be written as soon as that harness lands.
- A **fuzz target**. `fuzz/` is a separate workspace that does not exist yet. The
  two entry points that take untrusted input — `from_name` on arbitrary UTF-8 and
  `plane_layout` on arbitrary `(fmt, w, h, align)` — are already covered by
  proptest, so the fuzz target is a strengthening rather than a gap.

## Configuration

None. No features, no environment variables, no runtime configuration. The table
is a static.

The generator reads `rustfmt.toml` at the repository root so its output matches
the project's formatting.

## Dependencies

| crate | why |
|---|---|
| `vaco-core` | `Error`, for `from_name` and `plane_layout` |
| `bitflags` | `PixFmtFlags` |
| `proptest` (dev) | the invariant and geometry property tests |

No external runtime dependencies beyond `bitflags`. `phf` was considered for the
name lookup and rejected: a generated sorted array plus `binary_search_by_key` is
faster than a runtime-constructed perfect hash at this size and adds no
dependency.

Nothing depends on this crate at layer 0. `vaco-frame`, `vaco-scale`, every video
codec and every video filter depend on it.

## Provenance

Format **names** are interface facts and are matched exactly (D15: CLI-visible
names are implementable). Descriptor **metadata** — plane count, component
offsets, shifts, depths, subsampling — is dictated by what each format physically
is, and is derived here from the format's own definition and from the family
grouping in `planning/research/01-libavutil-swr-sws.md` §2. No FFmpeg source was
consulted (D7).
