# `vaco-chlayout`

## What it is

The channel-layout model: the vocabulary of individual speaker positions, the
named standard layouts, the three-way channel-order distinction, the
mask ↔ layout conversion, and the layout description grammar that `-ch_layout`
accepts and `ffprobe` prints.

Three types:

| type | what it is |
|---|---|
| `Channel` | one speaker position, or an ambisonic component, or one of the two non-positions (`UNK`, `UNSD`) |
| `Label` | a per-channel display name, as in `FL@Left`; capped at 15 bytes by construction |
| `ChannelMap` | `Box<[(Channel, Option<Label>)]>` — the per-index map of a custom layout |
| `ChannelOrder` | `Unspecified` / `Native` / `Custom` / `Ambisonic` |
| `ChannelLayout` | a channel count plus a `ChannelOrder`, plus a mask when the order is `Native` |

It is naming and structure only. No rematrixing coefficients, no downmix policy,
no "closest layout to" search — those need psychoacoustic judgement and belong in
`vaco-resample`.

## How it works

### Why not a bitmask

A `u64` mask cannot express more than 63 positions, cannot express ambisonics,
and cannot express "8 channels, positions unknown" — and all three occur in real
files. So the model is a count plus an interpretation:

- **`Unspecified`** — the count is known, the positions are not. A raw PCM stream
  with no header is exactly this.
- **`Native`** — positions given by a bitmask, laid out in ascending bit order.
  This is the common case and the only order that can have a *name*.
- **`Custom`** — an explicit per-index map of `(Channel, Option<Label>)`.
  Permits repeats, gaps, arbitrary order, channels that are not maskable at all,
  and a display label per channel.
- **`Ambisonic`** — `(order + 1)²` ACN-ordered components, optionally followed by
  non-diegetic extras. `order` is a `u16`, which covers the reference parser's
  whole accepted range (up to 46 339, where `(order + 1)²` overflows an `int`).

### `Channel` identity is a numeric id

Every channel has an id — the same number the `USR<n>` syntax names, and, for ids
below 64, the mask bit it occupies. `Channel` implements `PartialEq`, `Ord` and
`Hash` **by id rather than by variant**, so `Channel::Unnamed(2)` and
`Channel::FrontCenter` are the same channel, and sorting a slice of channels puts
it in mask order. `Channel::from_id` is the canonical constructor and never
produces a redundant `Unnamed`.

The id space, recovered by probing (see Provenance):

| ids | meaning |
|---|---|
| `0..=17` | Microsoft's `WAVE_FORMAT_EXTENSIBLE` `dwChannelMask` positions, in that order |
| `18..=28`, `45..=60`, `63` | unassigned; print as `USR<n>` |
| `29..=44` | the reference's extension — downmix, wide, surround-direct, `LFE2`, top-side, bottom-front, side-surround, top-surround |
| `61`, `62` | binaural left/right |
| `512` | `UNSD` — a gap: the slot exists and carries nothing |
| `768` | `UNK` — present but with no defined position |
| `1024..=2047` | ambisonic components `AMBI0`..`AMBI1023` |
| everything else up to `i32::MAX` | `USR<n>` |

### Canonicalisation is not cosmetic

`ChannelLayout::custom` and the parser both funnel through one private
`from_channel_list`, which collapses a channel list into the tightest order that
can express it:

1. a leading run of `AMBI0..AMBI(k-1)` whose length is a perfect square, with
   nothing ambisonic after it → `Ambisonic`;
2. any channel carrying a label → `Custom`, because neither collapse below can
   keep one;
3. all channels `Unknown` → `Unspecified`;
4. strictly ascending and entirely maskable → `Native` — so `FL+FR` is `stereo`
   but `FR+FL` is a different layout and stays custom;
5. otherwise `Custom`.

