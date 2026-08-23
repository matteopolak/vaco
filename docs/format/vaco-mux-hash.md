# `vaco-mux-hash`

Layer 4. Utility checksum muxers: `crc`, `md5`, `hash`, `framecrc`,
`framemd5`, `framehash`, `streamhash` (FM-20, issue #572).

---

## What it is

The differential-testing oracle the rest of this project's containers and
codecs are checked against: dump per-packet or per-file checksums from this
crate and from the reference over the same decoded/copied media, and diff the
text. That makes the *exact byte layout* of every line load-bearing in a way
it is not for any other muxer in this workspace — a stray space or the wrong
CRC variant turns every downstream comparison into noise.

Everything below was captured by probing the installed reference (ffmpeg 8.1,
built `--enable-gpl --enable-libx264 …`, run under `LC_ALL=C`), not recalled —
per D7/D17, this crate's source was never opened. `uncodedframecrc` (an eighth
name under the same issue) is **not implemented** here; see
[§ Why `uncodedframecrc` is out of scope](#why-uncodedframecrc-is-out-of-scope).

## How it works

### Two families

| | `crc` / `md5` / `hash` | `framecrc` / `framemd5` / `framehash` | `streamhash` |
|---|---|---|---|
| Module | `whole` | `frame` | `stream` |
| Output | one line, whole file | header + one line per **packet** | one line per **stream** |
| Scope of each hash | every packet, every stream, in write order | that one packet's payload | that stream's own packets |

### The line formats, verbatim

`framecrc` (`ffmpeg -f lavfi -i testsrc=size=64x64:rate=5:duration=1
-pix_fmt yuv420p -c:v rawvideo -frames:v 2 -f framecrc -`, byte-inspected with
`od -c` — a terminal capture alone hides the padding):

```
#software: Lavf62.12.100
#tb 0: 1/5
#media_type 0: video
#codec_id 0: rawvideo
#dimensions 0: 64x64
#sar 0: 1/1
0,          0,          0,        1,     6144, 0xb907b704
0,          1,          1,        1,     6144, 0x3e18b700
```

Field grammar: `<stream>,<dts>,<pts>,<duration>,<size>, 0x<crc>[, F=0x<flags>]`.
The four numeric fields are right-justified to widths **11, 11, 9, 9**
(`printf("%d,%11lld,%11lld,%8lld,%8d, ", ...)`-shaped; Rust's own integer
padding matches this exactly) and *widen*, never truncate, past that — the
`h264` probe below shows a 20-digit `pts` sitting in an 11-wide field. `size`
is unsigned; every other numeric field is signed. Audio streams replace
`#dimensions`/`#sar` with `#sample_rate <i>: <rate>` and
`#channel_layout_name <i>: <name>` (`mono`, `stereo`, …).

A **missing `pts` prints as the literal decimal `-9223372036854775808`**
(`i64::MIN`, the raw `AV_NOPTS_VALUE`) — **not** the string `N/A` that
`ffprobe` shows for the same packet. Measured on a raw H.264 elementary
stream, which the reference never assigns real timestamps to:

```
$ ffmpeg -f h264 -i t.h264 -c copy -f framecrc -
#extradata 0:       36, 0xa9970a1a
#software: Lavf62.12.100
#tb 0: 1/1200000
#media_type 0: video
#codec_id 0: h264
#dimensions 0: 64x64
#sar 0: 1/1
0,     -96000, -9223372036854775808,    48000,     1641, 0xe1a7ba2e
```

`vaco_core::Timestamp`'s own `Display` prints `N/A` for this case — deliberately
**not** used in `crate::frame`'s line assembly, which reads `.ticks()` and maps
`None` to `i64::MIN` by hand.

`framemd5`/`framehash` share the same four numeric fields and widths, but:

* print three extra header lines **before** `#software`, and one column-header
  comment line after the per-stream block (`od -c`-checked spacing):

  ```
  #format: frame checksums
  #version: 2
  #hash: MD5
  #software: Lavf62.12.100
  ⋮
  #stream#, dts,        pts, duration,     size, hash
  0,          0,          0,        1,     6144, a111606e32508d2d9bb294bed727979e
  ```

* print the digest as **plain lower-case hex, no `0x`**;
* **never** print the `F=` flags field — measured on a real B-frame `.mp4`
  (`libx264 -g 2 -bf 2`), where `framecrc` shows `F=0x0` on every non-key
  packet and `framemd5` on the identical file shows no such field on any line.

### The `F=` field's exact rule (`framecrc` only)

Measured across an all-keyframe stream (MJPEG, AAC — never shows `F=`, ever)
and a stream with real non-key packets (H.264 with B-frames — every non-key
packet shows `F=0x0`, every keyframe shows nothing): the rule is **omit `F=`
exactly when `pkt->flags == AV_PKT_FLAG_KEY`**, not "omit for a keyframe" —
those two readings are indistinguishable until you check an all-keyframe
stream, where the second reading would wrongly predict `F=` never appearing
*because every packet is a keyframe*, and the first correctly predicts the
same absence for the right reason (flags really do equal exactly `KEY`,
nothing else, on every packet).

### The whole-file and per-stream muxers have no header at all

```
$ ffmpeg -f crc -                 →  CRC=0x88956e14
$ ffmpeg -f md5 -                 →  MD5=0c006add1a6bfa412f0f804469a09083
$ ffmpeg -f hash -                →  SHA256=<64 hex chars>   (default, no -hash)
$ ffmpeg -f streamhash -hash crc32 -
0,v,CRC32=e03bd439
1,a,CRC32=f2a6c4ff
```

`streamhash`'s per-line grammar is `<stream index>,<media letter>,<ALGO>=<hex>`
— the media letter is `vaco_core::MediaType::specifier_char` (`v`/`a`/`s`/`d`/`t`,
the same letters `-map`/`-c:v` use), confirmed for `v`/`a`; `s`/`d`/`t` are
inferred by the same convention, not independently probed.

### The CRC-32 finding: `crc`/`framecrc` are secretly Adler-32

This is the fact worth writing down once, because it contradicts the muxer's
own name. Established by elimination, not recall:

1. Extracted the exact bytes ffmpeg hashes (`-f rawvideo` dump of one frame)
   and confirmed with `-f md5` that they are byte-identical to what `crc`/
   `framecrc` see.
2. Ran every catalogued 32-bit CRC variant (`CRC-32/ISO-HDLC`, `BZIP2`,
   `Castagnoli`/`ISCSI`, `MPEG-2`, `POSIX`/`cksum`, `JAMCRC`, `CRC-32Q`,
   `XFER`, `AUTOSAR`, `BASE91-D`, `CD-ROM-EDC`) against those bytes, in every
   `init`/`refin`/`refout`/`xorout` combination the catalogue defines. **None
   matched** `crc`'s `0xd107b705` for a one-frame file, nor `framecrc`'s
   `0xb907b704` for the same frame.
3. `ffmpeg -f hash -hash crc32 -` on the same one frame gives `64c266ad` —
   plain `zlib.crc32`. So the generic `hash` family's `crc32` *is* ordinary
   CRC-32; the dedicated `crc` muxer's number is something else entirely.
4. The `"123456789"` RFC-1950 Adler-32 check value is `0x091E01DE`. The `crc`
   muxer on that exact 9-byte payload (via the `s8` raw-PCM demuxer, to rule
   out any encoder reformatting) prints **exactly** `0x091e01de`. The `crc`
   muxer is Adler-32, standard seed.
5. `framecrc` on the same 9 bytes prints `0x091501dd` — close to, but not, the
   standard Adler-32 value. Adler-32's state is linear in its seed: solving for
   the seed that explains the difference (`Δa = -1, Δb = 0` at length 9) gives
   seed `(a=0, b=0)` exactly, confirmed by direct computation.
6. Decisive cross-check: `ffmpeg -f framehash -hash adler32 -` on the *same*
   two frames that gave `framecrc` its `0xb907b704`/`0x3e18b700` prints
   `d107b705`/`5618b701` — the **standard**-seeded value, not the zero-seeded
   one. This proves the zero seed is specific to the dedicated `framecrc` code
   path, not a property of "Adler-32 hashed one packet at a time" in the
   generic family.
7. Whole-file confirmation: `ffmpeg -f crc -` and `ffmpeg -f hash -hash
   adler32 -` on the same two-frame file both print `88956e14`. The dedicated
   `crc` muxer is `-hash adler32` under the label `CRC=0x...` instead of
   `adler32=...`; only `framecrc`'s *per-packet* variant is a genuinely
   different algorithm from its `-hash` counterpart.

Net: `crc.workspace` (the real CRC-32 crate) backs `-hash crc32` in the
generic family; the dedicated `crc`/`framecrc` pair is a nine-line hand-rolled
Adler-32 (`crate::algo::adler32_seeded`), matching how `vaco-probe`'s
`HashAlg::Adler32` was already justified in this workspace ("nine lines, not
worth a dependency").

### `-hash` algorithm names

Probed by trying candidates against `ffmpeg -f hash -hash <name> -` and
reading the exit code (an unrecognised name fails with the same generic
"incorrect codec parameters" message as every other rejected name, so there is
no enumerated error list to read instead):

| Works | Output label | This crate |
|---|---|---|
| `md5` | `MD5` | `HashAlgo::Md5` |
| `sha160` (**not** `sha1` — that name is refused) | `SHA160` | `HashAlgo::Sha160` |
| `sha224` | `SHA224` | `HashAlgo::Sha224` |
| `sha256` (the reference's own default) | `SHA256` | `HashAlgo::Sha256` |
| `sha384` | `SHA384` | `HashAlgo::Sha384` |
| `sha512` | `SHA512` | `HashAlgo::Sha512` |
| `sha512/224` | `SHA512/224` | `HashAlgo::Sha512_224` |
| `sha512/256` | `SHA512/256` | `HashAlgo::Sha512_256` |
| `crc32` | `CRC32` | `HashAlgo::Crc32` |
| `adler32` | `adler32` (lower case — the one exception) | `HashAlgo::Adler32` |
| `murmur3` | `murmur3` | **not implemented** |
| `ripemd128`/`160`/`256`/`320` | `RIPEMD*` | **not implemented** |

`murmur3` and the four RIPEMD widths have no pre-declared pure-Rust crate in
`[workspace.dependencies]` (only `crc`, `md-5`, `sha1`, `sha2` are), and this
crate was told not to add one. `HashAlgo` simply has no variant for them —
there is no `-hash` string parser in this crate to politely refuse a name
with (see [§ Options](#configuration)), so the gap is structural rather than
a runtime error.

Also measured: `md5` and `framemd5` are not bespoke algorithms — `ffmpeg -h
muxer=md5` lists its own `-hash` option, default `"md5"`. They are the generic
family with a different default, which is why this crate models `md5` /
`framemd5` as constructors on the same [`WholeHashMuxer`] / [`FrameHashMuxer`]
types rather than separate ones.

### `#extradata` header line

`framecrc`/`framemd5`/`framehash` print `#extradata <i>: <len>, 0x<hash>` when
a stream has extradata (measured on an H.264 stream: `#extradata 0: 36,
0xa9970a1a`). **This crate does not currently emit that line at all** — probing
its exact hash convention needs the raw extradata bytes, and this build's
`ffprobe` did not expose them through any field tried (`-show_entries
stream=extradata` came back empty for the container tested). Left out rather
than guessed; a future pass with a way to read `ffprobe`'s raw extradata bytes
can add it.

## How to change it

* **Line assembly** lives in `crate::frame::FrameHashMuxer::write_packet`
  (per-packet) and `crate::header::write_common_header` (the shared `#`
  block). If a new measurement disagrees with a width or a separator here,
  fix the format string and the fixture-backed test in the same module —
  they're both right next to it.
* **Algorithms** live in `crate::algo`. Add a `HashAlgo` variant, a
  `RunningHash` arm, and a `digest_hex` arm together; the
  `digest_hex_and_running_agree` test in that module will catch a mismatch
  between the one-shot and incremental paths for any algorithm you add.
* **Per-stream time base** (`crate::header::resolve_time_base`) is this
  crate's own opinion, not the reference's literal `st->time_base`, because
  [`vaco_format_core::Muxer::add_stream`] never receives one — see that
  function's doc comment for why, and why every registration's
  `stream_time_base` override always agrees with what it prints.

### Regenerating the fixtures

`tests/fixtures/testsrc_64x64_frame{0,1}.yuv` are the first two raw frames of:

```sh
ffmpeg -f lavfi -i testsrc=size=64x64:rate=5:duration=1 -pix_fmt yuv420p \
  -c:v rawvideo -frames:v 2 -f rawvideo two_frames.raw
# then split at the 6144-byte boundary (64*64 + 2*32*32, yuv420p)
```

Verify a fixture hasn't drifted with `md5sum` against `ffmpeg -frames:v 1 -f
md5 -` on the same command — that's how these two were captured and checked
originally.

## Configuration

None. There is no CLI-facing options channel in this crate:
[`vaco_format_core::MuxerDesc::open`] takes only a sink (`fn(Box<dyn
MediaSink>) -> Result<Box<dyn Muxer>>`), so a registration's algorithm choice
(e.g. `hash`'s SHA-256 default) is fixed at the `MuxerDesc` level. A caller
that wants a different `-hash` algorithm than a registration's default
constructs `WholeHashMuxer::hash`, `FrameHashMuxer::framehash`, or
`StreamHashMuxer::new` directly with the `HashAlgo` it wants, bypassing the
registry descriptor. Wiring an actual `-hash <name>` CLI flag through to one
of these constructors is a future CLI-layer concern, not this crate's.

## Dependencies

* `vaco-core`, `vaco-io`, `vaco-limits`, `vaco-packet`, `vaco-format-core`,
  `vaco-codec-core` — the same baseline every sibling mux crate takes.
* `crc`, `md-5`, `sha1`, `sha2` (all pre-declared in
  `[workspace.dependencies]`, D10 gate 1) — every algorithm in `HashAlgo`
  except Adler-32, which is nine lines hand-written in `crate::algo` rather
  than a fifth dependency for a nine-line algorithm.
* No `vaco-codec-*`/`vaco-parse-*` dependency (D14.1): this crate only ever
  sees already-produced packets and their declared `CodecParameters`.

## Why `uncodedframecrc` is out of scope

`uncodedframecrc` hashes *decoded frames*, and its line carries per-frame
geometry (width/height/pixel format for video; sample format/channel
layout/sample count for audio) that in the reference is read off the
`AVFrame` for that specific frame. [`vaco_format_core::Muxer::write_packet`]
receives one [`vaco_packet::Packet`] and whatever
[`vaco_codec_core::CodecParameters`] were frozen at `add_stream` — there is no
per-call frame geometry, and nothing in the trait guarantees a packet's bytes
are a stride-free, tightly-packed plane the way an `AVFrame` filled a raw
encoder's packet in the reference (a scaled or padded frame's rows can be
wider than its active pixels). Implementing this honestly needs either a
frame-level hook the `Muxer` trait does not have, or a documented assumption
this crate is not positioned to bless on the trait's behalf mid-wave. Per the
brief for issue #572, this crate implements the seven packet muxers and
leaves this one open rather than changing a frozen interface or guessing.
