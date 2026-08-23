# `vaco-demux-image2`

Layer 4. `image2` (pattern/glob/sequence file demuxer) and its 37 `*_pipe`
splitters. FM-35a, issue #592. Companion crate: `vaco-mux-image2` (the write
side, FM-35b, issue #593).

---

## What it is

Two different jobs the reference groups under one family name:

* **`image2`** — a filename *pattern* (`out%03d.png`, `out*.png`) resolved
  against the filesystem, one whole file per packet.
* **37 `*_pipe` splitters** (`png_pipe`, `jpeg_pipe`, `bmp_pipe`, …) — find
  image boundaries in a *byte stream* that may hold several images back to
  back (`cat *.png | ffmpeg -f image2pipe ... | ffmpeg -f png_pipe -i -`).
  This is framing, not decoding, per D14.1: none of it names a codec crate.

**Not registered:** `image2pipe` (a content-sniffing dispatcher over the 37
splitters, not a splitter of its own — left out for time, not forgotten; see
the issue-closing comment) and `yuv4mpegpipe` (registered by
`vaco-demux-raw`, matching the reference's own `img2dec.c`/`yuv4mpegdec.c`
module boundary).

## The splitter count: 37, not 42

`planning/20-roadmap.md` says 42. Measured directly:

```sh
ffmpeg -demuxers | awk '{print $2}' | grep -E '_pipe$'
```

against ffmpeg 8.1 lists exactly **37** names. `image2pipe` and
`yuv4mpegpipe` also end in `pipe` but are not per-codec splitters (see
above), so they are correctly excluded from that count, not missed.

## How it works

### `image2`: the registry seam does not fit this format

`DemuxerDesc::open` is `fn(Box<dyn MediaSource>, &dyn ParserProvider) ->
Result<Box<dyn Demuxer>>` — one already-open source, no filename. `image2`'s
entire job is opening *many* files by a name pattern that has nowhere to go
through that signature at all (worse than `vaco-demux-raw`'s "the registry
gets reference defaults for `-pixel_format`" gap — there, at least a *path*
exists; here there is no `MediaSource::path()` to read one from).

So there are two ways in:

* `Image2Demuxer::open_pattern(pattern, Image2Options)` — the real thing, for
  a caller that has both (an embedder, the eventual CLI layer, this crate's
  own tests).
* `DEMUXER_IMAGE2`'s registry `open` — receives one already-open source and
  treats it as `-pattern_type none` pointed at an already-resolved file: the
  whole source is one packet, second `read_packet` is `Eof`. Correct for
  "one source, no pattern," not a fallback approximation of the real thing.

**Fixing the gap properly** would mean adding a `fn path(&self) ->
Option<&str>` (or similar) to `vaco_io::MediaSource`/`ProbeData`, so a
demuxer whose whole job is opening more files by name has something to read
the pattern from. That is a `vaco-io`/`vaco-format-core` change, outside this
crate's scope — reported here rather than worked around.

### `image2`'s options, measured via `ffmpeg -h demuxer=image2`

| Option | Default | Notes |
|---|---|---|
| `-pattern_type` | unnamed value `4` | See "What was wrong in the brief" below. |
| `-start_number` | `0` | First index tried. |
| `-start_number_range` | `5` | Search window: `[start_number, start_number + range - 1]`. |
| `-framerate` | `25` | PTS stride when `-ts_from_file none`. |
| `-loop` | `false` | Restart the sequence/glob list at the end instead of `Eof`. |
| `-ts_from_file` | `none` | `sec`/`ns`: PTS from the file's mtime instead of a counter. |
| `-pixel_format`, `-video_size`, `-export_path_metadata` | — | Not consumed by this crate; see "Not implemented" below. |

