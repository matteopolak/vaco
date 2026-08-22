# `vaco-parse-opus`

## What it is

Header parsing for Opus: the identification header (`OpusHead`, and its MP4
`dOps` spelling), the comment header (`OpusTags`), and the TOC byte and frame
packing that split a packet into frames. It produces the `CodecParameters` a
container reports and the duration of each packet. **It does not decode.**

Unlike AAC there is no licensing reason to stop at parsing — D9 has Opus GREEN
and shippable. The reason is scope: v0.1 is `ffprobe` on modern containers (D5),
which needs stream properties and packet boundaries and nothing else.

## How it works

| Module | Syntax | Specification |
|---|---|---|
| `head` | `OpusHead`, `dOps`, channel mapping families | RFC 7845 §5.1, RFC 8486 §3, Opus-in-ISOBMFF |
| `comment` | `OpusTags` | RFC 7845 §5.2 |
| `packet` | the TOC byte and frame packing codes 0..=3 | RFC 6716 §3 |

### What the caller must have done first

**Opus packets are framed by the container, never by themselves.** There is no
sync word, no length prefix, and no way to find a packet boundary by looking at
the bytes. `OpusPacket::parse` therefore takes a slice that is *exactly* one
packet — an Ogg packet reassembled from its segments, one Matroska block frame,
one MP4 sample, or one RTP payload — and treats its length as authoritative.

`OpusParser` implements `vaco_codec_core::Parser` so that a demuxer can reach
this crate the same way it reaches every other, and it inherits that
requirement: **one `push` per packet**. It is not a resynchronising byte-stream
splitter, because there is nothing to resynchronise to. Pushing two packets
before draining produces one nonsense packet rather than two good ones. That is
stated in the crate docs as well, because it is the kind of contract a caller
otherwise discovers from a bug report.

The one place Opus *is* self-delimiting is inside a multi-stream packet, where
every stream but the last codes its own length (RFC 6716 Appendix B). That is
`OpusPacket::parse_self_delimited`, and `split_streams` walks a whole
multi-stream packet with it. A 5.1 file's Ogg packets really do look like
`fc 02 ff fe | fc 02 ff fe | fc 02 ff fe | fc …` — four streams, the first three
length-prefixed.

### Where the reported parameters come from

* **`sample_rate` is always 48000.** `input_sample_rate` in the identification
  header describes the material *before* encoding and changes nothing; RFC 7845
  §5.1 says as much, and the reference agrees — a header declaring 8000 still
  reports `sample_rate=48000`. Measured across 48000, 44100, 16000, 8000, 0, 1,
  96000 and `0xFFFFFFFF`.
* **`initial_padding` is `pre_skip`**, verbatim.
* **`channels` is `Output Channel Count`**, with no doubling anywhere — a
  welcome contrast to AAC.
* **`channel_layout` depends on the mapping family**, and families 0 and 1 use
  *Vorbis* channel order, which is not AAC's. Four channels is `quad`, not
  `4.0`; seven is `6.1`, not `6.1(back)`.

`output_gain` is parsed and exposed (`output_gain_db()`) but has no effect on
anything `ffprobe` prints.

### Validation, all probed

| Field | Rule | Reference's message |
|---|---|---|
| version | accepted iff `version >> 4 == 0`, so 0x00..=0x0f | `Header processing failed` |
| channels | never zero | `Zero channel count specified in the extradata` |
| family 0 | 1 or 2 channels | `Channel mapping 0 is only specified for up to 2 channels` |
| family 1 | 1..=8 channels | `Channel mapping 1 is only specified for up to 8 channels` |
| family 2 | `(n+1)^2` or `(n+1)^2 + 2` channels | `…only specified for channel counts which are (n + 1)^2 or (n + 1)^2 + 2` |
| family 3 | not implemented | `Mapping type 3 is not implemented.` |
| streams | `1 <= streams`, `coupled <= streams`, `streams + coupled <= 255` | `Invalid stream/stereo stream count: 0/0` |
| mapping | each index `< streams + coupled`, **except 255** | `Invalid channel map for output channel 0: 9` |

Family 3 is `Error::Unsupported`, not `Error::InvalidData` — the file is well
formed and we are the ones who do not implement it, which is the distinction
`vaco_core::Error` draws and the one the reference's own message draws too.

Mapping index 255 is RFC 7845 §5.1.1's "this output channel is silent" escape
and is deliberately not range-checked. The reference accepts it; so do we.

Note what is *not* checked: `channel_count` need not equal
`streams + coupled`. A header claiming two output channels over three decoded
ones is accepted by the reference, and is accepted here.

### Frame lengths

RFC 6716 §3.2.1: a length byte below 252 is the length; 252..=255 takes a second
byte and the length is `second * 4 + first`. Frames cap at 1275 bytes, packets at
120 ms, and 120 ms of 2.5 ms frames is 48 — hence `MAX_FRAMES`.

Every length is bounded against the bytes actually present *before* it is used.
This is the classic Opus parser bug: a code-2 or code-3 VBR packet can declare
frame sizes the packet does not contain, and subtracting before checking
underflows. Padding compounds it, because a run of `255` bytes escapes to an
arbitrarily large padding length.

