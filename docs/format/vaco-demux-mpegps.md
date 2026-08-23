# `vaco-demux-mpegps`

## What it is

The MPEG program stream demuxer: ISO/IEC 11172-1 (MPEG-1 systems) and
ISO/IEC 13818-1 §2.5 (MPEG-2 program stream). It reads `.vob`/`.mpg`/`.mpeg`/
`.vcd` files and files produced by `ffmpeg -f mpeg`/`-f vcd`/`-f vob`/
`-f svcd`/`-f dvd`. One demuxer covers every profile; reading is symmetric
across them, unlike muxing (`vaco-mux-mpegps`), which needs five separate
profiles for their differing pack-size and system-header constraints.

Registers as `mpeg` (matching the reference's demuxer name), long name
`MPEG-PS (MPEG-2 Program Stream)`.

## How it works

### Layout

| Module | Contents |
|---|---|
| `probe` | content detection — a rhythm of plausible start codes, since `00 00 01` alone is not distinctive |
| `pack` | pack headers and the system header, both the MPEG-1 and MPEG-2 syntaxes |
| `pes` | PES packet headers, both syntaxes |
| `substream` | `private_stream_1` sub-stream ids (AC-3/DTS/LPCM/subpicture) |
| `keyframe` | best-effort MPEG-1/2 `picture_coding_type` sniff, for `PacketFlags::KEY` |
| `demux` | framing, PES assembly, the SCR clock, seeking |

### The two envelopes

A program stream can carry **either** PES syntax:

* **MPEG-1** (ISO/IEC 11172-1 §2.4.3.7): no flags byte, no
  `PES_header_data_length` — optional stuffing, an optional STD buffer
  scale/size field, then the timestamp fields directly behind a marker
  nibble. `ffmpeg -f mpeg`/`-f vcd` write this.
* **MPEG-2** (ISO/IEC 13818-1 §2.4.3.7): the familiar flags-byte-plus-length
  form MPEG-TS also uses. `ffmpeg -f vob`/`-f svcd`/`-f dvd` write this.

`pes::PsPesHeader::parse` distinguishes them by the top two bits of the
first optional-header byte (`'10'` → MPEG-2; anything else → MPEG-1) and
dispatches accordingly. Getting this wrong misframes every packet in an
`mpeg`/`vcd` file by at least three bytes — verified directly against
`ffmpeg -f mpeg` output in `pack.rs`'s and `pes.rs`'s unit tests, and again
end-to-end in `tests/reference_files.rs` against real captured bytes.

Likewise, pack headers come in two incompatible shapes distinguished by the
first byte after the pack start code (top nibble `0010` = MPEG-1, fixed 12
bytes; top two bits `01` = MPEG-2, 14 bytes plus 0–7 stuffing bytes). The
33-bit SCR bit-splitting formula for each is verified against real
`ffmpeg -f mpeg`/`-f vob` bytes in `pack.rs` (monotonically increasing SCR
across consecutive packs, at a rate consistent with the encoded
`mux_rate` — a wrong bit split would not produce that).

### `private_stream_1` substreams

DVD/SVCD route AC-3, DTS, LPCM and subpicture tracks through the single
`stream_id` `0xBD`, with a one-byte sub-stream id at the front of the PES
payload distinguishing them (`substream::classify`). This is a DVD-Video/
SVCD authoring convention, not an ISO/IEC 13818-1 table — reproduced from
public technical references to the DVD-Video specification, not from any
FFmpeg source. `substream::LpcmHeader` decodes the further 3-byte LPCM
sub-header (bit depth, sample rate, channel count).

### Discovery and reframing

Streams are registered lazily, the same way `vaco-demux-mpegts` registers
PIDs: `MpegPsDemuxer::open` eagerly pumps a bounded prefix (`OPEN_SCAN_PACKS`
packs) so the common case — every stream declared in the system header, plus
the first private-stream-1 packet of each substream — is known by the time
`open` returns. `read_packet` keeps registering new ones for as long as the
file keeps introducing them.

**This demuxer looks up a `vaco_codec_core::Parser` for each newly
discovered stream and reframes PES payloads into codec frames through it**,
unlike `vaco-demux-mpegts` (issue #632: that demuxer receives a
`ParserProvider` and never calls it, so one PES payload becomes one packet
regardless of how many codec frames it holds — measured as a 2836-byte
packet where the reference emits thirteen). The lookup happens once, while
`open` still holds the borrowed `&dyn ParserProvider` (the frozen
`DemuxerDesc::open` signature does not let a `Demuxer` keep it past
construction — see "How to change it").

**What this does not fix today**: `vaco_codec_core::CodecId` has no MPEG-1/2
video, MPEG audio (layer I/II), AC-3, DTS or DVD-flavoured LPCM variant
(surveyed 2026-08-23). With no codec id there is no parser to look up, so in
practice every stream this crate can currently classify falls back to
whole-PES-payload packets — the same observable shape as #632, but for a
different reason (no parser exists to call, not one that exists and is
ignored). It will start reframing automatically the day those codec ids and
parsers land, with no change needed in this crate.

### The SCR clock

A single `vaco_format_core::time::WrapState` at `SCR_WRAP_BITS = 33` (the
same width as MPEG-TS's PCR) is shared by the pack SCR and every packet's
PTS/DTS, since a program stream is single-program. `MpegPsDemuxer::seek`
uses `vaco_format_core::seek::binary_search` with a probe closure that
scans forward from a byte position for the next pack header and reports its
SCR — a direct analogue of MPEG-TS's PCR-based bisection seek.

## How to change it

* **New `private_stream_1` sub-id range**: add a variant and range to
  `substream::SubstreamKind`/`classify`. Nothing else needs to change —
  `demux.rs`'s `stream_for_substream` calls `classify` generically.
* **Reframing activates once `vaco-codec-core` grows the missing codec ids**
  (MPEG-1/2 video, MPEG audio, AC-3, DTS, LPCM). No change is needed *here*:
  `finish_pes` already calls `self.es[..].parser.parse()` when a parser
  exists. Growing `vaco-codec-core`'s `CodecId` and wiring `parser_for` in
  the registry is what activates it.
* **Byte-exact seek/duration parity with the reference** has not been
  measured (see "What was and was not measured" below); the seek and
  duration paths are structurally present, exercised by unit tests, but not
  checked against `ffprobe`/`ffmpeg -ss` field-for-field.
* **Do not add a `vaco-format-mpeg-common` dependency here without reading
  the note below first** — that crate does not exist, and creating it is
  out of this crate's scope.

### Why this crate does not share `pes.rs`/`pack.rs` with `vaco-demux-mpegts`

Plan 18 §8.3 names `vaco-format-mpeg-common` (start-code scanning, PES
header parse/serialise, the 33-bit timestamp codec, SCR/PCR encoding) as the
intended single home for what MPEG-TS and MPEG-PS share, so that timestamp
handling cannot drift between them. It does not exist: it is its own
roadmap work package (`SH-04`, plan 20 line 685) that no wave has assigned
yet, and this brief's scope was three specific crate directories, none of
which is a new shared helper crate.

**PES *is* genuinely shared conceptually** — the 33-bit timestamp
bit-layout in `pes.rs` here is bit-for-bit the same formula
`vaco-demux-mpegts::pes` uses, both independently derived from
ISO/IEC 13818-1 §2.4.3.6/.7 (not copied from each other; see D7). Where they
diverge is real: MPEG-PS also has to parse the *older* MPEG-1 PES envelope,
which MPEG-TS never carries, so a shared crate's `PesHeader` would need the
`PesSyntax` dispatch this crate has and MPEG-TS does not.

Two options were available and neither was taken:

1. **Create `vaco-format-mpeg-common` and move both crates onto it.** Out of
   scope for this brief — it would mean writing into a fourth crate this
   brief does not name, and moving `vaco-demux-mpegts`'s existing code,
   which the brief explicitly says to stop and report rather than do.
2. **Depend directly on `vaco-demux-mpegts`** for its `pes` module. Rejected
   even though `xtask layer-check` would allow it (both crates are layer 4,
   and same-layer edges are permitted): it would make a program-stream
   demuxer depend on a transport-stream demuxer for an implementation
   detail neither crate's public API is built around, and it does nothing
   to fix the *duplication* plan 18 is actually worried about — the 33-bit
   timestamp codec would still be typed once in `vaco-demux-mpegts` and used
   once in `vaco-demux-mpegps`, indistinguishable from copy-paste from the
   caller's side, just with an extra crate edge.

**What unifying them later would take**: create `vaco-format-mpeg-common` at
layer 4, move the timestamp codec (`decode_timestamp`/`encode_timestamp`)
and the MPEG-2 PES envelope (the part MPEG-TS and MPEG-PS share) into it,
leave the MPEG-1-only envelope and the TS-specific adaptation-field/PCR code
in their respective crates, and have both `vaco-demux-mpegts` and
`vaco-demux-mpegps` (and `vaco-mux-mpegps`, and eventually `spdif`/`vobsub`,
per plan 18's dependent list) depend on it. Estimated at plan 20's own line:
2 pw. This crate's `pes.rs`/`pack.rs` module docs point back to this section
so the day that crate exists, the migration path is already written down.

## Configuration

No crate-specific options. `FormatOptions` (the 38 generic container
options) apply as usual; the fields this crate actually reads today are
none beyond what `IoContext`/`Budget` construction needs — `analyzeduration`/
`probesize`-style tuning of the eager open-time scan is not yet wired to
`OPEN_SCAN_PACKS`, which is a fixed constant (64 packs).

Constants worth knowing when tuning behaviour:

| Constant | Value | Meaning |
|---|---|---|
| `demux::MAX_PAYLOAD_BYTES` | 6 MiB | Ceiling on an unbounded (`PES_packet_length == 0`) PES payload |
| `demux::MAX_RESYNC_BYTES` | 1 MiB | How far a resync scan looks for the next start code |
| `demux::OPEN_SCAN_PACKS` | 64 | Packs scanned eagerly during `open`, before falling back to lazy discovery |
| `probe::STRONG_RUN` | 4 | Plausible start codes needed for `ProbeScore::MAX` |

## Dependencies

`vaco-core`, `vaco-io`, `vaco-limits`, `vaco-packet`, `vaco-format-core`,
`vaco-codec-core` — all below or at this crate's own layer (4), per D14.1.
No `vaco-parse-*` or concrete `vaco-codec-<name>` crate, and no
`vaco-registry` dependency: parsers arrive through the injected
`&dyn ParserProvider`, exactly as `vaco-demux-mp4` does.

## What was and was not measured

Exercised directly against real `ffmpeg 8.1` output (2026-08-23), embedded
as `tests/fixtures/*` (small, truncated captures, not full files):

* Pack header parsing, both syntaxes, including the SCR bit-splitting
  formula (monotonic SCR progression across real consecutive packs).
* System header parsing (rate/audio/video bounds, per-stream P-STD entries).
* PES header parsing, both syntaxes, on real video and MPEG-audio/AC-3
  packets.
* Stream discovery producing the right count and media types, including an
  AC-3 track carried in `private_stream_1`.
* End-to-end packet reads yielding a non-empty payload for every registered
  stream.

**Not measured against the reference, and known to be approximate**:

* `duration()`/`start_time` estimation policy (the reference's own
  heuristics for a program stream are not documented and were not probed
  here — see plan 18 §1.7 for why this is generally the highest-risk area).
* Keyframe detection (`keyframe::is_keyframe`) is a best-effort
  `picture_coding_type` sniff assuming one picture per PES payload; it is
  not a real MPEG-1/2 video parser and can be wrong when a PES packet spans
  more than one picture.
* Byte-exact seek landing position versus `ffmpeg -ss` on a real file.
* DVD subpicture (`0x20..=0x3F`) and SDDS (`0x98..=0x9F`) substreams are
  classified but never exercised against a real captured file — no test
  fixture carries either.
