# `vaco-codec-vc1`

Layer 4. VC-1 / WMV3 video decode — Simple/Main profile, progressive I-frame
only — from the freely-published SMPTE ST 421:2013 standard.

## What it is

A partial, genuinely-verified decoder for VC-1's Simple and Main profiles,
covering exactly the progressive I-picture path: sequence/entry-point
metadata, the I-picture header, the intra macroblock/block layer (`CBPCY`,
`ACPRED`, DC/AC entropy decode with all three AC escape modes), dequant, and
the exact Annex A 8x8 integer inverse transform.

Built from `Vaco-Spec-Ref: smpte-st421-2013` (SMPTE ST 421:2013, fetched
freely from <https://pub.smpte.org/pub/st421/st0421-2013.pdf> — a published
Standard is Tier A per D7/plan 07 SS1.6.1) plus `Vaco-Spec-Ref: smpte-rp227`
(SMPTE RP 227, "VC-1 Bitstream Transport Encodings",
<https://multimedia.cx/mirror/rp227.pdf>) for the pointer into ST 421's own
Annex J/L that RP 227 supplied.

## Legal status: encumbered, gate pending an owner ruling

VC-1 was **entirely absent** from `planning/research/07-legal-patents-licensing.md`
before this crate — a real gap the previous agent on issue #42 found and
this pass closed by adding a row, not a ruling. VC-1 is patent-encumbered
(developed by Microsoft, later placed under an MPEG-LA-administered
contributor patent pool covering encode and decode). Registered
`encumbered = true` / `default = false` per D4/D4.1, mirroring
`vaco-codec-h264` and `vaco-codec-aac` exactly. This is a provisional
GREEN-pending-review gate: it means "not shipped by default until someone
with legal authority decides," not "cleared."

## How it works

- `src/header.rs` — `parse_extradata()` decodes `STRUCT_C` (Annex J.2,
  Table 263) plus this crate's own width/height convention (see that
  module's doc for why: real ASF/AVI containers hand a decoder only the
  bare 4-byte `STRUCT_C`, and today's `vaco_codec_core::Decoder` interface
  has no channel to forward a container's declared width/height to a built
  decoder at all — a real, separate interface gap this crate documents
  rather than works around). `parse_i_picture_header()` decodes Table 16
  (SS7.1.1) for Simple/Main profile.
- `src/tables.rs` + `src/tables_ac_data.rs` — every VLC table this crate
  uses, transcribed directly from the SMPTE PDF's own printed tables. The
  large AC coefficient tables (SS11.8.6/11.8.7, ~340 entries across code
  tables, run/level tables, and four escape-delta tables) were extracted
  **programmatically** from the PDF's own table structure — not retyped by
  hand — specifically to remove the transcription-error class this
  project's own H.264 CAVLC and MPEG-2 `CODED_BLOCK_PATTERN` history
  warns about. It worked: the first (hand-typed) version of the small
  64-entry `CBPCY_I` table had exactly this error — one entry duplicated
  from a neighbouring row instead of its own value — caught immediately by
  this crate's own `is_prefix_free` unit test, not by the differential
  fixture. Every table's own unit test also asserts tier-1
  (prefix-free/coverage) properties directly, per `AGENT-CONSTRAINTS.md`'s
  guidance that a crate whose tables are already `vaco_codec_vlc::VlcEntry`
  arrays should run that check in its own suite rather than via the
  external `vlc-scan` sweep.
- `src/transform.rs` — Annex A's 8x8 inverse transform: the exact `T8`
  matrix and the two-pass `E = (D·T8+4)>>3`, `R = (T8'·E + C8·1_8 + 64)>>7`
  formula, read directly off the specification's own page image (the
  running PDF-to-text extraction badly mangled the matrix's minus signs and
  had to be double-checked against the rendered page).