Padding is handled differently in the two framings, which is easy to get wrong:
for a packet whose length the caller supplied, the padding is a suffix and is
cut away *before* the frame lengths are worked out from what is left; for a
self-delimited packet the end is not known until the frames have been read, so
the padding is skipped *afterwards*.

## How to change it

* `IdentificationHeader` and `OpusPacket` grow fields safely.
  `IdentificationHeader` is `#[non_exhaustive]`.
* **`to_opus_head()` must stay the exact inverse of `parse`.** The
  `accepted_heads_round_trip` property test and the `parse_opus_head` fuzz target
  both assert it, because `dOps` → `OpusHead` conversion is what a demuxer does
  for MP4 and a lossy conversion there would change `extradata` for every MP4
  Opus track.
* **Everything in `comment` borrows.** Adding an owned `String` field would make
  every caller pay for metadata most of them do not read. If you need owned
  values, convert at the call site.
* **Gotcha: the comment count is a `u32` and the packet is not.** A header can
  claim four billion comments in twenty bytes. `CommentHeader::parse` walks the
  list once, bounded by the bytes that exist, and errors rather than trusting
  the count.
* **Gotcha: `MAX_FRAMES` is a duration bound, not a count bound.** The `count`
  field in a code-3 packet is six bits, so 63 fits — but 63 frames of 20 ms is
  1260 ms, which the format forbids. The check is `count * frame_samples <=
  5760`, not `count <= 48`.

## Performance

`cargo bench -p vaco-parse-opus`, on an Apple Silicon laptop. Medians.

| Benchmark | Time |
|---|---|
| `identification_header` (stereo, family 0) | 10 ns |
| `identification_header` (5.1, family 1) | 28 ns |
| `comment_header` (vendor + 4 tags, walked) | 73 ns |
| `packet_code0` (40 / 200 / 1200-byte payload) | 28 / 29 / 32 ns |
| `packet_code3_vbr` (2 / 6 / 48 frames) | 32 / 32 / 32 ns |
| `packet_multistream` (2 / 4 / 16 streams) | 75 / 132 / 677 ns |

Packet parsing is flat in payload size, as it should be — the frames are
borrowed, never copied — and flat in frame count up to the 48-frame maximum.
Multi-stream splitting is linear in the stream count, which is the one shape
that scales with the file rather than with the packet.

## Configuration

None. No features, no environment variables.

`OpusParser::new` takes a `vaco_limits::Limits` and charges packet allocation
against it. Header and packet parsing allocate nothing at all: the identification
header's mapping table is an `ArrayVec<u8, 255>` sized by the format's own
one-byte field, and packet frames are borrowed slices.

## Dependencies

| Crate | Why |
|---|---|
| `vaco-bitstream` | `ByteReader` for the fixed-layout headers |
| `vaco-codec-core` | the `Parser` trait, `CodecParameters`, `CodecId` |
| `vaco-chlayout` | channel layouts, including the ambisonic ones |
| `vaco-core` | the error taxonomy |
| `vaco-limits` | `Budget` for packet allocation |
| `vaco-packet` | `Packet` |
| `arrayvec` | fixed-capacity storage for the channel mapping, so the header parser allocates nothing |

Dev-only: `proptest`, `divan`.

## Testing and probing

`cargo test -p vaco-parse-opus` runs 35 unit and property tests. Every expected
value carries a `// measured:` comment.

Fuzz targets: `parse_opus_head`, `parse_opus_packet`.

### How the reference was probed

The shortest path to the identification-header parser is a real Ogg Opus file
with its first page rewritten:

1. `ffmpeg -f lavfi -i sine=f=440:r=48000:d=2 -ac 2 -c:a libopus -b:a 64k
   out.opus`.
2. Replace the `OpusHead` packet in page 0, rebuild the segment table, and
   recompute the page CRC (CRC-32, polynomial `0x04c11db7`, initial value zero,
   no reflection, no final XOR — the CRC field is zeroed before the sum). The
   patcher round-trips the original file byte for byte, which is the check that
   it is not itself the thing being measured.
3. `ffprobe -v error -show_streams -of json`.

WebM would have been simpler for same-length edits — Matroska stores the
`OpusHead` verbatim in `CodecPrivate` with no checksum — but a mapping-family
change alters the length, and fixing EBML sizes up the element tree is more work
than an Ogg CRC.

Packet framing was probed by generating with `-frame_duration 2.5/5/10/20/40/60`
and reading `ffprobe -show_packets` durations at `time_base=1/48000`: 120, 240,
480, 960, 1920 and 2880 samples, with TOC bytes `0xe4`, `0xfc`, `0xfd` and
`0xff`. Multi-stream self-delimiting framing was read directly out of the Ogg
packets of a 5.1 file.

## Known divergences

None found. Every observable the reference produces for an Opus stream —
`sample_rate`, `channels`, `channel_layout`, `initial_padding`, packet duration —
is derivable from the headers, so a parse-only build reproduces all of it. That
is the opposite of the AAC situation and worth noting: Opus put its
configuration in the header where it belongs.
