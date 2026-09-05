# `vaco-codec-av1`

Layer 4. Intra-only AV1 video decode (AV1 Bitstream & Decoding Process
Specification v1.0.0 with Errata 1) — a real AV1 key frame decoded end to
end: OBU/sequence/frame header, the symbol decoder and its adaptive CDF
machinery, the tile/superblock/partition/mode-info walk, coefficient
decode, dequantization, inverse transforms, and intra prediction (basic/
Paeth, DC, smooth, directional with the intra edge filter and upsampling,
and CFL), followed by CDEF and scalar super-resolution. Inter prediction,
deblocking/loop restoration, film grain, threading/DPB management and Argon
conformance are explicitly
out of scope — later work, other crates.

## What it is

Builds on `vaco-parse-av1` (OBU framing, sequence header, `av1C`, the
partial frame header stop point) per the D14 layering split: that crate
owns everything the *format* layer needs (container-level identification),
this crate owns everything the *decode* process needs beyond that. It exports
`Av1Decoder` and the `AV1_DECODER` descriptor for codec id `av1`. Its registry
fragment remains disabled while the reconstruction gaps below are unresolved;
direct decoder callers and tests use the shared symbol engine.

| Module | Contents |
|---|---|
| `symbol.rs` | Compatibility re-export of `vaco-codec-msac::av1::SymbolDecoder`; the shared crate owns §8.2's range arithmetic and CDF adaptation |
| `cdf.rs` | `TileCdf` — one fresh copy of every default CDF array (§9.4) this crate's syntax-element set reads, built once per tile; `qctx()`'s base_q_idx bucketing for the four coefficient-table families |
| `tables.rs` + `tables/{default_cdf,scan,conversion,quant}.rs` | mechanically-extracted spec tables (default CDFs, scan orders, size/context conversion tables, quantizer lookups) |
| `frame_header.rs` | `FrameHeader::parse`/`parse_from_reader` — intra frame syntax, including retained CDEF strengths/damping and restoration modes; deblocking and film-grain parameters remain syntax-only |
| `cdef.rs` | Direction search, constrained filtering, variance adaptation, damping, and chroma direction mapping; see [CDEF](../av1-cdef.md) for oracle coverage and remaining frame-conformance gaps |
| `superres.rs` | AV1 §7.16's post-CDEF horizontal eight-tap upscaler; see [AV1 super-resolution](../av1-superres.md) for oracle coverage and the inter-reference boundary |
| `transform.rs` | `Av1TxType`, `inverse_transform_2d` — the full §7.13 inverse DCT/ADST/WHT/identity transform network |
| `predict.rs` | `predict_intra`/`predict_chroma_from_luma` — §7.11.2/§7.11.5 |
| `framebuf.rs` | `Picture`/`Plane` — the private `u16`-backed reconstruction buffer intra prediction needs (reads already-written pixels of the buffer being written) |
| `decode.rs` | the tile/superblock/partition/mode-info/residual walk (§5.11, §6.10, §7.4-§7.12) and the `Decoder` wiring |

## How it works

`Av1Decoder::send_packet` walks a temporal unit's OBUs
(`vaco_parse_av1::obu::units`). A `SEQUENCE_HEADER` OBU replaces the held
`SequenceHeader`; a `FRAME_HEADER` OBU parses a `FrameHeader` and holds it
pending; a `TILE_GROUP` OBU decodes against that pending header. A combined
`OBU_FRAME` (frame header and tile group in one payload — common
real-encoder output for single-tile frames) is handled by
`FrameHeader::parse_from_reader`, which lets the frame header be read from
a `BitReader` shared with the tile-group bytes that immediately follow it
in the same OBU, rather than requiring the frame header its own trimmed
payload slice up front (its own bit length is exactly what parsing it
determines).