- `src/decoder.rs` — the macroblock/block decode loop: `CBPCY` neighbour
  prediction (SS8.1.2.1's `predicted_Y0..Y3` formula), DC differential
  decode + prediction (SS8.1.3.1/8.1.3.2), AC run/level/escape decode
  (SS8.1.3.4/8.1.3.5, all three escape modes including fixed-length Mode 3
  with its per-picture-persistent `ESCLVLSZ`/`ESCRUNSZ`), AC prediction
  (SS8.1.3.7), dequantization (SS8.1.3.8), and reconstruction — including
  SS8.1.3.10's easy-to-miss rule that Simple/Main I frames skip the final
  `+128` DC offset entirely when overlap filtering is off.

## What is cut

- **Only the High Rate Intra/Inter AC coding sets are transcribed**
  (SS11.8.6/11.8.7 — the pair a `PQINDEX <= 8` picture selects at coding-set
  index 0). The other six of the eight nominal coding sets (SS11.8.1-11.8.5)
  are large table sets this pass did not have the budget to transcribe and
  verify to this project's own standard. A picture selecting an
  untranscribed set returns `Error::Unsupported` by name.
- **`OVERLAP == 1` and `LOOPFILTER == 1` are both refused.** SS8.1.3.10
  couples the overlap-smoothing filter to the reconstruction formula itself
  (whether `+128` is added before clamping) — implementing one without the
  other would silently offset every pixel by a constant. The real fixture
  this crate verifies against has both off, so its `OVERLAP == 0` /
  `LOOPFILTER == 0` path needed no filter at all to be correct.
- **`MULTIRES`/non-zero `RESPIC`** (down-sampled I-frame decode + Annex B
  up-sampling) is refused.
- **P, B, BI pictures, interlace coding, and Advanced profile** are all
  refused outright — this crate is I-frame-only, Simple/Main-only,
  progressive-only.

Every one of the above is a real `Error::Unsupported` return; this crate
never fabricates pixels for a shape it has not implemented.

## Verification

`tests/oracle.rs` decodes a real Main-profile fixture
(`fate-suite.ffmpeg.org/vc1/SMM0015.rcv`, a public FFmpeg FATE sample,
720x576) and compares against `ffmpeg 8.1`'s own decode of the same file,
**Y/U/V measured separately**. Result: **byte-exact on all three planes**
(max absolute difference 0, mean absolute difference 0.000). The fixture's
own sequence header was decoded bit-by-bit by hand against Annex J.2/L
*before* any decoder code was written (`header.rs`'s
`real_fixture_struct_c_decodes_as_measured` test pins that derivation), and
the RCV container's own frame-layer offsets were independently verified to
land exactly on the file's end after walking all 25 frames.

15 further unit/property tests across `tables.rs` (prefix-free +
coverage for every VLC table), `transform.rs` (DC-only-block-is-uniform —
an oracle-diversity check per this project's "an oracle you wrote shares
your misreading" lesson — plus an output-range sweep), `header.rs`, and
`decoder.rs` (garbage input is a clean error, not a panic).

## How to change it

- To add another AC coding set: extract its VLC/run-level/delta tables the
  same programmatic way `tables_ac_data.rs` was generated (do not hand-type
  a table this size), wire it into `decoder::decode_frame`'s
  `(ph.pqindex <= 8, ph.transacfrm2, ph.transacfrm)` match, and verify
  against a real fixture that actually selects it.
- To add P/B pictures: `header::parse_i_picture_header` and
  `decoder::decode_frame` are both I-picture-shaped by construction: motion
  vectors, `VOPDQUANT` (per-macroblock quantizer variation), and
  inter-block transform-size switching (`VSTRANSFORM`) do not exist
  anywhere in this crate yet.
- To add Advanced profile: this crate's `header::parse_extradata` refuses
  `PROFILE == 12` outright. Advanced profile's sequence/entry-point layer
  is a real in-band, start-code-framed bitstream (Annex E) rather than an
  out-of-band `STRUCT_C`, and its own `Sequence layer syntax and semantics`
  table (Table 3) is materially different from Table 16 — this is a
  separate parser, not a small extension.

## Configuration

None. No features, no environment variables. Behaviour is entirely
determined by the extradata and packet bytes handed to the `Decoder`.

## Dependencies

`vaco-codec-vlc` (`VlcTable`/`VlcEntry`) for every table decode.
`vaco-bitstream::BitReader` for the MSB-first picture/macroblock/block
bitstream. No dependency on `vaco-codec-mpegvideo`: that crate's shared
inter-prediction/B-picture/MPEG-zigzag machinery has no caller in this
crate's I-frame-only scope, and VC-1's own zigzag tables, `CBPCY`
neighbour-prediction rule, and Annex A transform are not shaped like that
crate's MPEG-derived equivalents — see `src/lib.rs`'s own doc for the
fuller reasoning.
