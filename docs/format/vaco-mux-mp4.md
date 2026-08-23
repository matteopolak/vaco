# `vaco-mux-mp4`

Layer 4. The MP4/MOV muxer: `ftyp`/`moov`/`mdat`, per-track sample tables,
chunked interleave, the trailer rewrite, `-movflags faststart`, fragmented
output (`moof`/`traf`/`tfhd`/`tfdt`/`trun`), `sidx`, `mfra`, iTunes-style
metadata (`udta ▸ meta ▸ ilst`, cover art, Nero chapters), and the
brand-variant containers (`mov`, `ipod`, `ismv`, `f4v`, `psp`, `3gp`, `3g2`).

Box *bytes* are not defined here. `vaco-format-isom::writer` owns every box
layout this crate emits — this crate decides *when* to write what: chunk
boundaries, fragment boundaries, table compression, the faststart rewrite.
Verified against its sibling reader directly: this crate's own tests mux a
file with `MovMuxer` and read it back with `vaco-demux-mp4`, rather than
hand-decoding bytes a second time to check them.

---

## What it is

One crate, eight registry names, one implementation:

| File | Contents |
|---|---|
| `mux.rs` | `MovMuxer`, the `vaco_format_core::Muxer` impl; dispatches to `progressive` or `fragmented` |
| `options.rs` | `MovFlags` (`-movflags`), `MuxOptions`, `Brand`, `ChapterMark`, `CoverArt` |
| `entry.rs` | `CodecParameters` → one `stsd` sample entry |
| `track.rs` | `TrackState`: per-track sample accumulation, and the `stts`/`ctts`/`stsc` run compression |
| `progressive.rs` | non-fragmented `write_header`/`write_packet`/`finish`, including `faststart` |
| `fragmented.rs` | `moof`/`mdat` fragment emission, the `movflags` fragmentation policy, `mfra`, buffered `sidx` |
| `meta.rs` | `udta`/`meta`/`ilst`, cover art, Nero `chpl` chapters, `tref` |
| `brand.rs` | `ftyp` brand/compatible-brand tables per profile, and the `MuxerDesc` registry entries |

---

## How it works

### Two very different write orders, one `Track` model

`TrackState` (in `track.rs`) is the same for both modes: `track_id`, the built
`stsd` entry, and — for progressive muxing — the accumulated `SampleRecord`s
and `ChunkRecord`s. Fragmented muxing does not use the accumulation fields;
`fragmented::FragmentedState` keeps its own per-fragment pending-sample lists,
because a fragment's samples are cleared the instant it is written and never
need `stts`/`stsc` run compression at all — `trun` states them individually.

### Progressive: mdat first, offsets never move

By default `mdat` is written **immediately after `ftyp`**, directly to the
sink, using a 16-byte `largesize` header (`size==1`, `"mdat"`, an 8-byte real
size) so the header's own length never has to change. Every sample's absolute
file offset is therefore known the instant it is written — nothing is ever
shifted. `moov` is built afterward, at `finish`, and appended: the same
"`moov` at the end" shape `ffmpeg 8.1`'s own default `mov` muxer produces
(measured: `ffmpeg -f lavfi -i testsrc=d=1 -c:v mpeg4 out.mp4`'s second
top-level box is `mdat`).

**Chunking** falls out of one invariant: only one thing ever writes to the
sink sequentially, so consecutive samples from the same track are always
byte-contiguous. `progressive::write_sample` tracks one `open_chunk`; a
track change closes it (pushing a `ChunkRecord` onto that track) and opens a
new one. No separate chunk-boundary policy exists — the interleave order
`vaco_format_core::mux::MuxWriter` already produces *is* the chunk boundary.

**`faststart`** needs `mdat`'s bytes to exist before `moov`'s chunk offsets can
be computed, and this crate's sink (`vaco_io::MediaSink`) cannot be read back
from — there is no way to "move" already-written bytes without a working read
side. So under `faststart` every sample's payload is buffered in memory
(`ProgressiveState::mdat_buf`) instead of being written to the sink as it
arrives, and `finish` writes the whole file — `ftyp` (already on the sink),
then `moov`, then the `mdat` header, then the buffer — once, in final order.
**This is a real memory cost**: a `faststart` mux holds every sample's payload
in RAM until `write_trailer`. There is no partial-buffering middle ground
without a sink that supports reading back what it wrote.

