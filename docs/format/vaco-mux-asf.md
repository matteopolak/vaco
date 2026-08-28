# `vaco-mux-asf`

Layer 4. The ASF muxer: Header Object generation, fixed-size-packet
packetisation, and the Simple Index Object. Companion to `vaco-demux-asf`,
independent of it at the source level (a writer's serialise state and a
reader's parse state are not the same concept twice) but its own tests
round-trip through that crate to verify what gets written actually demuxes
back to what was asked for — the same relationship `vaco-mux-avi` has with
`vaco-demux-avi`.

Registered as `asf` and `asf_stream` — measured (`ffmpeg -h muxer=asf_stream`)
to be the same writer, same default codecs, same `-packet_size` option; the
two-name split is the reference's "file" vs "stream" output distinction, not
a byte-layout difference this crate needs to react to.

---

## What is supported

Video: H.264, HEVC, VP8, VP9, MJPEG, PNG (the same generic
`BITMAPINFOHEADER`/`biCompression` mechanism `vaco-mux-avi` uses) and VC-1
(`WMV3`, ASF's own native codec). Audio: PCM, MP3, AAC, Windows Media Audio
1/2/9-Pro. Anything else is `Error::Unsupported` from `Muxer::add_stream`,
never a guessed tag `vaco-demux-asf` would misidentify.

## What is deferred

- **Compressed payloads** (`[ASF] §5.2.3.2/.4`) are never written, only
  read. Every packet uses the ordinary payload shape, which the spec always
  permits — compressing is an optional space saving, not a requirement.
- **The top-level Index Object** (`[ASF] §6.2`) is not written, only the
  Simple Index Object (one per video stream).
- **Packet `Duration`** is always written as `0` — this crate does not track
  per-packet duration separately from the next packet's Send Time.
- **`Maximum Bitrate`** in the File Properties Object is always `0` — not
  tracked.

---

## How it works

### Packetisation, the hard part

Every physical Data Packet is exactly `AsfMuxer::packet_size` bytes (default
3200 — measured: `ffmpeg -h muxer=asf_stream` reports `-packet_size <int> …
(default 3200)`, and it is the value the reference's own `-f asf` output
uses for `Minimum`/`Maximum Data Packet Size`) and always uses the
*multiple-payload* framing (`[ASF] §5.2.3.3`), even for a single payload —
measured to be what `ffmpeg 8.1`'s own muxer does too, and it means this
crate has exactly one payload-serialisation path (`PayloadEntry::serialize`)
rather than two.

`Muxer::write_packet` hands over one whole media object. Two things can
happen:

- **It fits** (with its 17-byte payload header) alongside whatever is
  already pending: it joins `AsfMuxer::pending`, flushed later when the
  packet is full, holds 63 payloads (the 6-bit `Number of Payloads` field's
  limit), or `write_trailer` runs out of packets to fill.
- **It does not fit even in an empty packet**: `write_fragmented` splits it
  into consecutive fragments, each alone in its own packet, with `Offset
  Into Media Object` tracking where each fragment starts. Every fragment
  carries the same 8-byte Replicated Data (object size + presentation time)
  — what lets `vaco-demux-asf`'s reassembly know when the object is
  complete without this crate saying so out of band.

The exact bytes chosen for `Length Type Flags` (`0x11`: multiple payloads,
WORD-width Padding Length, no error correction/sequence/packet-length
fields) and `Property Flags` (`0x5D`: the spec's own recommended field
widths) were confirmed to match `ffmpeg 8.1`'s own muxer output byte for
byte, not merely chosen to be spec-legal.

### Every stream's declared time base is milliseconds

`Muxer::stream_time_base` returns `1/1000` for every stream, video or audio.
ASF's wire format only ever carries millisecond-precision presentation
times regardless of media type, so rather than choosing a stream-specific
time base and converting at `write_packet` time, this crate lets the
generic interleave pipeline's M1 rescale step do the conversion once,
upstream — `write_packet` then just reads `packet.pts.ticks()` directly as
milliseconds.

### What gets patched, and what does not

`File Properties`' `File Size`/`Data Packets Count`/`Play Duration`/`Send
Duration` and the Data Object's own `Object Size`/`Total Data Packets` are
placeholders until `write_trailer`, which seeks back and patches them if the
sink can seek (`IoWriter::is_seekable`); a non-seekable sink keeps the
placeholders at `0`, the same convention `vaco-mux-avi` documents for
`dwTotalFrames`. `Play Duration` and `Send Duration` are both written as
`max_pts_ms * 10_000` (ms → 100ns) — this crate does not track the two
separately, matching that both come out equal in `ffmpeg`'s own
non-streaming output for a well-behaved constant-bitrate file.

### The Simple Index Object

Built once per video stream at `write_trailer` from every keyframe's
`(packet_number, presentation_time_ms)`, recorded as packets are flushed.
The fixed-interval index (`[ASF] §6.1`: entry `k` names the nearest-past
keyframe for time `k * IndexEntryTimeInterval`) is derived by walking the
recorded keyframes once, advancing to the next one only when its time has
actually passed the current boundary — the same algorithm
`vaco-demux-asf::index::simple_index_to_packet_index` inverts on the read
side.

---

## What was exercised, and what was not

- **Exercised** (`tests/roundtrip.rs`, round-tripped through
  `vaco-demux-asf`): stream shape and codec-ID round-trip for H.264 video
  and PCM audio; packet order, timestamps, and keyframe flags; a
  600-byte media object muxed with a 256-byte packet size, forcing
  fragmentation, reassembled byte-exact on the read side; the trailer patch
  path (`Play Duration` present and correct after a fresh `open`); a codec
  with no ASF mapping rejected at `add_stream` rather than silently
  mis-tagged.
- **Structurally present, not exercised end-to-end**: the Simple Index
  Object's *use* for seeking (built and byte-valid, but no test seeks
  through a muxed file using it — `vaco-demux-asf`'s own index tests cover
  the parsing/conversion side against hand-built bytes instead); WMA/VC-1
  codec paths (mapped and unit-tested for the FourCC/tag round-trip in
  isolation, not muxed through a full file in this crate's own tests).

---

## H.264/HEVC framing, and the Presentation Time field

`-c copy` from an `avcC`/`hvcC`-framed source (MP4, say) needed two fixes to
decode cleanly out of ASF, neither obvious from the byte layout alone:

- **Length-prefixed samples must become Annex B first.** ASF has no
  length-prefixed convention for H.264/HEVC payloads — measured, a
  length-prefixed sample fed straight through decoded as "No start code is
  found" on every access unit. `AsfMuxer::maybe_convert` and
  `AsfMuxer::check_bitstream` mirror `vaco-mux-mpegts`'s pair exactly:
  `StreamOut::length_size` records the container's declared length width at
  `add_stream` time, `maybe_convert` rewrites to Annex B as a fallback with no
  `BsfProvider`, and `check_bitstream` asks M6 for `h264_mp4toannexb`/
  `hevc_mp4toannexb` when one is wired.
- **The "Presentation Time" field wants decode order, not display order.**
  `write_packet` used `packet.pts`; with a B-frame source that is not
  monotonic across calls, and a real ASF reader treats a non-monotonic value
  as corrupt framing — it decoded a different (wrong) picture into each slot,
  same access-unit count, different YUV bytes throughout. Swapping to
  `packet.dts` (monotonic by construction) made a decoded-video MD5 match the
  reference exactly. See the comment at the `pts_ms` assignment in
  `write_packet` for the measurement.

## How to change it

- **Add a codec mapping**: `codec::video_fourcc`/`audio_format_tag` are the
  one place to extend, each with a matching entry in
  `vaco_format_asf::codec`'s read-side table so the pair actually round-trips
  — `codec::tests::video_and_audio_mappings_round_trip_through_the_read_side`
  is the test that would catch a mismatch.
- **Change the packet-size default**: `DEFAULT_PACKET_SIZE`, or call
  `AsfMuxer::with_packet_size` per instance.
- **Write compressed payloads**: would need a second `PayloadEntry`
  variant and a decision about which conditions (`[ASF] §5.2.3.2`'s list:
  same stream, uniform key-frame-ness, ≤256 bytes, ≤8 bytes replicated data,
  consecutive object numbers, constant-interval timestamps) this crate is
  willing to detect automatically versus require the caller to request.
- **Write the top-level Index Object**: `vaco-demux-asf::index::IndexObject`
  already documents the exact byte layout the read side expects; nothing
  here builds it.

## Configuration

None beyond `AsfMuxer::with_packet_size` and
`AsfMuxer::with_creation_date_100ns` (see below) — no generic
`FormatOptions` knob is consulted.

### Creation Date, and why this crate never calls the clock

The File Properties Object's `Creation Date` is 100-nanosecond ticks since
1601-01-01 00:00:00 UTC. `std::time::SystemTime::now()`/`Instant::now()`
panic on `wasm32-unknown-unknown`, and this crate builds for that target
(`cargo xtask wasm-check`), so it never reads the clock itself. The field
defaults to `0` ("not stated") and a caller supplies a real value via
`AsfMuxer::with_creation_date_100ns` — typically converting a Unix
timestamp obtained through `vaco-time`, which *is* the crate responsible for
wasm-safe time.

## Dependencies

`vaco-core`, `vaco-io` (`IoWriter`, `MediaSink`), `vaco-limits`,
`vaco-packet`, `vaco-format-core` (`Muxer`, `MuxerDesc`), `vaco-format-asf`
(GUIDs, the object-header byte layout is reproduced inline via the local
`object()` helper rather than depending on `vaco-format-asf::object`'s
*iterator*, which is a read-side concept this crate has no use for),
`vaco-format-riff` (`chunk::ChunkId`, the WMA format-tag constants),
`vaco-codec-core` (`CodecId`, `CodecParameters`). Dev-only:
`vaco-demux-asf`, for the round-trip test.