**Sequence mode** (`SequencePattern` in `pattern.rs`) parses `%d`/`%0Nd` and
one `%%` escape, no filesystem access. `find_sequence_start`
(`fsutil.rs`) tries `[start_number, start_number + start_number_range - 1]`
in order and returns the first index whose file exists; once running, each
subsequent frame requires the *immediately next* index to exist — a gap ends
the sequence, matching the simplest reading of what `-start_number_range`
is documented to bound (only the *first* index's search).

Measured error, `ffmpeg -start_number 5 -i 'out%03d.png'` against files
starting at `out010.png`:

```
Could find no file or sequence with path 'out%03d.png' and index in the range 5-9
```

reproduced verbatim (`fsutil::find_sequence_start`), including the exit
path: the reference reports it via `Error opening input: No such file or
directory` and exits `254`; this crate reports the same condition as
`Error::Io(ErrorKind::NotFound)` with that exact message.

**Glob mode** (`glob.rs` + `fsutil::glob_list`): `*`, `?`, `[abc]`,
`[!abc]`/`[a-z]`, matched with the classic iterative two-pointer algorithm
(no recursion, cannot blow the stack on adversarial input), directory-listed
via `std::fs::read_dir` and sorted lexicographically — `glob(3)`'s own
default order.

**`-pattern_type`'s default is not what the brief said.** `ffmpeg -h
demuxer=image2` on this reference build (8.1) prints named constants `glob`
(1), `sequence` (2), `none` (3), and a default numeric value of `4` with
**no name attached**. The historical `glob_sequence` (value `0`) is not
merely undocumented here — `ffmpeg -pattern_type 0 -i 'out*.png'` is refused
outright (`Unknown value '0' for pattern_type option`, measured). So on this
build there is no way, by name or by number, to select it. `PatternType::Auto`
reproduces the unnamed default's *observed* behaviour instead of a name that
does not exist: try sequence-style number matching, and fall through to
treating the path as a literal single file when it has no `%d`.

### The 37 pipe splitters

One shared engine (`pipe/mod.rs`'s `PipeDemuxer`): buffer the whole
remaining input once (bounded, `MAX_BUFFERED = 512 MiB`, mirroring
`vaco-demux-raw::bitstream`'s own "compute the packet table once" trade-off),
split it into spans per `PipeSpec::framing`, then hand spans back as packets
via `Demuxer::read_packet`.

**Measured, not assumed, which splitters actually support concatenation.**
`ffmpeg -f lavfi -i testsrc=... -f image2pipe -c:v <codec> - | ffmpeg -f
<name>_pipe -i - -show_packets` answers a factual question directly:

| Result | Formats |
|---|---|
| 3 packets from 3 concatenated images (real per-image framing) | `png`, `jpeg`, `jpegls`, `j2k`, `bmp`, `webp`, `ppm`, `pgm`, `pgmyuv`, `pbm`, `pam`, `pfm`, `qoi`, `xwd`, `hdr`, `xbm` |
| **1 packet spanning the whole input**, regardless of concatenation | `gif`, `tiff`, `sgi`, `dpx`, `exr`, `pcx`, `sunrast` |

The second row is the *reference's own* behaviour, not a shortcut this crate
took — `png_pipe` and `gif_pipe` are not the same shape of demuxer on ffmpeg
itself. `ImageFraming::WholeRemaining` reproduces it exactly.

No encoder exists in this ffmpeg build for `cri`, `dds`, `gem`, `jpegxl`,
`jpegxs`, `pgx`, `photocd`, `pictor`, `psd`, `qdraw`, `svg`, or `vbn`, so
their concatenation behaviour could not be measured at all (`vbn`'s one
encode that did not error gave an inconclusive 2-packets-from-3-images
result and is treated as `WholeRemaining` rather than trusted). All twelve
default to `WholeRemaining`, which is always a safe answer even when it is
not the reference's actual one.

Full per-format framing strategy, in `pipe/mod.rs`'s registration order:

| Splitter | Framing | Status |
|---|---|---|
| `bmp_pipe` | `BmpSized` (`"BM"` + LE u32 total size @2) | Measured |
| `cri_pipe` | `WholeRemaining` | No encoder to measure against |
| `dds_pipe` | `WholeRemaining` | No encoder |
| `dpx_pipe` | `WholeRemaining` | Measured (reference does not split) |
| `exr_pipe` | `WholeRemaining` | Measured |
| `gem_pipe` | `WholeRemaining` | No encoder |
| `gif_pipe` | `WholeRemaining` | Measured |
| `hdr_pipe` | `Radiance` (new-format-RLE scanline walk; old format falls back to `WholeRemaining`) | Measured (splits); old-format path untested (no such sample) |
| `j2k_pipe` | `Marker` SOC `FF4F`…EOC `FFD9`, no stuffing rule | Measured |
| `jpeg_pipe` | `Marker` SOI `FFD8`…EOI `FFD9`, JPEG stuffing/restart-aware | Measured |
| `jpegls_pipe` | Same as `jpeg_pipe` (shares JPEG's marker syntax, T.87) | Measured |
| `jpegxl_pipe` | `WholeRemaining` | No encoder; naked codestream has no findable boundary without decoding regardless |
| `jpegxs_pipe` | `WholeRemaining` | No encoder; marker codes not established from a public source with confidence |
| `pam_pipe` | `Netpbm` | Measured |
| `pbm_pipe` | `Netpbm` (binary `P4`; ASCII `P1` falls back to whole-remaining) | Measured |
| `pcx_pipe` | `WholeRemaining` | Measured |
| `pfm_pipe` | `Netpbm` | Measured |
| `pgm_pipe` | `Netpbm` (binary `P5`; ASCII `P2` falls back) | Measured |
| `pgmyuv_pipe` | `Netpbm` (byte-identical `P5` header to `pgm`; extension-only signature) | Measured |
| `pgx_pipe` | `Pgx` | No encoder; public JPEG2000-Part-4 spec |
| `phm_pipe` | `Netpbm` | No encoder; modelled on measured `pfm` |
| `photocd_pipe` | `WholeRemaining` | No encoder |
| `pictor_pipe` | `WholeRemaining` | No encoder |
| `png_pipe` | `Png` (chunk walk to `IEND`) | Measured |
| `ppm_pipe` | `Netpbm` (binary `P6`; ASCII `P3` falls back) | Measured |
| `psd_pipe` | `WholeRemaining` | No encoder |
| `qdraw_pipe` | `WholeRemaining` | No encoder |
| `qoi_pipe` | `Qoi` (fixed 8-byte end marker scan) | Measured |
| `sgi_pipe` | `WholeRemaining` | Measured |
| `sunrast_pipe` | `WholeRemaining` | Measured |
| `svg_pipe` | `SvgText` (scan for `</svg>`) | No codec in this build to measure against; the text grammar itself is unambiguous |
| `tiff_pipe` | `WholeRemaining` | Measured |
| `vbn_pipe` | `WholeRemaining` | One inconclusive probe (2 packets from 3 images); not trusted |
| `webp_pipe` | `RiffSized` (`"RIFF"`+`"WEBP"` + LE u32 size @4) | Measured (via `cwebp`, since this ffmpeg build has no WebP encoder) |
| `xbm_pipe` | `CArrayText` (scan for closing `};`) | Measured |
| `xpm_pipe` | `CArrayText` | No encoder; same C-array grammar as `xbm`, unverified for XPM specifically |
| `xwd_pipe` | `Xwd` (X11 `XWDFileHeader`, 25 BE `u32` fields) | Measured |

`CodecId` (`vaco-codec-core`) has variants for only six of the 37: `png`,
`jpeg` (used for `jpeg_pipe`/reported as `mjpeg`), `gif`, `bmp`, `tiff`,
`webp`. The other 31 carry `codec_id = None` and the reference's exact name
as `raw_codec_name` stream metadata — `vaco-demux-raw`'s own documented
convention for the identical gap.

## How to add a splitter

1. Pick a `pipe::framing::ImageFraming` strategy, or add one to `framing.rs` if
   none fits (framing lives in this crate, never behind a decoder).
2. One `pipe!(...)` macro invocation in `pipe/mod.rs` — name, long name
   (`"piped <x> sequence"` is generated), extensions, framing, `CodecId` if
   one exists, and `magic_sets` for content probing (empty means
   extension-only).
3. Add it to `PIPE_DEMUXERS` and bump the count in `lib.rs`'s
   `there_are_exactly_thirty_eight_registrations` test.
4. A `vaco-component.toml` row, then `cargo xtask gen-registry`.

## Not implemented

`-pixel_format`, `-video_size` (raw/headerless pixel formats need these to
know their own dimensions; none of the 37 splitters here are headerless raw
formats, so they are currently unused) and `-export_path_metadata`. None of
the 37 splitters populate `VideoParameters::width`/`height` even where the
header trivially has them (PNG's `IHDR`, BMP, the netpbm family) — left for a
follow-up; framing does not need them and this crate's scope was the framing
plus the two mux-side/demux-side entry points.

## Configuration

No Cargo features beyond the crate-wide `demux-image2` (registry-generated).
Runtime configuration is entirely `Image2Options`/`PipeOptions`, both
plain-`Default` structs (no `vaco-opts` derive yet — no sibling format crate
in this codebase has adopted it either; see `vaco-demux-mp4::Mp4Options` for
the same convention).

## Dependencies

`vaco-core`, `vaco-io`, `vaco-limits`, `vaco-packet`, `vaco-format-core`,
`vaco-codec-core` (for `CodecId`/`CodecParameters` only — no `vaco-parse-*`
or `vaco-codec-<name>` crate, per D14.1). `std::fs` for the filesystem half
(`fsutil.rs`), which compiles on `wasm32-unknown-unknown` and fails every
call at runtime there, by design — see the crate's top-level docs for why
that split sits at a module boundary rather than a `#[cfg]`.

## Gotchas

* The pattern/glob engine (`pattern.rs`, `glob.rs`) is pure and portable;
  `fsutil.rs` is the one module that is not. Adding filesystem access
  anywhere else in this crate defeats that split.
* `SequencePattern::matches` only round-trips non-negative indices — see its
  doc comment for why a zero-padded negative number is ambiguous.
* An `ImageFraming` strategy must always make forward progress (`compute_spans`'
  loop guards check `end <= start` explicitly): this is the property the
  `image2_pipe_framing` fuzz target exists to keep true against attacker
  bytes, not just the format's own valid samples.