The `moov` byte length depends on the chunk offsets it carries, and the
offsets depend on `moov`'s own length plus `ftyp`'s — a fixed point.
`finish_faststart` resolves it exactly like any two-pass writer: build `moov`
assuming a trial prefix length; if the built length does not match the trial,
rebuild with the length just produced. Switching a track's `stco` to `co64`
only ever pushes offsets *up*, never back below the threshold that required
`co64` in the first place, so this converges within two passes in practice
(`MAX_FASTSTART_PASSES` bounds it at eight, generously). **This is the part
the brief called out as easy to get subtly wrong** — an earlier version of
this code computed the shift as `moov_len + 16` and silently dropped `ftyp`'s
length, which a `proptest` over arbitrary sample-size sequences caught
immediately (`tests/roundtrip.rs`'s
`faststart_offsets_are_exact_for_arbitrary_sample_shapes`).

### Fragmented: `movflags`-driven boundaries, `moof`-relative addressing

A fragment boundary is checked on every packet (`fragmented::should_flush`),
in this order: `frag_every_frame` (always), `frag_keyframe` (a sync sample on
the first-added track), `frag_duration` (elapsed DTS on the first track past
the threshold), `frag_size` (accumulated bytes past the threshold). A file
with none of these set still produces one giant final fragment at `finish`.

`default_base_moof` (and `dash`/`cmaf`, which imply it) sets
`tfhd.default-base-is-moof` and gives `trun.data_offset` relative to the
enclosing `moof` — which is what makes the buffered `sidx` path below safe:
nothing about a moof-relative fragment depends on its *absolute* file
position, so inserting `sidx` between `moov` and the first fragment shifts
every fragment uniformly and nothing has to be rebuilt. Without
`default_base_moof`, every `tfhd` states an explicit `base_data_offset` — one
`traf` at a time, always stated (a strict subset of what §8.8.7.1 permits,
chosen because it is one shape instead of two).

`separate_moof` emits one `moof`+`mdat` pair per track per fragment interval
instead of one `moof` with several `traf`s.

`dash`/`cmaf` buffer the *fragment stream* (not the header) in memory so a
`sidx` covering the whole file can be written right after `moov`, before the
first fragment — the same faststart-style tradeoff, scoped to fragmented
output. `mfra`'s `tfra.moof_offset` is recorded relative to the start of the
fragment stream while `sidx`'s final length is still unknown, then corrected
by exactly that length once `sidx` is built (`fragmented::finish`).

### What is simplified

* **One `tfra` entry per track per fragment**, pointing at that fragment's
  first sample when it is a sync sample. Correct whenever a fragment starts on
  one (always true under `frag_keyframe`, or a single-fragment file);
  approximate otherwise.
* **`sidx` is one presentation-timeline index for the whole file**, one
  reference per fragment — not DASH's multi-`Representation` manifest story,
  which is a packaging concern above a single container muxer.
* **CMAF conformance is not attempted** beyond the flag combination: chunk-level
  `styp` alignment and CMAF's stricter brand/profile rules are out of scope.

### Metadata

`udta ▸ meta ▸ ilst` at the *movie* level (not per track) — `MuxOptions::tags`
(a `(FourCc, String)` list), `MuxOptions::cover_art` (`covr`, JPEG or PNG).
Chapters are written Nero-style (`udta ▸ chpl`), which is what `ffmpeg 8.1`'s
own `mov` muxer does by default; `meta::build_chapter_tref` exists as a
primitive for a caller building an actual `QuickTime` chapter *track*, but
`MovMuxer` does not synthesize one itself. Verified round-trip against
`vaco-demux-mp4`'s own `Demuxer::metadata()`.

### Codec support

Video: H.264 (`avcC`/`avc1`), HEVC (`hvcC`/`hev1`), AV1 (`av1C`/`av01`),
VP8/VP9 (`vpcC`/`vp08`/`vp09`), MJPEG and PNG (`esds` object types `0x6C`/
`0x6D` inside an `mp4v` entry, mirroring `vaco-format-isom::esds`'s own
`object_type_codec` table so the round trip names the same codec back).
Audio: AAC (`esds`/`mp4a`), Opus (`dOps`/`Opus`), FLAC (`dfLa`/`fLaC`), MP3
(`.mp3`, no config box — self-describing per frame). Every config record is
`CodecParameters::extradata` written verbatim; see
`docs/format/vaco-format-isom.md`'s *Writers* section for why that boundary
exists (D14.1).

### Brand variants

