# vaco-mux-ogg

## What it is

The Ogg muxer, plus five registrations differing only in declared default
codecs and extensions: `ogg`, `oga`, `ogv`, `opus`, `spx`. One
implementation, `writer::OggMuxer`, behind all five `MuxerDesc` constants.

Issue #584 (FM-30a), epic #68 (FM-30).

## How it works

### Page writing reuses the demuxer's definition of a page

`vaco-mux-ogg` depends on `vaco-demux-ogg` and reuses `page::CAPTURE_PATTERN`,
`page::FIXED_HEADER_LEN`, `page::CHECKSUM_OFFSET`, `page::OggHeaderFlags`,
`page::MAX_SEGMENTS`/`CONTINUATION_VALUE`/`GRANULE_UNSET`, and
`crc::crc32` — one definition of what a page *is*, shared with the sibling
demuxer per D19, the same pattern `vaco-mux-flv` already uses against
`vaco-demux-flv`. `writer::PageBuilder::push_packet` is the write-side
mirror of `vaco_demux_ogg::page::packet_spans`: given a packet's payload,
it emits lacing values (including the exact-multiple-of-255 trailing zero)
and reports whether the segment table filled up mid-packet, in which case
the caller flushes the page and resumes into a new one flagged
`CONTINUED`.

### The granule field is a plain running total — no `pre_skip` adjustment

`StreamState::granule_cursor` accumulates each terminated packet's
duration (in the stream's own tick unit) from zero. This is directly the
value written into the granule field for **every** codec, Opus included —
**adding `pre_skip` back in was tried and measured wrong**: for 30
synthetic 960-sample Opus packets it produced `29112` against the correct
`28800`, exactly one `pre_skip` (312) too many. The reasoning that
resolves the apparent asymmetry with the demuxer side: RFC 7845's
`pre_skip` shifts the *demuxer's reported timestamp*
(`timestamp = granule - pre_skip`), not the granule field itself — a
caller's first packet already reports `pre_skip` samples' worth of real
encoder output, it is simply marked "do not play these". See
`writer::StreamState::granule_cursor`'s doc comment for the full
derivation, and `tests/roundtrip.rs` for the passing case this fixed.

### Header packets: synthesised for Opus and FLAC, best-effort otherwise

`CodecParameters::extradata` is one blob; Ogg's own codecs need one
(Opus, FLAC) to three (Vorbis, Theora) header packets. This crate follows
the convention its sibling demuxer reads back — `extradata` is exactly the
*identification* packet (`OpusHead`, or FLAC's raw 34-byte `STREAMINFO`) —
and `src/headers.rs` synthesises the mandatory second packet each format
still needs to be well-formed (`OpusTags` with a vendor string and zero
comments; a native FLAC `VORBIS_COMMENT` metadata block, same content).
**This is what makes Opus and FLAC round-trip through `vaco-demux-ogg`** —
verified directly in `tests/roundtrip.rs`, which muxes real packets and
reads them back with the sibling crate rather than asserting on
self-produced bytes.

Vorbis, Theora and Speex do **not** get this treatment: each needs a
*setup* header (encoder-chosen codebooks or quantisation tables) that
cannot be synthesised generically, and no crate in this workspace defines
a convention for packing three packets into one `extradata` blob to
unpack. `add_stream` writes only the single packet in `extradata` as that
stream's sole header page — a caller muxing Vorbis today gets a file whose
identification header is present and whose setup header is simply
missing, which no real decoder can use. This is a known, disclosed gap,
not a silent one.

### Page boundaries are this muxer's own policy

RFC 3533 fixes what a page *is*, not how many packets share one. This
muxer puts each header packet on its own page (BOS only on the very
first), then accumulates data packets until the body reaches
`writer::PREFERRED_PAGE_BODY` (4 KiB) or the segment table fills up. Real
encoders make different choices — `ffmpeg`'s own default groups a
stream's non-identification header packets onto one shared page (measured:
a real Vorbis file's second page holds both the comment and setup
packets) — so a remux through this crate will not be byte-identical to one
through the reference even when both are perfectly valid Ogg. D6 §0.3
already names this as the expected shape for a container whose spec
"permits a large space of valid files."

### The five registrations' defaults are measured, not assumed

```sh
ffmpeg -h muxer=ogg   # Default video codec: theora. Default audio codec: flac.
ffmpeg -h muxer=oga   # Default audio codec: flac.
ffmpeg -h muxer=ogv   # Default video codec: vp8.
ffmpeg -h muxer=opus  # Default audio codec: opus.
ffmpeg -h muxer=spx   # Default audio codec: speex.
```

`MUXER_OGG` and `MUXER_SPX` leave their measured default (`theora`,
`speex`) as `None` rather than the wrong value: `vaco_codec_core::CodecId`
has no variant for either (confirmed by reading the enum). `MUXER_OGV`'s
`vp8` default is expressible and is set exactly.

## How to change it

- **Adding a codec's header synthesis** (Vorbis, once a convention for
  multi-packet `extradata` exists somewhere in the workspace, or Theora)
  goes in `src/headers.rs`, following the `opus_tags`/`flac_first_packet`
  shape, then a new match arm in `writer::OggMuxer::add_stream`.
