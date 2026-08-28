# `vaco-codec-subtitle-teletext`

Layer 4. EBU/ETSI Teletext decode (EN 300 706): Hamming 8/4 and 24/18
forward error correction, odd-parity Latin text, page header parsing and
Level 1 page assembly into a 40-column by 25-row character grid.

## What it is

A standalone library, **not** a `vaco_codec_core::Decoder`. `CodecId::
DvbTeletext` and `MediaType::Subtitle` already exist, but `vaco_frame::
FrameData` has exactly two variants (`Video`, `Audio`) — there is no shape in
this workspace's decoder output today for a character-grid page. Rather than
lie about that in a `vaco-component.toml` fragment (or block on a change to
a crate this one does not own), [`TeletextDecoder`] is a plain type with its
own output type, [`Page`], callable directly. See the crate's top-level doc
comment for the exact gap.

## How it works

[`TeletextDecoder::push`] takes bytes shaped like `vaco-subtitle-bitmap`'s
`dvbtxt` demuxer output (or the equivalent PES elementary-stream bytes): a
run of EN 300 472 46-byte data units, not necessarily aligned to the call's
boundaries — a fixed-size carry buffer stitches a data unit split across two
calls back together, since the raw `dvbtxt` demuxer's 1024-byte chunking
does not divide evenly by 46.

Each data unit's 42-byte EN 300 706 packet starts with two Hamming 8/4 bytes
(`hamming::decode8`) giving a magazine number (1-8) and packet number
(0-31). Packet 0 is a page header (`page::Page::from_header`): page number,
13-bit subcode and all eleven `C4`-`C14` control bits, each Hamming-decoded
from the header's own bytes. Packets 1-24 are body rows, decoded byte-by-byte
as odd-parity Latin text (`parity::decode`) through a Table 26 spacing-
attribute state machine (colours, flash, conceal, box, double height/width/
size, hold mosaics) landing in a 40-column `Row`. Packets 25-28 are Hamming
24/18-decoded (`hamming::decode24`) only far enough to detect corruption —
their addressing semantics are not applied (see Level 1.5 below). A
magazine can only ever be assembling one page at a time (EN 300 706 §7.2.1),
so the decoder holds exactly eight `Option<Box<Page>>` slots and emits a
`PageEvent` when a later header (or `finish()`) supersedes one.

## How to change it

`hamming.rs` and `parity.rs` are pure functions derived from the
specification's own encoding equations — verify a change against those
equations, not against a transcribed decision table (see `hamming.rs`'s
module docs for why). `charset.rs` holds the Latin G0 table and the English
national-option substitution; a second language's Table 36 sub-set would add
a lookup keyed on `ControlBits::national_option` there, replacing the
"English always" behaviour `latin_g0` documents today. `page.rs`'s
`apply_control` is the Table 26 spacing-attribute state machine — a new
attribute (or a fix to the Set-At/Set-After distinction the crate docs admit
is not modelled) belongs there. `decoder.rs` owns the eight-magazine state
machine and the enhancement-packet skip logic.

## Configuration

No `Limits`/`Budget` threading: every allocation this crate makes is
compile-time bounded (`[[Cell; 40]; 25]` per page, `[Option<Box<Page>>; 8]`
in the decoder, a fixed 46-byte carry buffer), since a Teletext page's shape
— eight magazines, 25 rows, 40 columns — is fixed by the specification, not
by anything an attacker's input states a length for. The one collection
sized from input, `TeletextDecoder::push`'s returned `Vec<PageEvent>`, is
bounded by the number of magazines (at most 8) regardless of input size.

## Dependencies

None beyond the standard library. Every allocation this crate makes is
compile-time bounded (see Configuration above), so it needed neither
`vaco-core`'s `Error`/`Result` nor `vaco-limits`'s `Budget` — both were
dropped from `Cargo.toml` rather than kept unused for consistency with a
decoder-trait-shaped crate this one deliberately is not.

## Level 1.5 coverage

Implemented: Hamming 8/4 and 24/18 decode, odd-parity Latin text, full page
header parsing, Level 1 page-grid assembly (spacing attributes, mosaics
preserved in their raw EN 300 706 §12.1 bit layout rather than rendered),
and the English Table 36 national-option substitution.

Not implemented: the G0/G2 character-set re-designation packets (X/28,
M/29), the `ESC` second-G0-set toggle, G2 supplementary-character access,
X/26 composite-character overwriting, and every non-English Table 36
national-option sub-set (German, French, Italian, Portuguese/Spanish,
Czech/Slovak, Polish, Turkish, Serbian/Croatian/Slovenian, Rumanian,
Estonian, Lettish/Lithuanian, Swedish/Finnish/Hungarian all render with
English's glyphs at the thirteen reserved code points). The `C4` erase-page
control bit is also not honoured precisely — every `X/0` starts a fresh
blank page regardless of `C4`, a superset of correct behaviour when `C4=1`
(the common case).

## Testing

Unit tests cover both Hamming decoders (round-trip, every single-bit
correction, double-bit detection, derived by encoding known nibbles/
triplets and decoding them back — not by transcribing the spec's decision
tables), odd-parity accept/reject, the Latin G0 table's ASCII and
English-substitution positions, page-header field extraction, the Table 26
spacing-attribute state machine, and the eight-magazine decoder state
machine including cross-`push` data-unit carry-over and enhancement-packet
skip.

`tests/self_verification.rs` hand-builds three EN 300 706-conformant
packets directly from the Hamming/parity encoding equations and decodes
them back through the real `TeletextDecoder`: a basic header-plus-body-row
page, the English national-option pound-sign substitution inside a coloured
row, and a single-bit Hamming error injected into a header byte to confirm
it is corrected rather than corrupting the page number. This is
self-verification against the specification's own tables — **not** a diff
against a reference decoder's output. No small public-domain `.ts` sample
containing a real DVB teletext elementary stream was reachable from this
environment (`ffmpeg` has a teletext decoder but no teletext encoder, so
there is no ordinary path to synthesise one either), and the fixture
repositories checked (`xavery/ttxinfo`, `orryverducci/TtxFromTS`,
`CCExtractor`'s TV-samples page) either bundle no sample data or gate it
behind a Google Drive folder outside this session's reach.

`teletext_hamming_decode` and `teletext_packet_parse` fuzz both Hamming
decoders and the full data-unit-to-page pipeline respectively; 30 seconds of
each found no crash and left `fuzz/artifacts` empty.