`decode_frame` builds a `FrameCtx` (grid of `MiCell`, the `Picture`
reconstruction buffer, per-frame flags) and calls `decode_tiles` →
`decode_one_tile` → `decode_partition` (the recursive partition tree,
§5.11.4) → `decode_block` (§5.11.5-9's reduced intra-only mode info) →
`residual`/`transform_block` → `coeffs` (§5.11.39) + `reconstruct`
(§7.12.3's dequantize/inverse-transform/add).

After tile reconstruction, CDEF runs first and `superres::upscale_picture`
runs second when the frame header sets `use_superres`; only then are the
visible planes copied to `Frame`. The superres filter derives phase from the
coded visible width but clamps its eight taps to the Mi-padded reconstruction
plane, which is why it must precede that final copy. See
[AV1 super-resolution](../av1-superres.md) for the exact oracle and table
provenance.

Since `is_inter` is always `0` here, two specification branches collapse
away entirely and are not implemented at all: `read_block_tx_size()`'s
variable-transform-tree path and `residual()`'s `transform_tree()` call —
every intra block takes the plain `read_tx_size()`/`transform_block()`-loop
path instead.

### Real bugs found decoding actual `libsvtav1` output, not just unit tests

Every module above had passing unit tests before any of these were caught
— each is a case where the *unit* was internally self-consistent but wrong
against the specification's own text, only visible once the whole pipeline
ran against a real encoder's bytes and was compared to `ffmpeg -c:v
libdav1d`'s reference decode (`tests/oracle.rs`):

- **`get_coeff_base_ctx`'s `is_eob` branch returned the wrong range.** It
  returned `SIG_COEF_CONTEXTS-4..SIG_COEF_CONTEXTS-1` (38..41) — a literal
  read of one clause of §8.3.2 — but `TileCoeffBaseEobCdf`'s own context
  dimension is sized `SIG_COEF_CONTEXTS_EOB` (4), not `SIG_COEF_CONTEXTS`
  (42); the specification's own formula is that raw range minus
  `SIG_COEF_CONTEXTS` plus `SIG_COEF_CONTEXTS_EOB`, re-based to 0..3. Every
  block with any nonzero coefficient at all indexed the 4-entry array out
  of bounds and silently fell back to a default-constructed (wrong) cdf for
  its first, most consequential coefficient-level symbol — every symbol
  read afterward in that transform block, and often the tile, came out as
  plausible-looking but wrong values. This was the single largest
  contributor: fixing it alone took a flat, real-DC-residual fixture from
  total garbage to byte-exact.
- **`read_cfl_alphas` used the same context formula for both alphas.**
  `cfl_alpha_u`'s context is `(signU-1)*3+signV`; `cfl_alpha_v`'s is the
  *different* `(signV-1)*3+signU`. The original code used `signs.min(5)`
  for both.
- **`get_tx_class`'s `HORIZ`/`VERT` ordinals were swapped** relative to the
  specification's own `TX_CLASS_HORIZ=1`/`TX_CLASS_VERT=2`. Every caller
  (`Mag_Ref_Offset_With_Tx_Class` row selection in `get_coeff_br_ctx`, the
  `+7`/`+14` branch in the same function) was written assuming the
  specification's numbering, so the swap combined into wrong neighbour
  offsets and wrong contexts for the six pure row/column transform types
  (`V_DCT`/`H_DCT`/`V_ADST`/`H_ADST`/`V_FLIPADST`/`H_FLIPADST`).
- **`get_coeff_base_ctx` never branched on transform class at all** — it
  always used the 2D neighbour offsets and `Coeff_Base_Ctx_Offset`, missing
  `Sig_Ref_Diff_Offset[HORIZ|VERT]` and `Coeff_Base_Pos_Ctx_Offset` for the
  same six transform types.
- **`palette_mode_info()` and the per-64x64 `read_cdef()` literal were
  never read at all**, on the documented (and wrong) assumption that this
  crate's own fixtures never set `allow_screen_content_tools` or
  `enable_cdef`. `libsvtav1` sets `allow_screen_content_tools` on by
  default for some content; skipping `has_palette_y`/`has_palette_uv`'s
  bits desyncs every symbol read afterward. Both are read now — a block
  that actually sets `has_palette_y`/`has_palette_uv` returns
  `Error::Unsupported` (palette prediction itself stays out of scope);
  `read_cdef()` assigns a strength-table index on the first non-skip block,
  including the enabled single-entry table with `cdef_bits = 0`.
- **`SymbolDecoder::exit_symbol` called `BitReader::get()` with the tile's
  entire remaining bit count** — `get()` panics past 32 bits, so any tile
  whose last symbol read finished with more than 32 unread bits left in it
  crashed the decoder outright. Fixed to `BitReader::skip_long`, which has
  no such limit.

### Known gap

A busier real fixture (mixed partition sizes, `SMOOTH`/directional intra
modes, ADST/flip-ADST transforms) still shows real, structured pixel error
against `ffmpeg`'s reference decode — not the diffuse, small deviation this
project's own shipping bar treats as acceptable. `predict_smooth`'s formula
and `Sm_Weights_Tx_*` tables were checked line-for-line against the
specification and matched; the defect was not isolated further within this
batch's own budget. Named in `tests/oracle.rs` as an `#[ignore]`d test
rather than silently dropped — see that file's own module doc.

## How to change it

- **New syntax element or context formula**: find the syntax in the AV1
  spec's Section 5 (structure) and its context/cdf in Section 8.3.2
  (parsing process), transcribe both, and add a unit test that would fail
  if either were transposed with a neighbouring one — the bugs above all
  came from exactly that kind of transposition, and all were unit-testable
  once known, just not before.
- **Adding a table**: prefer `scripts/extract_cdf.py` (mechanical
  brace-to-Rust-array transliteration) over hand transcription for
  anything past a handful of entries; verify with a structural test
  (permutation, monotonicity, shape) rather than eyeballing the numbers.
- **Extending scope** (palette prediction, `use_filter_intra`'s recursive
  filter, inter prediction): the corresponding bit-consuming reads already
  exist and correctly return `Error::Unsupported` by name when a real
  stream actually exercises them — implement the missing math behind that
  same call site rather than adding a new read path.
- **Extending superres to inter frames**: implement the decoder's reference
  store and `frame_size_with_refs()` together. AV1 §5.9.7/§6.8.6 derives the
  referenced `UpscaledWidth` before `superres_params()`; today inter frames
  are rejected before that syntax, so this remains explicitly unreachable
  rather than partially guessed.
- Do not add a comparison test without an `#[ignore]`/named-gap doc comment
  unless it actually passes — `tests/oracle.rs` is the place regressions
  and gaps both get recorded, not just the passing cases.

## Configuration

No env vars or flags. `Av1Decoder::new(limits: vaco_limits::Limits)` bounds
all allocation (picture planes, per-tile context arrays) through
`vaco_limits::Budget`.

## Dependencies

| Crate | For |
|---|---|
| `vaco-parse-av1` | OBU framing, sequence header, `av1C`, partial frame header (per D14, this crate must not duplicate any of it) |
| `vaco-codec-core` | the `Decoder` trait, `DecoderDesc` registration, the `Machine<Frame>` send/receive state machine |
| `vaco-codec-msac` | shared AV1 symbol decoder and CDF adaptation |
| `vaco-frame` / `vaco-pixfmt` | output frame and pixel-format types |
| `vaco-packet` | input compressed packets |
| `vaco-limits` | allocation budgets for header-derived sizes |
| `vaco-core` / `vaco-bitstream` | shared error taxonomy and bit access primitives |

Dev-only: `proptest`. No external runtime dependencies.

## Verification

`tests/oracle.rs`: two real `libsvtav1`-encoded fixtures (uniform 128 —
`skip=1`, pure prediction; uniform 100 — forces a real large DC-only
residual through the whole symbol/cdf/dequant/inverse-transform chain)
decode byte-exact against `ffmpeg -c:v libdav1d`. A third, busier fixture
is present but `#[ignore]`d (see "Known gap" above). `fuzz/fuzz_targets/
av1_decode.rs` runs arbitrary bytes through `send_packet`/`receive_frame`
twice (the second call exercising the sequence header persisted across
temporal units) — run with:

```sh
cargo +nightly fuzz run av1_decode --no-default-features --features codec-av1 -- -max_total_time=30
```

## Specification

`aom-av1-spec` (AV1 Bitstream & Decoding Process Specification v1.0.0 with
Errata 1), recorded in `provenance/sources.toml`. Large mechanically-
extracted tables recorded in `provenance/vaco-codec-av1.toml`.