The ambisonic collapse is checked **first** and is the one that ignores labels:
the reference discards a label written on an ACN component, so
`AMBI0@z+AMBI1+AMBI2+AMBI3` is plain `ambisonic 1`, while a label on a
non-diegetic extra survives into `ambisonic 1+2 channels (FL@x+FR)`.

This matches the reference, and it buys an invariant the whole crate rests on: a
`ChannelOrder::Custom` value is by construction one that no other order could
express, so **`describe` is a fixed point** — describing a layout and parsing the
result lands back on the same value. The fuzz target asserts exactly that.

### Representation: why the map is boxed

`ChannelLayout` is embedded **by value** in `FrameData::Audio` in `vaco-frame`,
so every audio frame pays its size — on the decode hot path — whether or not the
layout is custom. And the overwhelmingly common layout is `Native`, which is a
bare `u64` mask and needs no map at all.

That makes inline storage for the map the wrong trade, and measurably so:

| map type | `ChannelOrder` | `ChannelLayout` |
|---|---|---|
| `SmallVec<[ChannelEntry; 8]>` | 208 | 224 |
| `SmallVec<[ChannelEntry; 4]>` | 112 | 128 |
| `SmallVec<[ChannelEntry; 2]>` | 72 | 88 |
| `Vec<ChannelEntry>` | 32 | 48 |
| **`Box<[ChannelEntry]>`** | **24** | **40** |

A boxed slice allocates only when a layout genuinely is custom — the case that
was already off the hot path — and an *empty* boxed slice does not allocate at
all, so an ambisonic layout with no extras stays free. `Native` and
`Unspecified` never touch the allocator in any of these, which is what makes the
inline capacity pure cost for them.

Two further bytes came from `Label`: its length field is a `NonZeroU8`, because a
`Label` is never empty. That hands `Option<Label>` a niche, so the option is
free, and `ChannelEntry` is 24 bytes rather than 28.

`layout_stays_small` asserts all of it. The bound is not decoration — the first
version of the label support reached 256 bytes and tripped
`clippy::large_enum_variant` on `FrameData`, three crates away from the cause.
The test turns that into a local failure.

The crate has **no runtime dependencies at all** as a result: `smallvec` went
with the inline storage.

### The description grammar

`ChannelLayout::from_name` accepts six forms, tried in order:

| form | example |
|---|---|
| ambisonic | `ambisonic 2+stereo` |
| standard name | `5.1(side)` |
| default for a count | `6c` → the first standard layout with 6 channels |
| unordered count | `6C`, `6 channels` |
| native mask | `0x3f`, `63`, `077` |
| channel list | `FL+FR`, `2 channels (FL+FC)` |

`describe` inverts it. Every edge of this grammar is a `D17` note in
`src/parse.rs`, and there are a lot of them — the summary is below under
Provenance.

## How to change it

- **`src/table.rs` order is load-bearing.** `LAYOUTS` is in `-layouts` listing
  order, and `default_for` resolves `<n>c` as *the first entry with `n`
  channels*. Moving `5.1` after `6.0` would silently change what `-ch_layout 6c`
  produces. `CHANNELS` is in bit order and `channel_table_is_consistent` asserts
  it.
- **Adding a channel** means a `Channel` variant, an arm in `Channel::id`, and a
  row in `CHANNELS`. The tests catch a mismatch between the three.
- **`Label::CAP` is measured, not chosen.** 15 bytes, because the reference
  stores a label in a NUL-terminated `char[16]`. It is enforced in
  `Label::new` rather than at the parser boundary so that no `Label` the
  reference could not have produced is constructible at all.
- **Never "tidy" a `D17` note.** Each one records a reference behaviour that
  decides whether a real command line is accepted. `USR018` really is an error
  while `USR010` is `BC`; `AMBI2X` really is `AMBI2` while `USR2X` is an error;
  `4 channels` parses and `4  channels` does not, but `2  channels (FL+FC)`
  does. If one of these looks like a bug, it is — the reference's, and D17 says
  we reproduce it.
