# `vaco-codec-subtitle-cc`

Layer 4. CEA-608 (line-21) and CEA-708 (DTVCC) closed-caption decode from
raw `cc_data` triplets (issues #394, #395).

## What it is

A standalone decoder for both closed-caption formats this workspace's
`CodecId::Eia608` names: CEA-608 (the analog line-21 format, four channels —
CC1-CC4 — across two fields) and CEA-708 (DTVCC, the digital-television
format assembled from its own packets). One triplet stream carries both,
distinguished triplet-by-triplet by the `cc_type` field, which is why one
crate covers both issues rather than two.

It takes raw `cc_data` bytes, not a `Frame`, and produces its own event type
rather than a `vaco_frame::Frame`. See `src/lib.rs`'s top doc comment for
the full explanation, but in short:

1. No H.264/HEVC/MPEG-2 parser in this workspace extracts caption bytes
   from a compressed stream and attaches them as
   `vaco_frame::FrameSideData::ClosedCaptions` yet, so there is no real
   compressed file that reaches this crate through the pipeline today.
   Taking raw bytes rather than reaching into a `Frame` means this gap
   cannot make the crate wrong; it will start working end-to-end the
   moment a producer exists, with no change needed here.
2. `vaco_frame::FrameData::Subtitle` (interface gap 17) landed during this
   crate's own development and names this crate as one of the three
   decoders it expects to be wired to eventually. That wiring — a
   `Decoder` impl, a `vaco-component.toml` fragment, and a decision about
   what a "packet" means for a format whose input is side data rather
   than a bitstream — is real design work not attempted here; this crate
   stays a standalone library with its own output type ([`Event`]) for
   now. `Event::Cea608`/`Event::Cea708`'s screens already produce the
   plain text `SubtitleContent::Text` wants.

## How it works

- `triplet.rs`: splits a `cc_data` byte slice into 3-byte triplets and
  classifies each by `cc_type` (line-21 field 1, field 2, DTVCC packet
  start, or DTVCC packet continuation). Invalid/padding triplets are
  skipped and counted, never treated as an error.
- `cea608.rs` + `cea608/tables.rs`: one `Cea608Decoder` per field, each
  tracking two time-multiplexed channels (a channel bit only appears on
  control codes, so which channel a plain character pair belongs to is
  state, not per-triplet). Implements pop-on (resume-caption-loading,
  end-of-caption swap), roll-up (2/3/4 lines, carriage-return scroll) and
  paint-on (resume-direct-captioning) modes, Preamble Address Codes (row,
  indent-or-color, underline), mid-row style changes, the special and two
  extended character sets, and the doubled-transmission suppression CEA-608
  requires for every control code.
- `cea708.rs` + `cea708/tables.rs`: assembles DTVCC packets from
  `cc_type` 2/3 triplets into a fixed 127-byte buffer (the format's own
  6-bit length field caps this, so the buffer is a stack array, not a
  heap allocation sized from input), demultiplexes service blocks, and
  interprets the window/pen command set — `DefineWindow`, the five
  window-visibility commands, `SetCurrentWindow`, `SetPenLocation`,
  `SetPenAttributes` (italics/underline), `SetPenColor` (foreground only)
  and `G0`/`G1` text. `SetWindowAttributes`, `EXT1` and everything behind
  it, and `P16` are parsed for correct byte-offset tracking but not
  semantically applied — see `cea708.rs`'s module doc for the exact line.
- `event.rs`: the shared output shape both formats produce — a [`Screen`]
  is a sparse, row- and column-sorted list of styled [`Cell`]s, found and
  replaced by linear scan rather than indexed into a fixed grid, which is
  what lets every access go through `.get()`/`.position()` instead of `[]`
  (`indexing_slicing` is denied workspace-wide).
- `srt.rs`: renders a `Screen` to plain text, for fixture comparison only —
  it is not a muxer and carries no timestamps of its own (this crate never
  sees one; see the timing note below).

### Timing

This crate has no timestamps to give you: it decodes whatever `cc_data`
bytes one `feed` call is handed and returns events in triplet order. A
caller feeding one video frame's side data per call gets caption events in
the same order those frames arrived, which is the only sense in which
"timing" applies until gap 1 above closes and an actual PTS is available to
attach.

## How to change it

- A new CEA-608 control code or character table entry goes in
  `cea608/tables.rs`, next to the table it belongs in; `cea608.rs`'s state
  machine should not need to change unless the code affects mode/cursor
  behaviour no existing code already covers.
- A new CEA-708 command goes in `cea708.rs`'s `dispatch_code` match and, if
  it has a non-trivial argument layout, a decode function in
  `cea708/tables.rs` alongside `define_window`/`pen_attributes`/etc.
  `tables::code_len` must stay in sync with the code space's length table
  (ANSI/CTA-708's own `C0`/`C1`/`C2`/`C3` byte-length table) or every code
  after an unrecognised one desyncs.
- Extending `Style`/`Color` (e.g. a CEA-708 window fill color) is additive:
  add the field, default it sensibly, and existing callers keep compiling.
- Gotcha: CEA-608's PAC and mid-row tables both use first-byte `0x11`
  (channel 1) with different second-byte ranges (`0x20-0x2F` mid-row vs.
  `0x40-0x7F` PAC) — a new second-byte value must be checked against both
  ranges, not just the table you're adding to, or it silently falls through
  to `Control::Unknown`.

## Configuration

None. There are no options, feature flags or environment variables —
decode behaviour is fixed by the two standards.

## Dependencies

None beyond `std`. Deliberately: every allocation this crate makes is
either a fixed-size stack array sized by a hard cap the wire format itself
imposes (a DTVCC packet's 6-bit length field, a service block's 5-bit
length field), or a `Vec` grown by pushing one element per input byte
already in hand — never a heap buffer sized from a declared/attacker-chosen
length before the bytes behind it have arrived. That is a strictly stronger
guarantee than routing through `vaco_limits::Budget`, not a shortcut around
it; see `src/lib.rs`'s "Allocation" doc section for the full reasoning.

## Verification

Every fixture in `tests/fixtures.rs` is either hand-built directly from
this crate's own reading of the CEA-608/CEA-708 code tables (self-
verification against the tables the decoder itself uses, not an
independent oracle) or extracted, byte-for-byte and unmodified, from a real
broadcast capture (`transformers_EIA608_H264.ts`, published at
`samples.ffmpeg.org/ffmpeg-bugs/trac/ticket2885/`) via PyAV's
`frame.side_data`. The real-world fixture's expected text was derived by
manually walking the same bytes through the tables in
`cea608/tables.rs` by hand, independently of the decoder producing it, and
the two agree — three consecutive, grammatically ordinary English sentences
including two bracketed sound cues (`[speaking foreign language]`,
`[shouting]`) came out of a 30-second slice of that broadcast with zero
parity errors, which is a strong signal for the CEA-608 half in particular.
The CEA-708 half has no equivalent real-world confirmation: this sample's
DTVCC service carries only empty windows (common for broadcasts of this
vintage, which populate CEA-608's "compatibility bytes" but not a
substantive CEA-708 service), so CEA-708 is verified by the hand-built
fixture and unit tests only.
