# `vaco-format-apetag`

Layer 4. APEv1/APEv2 tag read and write, and ReplayGain across all four
conventions it and its neighbours use (SH-08, with SH-10 merged in).

This is **not** a demuxer and registers no component — `vaco-demux-ape`,
`vaco-demux-wv` (WavPack), `vaco-demux-mpc` (Musepack), `vaco-demux-tta` and
`vaco-demux-mp3` are the eventual callers, the way a container demuxer calls
into `vaco-format-riff` or `vaco-format-id3`. None of those demuxers exist
yet in this workspace, so this crate is exercised here only by its own unit,
property and fuzz tests.

---

## What it is

| Module | Contents |
|---|---|
| `tag` | the APEv1/APEv2 header/footer, item list, parse and serialise |
| `locate` | finding a tag at the start or end of a file, honouring the ID3v1 coexistence rule |
| `replaygain` | ReplayGain from Vorbis-comment-shaped text, and from a LAME binary header |

Written from the APEv2 tag specification published on the Hydrogenaudio
wiki (originally Monkey's Audio's own documentation, now the community
reference) and, for ReplayGain, the ReplayGain specification and the LAME
Tag specification, both likewise on Hydrogenaudio's wiki. No FFmpeg source
was consulted (D7/D15).

**Not probed against `ffmpeg`.** No muxer in the `ffmpeg` 8.1 build
available while writing this crate exposes an option to write an APE tag —
`mp3`, `wv` and `caf` were all checked via `ffmpeg -h muxer=<x>` and none
offers a `-write_apetag`-shaped option. That is itself the finding plan 13
§1b asks to be recorded rather than papered over: the tag structure here
comes from the published specification directly, and the LAME-header
ReplayGain fields are likewise unverified against a live encoder (`ffmpeg`'s
`libmp3lame` wrapper does not run LAME's own ReplayGain analysis pass, which
is what populates those fields). Both are flagged again at the exact
functions concerned (`tag`'s module docs; `replaygain::decode_gain_field`).

---

## How it works

### Tag structure (`tag`)

A mandatory 32-byte footer, optionally preceded by a byte-identical 32-byte
header (the only difference is one flag bit), with the item list in
between. `tag_size` in the footer counts from the first item to the end of
the footer *inclusive* — it does not include a leading header — which is
the field `ApeTag::parse` uses to find where the item list starts, clamped
against what the buffer actually holds (never trusted past that, the same
"declared size is clamped, not trusted" discipline `vaco-format-riff` uses
for RIFF chunk sizes). Each item is `value_size:u32le, flags:u32le,
key:NUL-terminated ASCII (2–255 bytes), value[value_size]`; a NUL-separated
value is the specification's own convention for a multi-valued item
(`ApeItem::text_values` splits it).

`ApeTag::to_bytes`/`to_bytes_with_header` serialise; a key outside the
2–255-byte printable-ASCII-excluding-`=` bound is rejected rather than
silently truncated or escaped.

### The ID3v1 coexistence rule (`locate`)

An MP3 can carry both an APE tag and a trailing 128-byte ID3v1 tag, and when
it does, the APE footer sits **immediately before** the ID3v1 tag, not at
the true end of file. `locate::find_trailing` checks for a trailing ID3v1
tag (`"TAG"` at `len - 128`) first and steps back over it before looking for
`"APETAGEX"`, so a footer sitting right before one is still found.
`locate::read_trailing` is the `IoContext`-driven counterpart for a demuxer
that does not want to hold the whole file in memory: it probes just enough
of the tail to read the footer's own `tag_size`, then re-reads exactly the
span the tag occupies.

### ReplayGain (`replaygain`)

Three of the four conventions (Vorbis comment, this crate's own APE items
converted to `(key, value)` text, and `vaco-format-id3`'s `TXXX` entries)
share one function, `from_text_entries`, because all three spell
`REPLAYGAIN_TRACK_GAIN` et al. identically and differ only in case — matched
case-insensitively rather than assuming one convention's casing. The fourth,
`from_lame_header`, decodes the binary "Radio"/"Audiophile" gain fields
LAME appends to the first MP3 frame's Xing/Info header; see the note above
on why this half is unverified.

---

## How to change it

- **A new text-convention caller** (Vorbis comments, a container's own
  metadata list): call `replaygain::from_text_entries` with whatever
  `(key, value)` pairs the caller already extracted — no format-specific
  glue needed, that is the whole point of the shared function.
- **Verifying the LAME-header layout against a live encoder**: needs an
  `ffmpeg` build (or standalone `lame`) that actually runs ReplayGain
  analysis during encode. Until then, `decode_gain_field`'s docs are the one
  place to update if the layout is confirmed or found wrong.
- **A leading (header-first) tag on read**: `locate` only implements the
  trailing case. A caller with a leading tag today has to call
  `ApeTag::parse` directly against a known offset; adding a `find_leading`
  alongside `find_trailing` is the natural extension.

## Configuration

None. Every parse function takes a `vaco_limits::Budget`; the caller chooses
`Limits::permissive()` or `Limits::strict()`.

## Dependencies

`vaco-core` (errors), `vaco-io` (`IoContext`, for `locate::read_trailing`'s
streaming path), `vaco-bitstream` (`ByteReader`), `vaco-limits` (`Budget`),
`bitflags` (`TagFlags`). No `vaco-format-core` dependency: this crate
registers no component and does no container probing.
