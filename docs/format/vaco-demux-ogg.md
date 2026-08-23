# vaco-demux-ogg

## What it is

The Ogg demuxer: RFC 3533 page/packet framing, plus a per-codec mapping from
the page-level granule position to a packet timestamp for Opus, Vorbis,
FLAC, Theora and Speex. Registers as `ogg`, answering to the extensions
`ogg`, `oga`, `ogv`, `ogx`, `opus`, `spx` (the muxer side, `vaco-mux-ogg`,
is what actually differentiates those into separate output formats with
different default codecs).

Issue #584 (FM-30a), epic #68 (FM-30).

## How it works

### Pages and packets are not the same thing

A physical Ogg file is a sequence of **pages** (`crate::page`): a fixed
27-byte header, a segment table of up to 255 lacing values, then a body.
Each lacing value is 0..=255; a run of `255`s glues segments into one
**packet**, terminated by the first value below 255 (or, for a packet that
is an exact multiple of 255 bytes, a trailing `0`). If a page's segment
table *ends* on a `255` with nothing left to say the packet is done, the
packet **continues** onto the next page for the same serial number, which
must carry the `CONTINUED` header flag.

Measured: a real Vorbis file's second page holds 44 packets (`ffprobe
-show_packets` on `ffmpeg -c:a vorbis`-produced output), so "one page, many
packets" is not a corner case, it is the common shape.

`crate::demux::OggDemuxer::pump_one_page` reads one page, reassembles
whatever packets it completes (buffering a continuation in
`LogicalStream::pending`, budget-tracked and capped at
[`demux::MAX_PACKET_BYTES`]), and classifies each completed packet as
*header* or *data* using a per-stream running count against
[`codec::total_header_packets`].

### Granule position → timestamp, per codec

A page's granule position states where decode stands *after the last
packet that finishes on this page* (RFC 3533 §6) — it says nothing about
packets that finished earlier on the same page. `crate::granule`'s
[`GranuleTimeline::assign`] handles this by giving every completed packet a
provisional duration (exact where possible, an estimate otherwise) and then
**snapping the last packet on the page** so the running cursor lands
exactly on the page's own stated position. This bounds any estimation error
to *within one page* — cross-page drift cannot accumulate.

Per codec, **measured** against real `ffmpeg 8.1` output where an encoder
exists in this environment, from-specification-only otherwise:

| Codec | Granule meaning | Per-packet duration | Measured? |
|---|---|---|---|
| Opus | 48 kHz samples including `pre_skip` (RFC 7845 §4) | Exact, via `vaco_codec_core::Parser::packet_duration` reached through `ParserProvider` | Yes — `libopus`, pre_skip 312, first packet `pts=-312`, page granule exact |
| Vorbis | Plain sample count | Approximate constant `blocksize_1 / 2` (assumes no block switching — see below) | Yes — native `vorbis` encoder, `blocksize_1=2048`, every packet duration 1024 exactly |
| FLAC | Plain sample count | Byte-length-weighted fallback across the page | Yes — page/`STREAMINFO` framing measured; per-packet weighting is a documented approximation, see below |
| Theora | `(granule >> shift) + (granule & mask)` = frame number (spec §7.4.4) | Exact: one tick per frame | **No** — no Theora encoder in this environment |
| Speex | Plain sample count | Exact: header's own `frame_size × frames_per_packet` | **No** — no Speex encoder in this environment |

**Why Vorbis is an approximation.** The exact per-packet sample count is
`(current_blocksize + previous_blocksize) / 4`, and *which* of the two
sizes named in the identification header a given packet uses is one bit
inside the **setup header** — reached only by walking codebooks, floors,
residues and mappings that precede the mode list. That is a second Vorbis
bitstream parser, which is out of scope for a container crate (D14.1's
carve-out is for granule interpretation, not for duplicating a codec
parser). This crate assumes every packet uses the long block
(`blocksize_1`), which is exact for content that never switches — the
measured test file, and a large share of real audio — and only wrong on
the packets either side of a switch, bounded by the next page's exact
snap.

**Why FLAC's fallback is weighted by byte length, not split evenly.** An
even split puts a short trailing frame's entire shortfall on *every*
packet in its page, not just the short one — measured concretely: on a
44.1 kHz mono file whose last page holds nine full 4608-sample frames and
one 648-sample tail frame, an even split reports **4212** for all ten
(wrong for nine of them), while weighting by each packet's own compressed
byte length reports the *last* packet's cursor at **648 samples short of
correct**, versus **3564 samples short** for the even split — see
`crate::demux`'s module docs for the exact numbers. Still approximate
(FLAC's own frame header states its exact block size, in a field this
crate does not parse), but a measured, one-line improvement over the naive
alternative.

### Codec identification

`crate::codec::identify` sniffs the first (BOS) packet's fixed signature:
`OpusHead`, `\x01vorbis`, `\x80theora`, `\x7FFLAC`, `Speex   `. Each
codec's identification header has a small number of fixed-position fields
this crate reads directly (channel count, sample rate, Vorbis's block
sizes, Theora's dimensions/frame-rate/granule-shift, FLAC's `STREAMINFO`,
Speex's rate/frame-size/`frames_per_packet`) — see the module docs for the
exact byte offsets and which ones were measured against a real file versus
taken from the published specification.

### `CodecId` has no Theora or Speex variant

Confirmed by reading `crates/signal/vaco-codec-core/src/lib.rs`'s
`CodecId` enum, not assumed. Both streams still demux and timestamp
correctly; `Stream::params.codec_id` is `None` and the name survives under
`Stream::metadata["ogg_codec"]` instead — the same pattern
`vaco-demux-mpegts` uses for `ts_codec`.

### Chained and multiplexed streams

Both are the same code path: **a new serial number is a new entry in
`OggDemuxer::streams`**, appended, never replacing an earlier one — whether
its BOS page is interleaved with existing streams from the file's start
(multiplexed) or arrives only after an earlier stream's `EOS` (chained).
No special-casing exists because every packet is processed per serial
number, independent of any other stream's state. What is **not** handled:
carrying one continuous timeline across a chain boundary — the new
stream's `GranuleTimeline` starts fresh, exactly as the reference reports a
stream/format change there.

### Reaching `vaco-parse-opus` without depending on it

D14.1 forbids a `crates/format/*` crate from depending on a
`crates/codec/*` crate directly. This crate never does; it reaches Opus's
exact per-packet duration through the injected `ParserProvider` —
`parsers.parser_for(CodecId::Opus)` — during `OggDemuxer::open` only. The
frozen `Demuxer::read_packet` signature carries no provider, so a logical
stream discovered *after* `open` returns (a chained Ogg past the first
one) gets no parser and falls back to the page-anchored estimate. This is
a real, disclosed gap, not silent degradation — `LogicalStream::parser` is
`None` in exactly that case.

## How to change it

- **Page/packet mechanics** live in `src/page.rs` (pure, I/O-free: header
  parsing, checksum verification, lacing → byte ranges) and `src/demux.rs`
  (buffered reading, resynchronisation, budget accounting, the actual
  read loop). Keep that split — `page.rs`'s functions are reused verbatim
  by `vaco-mux-ogg` for the inverse operation.
- **A new codec's identification/granule mapping** goes in `src/codec.rs`
  (the fixed-header field reader) and `src/granule.rs` (the
  `GranuleMapping` variant and its `timestamp`/`initial_cursor`/
  `fixed_duration` methods). `src/demux.rs`'s `describe` function is where
  a new codec's `CodecParameters`/time-base get built.
- **Duration estimation is not implemented.** `Demuxer::duration()` uses
  the trait's `None` default. Adding it needs a tail scan in the style of
  `vaco-demux-mpegts::estimate_duration` — read backward from the file's
  end, per stream, to find each stream's last page.
- **Seeking is byte-only.** `SeekTarget::Timestamp` returns
  `Error::Unsupported`. A real implementation needs the same
  index-then-bisection strategy `vaco-demux-mpegts` uses, adapted to pages
  (the probe function would parse just the page header at a candidate
  offset — cheap, since granule is a header field, not a body one).
- **Gotcha:** `page::packet_spans`' "at most the last span is incomplete"
  invariant is depended on by `demux.rs` without re-checking it; a change
  to the lacing algorithm must preserve it (a property test asserts it —
  `tests/properties.rs::at_most_the_last_span_is_incomplete`).

## Configuration

No format-specific options; only the generic `vaco_format_core::FormatOptions`
fields any demuxer sees (this crate does not read any of them beyond what
`IoContext`/`Budget` already consume from `Limits`).

## Dependencies

- `vaco-core`, `vaco-io`, `vaco-limits`, `vaco-packet` — the standard
  format-layer stack.
- `vaco-format-core` — `Demuxer`, `DemuxerDesc`, `ParserProvider`,
  `Stream`, probing.
- `vaco-codec-core` — `CodecId`, `CodecParameters`, `Parser` (the trait
  object type only; never a concrete codec crate, per D14.1).
- `vaco-chlayout` — `ChannelLayout::default_for`, for populating
  `AudioParameters::layout` from a channel count.
- `bitflags` — `OggHeaderFlags`.

## What was measured, and how to re-measure it

Every "measured" claim above came from this build's pinned `ffmpeg 8.1`:

```sh
ffmpeg -f lavfi -i "sine=frequency=440:duration=2:sample_rate=48000" \
       -c:a libopus opus.ogg
ffmpeg -f lavfi -i "sine=frequency=440:duration=2:sample_rate=44100" -ac 2 \
       -c:a vorbis -q:a 4 -strict -2 vorbis.ogg
ffmpeg -f lavfi -i "sine=frequency=440:duration=2:sample_rate=44100" \
       -c:a flac flac.oga
ffprobe -v quiet -show_format -show_streams -show_packets -of json <file>
```

`tests/reference.rs` (`#[ignore]`d, needs `VACO_OGG_FIXTURE=<path>`) prints
this crate's own view of a file the same way, so a newer reference version
can be re-checked directly rather than trusted from this document.

**No Theora or Speex encoder exists in this environment** — confirmed with
`ffmpeg -encoders`. Their granule mappings are implemented from the public
specification only and are unmeasured; treat them as the two lightly-tested
codecs the brief for this issue explicitly allows.

## Fuzzing

`fuzz/fuzz_targets/ogg_demux.rs` drives the whole demuxer — `open` through
resynchronisation, chained-stream discovery and granule mapping — over
arbitrary bytes with `Limits::strict`, then exercises a byte-position seek.
See that file's own doc comment for the invariants it checks beyond "does
not panic".

## Known gaps (say plainly what is not done)

1. No duration estimation (`Demuxer::duration()` always `None`).
2. Seeking is byte-only; no timestamp seek.
3. Vorbis and FLAC per-packet timing inside a page are documented
   approximations (see above); only the page boundary itself is exact.
4. Theora and Speex are unmeasured against a real encoder.
5. A logical stream discovered after `open()` returns never gets an exact
   Opus parser (see "Reaching `vaco-parse-opus`" above).