Not specified anywhere reachable — measured, `ffmpeg 8.1`,
`-fflags +bitexact` **before** the output (`AGENT-CONSTRAINTS.md`'s position
trap: before the *input* would land on a demuxer that does not exist here):

```sh
ffmpeg -y -f lavfi -i testsrc=d=1:size=64x64:rate=5 -c:v mpeg4 -f mp4  out.mp4
ffmpeg -y -f lavfi -i testsrc=d=1:size=64x64:rate=5 -c:v mpeg4 -f mov  out.mov
ffmpeg -y -f lavfi -i testsrc=d=1:size=64x64:rate=5 -c:v mpeg4 -f ipod out.m4v
ffmpeg -y -f lavfi -i testsrc=d=1:size=64x64:rate=5 -f lavfi -i sine=d=1 \
       -c:v mpeg4 -c:a aac -f psp out.mp4
ffmpeg -y -f lavfi -i testsrc=d=1:size=64x64:rate=5 -c:v mpeg4 -f 3gp  out.3gp
ffmpeg -y -f lavfi -i testsrc=d=1:size=64x64:rate=5 -c:v mpeg4 -f 3g2  out.3g2
ffmpeg -y -f lavfi -i testsrc=d=1:size=64x64:rate=5 -c:v libx264 -f ismv out.ismv
ffmpeg -y -f lavfi -i testsrc=d=1:size=64x64:rate=5 -f lavfi -i sine=d=1 \
       -c:v libx264 -c:a aac -f f4v out.f4v
```

| Profile | `major_brand` | `minor_version` | `compatible_brands` |
|---|---|---|---|
| `mp4` | `isom` | `0x200` | `isom iso2 mp41` |
| `mov` | `qt  ` | `0x200` | `qt  ` |
| `ipod` | `M4V ` | `0x200` | `M4V  isom iso2` |
| `ismv` | `isml` | `0x200` | `isml piff` |
| `f4v` | `f4v ` | `0x200` | `f4v  isom iso2 avc1` |
| `psp` | `MSNV` | `0x200` | `MSNV isom iso2` |
| `3gp` | `3gp4` | `0x200` | `3gp4 isom iso2` |
| `3g2` | `3g2a` | `0x10000` | `3g2a isom iso2` |
| `avif` | `avif` | `0` | `avif mif1 miaf MA1B` |

`avif`'s brand is recorded (`brand::AVIF`) but not registered — an AVIF file
is a HEIF item structure (`meta ▸ iinf/iloc/iprp/ipco/pitm`), not a
`moov`/`trak` track, and building that is a different box model than the rest
of this crate. `MUXER_AVIF` exists in code and always returns `Unsupported`.

---

## How to change it

### Adding a codec

`entry.rs`'s `build_video`/`build_audio` match arms, plus `mux.rs`'s
`SUPPORTED_VIDEO`/`SUPPORTED_AUDIO` (checked at `add_stream`, per M15). If the
codec needs a config box `vaco-format-isom::writer` does not have yet, add the
writer there first (it is a one-line `bx`/`fullbx` wrapper for anything whose
record is opaque bytes) — see that crate's *Writers* section.

### Adding a `movflags` bit

`options.rs`'s `MovFlags`, then wire it into `MuxOptions::effective_flags`
(implications), `MuxOptions::validate` (conflicts) and whichever of
`progressive.rs`/`fragmented.rs` acts on it. `fragmented::should_flush` is
where a new fragmentation *trigger* goes; `build_traf`/`build_combined_fragment`
is where a new *addressing* mode goes.

### Adding a brand variant

`brand.rs`: a `BrandSpec` constant, a `Brand` enum variant, an `open_*`
closure, and a `MuxerDesc` constant — then a `vaco-component.toml` row and
`cargo xtask gen-registry`. Get the brand bytes from `ffmpeg -fflags
+bitexact -f lavfi ... -f <name> out.ext` and read the first 32 bytes; do not
guess them from the spec, because nothing in the spec assigns them.

### The two-pass fixed point (faststart, and `moof`-relative fragments)

Both `progressive::finish_faststart` and `fragmented::build_combined_fragment`
solve the same shape of problem: a box's byte length depends on values that
depend on the box's own length. The pattern is always: build once with a
trial length, compare the result's actual length to the trial, and either
stop (converged) or retry with the length just produced. It converges because
growing the box (switching a table to a wider form) can only push the values
it carries *up*, never back across the threshold that required the wider form.
If you add a table whose width can shrink as values grow, this pattern breaks
and needs a different argument.

### Gotchas

