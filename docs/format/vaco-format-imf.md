# `vaco-format-imf`

Layer 4. FM-55b/FM-55c (epic FM-55, shared with `vaco-format-gxf` for
ownership reasons only — the two share nothing technically). IMF (SMPTE ST
2067, "Interoperable Master Format") demuxer: Composition Playlist / Packing
List / ASSETMAP parsing, virtual-track assembly, and essence integration
over `vaco-demux-mxf`'s OP-Atom (clip-wrapped) support. Registered as `imf`.

Written from the published ST 2067-3 (CPL), ST 429-9 (ASSETMAP) and ST
429-8 (PKL) schemas, clean-room (D7/D15). **No reference implementation was
available to cross-check against** — this machine's `ffmpeg 8.1` has no
`imf` demuxer at all (`ffmpeg -demuxers` / `ffmpeg -h demuxer=imf` both
confirm "Unknown format 'imf'"). Every other format crate in this project's
format work has had a real `ffmpeg` build to measure against at some point;
this one does not, and every place below that would normally say "measured"
says "spec-derived" instead. See "What is not measured" for the specific
consequences.

---

## What it is

| Module | Contents |
|---|---|
| `xml` | A generic, bounded XML tree (`XmlNode`) over `quick-xml` — deliberately similar to `vaco-demux-dash::tree`, not shared with it; see that module's own doc comment for the D19 tension |
| `cpl` | ST 2067-3 Composition Playlist: `Segment` > `Sequence` > `Resource`, and `Cpl::virtual_tracks` — the real edit-decision-list timeline |
| `assetmap` | ST 429-9 ASSETMAP: `UUID` → relative path |
| `pkl` | ST 429-8 Packing List: asset inventory (`Hash`/`Size` read, never verified) |
| `package` | Ties a parsed CPL to `ASSETMAP.xml` found next to it, and resolves `TrackFileId`s to real paths |
| `fsio` | `std::fs::File` as a `vaco_io::RawSource`, so a track file's essence is read on demand rather than buffered whole |
| `demux` | `ImfDemuxer`: `open` (parses the CPL) + `bind_url` (resolves the package, opens each track's essence) + `read_packet`/`seek` |

---

## How it works

### The CPL is not the timeline; `virtual_tracks()` is

A CPL states `Segment`s, and each `Segment` states one `Sequence` per active
track. The composition's actual timeline for one track is every `Sequence`
across every `Segment` that shares a `TrackId`, concatenated in segment
order — `Cpl::virtual_tracks` performs exactly that grouping once, so
nothing downstream needs to know a composition can have more than one
`Segment`.

Each `Resource` inside a `Sequence` names a `TrackFileId` (a UUID the
ASSETMAP resolves to a real file) and an edit-unit range of that file:
`EntryPoint` (default `0`), `SourceDuration` (default
`IntrinsicDuration - EntryPoint`), `RepeatCount` (default `1` — the whole
`EntryPoint..EntryPoint+SourceDuration` range plays that many times before
the sequence moves to its next `Resource`).

### The two-call open IMF needs

`ImfDemuxer::open` only ever sees the CPL's own bytes — enough to parse
every `Resource` and build one placeholder `Stream` per virtual track, but
not enough to know real codec parameters (resolution, sample rate), since a
CPL never restates them. `ImfDemuxer::bind_url`, given the CPL's own path
(the same seam `INTERFACE-GAPS.md` gap 7 names, explicitly anticipating
"MXF OP-Atom" as a future case), finds `ASSETMAP.xml` next to it via
`std::fs`, opens the first resource of every virtual track, and fills in
real `CodecParameters` from what `vaco-demux-mxf` reports for that file. The
`vaco-cli` input path already calls `bind_url` once, immediately after
`open`, for exactly this class of format; a caller that skips it (a fuzz
target driving `(desc.open)(..)` directly) gets a demuxer whose streams are
bare placeholders and whose `read_packet` returns `Error::NotSeekable`
rather than panicking.

### Local files only, and why that is not a shortcut

An IMF package is a set of files delivered together — every real tool this
crate's spec reading found treats it as local storage, never something
streamed the way a DASH `MPD` is. `package.rs` resolves `ASSETMAP.xml` and
track files with `std::fs` directly, `vaco-demux-image2::fsutil`'s own
choice for the same "a demuxer whose real unit of work is a local file set"
situation — not `vaco-format-adaptive::RemoteAccess`'s protocol-registry
machinery built for HTTP-fetched manifests.

That choice has a named cost: `bind_url` hands this crate only the URL
string the caller opened the CPL from, with no protocol/whitelist context.
A CPL genuinely fetched over `http(s)` and then resolved against `std::fs`
would defeat W3 (a remote manifest's default whitelist excludes `file`) — a
malicious remote CPL could name a `TrackFileId` an ASSETMAP entry points at
an absolute local path a whitelist would otherwise refuse.
`package::local_path_only` closes that specific hole the cheap way
available at this layer: it refuses any `cpl_path` that looks like
`scheme://...` rather than attempting to resolve one. See "How to change
it" for what a full fix would need.

### Reading essence: a `vaco-demux-mxf` fix this crate's own need surfaced

Frame-accurate access into OP-Atom (clip-wrapped) MXF essence needed a new
`MxfDemuxer::read_edit_unit(stream_index, n)` method, added to
`vaco-demux-mxf` as part of this work. Building it surfaced a real,
previously-latent bug in that crate: a clip-wrapped file's
`IndexEntryArray::StreamOffset` is relative to the essence element's
**value** start, not its key start (unlike frame-wrapped files, where the
two coincide per element) — confirmed against the real fixture
`opatom_mpeg2_sample.mxf` (index entries land exactly on `00 00 01` MPEG-2
start codes only when measured from the value start). `demux.rs`'s own
`FirstEssenceElement`/`is_clip_wrapped` account for both shapes now; see
`vaco-demux-mxf`'s own doc comments and `planning/TECH-DEBT.md` for the full
account. `ImfDemuxer::read_packet` uses `read_edit_unit` to pull exactly the
edit units a `Resource` names — `entry_point + (n % source_duration)` for
the `n`th unit of a (possibly repeated) resource — re-timestamping each onto
the composition's own continuous timeline. Multiple virtual tracks (e.g.
one video, one audio) interleave by picking whichever track's next
composition-timeline position is smallest, since every track shares one
edit-unit domain (the CPL's own `EditRate`).

---

## What is not measured

No `ffmpeg` build with an `imf` demuxer was available on this machine (see
above). Verification here is therefore self-consistency, not a
byte-for-byte comparison against a measured reference:

- `tests/end_to_end.rs` builds a real OP-Atom MXF track file with this
  workspace's own `vaco-mux-mxf::MUXER_OPATOM` (itself measured against
  `ffmpeg` in that crate's own test suite), wraps it in hand-built
  CPL/ASSETMAP XML with two `Segment`s over the same virtual track — one
  plain range, one with `RepeatCount=2` — and checks the exact frame values
  read back through the full `open` + `bind_url` + `read_packet` path.
- Every CPL/ASSETMAP/PKL field name and structure is read directly from the
  published schemas (D7/D15's clean-room posture); there is no second leg
  of "and a reference agrees" the way `vaco-mux-mxf`'s D-10/OP-Atom work
  had.

## Scope limits, stated rather than silently absent

- **Only `MainImageSequence`/`MainAudioSequence` are read.**
  `MarkerSequence`, `MainSubtitleSequence`, `MainCaptionSequence`,
  `AncillaryDataSequence` and the IAB/ACES extension sequences are not —
  each would need its own `SequenceKind` variant and a stream/track shape
  this crate does not yet have a home for.
- **A `Resource` whose own `EditRate` differs from the composition's is
  `Error::Unsupported`.** Legal per the schema (a track file authored at a
  different rate than the composition plays it at); this crate does not
  retime, and has not measured a real file exercising the path.
- **Every essence file's own index entries are assumed to enumerate edit
  units in the CPL's own `EditRate`.** `demux.rs`'s own module docs name
  this explicitly — `EntryPoint`/`SourceDuration` are used directly as
  zero-based indices into `vaco-demux-mxf`'s `PacketIndex` with no
  independent rate check, since no counter-example was available to check
  against.
- **A multi-`Chunk` ASSETMAP `Asset`** (one asset split across several
  files) is `Error::Unsupported` — chunk reassembly is not implemented.
- **The PKL's `Hash`/`Size` are read, never verified.** Best-effort: a
  missing or unparseable PKL does not stop the composition from opening.
- **`seek` lands exactly on the requested composition-timeline edit unit**
  (per virtual track), clamped to that track's own range. It does not walk
  back to find a keyframe-flagged index entry the way `vaco-demux-mxf`'s own
  `seek` does — a caller decoding a long-GOP codec from a non-keyframe
  position gets whatever `Packet::flags` honestly reports, the same
  contract every other container demuxer in this workspace already leaves
  to its caller.

## How to change it

- **A full fix for the `bind_url`/W3 gap** needs `DemuxerDesc::open` (or an
  equivalent seam) to carry the protocol/whitelist context a remotely-opened
  format needs — the same gap `vaco-demux-dash`/`vaco-demux-hls` already
  live with, not something this crate can close alone.
- **A new `SequenceKind`** (subtitles, markers) needs a variant in
  `cpl::SequenceKind`, a case in `cpl::parse_segment`'s kind list, and a
  `MediaType`/stream shape in `demux::ImfDemuxer::open` to hand it to.
- **Chunk reassembly** would replace `assetmap::AssetMapEntry::path`'s
  single `String` with an ordered list, and `package::Package` would need
  to concatenate chunks when resolving a track file — `fsio::FileRawSource`
  would need a multi-file variant.

## Configuration

None yet — no CLI-facing option channel exists for this format (the same
"no channel to this point yet" gap `vaco-demux-image2`'s own docs name for
`-pattern_type`/`-start_number`).

## Dependencies

`quick-xml` (the only XML dependency; all interpretation is this crate's
own), `vaco-demux-mxf` (essence integration — the reason this crate is
layer 4, not lower), `vaco-format-core`, `vaco-io`, `vaco-limits`,
`vaco-packet`, `vaco-codec-core`, `vaco-core`. Dev-only: `vaco-mux-mxf`
(builds the real OP-Atom fixture in `tests/end_to_end.rs`), `vaco-chlayout`,
`proptest`, `tempfile`.
