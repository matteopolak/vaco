# `vaco-format-asf`

Layer 4. The shared ASF (Advanced Systems Format) object model: GUIDs, the
24-byte object header walk, and codec-ID mapping. Exists so
`vaco-demux-asf` and `vaco-mux-asf` share one definition of "what a GUID is"
and "how an object header is laid out" rather than each defining it (D19),
the same role `vaco-format-riff` plays for AVI's demux/mux pair.

---

## What it is

| Module | Contents |
|---|---|
| `guid` | `Guid` — the 16-byte little-endian-ish object identifier, its `Display` (canonical `AABBCCDD-EEFF-...` text form) |
| `well_known` | Every standard GUID `[ASF] §10` names, generated into one table via a macro |
| `object` | `ObjectHeader`/`ObjectIter` — the 24-byte `Object ID + Object Size` prefix every ASF object shares, and the walk over a run of them |
| `codec` | Codec-ID mapping for the Audio/Video Media Types, bridging `vaco-format-riff` |

Written from Microsoft's *"Advanced Systems Format (ASF) Specification"*,
Revision 01.20.06 (the publicly published specification, distributed through
Microsoft's Open Specifications programme). No FFmpeg source was consulted
(D7/D15); the specification text was retrieved as a PDF mirror
(`noullet-gei.gitlab.io/dmm/Chap4.4/ASFSpec.pdf`, converted to text with
`pdftotext -layout`) and every byte layout below traces to a table in that
document, not to any implementation's headers.

---

## How it works

### GUID byte order

`[ASF] §2.1` states every object is stored little-endian. A GUID is the
classic Microsoft layout: `Data1:u32(LE) Data2:u16(LE) Data3:u16(LE)
Data4:u8[8]` — the last eight bytes are **not** byte-swapped, which is why
the canonical text form reads `Data4` left-to-right exactly as the bytes sit
on disk while `Data1..Data3` are byte-reversed. `Guid::from_fields` takes the
four dashed groups exactly as the spec's own GUID tables print them, so
`well_known`'s table is a direct transcription with nothing to get backwards
— confirmed against a byte-level dump of `ffmpeg 8.1`'s own `-f asf` output
(`ASF_Header_Object`'s first 16 bytes measured to be `30 26 B2 75 8E 66 CF 11
A6 D9 00 AA 00 62 CE 6C`, matching `Guid::from_fields(0x75B22630, 0x668E,
0x11CF, [0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C])`).

### The object walk

Every ASF object — a Header Object child, a Header Extension Object child,
the Data Object, an index object — shares:

```text
Object ID:   GUID    128 bits
Object Size: QWORD    64 bits (LE), includes this 24-byte prefix
Object Data: BYTE[Object Size - 24]
```

`ObjectIter` walks a byte slice as a run of these, the same discipline
`vaco-format-riff::chunk::ChunkIter` uses for RIFF chunks: a declared size
is clamped to what is actually present rather than trusted, so a lying
`Object Size` yields a short object, never a panic or an unbounded read. A
header this short to parse at all stops the iterator after yielding one
`Err`, rather than looping on the same truncated tail.

### Codec identity

`[ASF] §9.1`/`§9.2` state that the Audio/Video Media Types' `Type-Specific
Data` are, byte-for-byte, `WAVEFORMATEX` and (a small prefix plus)
`BITMAPINFOHEADER` — structures `vaco-format-riff` already parses. `codec`
is a thin bridge: it calls `vaco_format_riff::wave`/`bitmapinfo` for parsing
and `wave_tags`/`video_tags` for the base codec identity, adding only the
handful of tags those tables do not resolve to a `CodecId` because they
predate that enum's WMA/VC-1 variants — `audio_codec_id` additionally
recognises the three Windows Media Audio format tags (`0x0160`/`0x0161`,
already named in `vaco_format_riff::wave`, plus `0x0162` for WMA
9/10 Professional, named here as `WAVE_FORMAT_WMAUDIO3` since the shared
crate's tests never probed it), and `video_codec_id` additionally recognises
VC-1's two FourCCs (`WMV3`, `WVC1`).

---

## How to change it

- **Add a GUID**: add one row to the `guid_table!` macro invocation in
  `well_known.rs`, with its name and the four dashed fields exactly as
  `[ASF] §10` prints them. The macro generates both the `pub const` and the
  `ALL` table `name_of` searches.
- **Add a codec mapping**: extend `codec::audio_codec_id`/`video_codec_id`
  (and their `_name` siblings) — but only for a codec `vaco-codec-core::CodecId`
  already has a variant for; see the module doc's note on "no guessed
  near-miss".
- **Object header changes**: this is the one seam both `vaco-demux-asf` and
  `vaco-mux-asf` build on. A change to `ObjectHeader`'s layout or
  `ObjectIter`'s clamping rules affects both crates identically, which is the
  point of the crate existing.

## Configuration

None — this crate has no `FormatOptions`-style knobs of its own; it is pure
functions over byte slices.

## Dependencies

`vaco-core` (`Error`, `Result`), `vaco-codec-core` (`CodecId`),
`vaco-format-riff` (`wave`, `bitmapinfo`, `wave_tags`, `video_tags`, `chunk`).
No `vaco-io` dependency at all — this crate never reads or writes a file; the
two sibling crates decide how much of one to hold in memory.