- **The golden table is a recording, not a wish list.** `GOLDEN` in
  `src/tests.rs` was produced in one scripted pass against FFmpeg 8.1 and pasted
  in unedited. To regenerate it, see the script under Provenance. Do not hand-fix
  a row to make a test pass.

## Provenance

Channel names, layout names and the bit assignment are **interface facts**
(D7/D9): a command line and an `ffprobe` field have to match byte for byte. Bits
`0..=17` are additionally dictated by Microsoft's published `dwChannelMask`
assignment for `WAVE_FORMAT_EXTENSIBLE`, which every container that carries a
mask carries — merger, not authorial choice. The 24-channel `22.2` arrangement is
SMPTE ST 2036-2 (ITU-R BS.2051 System H).

None of it was read out of FFmpeg's source. Everything below was probed against
the shipped `ffmpeg` / `ffprobe` 8.1 binaries.

### The two printed tables

```
$ ffmpeg -hide_banner -layouts
```

prints the individual channels with their descriptions, and the standard layouts
with their decompositions. Both are copied into `src/table.rs` in the order
printed, because that order is itself observable.

### The bit indices, which are printed nowhere

Recovered from the `USR<n>` parse form, which names a channel by its numeric id.
Feeding each id back and reading the name the tool prints yields the whole
assignment including the holes:

```sh
for n in $(seq 0 70); do
  printf 'USR%-4s ' "$n"
  ffprobe -v error -f s16le -ar 48000 -ch_layout "USR$n" -i /dev/zero \
          -show_entries stream=channel_layout -of csv=p=0
done
# USR0 -> FL, USR1 -> FR, ... USR17 -> TBR, USR18 -> USR18 (a hole),
# ... USR29 -> DL, ... USR44 -> TTR, ... USR61 -> BIL, USR62 -> BIR
```

The same trick reaches the three id spaces above 63: `USR512` prints `UNSD`,
`USR768` collapses the layout to unspecified (so 768 is `UNK`), and
`USR1024`..`USR2047` print as `AMBI0`..`AMBI1023`.

The masks in `LAYOUTS` were computed from the printed decompositions and then
verified in both directions — `-ch_layout 0x1f80003ffff` must print `22.2`, and a
`5.1` WAV must carry `dwChannelMask = 0x3f` at offset 40 of its
`WAVE_FORMAT_EXTENSIBLE` header:

```sh
ffmpeg -f lavfi -i "anullsrc=channel_layout=5.1" -t 0.001 -c:a pcm_s16le -f wav out.wav
xxd -p -s 40 -l 4 out.wav     # 3f000000
```

That WAV probe is also how `Native` and `Custom` were told apart: they describe
identically, but only a native layout makes the muxer emit an extensible header.
`FL+FC` writes `dwChannelMask = 0x05`; `FC+FL` writes a plain 16-byte `fmt`
chunk with no mask at all.

### The grammar, and the right place to probe it

Use `-ch_layout` on a raw demuxer:

```sh
ffprobe -v error -f s16le -ar 48000 -ch_layout "$s" -i /dev/zero \
        -show_entries stream=channels,channel_layout -of default=nw=1
```

**Not** `anullsrc=channel_layout=...` — the filtergraph's own option parsing
trims and unescapes before the layout parser sees the string, which makes
`"4 channels "` and `"5.1 "` look accepted when they are not. Two of this crate's
whitespace rules were initially wrong for exactly that reason.

For `describe` output, read `ffmpeg`'s stream banner rather than `ffprobe`'s
`channel_layout` field: `ffprobe` prints `unknown` for an unspecified layout,
where `av_channel_layout_describe` returns `N channels`. Both callers also
**truncate** — the banner at 228 bytes, `ffprobe` at 128 — which is a caller
buffer limit, not part of the description. `describe` here is unbounded, and
`the_full_mask_names_every_bit` checks our 64-channel string against the
reference's recorded 228-byte prefix.

### Regenerating the golden table

