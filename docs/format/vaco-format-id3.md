# `vaco-format-id3`

Layer 4. ID3v1/ID3v1.1 and ID3v2 (2.2, 2.3, 2.4) tag parsing.

This is **not** a demuxer and registers no component — `vaco-demux-mp3` (and
any other raw-stream demuxer carrying ID3 tags) calls into this crate, the
way a container demuxer calls into `vaco-format-riff` or `vaco-format-isom`.

**Note on the work package.** The GitHub issue this crate was built against
(`SH-06`) describes it as wrapping the `id3` crate behind the D11 adapter
boundary. The brief given to build it said the opposite — implement from the
published ID3v2.3.0/2.4.0 specifications and ID3v1's layout, with no `id3`
dependency offered — and that is what this crate does: a clean-room parser,
no external ID3 crate anywhere in its dependency graph. Flagging the
discrepancy here rather than silently picking one.

---

## What it is

| Module | Contents |
|---|---|
| `header` | the ten-byte ID3v2 header and footer, and their flags |
| `synchsafe` | the 7-bit-per-byte size encoding, and where it does and does not apply |
| `unsync` | undoing ID3v2 unsynchronisation |
| `frame_header` | per-version frame headers (v2.2's 6-byte form; v2.3/v2.4's 10-byte form) and frame flags |
| `encoding` | the four text encodings and null-terminated string reading |
| `frames` | frame content decoding and the frame-ID → metadata-key table |
| `tag` | assembling a whole tag: header, extended header, unsynchronisation, the frame walk |
| `id3v1` | the 128-byte ID3v1/ID3v1.1 tag, and its 192-entry genre table |
| `skip` | skipping a tag at the start of a stream, for probing past it |

Written from the ID3v2.3.0 and ID3v2.4.0 informal standards (id3.org) and
ID3v1's layout. No FFmpeg source was consulted (D7/D15); every codec-name and
metadata-key mapping below came from running `ffmpeg`/`ffprobe` 8.1, with the
command recorded next to it.

---

## How it works

### The two size encodings, and why the distinction matters

The ID3v2 **header** size is always synchsafe (7 usable bits per byte), in
every version. The **frame** size is not: ID3v2.3.0 frame sizes are a plain
32-bit big-endian integer, while ID3v2.4.0 frame sizes are synchsafe too.
Probed directly — a 210-byte `TXXX` frame's size field is `00 00 00 D2` under
`-id3v2_version 3` (byte `0xD2` has its top bit set, illegal in synchsafe,
proving plain binary) and `00 00 01 52` under `-id3v2_version 4` (a genuine
synchsafe encoding of 210, not the plain-binary form). `crate::synchsafe`
carries this proof in its doc comment with the exact command; `FrameHeaderV34::parse`
takes the major version specifically so it can pick the right decode.

### Unsynchronisation

Removing it is one rule regardless of why a byte pair was escaped: every
`$00` immediately following an `$FF` is dropped (`unsync::remove`). It
applies at two scopes — the whole tag (ID3v2.3.0/2.4.0's tag-level flag,
applied before frame headers are even read, since headers inside an
unsynchronised tag are unsynchronised too) and, in ID3v2.4.0 only, a single
frame independently. `tag::Id3v2Tag::parse` applies both where their
respective flags say to; applying the whole-tag pass first and then finding
nothing left to do for a frame's own flag is a safe no-op, not a bug.

### Frame flags: compression and encryption are detected, not decoded

`Id3FrameFlags` (a `bitflags` set, one shape for both versions despite the two
specs putting the bits in different positions) is decoded from the two raw
flag bytes. A frame declaring `COMPRESSION` or `ENCRYPTION` is returned as
`Frame::Unsupported` rather than guessed at — see *Deferred* below for why.
`GROUPING` (skip one byte) and, in v2.4, `DATA_LENGTH_INDICATOR` (skip four
bytes) are both handled, since both are just "skip N known bytes before the
real content starts".

### The extended header