* **`Vec::with_capacity` is denied** (`clippy::disallowed_methods`) — this
  crate's per-track/per-fragment buffers grow with `push`/`extend_from_slice`
  instead. Not a performance concern at these sizes.
* **`Muxer::init` must run before `write_header`.** `MuxBuilder::open` always
  does this; a test or caller driving the `Muxer` trait directly and skipping
  it will find `fragmented::FragmentedState` sized for zero tracks — samples
  silently vanish (`buffer_sample` finds no slot and no-ops) rather than
  erroring. `fragmented::write_header` now defends against this by resizing on
  the way in, but `init` is still the contract; do not rely on the defense.
* **`ChunkOffsets::offset` and `stsc`'s `first_chunk` are one-based.**
  `vaco-format-isom::stbl` reads them that way; `TrackState::stsc_runs`
  produces one-based chunk numbers to match.

---

## Configuration

Not routed through `vaco_format_core::FormatOptions` — `movflags` and
everything else here is MP4-specific in the same way `AviMuxer`'s and
`OggMuxer`'s own construction arguments are. `MovMuxer::with_options(sink,
MuxOptions)` is the entry point for anything beyond the registry's default
(`MovMuxer::new`, which the `MuxerDesc::open` closures in `brand.rs` call with
`MuxOptions::default()` except for `brand` itself).

| Field | Effect |
|---|---|
| `brand` | which `ftyp` profile (`brand.rs`) |
| `movflags` | `MovFlags` — see *How it works* |
| `frag_duration` / `frag_size` | fragmentation thresholds |
| `creation_time_unix` | stamped into `mvhd`/`tkhd`/`mdhd`; `None` writes `0`, matching `ffmpeg 8.1` absent `-metadata creation_time` |
| `bitexact` | suppresses `creation_time_unix` even when set |
| `tags` / `cover_art` / `chapters` | `udta` contents |

---

## Dependencies

`vaco-format-isom` (box writers), `vaco-format-core` (`Muxer`, `MuxerDesc`,
`mux::{BitstreamAction, CodecSupport, global_header_action}`), `vaco-codec-core`
(`CodecParameters`), `vaco-io` (`IoWriter`, `MediaSink`), `vaco-packet`,
`vaco-core`. Dev-only: `vaco-demux-mp4` (this crate's tests round-trip through
it), `proptest`.

---

## Tests and fuzzing

`src/track.rs` has unit tests for the `stts`/`ctts`/`stss`/`stsc` run
compression. `tests/roundtrip.rs` drives `MovMuxer` directly through the
`Muxer` trait (bypassing `MuxBuilder`, so this crate's own logic is what is
under test) and reads the result back with `vaco-demux-mp4`: progressive,
faststart (plus the byte-order check that `moov` precedes `mdat`),
fragmented, `separate_moof`, `dash` (checked for a `sidx` box), two
interleaved tracks, and the iTunes-tag round trip. One `proptest` — arbitrary
sample-size sequences through `faststart`, checking every payload byte comes
back unchanged — found the `ftyp`-length omission described above.

`fuzz/fuzz_targets/mp4_mux.rs` builds arbitrary packets (sizes, key flags,
DTS deltas) from fuzz bytes, muxes them through a mode also chosen from the
input (progressive, faststart, `frag_keyframe`, `frag_every_frame`,
`separate_moof`, `dash`), then demuxes the result with `vaco-demux-mp4`,
asserting neither side panics or loops. 30-second run: `exit=0`,
`execs≈66,600` (numbers vary run to run; re-run with
`cargo +nightly fuzz run mp4_mux --features mux-mp4 -- -max_total_time=30`),
`find fuzz/artifacts -type f` empty.

---

## Deferred

* **Common Encryption** (`pssh`/`saiz`/`saio`/`senc`/`tenc`). Not implemented;
  `query_codec`/`add_stream` do not gate on it because nothing here asks for
  it yet — a caller cannot currently request CENC output at all.
* **PCM and AC-3/E-AC-3** have no sample-entry mapping (`entry.rs`).
* **`QuickTime` chapter tracks** (a real `text`/`tx3g` track plus `tref ▸
  chap`) — only Nero `chpl` chapters are synthesized automatically.
* **`tfra` exactness** beyond "first sample of a fragment that starts on a
  sync sample" — see *What is simplified* above.
* **`sidx` as a true multi-segment DASH index** — one whole-file index only.
* **AVIF** — brand bytes only; no HEIF item writer.
