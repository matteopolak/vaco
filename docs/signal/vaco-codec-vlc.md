# `vaco-codec-vlc`

Layer 3. Variable-length ("Huffman"/VLC) code tables and readers (D-01),
built as a prerequisite of AAC-LC decode (#152, T3-03).

## What it is

`VlcEntry` (a `code`/`len`/`symbol` triple, transcribed straight out of a
specification's own codeword/length columns) and `VlcTable` (a borrowed slice
of entries plus a linear-scan `decode` against a `BitReader`), together with
two transcription-error checks, `is_prefix_free` and `kraft_numerator`.
`VlcTable::build_lookup` also provides a bounded-width direct lookup for hot
tables without requiring a `2^max_len` allocation.

### Why this exists

`vaco-codec-mpegaudio` and `vaco-codec-ac3` each grew their own version of
this: a `static` array of codeword/length/value rows, a peek-and-scan reader,
and a hand-rolled prefix-free unit test. AAC's spectral data needs the same
mechanism a third time — 12 real Huffman codebooks (ISO/IEC 14496-3 subpart 4
Table 4.69/4.70) plus a scalefactor codebook — and a fourth copy is exactly
the kind of repetition a shared crate should absorb.

**One claim in the brief that dispatched this crate turned out to be wrong**,
worth recording here rather than silently dropping: the dispatch said AC-3's
mantissa decoding was a second instance of Huffman decoding. It is not —
`vaco-codec-ac3/src/mantissa.rs`'s `decompose_group` is fixed-width
grouped-radix decomposition (`group_code = digit[0]*levels^(count-1) + ...`,
read as one fixed-width field and unpacked by repeated `% levels` / `/=
levels`), not a variable-length prefix code at all. There is no codeword
table there, prefix-free or otherwise. MP3's `huffman.rs` is the only real
precedent this crate generalises; AAC's tables are the reason it exists.

## How it works

### Decode is a bounded linear scan, matching MP3's own precedent

`VlcTable::decode` peeks `max_len` bits (the table's longest codeword, computed
once in `VlcTable::new`), and for each entry right-shifts its own `code` up to
that width and compares. First match consumes that entry's own `len` bits (not
`max_len`) and returns its `symbol`; no match consumes nothing and returns
`None`. Same trade-off MP3's `huffman.rs` documents for its own tables:
"correctness-first; a real decode tree is future work." A future faster
implementation can replace the scan without moving any table, because a table
is just a `&[VlcEntry]`.

`VlcTable::new` is **not** `const fn`: computing "the longest length in a
slice" needs indexing, which `clippy::indexing_slicing` denies even inside a
`const fn`, so the max is computed with `Iterator::max` instead, at ordinary
runtime. Constructing a table (one pass over a handful to a few hundred
entries) is cheap enough to do at each decode call site.

### Width-bounded lookup

`VlcTable::build_lut` is useful when `max_len` is small, but its storage grows
as `2^max_len`: a 16-bit table is 65,536 `(symbol, len)` slots (512 KiB with
the current representation), and a 32-bit table is not a reasonable eager
allocation. `build_lookup(width)` limits storage to `2^width` slots. Entries
whose codeword fits in the selected prefix are direct hits; a slot that could
contain a longer codeword is marked for the original bounded scan. This keeps
the exact first-match behaviour even for malformed tables while making the
width/cache tradeoff explicit at the call site:

```rust
let table = VlcTable::new(&TABLE);
let lookup = table.build_lookup(12).expect("lookup width fits this table");
let symbol = lookup.decode(&mut reader);
```

Choose `width` from a same-session benchmark of the real table and workload.
Increasing it reduces fallback scans but grows the lookup's resident memory;
width zero is a valid correctness-only lookup that delegates all reads to the
linear scan.

### The two checks a transcribed table should pass

- `is_prefix_free` — structural: no codeword may be a prefix of another,
  checked by shifting the longer of any two codewords down to the shorter's
  length and comparing. Never needs to know what any codeword "should" be,
  which is what makes it useful against a transcription error rather than a
  restatement of one.
- `kraft_numerator(entries, scale_len)` — returns `Σ 2^(scale_len - len)` as an
  exact `u64`. A code is *complete* at `scale_len` exactly when this equals
  `1u64 << scale_len`; comparing exact integers rather than a float sum with an
  epsilon is deliberate, so a one-bit transcription slip cannot hide inside a
  rounding tolerance.

Both mirror the two-part check MP3's own Huffman tables were held to before
this crate existed (`huffman.rs`'s
`every_table_is_a_complete_prefix_free_code`).

## How to change it

- A new codec's Huffman table: transcribe the specification's own
  codeword/length/value columns directly into a `static [VlcEntry; N]`, wrap it
  in `VlcTable::new(&TABLE)` at the decode call site, and add both
  `is_prefix_free` and (if the specification states the code is complete)
  `kraft_numerator` checks as unit tests — this is the pattern to copy, not
  something to reinvent per codec.
- If a decode hot loop's profiling ever points at `VlcTable::decode`'s linear
  scan, benchmark `build_lookup(width)` against the existing full `build_lut`
  path on the real table. Keep the smallest width that meets the measured
  throughput target; every table stays exactly as declared and longer entries
  retain the scan fallback.
- This crate does not include a canonical-code *builder* (turning a bare list
  of lengths into codewords). Every codec that has driven this crate's design
  so far (MP3, AC-3's exponents, AAC's Huffman tables) states codewords
  directly in its specification text, so there has been nothing to build one
  against yet. Add one if a future codec's specification gives lengths only.

## Configuration

None. No features, no environment variables.

## Dependencies

`vaco-bitstream` (`BitReader`, and `BitWriter` in the crate's own tests only).
No external runtime dependencies.

## Verification

Unit tests in `src/lib.rs`: a toy complete code cross-checked by both
`is_prefix_free` and `kraft_numerator`, a deliberately-broken prefix collision
caught, an incomplete code's smaller Kraft sum, full decode of every symbol in
the toy code, a failed decode consuming nothing, an empty table never
panicking, a round trip through a real `BitWriter`, and exhaustive agreement
between width-bounded lookups and the scan (including malformed ordering). No
fixtures needed —
this crate has no format-specific behaviour to compare against a reference
decoder; its correctness properties (prefix-freedom, Kraft completeness,
consume-nothing-on-failure) are checked directly.

The width choice is a measured tradeoff, not a fixed constant. In ten
interleaved A/B rounds on this Apple M5 (20 samples per round, synthetic
30-entry 2--16-bit table, 60,000 codewords per sample), median decode time was
345.0 us for the linear scan, 290.45 us with an 8-bit bounded lookup, 224.45
us with 12 bits, and 123.7 us with the full 16-bit lookup. The 12-bit table
uses 4,096 slots (32 KiB for the current representation) versus 65,536 slots
(512 KiB) for the full table, so it is the measured cache-sized choice for
this workload; callers must repeat the comparison for their own table shape.