Detected and skipped, not interpreted — nothing in this crate needs anything
inside it. The two versions disagree on the size field's shape (ID3v2.3.0's
excludes itself and is plain binary; ID3v2.4.0's includes itself and is
synchsafe), which `tag::extended_header_len` handles. This is the one piece
of this crate read from the specification rather than confirmed against
`ffmpeg`, because `ffmpeg` does not write an extended header under any option
found — see *Deferred*.

### Text encoding

Four encodings (`encoding::Encoding`), decoded to `String` with no failure
mode: ISO-8859-1 maps every byte to a valid code point by construction, and
invalid UTF-16/UTF-8 sequences are replaced with U+FFFD. `read_terminated`
splits a field at its encoding's null terminator (1 byte for
Latin-1/UTF-8, 2 for either UTF-16 form, scanned on a two-byte boundary so an
odd trailing byte can never panic looking for one).

### Frame content and the metadata-key table

`frames::decode` turns one frame's body into a `Frame`: plain text, `TXXX`'s
`{description, value}` pair, `COMM`'s `{language, description, text}`, or
`APIC`'s `Picture`. `frames::metadata_key` is the frame-ID → `ffprobe`
`TAG:<key>` table; `tag::Id3v2Tag::parse` uses it to build `entries`, with
`TXXX` contributing its own description as the key instead of a fixed one —
see the *Fidelity* table below for every mapping and the command that
confirmed it.

### `APIC` becomes a picture, not a text tag

Probed directly: an `APIC` frame surfaces in the reference as a second,
`attached_pic`-disposition video stream, not a `TAG:` entry. That is a
demuxer-level decision (which stream, which disposition flag) this crate does
not make — `Id3v2Tag::pictures` hands back the decoded `mime_type`,
`picture_type`, `description` and raw image bytes, and the demuxer decides
what to do with them.

### ID3v1 and its genre table

`id3v1::Id3v1Tag::parse` reads the fixed 128-byte layout and detects the
ID3v1.1 track-number convention (`comment[28] == 0`) the way every real
reader does — there is no way to distinguish it from a genuine 30-byte
comment ending in two nulls, and every implementation resolves the ambiguity
identically, so this crate does too. The 192-entry genre table
(`id3v1::ID3V1_GENRES`, via `id3v1_genres`) was obtained by **direct
measurement**: a synthetic ID3v1 tag was built for every byte value `0..=255`
and read back with `ffprobe -show_entries format_tags=genre`. Values `192..=255`
produce no `genre` tag at all in the reference (confirmed at 200 and at 255,
the conventional "unspecified" sentinel) — `genre_name` returns `None` for
that whole range rather than a guess, and the table itself only needs 192
entries.

### Skipping a tag at the start of a stream

`skip::detect`/`skip::skip` peek the ten-byte header (via
`vaco_io::IoContext::peek`, which works on a forward-only pipe) and report
how far to skip — the header's `total_len`, including the footer if the tag
declares one. This is what an MP3 (or any other raw elementary stream) demuxer
needs before it can probe or decode: an ID3v2 tag sits in front of the actual
codec data, and probing the tag's own bytes as if they were audio would
misidentify or fail outright.

---

## Fidelity: what was measured against `ffprobe` 8.1, and what was not

Every metadata key below is the exact command used:
`ffmpeg -f lavfi -i sine=... -metadata <key>=<value> -id3v2_version 3 -c:a
mp3 out.mp3`, then `ffprobe -show_entries format_tags`.