`GOLDEN` in `src/tests.rs` is 234 recorded `(input, description-or-error)` rows:

```sh
: > golden.txt
while IFS= read -r s; do
  out=$(ffmpeg -hide_banner -v info -f s16le -ar 48000 -ch_layout "$s" \
                -i /dev/zero -t 0.001 -f null - 2>&1 |
        grep -m1 -E "^  Stream #0:0: Audio:")
  if [ -z "$out" ]; then printf '%s\t<ERR>\n' "$s"
  else printf '%s\t%s\n' "$s" \
       "$(echo "$out" | sed 's/^.*Hz, //; s/, s16.*$//')"
  fi >> golden.txt
done < cases.txt
```

`cases.txt` is the list of inputs; the current one is recoverable from the first
column of `GOLDEN`.

### The reference behaviours worth knowing before touching the parser

Each of these is a `D17` note in `src/parse.rs` with the probe behind it:

- Whitespace is skipped by `strtol` in the count and mask forms — `" 63"`,
  `"+63"` and `"+6C"` parse — but a standard layout name is matched exactly, so
  `" 5.1"` and `"5.1 "` are rejected. Individual channel names *are* trimmed:
  `"FL +FR"` is `stereo`, `"F L"` is an error.
- `-1` and `-0x3f` are rejected, but `0xffffffffffffffff` is accepted as 64
  channels: the mask is read unsigned with the sign rejected separately.
- The count forms are base 10 and the mask form is base 0, so `010c` is *ten*
  channels while `010` alone is the mask `8`. `0x4c` is the mask `0x4c`, because
  the base-10 count parse stops at the `x` and falls through.
- Exactly one trailing `+` is ignored — `FL+` is one channel — while `FL++FR`,
  `FL+FR++` and `+FL` are all errors.
- `USR<n>` requires the number to consume the whole token (`USRX` and `USR018`
  are errors) but tolerates a *missing* number (`USR` is `USR0`, i.e. `FL`).
  `AMBI<n>` ignores whatever follows the number, which is why the string
  `AMBISONIC` parses — as `AMBI` + `strtol("SONIC")` = `AMBI0`.
- `<n> channels (<list>)` is a separate, laxer code path from the bare
  `<n> channels`: `2channels (FL+FC)` and `2  channels (FL+FC)` are accepted
  while `4  channels` is not. The count is checked against the list.
- `ambisonic ` with no order at all is order 0, and so is `ambisonic +stereo`.

## Known divergences and gaps

- **A label truncated mid-character is not reproduced byte for byte.** The
  reference's 15-byte cut is a `strncpy` into a `char[16]` and knows nothing
  about UTF-8: nine `é` come back as fourteen bytes of `é` plus a dangling lead
  byte. We cut at the last character boundary at or below 15 bytes instead,
  keeping seven `é`. This is deliberate and it is the only remaining divergence
  in the label handling: the reference's output there is **not valid UTF-8**, so
  it cannot survive in the `String` `describe` returns, and every lossy rendering
  of it re-parses to a third value — which would break the
  describe-is-a-fixed-point invariant that the whole grammar rests on. An
  all-ASCII label, which is every label in the recorded corpus, is byte-identical.
  Three cases are pinned in `LABEL_TRUNCATION_DIVERGENCE` so the gap is visible
  and shrinking it is a deliberate act. Closing it properly would need a
  byte-oriented `describe` counterpart, which nothing yet asks for.