- **Page-flush policy** is entirely `writer::PREFERRED_PAGE_BODY` and the
  segment-table-full check in `write_packet`'s loop; changing either
  changes page boundaries but never correctness (the granule is always
  computed from `granule_cursor`, independent of how packets are grouped
  into pages).
- **Gotcha:** `StreamState::pending_continued` must be set from
  `flush_page`'s `continues_next` argument and read back into the *next*
  flushed page's flags — it is easy to add the continuation bookkeeping in
  `PageBuilder` and forget that the `CONTINUED` header bit itself still
  needs to be threaded through per-stream state across two separate
  `flush_page` calls, since a page and the decision about the *next* one
  are not made at the same call site.

## Configuration

No format-specific options are read from `FormatOptions`
(`vaco-mux-ogg` does not use the format-options struct at all; a muxer's
options in this codebase are per-`Muxer`-implementation, and this one has
none yet — see `writer::PREFERRED_PAGE_BODY` for the one tunable, which is
a compile-time constant today).

## Dependencies

- `vaco-core`, `vaco-io`, `vaco-limits`, `vaco-packet` — the standard
  format-layer stack.
- `vaco-format-core` — `Muxer`, `MuxerDesc`.
- `vaco-codec-core` — `CodecId`, `CodecParameters`.
- `vaco-demux-ogg` — `page` (wire shapes) and `crc` (RFC 3533 CRC-32).
  This is the D19 shared-definition dependency the module docs describe,
  not a layering violation: both crates sit at layer 4 and neither is a
  codec crate.

## Fuzzing

Not applicable in the D6 sense (fuzz targets are for parsers of untrusted
input; a muxer's input is caller-constructed `Packet`s, not attacker-chosen
bytes). Correctness is instead checked by `tests/roundtrip.rs`, which is a
stronger test than a fuzz target could be here: it proves agreement with an
independently-developed sibling crate rather than merely the absence of a
panic.

## Known gaps (say plainly what is not done)

1. Vorbis, Theora and Speex muxing writes only an identification header
   packet; no setup header, so the output is not decodable by a real
   decoder for those three codecs.
2. Page boundaries are a policy choice and will not be byte-identical to
   the reference (see above) — this is disclosed as an expected divergence
   under D6 §0.3, not a bug to fix.
3. No `oggpagesize`/`page_duration`-equivalent options; `PREFERRED_PAGE_BODY`
   is a fixed constant.
4. Serial numbers are assigned as a simple sequential counter, not
   (pseudo-)randomly — legal per RFC 3533, which requires only uniqueness
   within the file, but a caller comparing serials against the reference's
   output will see different numbers.