| Frame | Key | v2.2 alias | How confirmed |
|---|---|---|---|
| `TIT2` | `title` | `TT2` | probed |
| `TPE1` | `artist` | `TP1` | probed |
| `TALB` | `album` | `TAL` | probed |
| `TYER` | `date` | `TYE` | probed |
| `TDRC` | `date` | — | spec equivalence (ID3v2.4.0 folds `TYER`/`TDAT`/`TIME` into `TDRC`); not independently probed |
| `TRCK` | `track` | `TRK` | probed |
| `TCON` | `genre` | `TCO` | probed |
| `TPE2` | `album_artist` | `TP2` | probed |
| `TCOM` | `composer` | `TCM` | probed |
| `TPOS` | `disc` | `TPA` | probed |
| `TPE3` | `performer` | `TP3` | probed (`-metadata performer=` maps to `TPE3`, not `TPE1`) |
| `TSSE` | `encoder` | — | probed (written automatically by the encoder, not via `-metadata`) |
| `COMM` (empty description) | `comment` | `COM` | probed |
| `TXXX` | *(its own description)* | `TXX` | probed — **not** `COMM`: `-metadata comment=` is written by `ffmpeg`'s own mp3 muxer as a `TXXX` frame with description `"comment"`, and a genuine `COMM` frame with an empty description separately confirmed to also read back as `comment` |

ID3v1 genre table: all 192 entries in `id3v1::ID3V1_GENRES` were obtained by
probing every byte value `0..=255` directly (see `id3v1` module docs) — this
is the one table in this crate covering its **entire** domain by
measurement rather than a documented subset, because doing so was cheap
(a script, not eighty manual encodes) and the risk of transcribing a
192-entry list from memory was exactly what plan 13 §1b warns against.

**Not verified against the reference — deliberately scoped out:**

- **Frame types.** `USLT`/synchronised lyrics, play counters, private
  frames, URL frames, and the rest of the ID3v2.4.0 §4 list beyond the table
  above. Add one the same way: a probed key mapping and a test that pins it.
- **Compressed and encrypted frames.** Detected (`Id3FrameFlags::COMPRESSION`/
  `ENCRYPTION`) and returned as `Frame::Unsupported`, never decoded. zlib
  inflation of attacker-controlled, budget-unaware input is its own hazard
  (a compression-bomb frame is a smaller version of the same "declared size,
  unknown truth" problem `vaco-limits` exists for), and ID3v2 frame
  encryption requires a key the format itself does not carry.
- **v2.2's `PIC` frame.** Structurally different from `APIC` (a 3-byte image
  *format* code instead of a MIME string) and rare enough today that
  decoding it was not worth the second code path; returned as
  `Frame::Unsupported`.
- **The extended header's exact byte layout.** Read from the published
  specification, not probed — `ffmpeg` does not write one under any option
  found. See `tag::extended_header_len`'s doc comment for the two versions'
  documented disagreement (whether the size field counts itself).
- **Multi-value ID3v2.4 text frames.** ID3v2.4.0 allows a `T***` frame to
  carry several values separated by `$00`; this crate decodes the whole
  field as one string, matching the common case and keeping scope down.

---

## How to change it

- **Add a frame mapping**: one match arm in `frames::metadata_key` (or a
  dedicated decoder alongside `decode_comm`/`decode_txxx`/`decode_apic` if
  the frame's layout is not "encoding byte + text"), backed by the probe
  command from the *Fidelity* table's format and a unit test.
- **Decode compressed frames**: would need a `Budget`-aware, bounded zlib
  inflate (a growing-buffer discipline like `vaco_limits::IncrementalVec`,
  not `miniz_oxide::inflate::decompress_to_vec` called on the whole frame at
  once) — flagged here rather than attempted, since getting the bound wrong
  recreates exactly the amplification bug this crate's other buffers are
  built to avoid.
- **Re-derive the ID3v1 genre table**: rerun the probe script described in
  `id3v1_genres`'s doc comment against whatever reference version is pinned.

## Configuration

None as flags, but every function that copies input-derived data —
`unsync::remove`, `tag::Id3v2Tag::parse` (unsynchronisation and picture
bytes) — takes a `vaco_limits::Budget`; the caller chooses
`Limits::permissive()` or `Limits::strict()`.

## Dependencies

`vaco-core` (errors), `vaco-io` (`IoContext::peek`/`skip`, for `skip`),
`vaco-bitstream` (`ByteReader`), `vaco-limits` (`Budget`), `bitflags` (frame
and header flag sets). No `vaco-codec-core` dependency: unlike
`vaco-format-riff`, ID3 metadata does not map to a `CodecId` — it is
container-level tag data, not codec identity.