- **An ambisonic layout with *unspecified* extras describes differently.**
  `ambisonic 3+3 channels` is a string both parsers accept and both then reject
  as structurally invalid (`is_valid()` is false; the reference logs "Invalid
  channel layout"). Until it is rejected, the reference describes it as
  `ambisonic 3+3 channels (NONE+NONE+NONE)` and we describe it as
  `ambisonic 3+3 channels`. The `NONE` there is a third non-channel sentinel the
  reference has and we do not — distinct from `UNSD` (id 512), it is the
  "no channel" error return of `channel_from_index`, which an unspecified layout
  always gives. We model that return as `Option::None` and materialise the
  extras as `UNK` instead, so the channel count agrees (19) and only the
  parenthetical differs. Reachable only through a layout that is invalid either
  way, which is why it is left alone.
- **`ChannelLayout`'s `channels` and `order` are public** while `mask` is
  private, so a caller can put the two out of step. `is_valid()` is the check;
  the constructors never produce an invalid value on their own.
- **No `vaco_opts::OptValue` impl**, for the same reason as `vaco-sampfmt`: the
  frozen manifest has no `vaco-opts` dependency.
- **`Channel::description` returns `None`** for `UNK`, `UNSD` and the numbered
  forms. The reference does print something for them, but not anywhere `-layouts`
  or `ffprobe` exposes, so it could not be probed and is not guessed at.

## Testing

`cargo test -p vaco-chlayout`. Thirty tests. The load-bearing ones:

- `parses_exactly_what_the_reference_parses` — all 234 golden rows, reported as
  one diff rather than one failure at a time.
- `every_description_parses_back_to_itself` — every description the reference
  emits is a fixed point of our parser.
- `the_full_mask_names_every_bit` — the 64-channel case the golden table cannot
  hold, checked against the reference's recorded prefix.
- `labels_survive_and_block_the_collapses_they_cannot_survive` and
  `labels_are_truncated_at_the_cap_by_construction` — the label semantics and the
  15-byte cap.
- `the_ambisonic_order_limit_is_where_the_reference_puts_it` — the 46 339/46 340
  boundary, through both the parser and the constructor.
- `the_label_truncation_divergence_is_exactly_what_we_documented` — pins the size
  of the one remaining gap.
- `layout_stays_small` and `the_common_layouts_do_not_allocate` — the
  representation bounds, and why they exist.
- Five proptest properties: describe round-trips, mask round-trips,
  `channel_at`/`index_of` inverse, channel-id round-trips, and arbitrary text.
  The layout generator draws labels that straddle both the 15-byte cap and a
  multi-byte character boundary, since that is where the fixed point is hardest
  to hold.

Beyond the golden table, the parser was diffed against the reference over 3 000
generated strings (two seeds x 1 500) — valid tokens, standard names, `USR`/`AMBI`
ids across every boundary of the id space, masks, count forms, labels,
`+`-joined combinations of all of those, and single-character mutations — using
the `describe` example as the driver. Result: **4 disagreements in 3 000**, all
four of them the `ambisonic <n>+<m> channels` case documented above, which is an
invalid layout in both implementations. No other class appeared.

Regenerate with the `describe` example and the reference loop under Provenance;
the string generator is half a page of Python and is worth rewriting to taste
rather than preserving.

The fuzz target is `fuzz/fuzz_targets/chlayout_parse.rs`:

```
cargo +nightly fuzz run chlayout_parse --features chlayout --fuzz-dir fuzz -- -max_total_time=90
```

It asserts the describe/parse fixed point and the index agreement over arbitrary
UTF-8. It walks only a bounded prefix of the channels, because a five-character
string (`16c`, `ambisonic 255`) can name tens of thousands of them and indexing
every one turns the target into a timeout generator rather than a bug finder.
That bound is the target's own budget, not a limit on the crate.

## Configuration

None. No features, no environment variables, no runtime configuration. Both
tables are statics.

## Dependencies

| crate | why |
|---|---|
| `proptest` (dev) | the round-trip properties |

**No runtime dependencies.** Two edges were deliberately removed rather than left
in place:

- `vaco-core` — nothing here returns the workspace `Error` type, since
  `from_name` returns `Option` per the frozen signature, and an unused
  dependency misrepresents the layering.
- `smallvec` — went with the inline map storage, see "Representation" above.

`vaco-frame` depends on this crate; `vaco-resample`, every audio codec, every
audio filter and `vaco-probe` will.
