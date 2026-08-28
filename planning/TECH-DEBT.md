# Technical-debt register

Where agents record what they had to work *around* rather than *on*, so it can
be paid down deliberately instead of rediscovered.

**Every agent appends here as part of finishing.** Not a separate task and not
optional: an agent that has just spent a session inside a crate knows things
about it that no later reader will, and that knowledge is otherwise lost when
its context ends. Recording it costs a paragraph.

## What belongs here

- **A file that has outgrown its module.** Say what the seams are — "`demux.rs`
  is 1986 lines and the EBML walk, the track table and the cue index never call
  each other" is actionable; "demux.rs is long" is not.
- **A design that fought you.** An interface you had to route around, a type
  that means two things, an invariant enforced by convention rather than by the
  compiler. The `Muxer::add_stream` gap that made `framecrc` print the wrong
  time base sat unrecorded for weeks and cost a full agent session once it
  finally mattered.
- **Duplication the `dup-check` gate cannot see** — two functions that do the
  same thing under different names, a table transcribed into three crates.
- **A comment or doc that is now wrong.** Not cosmetic: three separate bugs
  this month were in code whose comment described the correct behaviour while
  the code did something else, and the comment is why nobody looked.
- **A test that cannot fail**, or that pins a point-in-time fact rather than an
  invariant. Several tests here passed happily while the code they covered
  wrote unreadable files.
- **An API with no caller.** `cargo xtask dead-code`'s orphan list is a list of
  promises nobody has kept; if you added to it, say who is going to call it.

## What does not belong here

Style preferences, "I would have structured this differently", and anything you
can fix in the change you are already making — fix it there instead. This is a
register of things too large or too far outside your ownership to fix in
passing, not a list of opinions.

## How to write a row

Name the file and the specific problem, say what it cost you or would cost the
next person, and propose the seam if you can see one. One entry per problem.
Append at the end and commit with the private `GIT_INDEX_FILE` recipe — this
file has the same multi-writer collision risk as `CONFORMANCE-FINDINGS.md`.

---

## Open

### The eleven files over 1500 lines

Surveyed 2026-08-27, excluding `tests/` and the generated `generated.rs`
(7721 lines, which is generated and fine):

```text
2317  crates/format/vaco-demux-mp4/src/lib.rs
2018  crates/format/vaco-demux-matroska/src/ebml/schema.rs
1986  crates/format/vaco-demux-matroska/src/demux.rs
1971  crates/format/vaco-format-isom/src/stbl.rs
1947  crates/app/vaco-cli/src/listing.rs
1766  crates/format/vaco-mux-matroska/src/mux.rs
1720  crates/model/vaco-pixfmt/src/table.rs
1706  crates/format/vaco-format-core/src/mux.rs
1681  crates/format/vaco-format-core/src/discovery.rs
1594  crates/format/vaco-demux-mpegts/src/demux.rs
1479  crates/format/vaco-format-core/src/time.rs
```

Length alone is not the problem — `schema.rs` and `table.rs` are declarative
tables and are fine at any length. The ones worth a look are the three in
`vaco-format-core`, because that crate is depended on by nearly everything and
a 1700-line `mux.rs` is where the `add_stream` interface gap hid.

**Not scheduled.** Listed so that the next agent working in one of these has
the number in front of it and can say whether the seam is real.

### `vaco-protocol-*`'s repeated duplex-dial preamble and per-crate `NATIVE_ONLY` paragraph

Landed six protocol crates this session (`crypto`, `httpproxy`, `ftp`,
`gopher`/`gophers`, `icecast`, `ipfs`/`ipns`) under `crates/io/`. Five of them
(`httpproxy`, `ftp`, `gopher`, `gophers`, `icecast`) need to write-then-read
(or otherwise treat a connection as duplex) before they have anything to hand
back through `vaco_protocol_core::Protocol`'s one-direction-only
`open`/`create`, so each independently reproduces the same three things
`vaco-protocol-tls/src/connect.rs` established first:

- a `dial_tcp(hp, env) -> Result<TcpStream>` that is just
  `env.check_scheme("tcp")?; vaco_protocol_socket::addr::connect(hp, None)`,
  byte-for-byte identical across `httpproxy`, `ftp`, `gopher`, and `icecast`;
- a `dial_tls(hp, env) -> Result<TlsStream>` that is just
  `env.check_scheme("tls")?` then `vaco_protocol_tls::connect::{connect_tcp,
  handshake}` with a default `TlsOptions`, identical between `gophers` and
  `icecast`;
- a byte-at-a-time `read_header_block`/response-status reader (to avoid a
  `BufReader` stranding tunnel/body bytes past the header block on the same
  socket that gets handed back) — `vaco-protocol-httpproxy::connect` and
  `vaco-protocol-icecast::protocol` each carry their own copy, differing only
  in the error type's scheme name.

None of this shows up in `cargo xtask dup-check`, because dup-check tracks
type names, not function bodies, and every copy uses a locally appropriate
name. It also means every one of these five crates carries its own paragraph
in `xtask/src/wasm.rs`'s `NATIVE_ONLY` list, each explaining the same
underlying wall (this dial pattern pulls in `vaco-protocol-socket`, and for
`gophers`/`icecast` also `vaco-protocol-tls`) in slightly different words —
five paragraphs that will all need editing together if the measured reason
ever changes.

**Proposed seam:** a small `vaco-protocol-dial` crate (layer 2, depending on
`vaco-protocol-socket`/`-tls`) exporting `dial_tcp`, `dial_tls`, and
`read_header_block` once. Each of the five crates above would shrink by
20-40 lines and lose its own copy of the wasm wall; `xtask/src/wasm.rs`
would carry one `NATIVE_ONLY` entry for `vaco-protocol-dial` instead of five
that have to be kept in sync by hand. **Not fixed here**: it touches
`vaco-protocol-tls` (predates this session, established the pattern first)
and would require re-running the full gate suite against all five
dependents, which is real scope beyond landing issues #546/#550 — flagging
for whoever picks up the next protocol crate that would otherwise become a
sixth copy.

**Resolved 2026-08-27.** `vaco-protocol-dial` now exports `dial_tcp`,
`dial_tls`, and `read_header_block`; `httpproxy`, `ftp`, `gopher`/`gophers`,
and `icecast` all call the shared versions and no longer carry their own.
The four crates' `xtask/src/wasm.rs` `NATIVE_ONLY` paragraphs collapsed to
one-liners pointing at a single new `vaco-protocol-dial` entry. Full gate
suite (check/test/clippy per crate, `layer-check`, `dup-check`, `dead-code`,
`wasm-check`, `unsafe-audit`, `owner-gate`, `time-gate`) re-run and green
across all five dependents plus `vaco-protocol-tls` itself.

`vaco-protocol-tls/src/connect.rs` was **not** moved onto the new crate:
`dial_tls` depends on `vaco-protocol-tls` for the handshake, so the reverse
dependency would be a cycle (`vaco-protocol-tls` -> `vaco-protocol-dial` ->
`vaco-protocol-tls`), confirmed absent from `cargo xtask layer-check`'s
acyclic result. `connect_tcp`/`handshake` stay where they are as the base
`dial_tls` calls into.

### `s337m` is registered twice, under two different implementations

FM-54 (#612) added `vaco_format_spdif::S337M_DEMUXER`, a real SMPTE 337M
demuxer alongside this crate's spdif/IEC 61937 work. Discovered while
writing the crate's (previously missing — see the registry fix below)
`vaco-component.toml`: `vaco-demux-raw` already registers a demuxer named
`s337m` (`vaco_demux_raw::bitstream::DEMUXER_S337M`, part of its
`BitstreamSpec`-driven sweep of ffmpeg's raw/bitstream demuxer names).
`cargo xtask gen-registry`'s `check_unique` rejects the same name twice
across crates in one kind, so only one can ever be reachable through the
registry.

Left `vaco-format-spdif::S337M_DEMUXER` unregistered rather than picking a
winner — I don't know which of the two implementations is more complete or
correct, and swapping the registry entry to point at a different crate's
symbol is exactly the kind of cross-crate decision that shouldn't be made
by whichever agent happens to notice the collision first. **Proposed
seam:** compare the two (`vaco-demux-raw`'s generic `BitstreamSpec` framing
vs. `vaco-format-spdif`'s dedicated SMPTE 337M parser) against a real
S/PDIF-wrapped bitstream and either register the better one under `s337m`
and delete the other, or split the name space (e.g. `vaco-demux-raw` keeps
`s337m` for the bare bitstream form, `vaco-format-spdif` registers under a
distinct name for the S/PDIF-framed form) if they actually serve different
inputs. Not something `dup-check` catches at all — it checks Rust type
names, not registry component names, so this collision was silent until
`gen-registry` itself refused it.

### Private-index commits, worked example from this session

`GIT_INDEX_FILE` + `write-tree` + `commit-tree` (plan 19 §5/§6) is the
prescribed way to append to a shared, append-only file like this one
without touching the real `.git/index` a concurrent agent may have staged
work in. Worked example, used to write this very entry:

```bash
export GIT_INDEX_FILE=/tmp/vaco-nut-techdebt-$RANDOM.index
git read-tree HEAD
git add planning/TECH-DEBT.md
TREE=$(git write-tree)
COMMIT=$(git commit-tree "$TREE" -p HEAD -m "docs(planning): append NUT/spdif findings to TECH-DEBT")
git update-ref refs/heads/main "$COMMIT"
unset GIT_INDEX_FILE
git reset -q HEAD -- planning/TECH-DEBT.md   # reconcile the REAL index against the new HEAD
```

The `git read-tree HEAD` that used to sit between `update-ref` and `unset`
ran against `$GIT_INDEX_FILE`, not `.git/index` — it refreshed the private
index it was already pointed at, not the real one, so `git status` kept
showing this path as staged-modified against the new HEAD until something
else touched it. `git reset -q HEAD -- <path>` after `unset` fixes the real
index directly, scoped to only the path(s) just committed, so a concurrent
agent's own staged changes elsewhere are untouched — `AGENT-CONSTRAINTS.md`'s
"When you genuinely share a file" section is where this line came from.

### `vaco-demux-avi`'s `strf` parsing captures extradata for audio, never video

Found while independently re-measuring AVI's H.264 framing during the
600 Hz grid work (`vaco-mux-avi`, #639/#640). `hdrl.rs::parse_strf` has two
branches, `b"vids"` and `b"auds"`. The audio branch takes `WaveFormatEx`'s
trailing bytes as `CodecParameters::extradata` whenever there are any
(MS-ADPCM coefficients, AAC's `AudioSpecificConfig`); the video branch reads
`BitmapInfoHeader`'s fixed 40-byte fields and stops — any bytes after that
(an `avcC`/`hvcC`-style configuration record, which is exactly what a real
`avc1`/`hvc1`-tagged `strf` carries per the measurement below) are simply
never looked at. This is not a forgotten line so much as a structural
asymmetry: the audio path already has the "trailing bytes are extradata"
idea, and the video path was never given it.

Consequence, independently confirmed by probing `ffmpeg -c copy -f avi`
output directly rather than reading the demuxer's own field list: the
reference stores H.264 in AVI **length-prefixed** (`strf` FourCC `avc1`,
`is_avc=true`, `nal_length_size=4`, a 45-byte `avcC` following the base
header), not Annex-B. `vaco-mux-avi` currently writes the opposite shape
(Annex-B, `H264` FourCC, no configuration record at all) — a real,
independently-verified divergence from the reference, not fixed as part of
this session's work since it touches `write_packet`/`strf`/
`check_bitstream` machinery that finding 16 already shaped for a different
answer, and deserves its own measurement pass across both mux and demux
rather than a drive-by change to one side.

**Proposed seam:** give `parse_strf`'s video branch the same "trailing bytes
are extradata" treatment the audio branch already has, gated on the FourCC
being one this crate maps to H.264/HEVC (`video_tags::codec_id`) so the
bytes are only interpreted as `avcC`/`hvcC` where that framing applies.
`vaco_codec_core::VideoParameters::nal_length_size` also needs setting from
the record's own length-size byte, mirroring the demuxer's own AAC/ADTS
config handling on the audio side.

**Resolved.** `parse_strf`'s video branch now captures trailing bytes as
extradata, gated on `video_tags::carries_config_record` (`avc1`/`AVC1`/
`hvc1`/`hev1`, not the Annex-B spellings). `nal_length_size` needed no
separate demuxer-side handling: it already comes free from the generic
`vaco-format-core::discovery` pipeline once a codec parser sees the
extradata, the same path MP4's `avcC` already went through. On the mux side,
`vaco-mux-avi` was measured writing the *opposite* of both this row's own
assumption and finding 16's: the reference does not convert H.264/HEVC
framing in either direction, and neither does this crate any more — see the
same session's commit for the mux-side change and the `write_strl`
comment beside `StreamOut::video_extradata`.

### `AviMuxer`'s slot-grid budgets hardcode `Limits::permissive()`

`grid_budget` (for the 600 Hz grid's empty-slot backfill, both the
inter-frame case and the trailing-slot extension added this session) is
constructed with `Budget::new(Limits::permissive())` inside `AviMuxer::new`,
ignoring the `FormatOptions` the caller passed in. The same budget also now
bounds the leading-audio-gap backfill (`maybe_backfill_leading_audio_gap`),
added this session — same shape, same caller-supplied-`Limits` gap.
Permissive's 1 GiB/2^32-fuel caps are generous enough that no real recording
should ever hit them, but an embedder who wants a stricter bound (the
`Limits::strict()`/library-embedding case `vaco_limits::Limits`'s own docs
describe) cannot get one without a `vaco-mux-avi` code change, since nothing
threads a caller-supplied `Limits` through `Muxer::new`/`FormatOptions`
today. Not fixed here: not a regression, and widening it is a
`FormatOptions`/`Muxer` interface question bigger than one crate.
`convert_budget`, the other budget this row used to name, no longer exists —
it belonged to the length-prefixed-to-Annex-B conversion this crate no
longer performs (see the row above).

### `vaco-mux-avi`: two fields measured but not resolved this session

- **`strf.nBlockAlign` for a compressed (VBR) audio stream.** The one AAC
  fixture available measured `3`, matching none of
  `bytes_per_sample × channels` (this crate's own formula, correct for CBR
  PCM), the sample rate, the declared bit rate, or the channel count in any
  combination tried. A second compressed-audio fixture with a different
  channel count or bit rate would very likely settle this quickly; none was
  available this session. This is the one remaining byte-level difference
  against the reference on the one two-stream fixture measured (2 bytes out
  of 39304).
- **The leading-audio-gap formula (`2^has_b_frames - 1`) above
  `has_b_frames = 2`.** Confirmed at `n = 0, 1, 2` across seven synthetic
  `libx264 -bf 0..7` fixtures, but `ffprobe`'s own `has_b_frames` field
  capped at 2 for every `-bf` value this build of `libx264` produced past
  that point, so `n = 3` and above were never observed. A source that
  reports `has_b_frames >= 3` (a different encoder, or a hand-built SPS)
  would settle whether the pattern really is `2^n - 1` or coincidentally
  matches only for small `n`.

### `just fuzz <target>` has no time limit

`justfile`'s `fuzz` recipe (singular, as opposed to `fuzz-all`) runs
`cargo +nightly fuzz run {{target}}` with no `-max_total_time`, unlike
`fuzz-all`'s own `secs` parameter. Run as `just fuzz avi_mux_packet` this
session, it sat at 100% CPU for several minutes past the intended 30-second
breadth-phase budget before being noticed and killed manually. Not fixed
here (a one-crate session is not the place to change a shared recipe used
by every fuzz target); the workaround is to invoke `cargo +nightly fuzz run
<target> -- -max_total_time=30` directly rather than `just fuzz`, or to add
a default `secs` parameter to the `fuzz` recipe itself mirroring
`fuzz-all`'s.

### `cargo fmt -p <crate>` reflows lines you never touched, which makes it unsafe here too

Ran `cargo fmt -p vaco-protocol-ftp` (and the same for `-httpproxy`,
`-gopher`, `-icecast`) while finishing the `vaco-protocol-dial` extraction,
expecting it to touch only the lines the extraction changed. It reformatted
unrelated lines in every file in each crate instead — e.g.
`vaco-protocol-ftp/src/sink.rs`'s `start_stor(...)` call, 96 columns and
already on one line, got wrapped to one argument per line, and
`vaco-protocol-gopher/src/selector.rs`'s test array literal wrapped the same
way. Reproduced in isolation (a two-line throwaway file with the same call
under `rustfmt.toml`'s `max_width = 100`): current `rustfmt` wraps calls
well inside the column limit that the code already checked in under. Not a
toolchain mismatch — `rustc`/`cargo`/`rustfmt` all report the
`rust-toolchain.toml`-pinned `1.97.1`/`1.9.0` build.

Caught only because the diff was reviewed before committing; reverted to
`HEAD` and reapplied the intended edits by hand instead of trusting `-p`'s
output wholesale.

**This means the tree is not actually rustfmt-clean against the `rustfmt`
this checkout runs**, and has been quietly relying on nobody running `cargo
fmt -p` broadly enough to notice — the scoped `-p` form `AGENT-CONSTRAINTS.md`
recommends in place of `--all` only bounds *which crate* gets reformatted,
not *which lines in it*, so it carries the same "silently reformats a
co-owner's file" risk the `--all` warning describes, one level down: inside
a crate you own outright, it will still rewrite lines a reviewer has to
untangle from your real diff by hand. **Not fixed here**: fixing it for real
means either running `cargo fmt --check -p <crate>` after every edit and
reformatting only the reported files/ranges, or reformatting the whole tree
once (a single, disruptive, coordinated pass, not something one agent should
do mid-session) and committing the result as its own change. Flagging so the
next agent who reaches for `-p` diffs before committing, not after.

### The private-index worked example's final `read-tree HEAD` never touches the real index

Followed the worked example directly above almost verbatim for the
`vaco-protocol-dial` extraction's commit, then found `git status` reporting
every file just committed as staged-deleted/staged-modified immediately
afterward. Cause: the example's last `git read-tree HEAD` runs *before*
`unset GIT_INDEX_FILE`, so it refreshes the private index at
`$GIT_INDEX_FILE`, not `.git/index` — the comment beside it ("refresh the
real index only") describes what the step needs to do, not what the command
as placed actually does. The real index is left holding whatever it had
before the commit, which is stale the moment `refs/heads/main` moves.

Harmless for a subsequent pathspec-limited `git commit -- <path>` (it reads
the working tree, not the index, per this file's own recipe above), but not
harmless for anyone who trusts `git status`, and not what "never touches the
working tree" promised. Fixed by borrowing the *other* private-index
recipe's own closing line (`AGENT-CONSTRAINTS.md`, "When you genuinely share
a file"): after `unset GIT_INDEX_FILE`, run
`git reset -q HEAD -- <every path just committed>` against the real index —
scoped to those paths, so it cannot discard anything another agent has
staged elsewhere. **Proposed seam:** add that line to this section's worked
example so the next agent doesn't have to rediscover it via a confusing
`git status`.

### C-13 (BMP/PCX/TGA/SGI/XWD/XBM/PNM/QOI): three codec crates exist with no way to reach them from the registry or the CLI

Built `vaco-codec-qoi`, `vaco-codec-pnm` and `vaco-codec-image-simple` —
fifteen codecs' worth of real, tested, differentially-verified decode/encode
— and could not write a single `vaco-component.toml` fragment of kind
`decoder`/`encoder` for any of them, for three separate, compounding
reasons, none inside these three crates:

1. `vaco_codec_core::CodecId` (hand-written enum + table in
   `crates/signal/vaco-codec-core/src/lib.rs`) has no variant for fourteen of
   the fifteen — only `Bmp` exists. `DecoderDesc.id: CodecId` cannot be
   constructed for the other fourteen, so a decoder fragment for them will
   not compile, let alone register.
2. `EncoderDesc` does not exist as a type anywhere. `vaco_registry::Kind`'s
   own doc comment and `xtask/src/registry.rs`'s `KINDS` table both say so
   explicitly ("`EncoderDesc` and friends" have not landed). An `encoder`
   fragment's `ctor` is accepted essentially unchecked as a result — there is
   no way to register one *meaningfully*, only to have the generator not
   refuse it.
3. Even where `CodecId`/`DecoderDesc` exist (BMP), `DecoderDesc` carries no
   constructor field (`make: fn(...) -> Box<dyn Decoder>`, the shape
   `ParserDesc` already has) and `vaco-cli`'s `check_codecs` accepts only the
   literal string `"copy"` for every output stream — there is no code path
   anywhere that turns a `DecoderDesc`/registered decoder name into a live
   `Decoder` instance, or an encoder name into a live `Encoder`.

Net effect: the brief's own suggested verification loop (`cargo run -p
vaco-cli -- -c:v <codec> ...`) does not exist for *any* codec in this tree
yet, native or otherwise — this is not specific to these three crates, it is
the first time anyone tried to register a leaf decoder/encoder at all.
Verification for this batch was done by calling the pure `decode`/`encode`
functions directly against ffmpeg-produced fixtures instead; that is a real
substitute for correctness but not for reachability.

**Proposed seam:** three separable pieces of framework work, each in a crate
none of C-13's three own: (a) generate `CodecId` from a `codecs.toml` the
way `vaco-pixfmt` generates its table (`vaco-codec-core`'s own
`CodecEntry` doc comment already says this is the plan); (b) design and add
`EncoderDesc` and a `Decoders`/`Encoders` registry provider parallel to
`Parsers`/`ParserProvider` (`vaco-codec-core` + `vaco-registry`); (c) wire
`vaco-cli`'s `check_codecs` and `exec.rs` to actually build a
`Decoder`/`Encoder` from the registry instead of special-casing `"copy"`.
Any one of the three blocks full registration; all three are needed before
`-c:v bmp`/`-c:v qoi`/etc. can do anything.

### `vaco-frame` has no palette side-data type, so a paletted image decoder cannot produce a paletted frame

BMP (1/4/8bpp) and PCX's single-plane 8bpp variant are both genuinely
palette-indexed on disk, and `vaco_pixfmt::PixFmt::Pal8` exists and is
flagged `PixFmtFlags::PALETTE` — but there is no `FrameSideData::Palette`
(or equivalent) variant to carry the 256-entry colour table beside the
frame, the way `vaco_frame::sidedata`'s existing variants
(`DisplayMatrix`, `Cropping`, `Metadata`, ...) carry everything else a
decoder needs to attach. `vaco-codec-image-simple::bmp::decode` therefore
expands paletted input straight to `rgb24` at decode time rather than
producing a `Pal8` frame plus a palette, which is a real, permanent loss of
information (a paletted BMP cannot round-trip back to a paletted BMP through
this decoder) and was the only reasonable choice available without adding a
type to a crate this brief does not own. PCX's palette-based single-plane
form was left unimplemented entirely for the same reason plus time, not
attempted-and-abandoned.

**Proposed seam:** a `FrameSideData::Palette([u32; 256])` (or a boxed/sized
variant, since not every palette uses all 256 entries) in
`crates/model/vaco-frame/src/sidedata.rs`, read the same way
`Frame::cropping()` reads `Cropping` today. Once it exists, BMP/PCX/SGI/XWD's
paletted variants become straightforward additions to the three crates
above, and this row can be deleted rather than carried forward.

### The single-shot "whole packet in, whole frame out" codec shape has no home in `vaco-codec-core`, so three crates each wrote it by hand

Every codec in `vaco-codec-qoi`, `vaco-codec-pnm` and
`vaco-codec-image-simple` is the same shape: one packet decodes to exactly
one frame, no reordering, no subframes, `Caps::empty()`. Each crate ended up
with its own ~15-line `ImageDecoder`/`ImageEncoder` pair
(`Machine::new(Caps::empty())`, `accept`/`emit`/`finish` on `send`,
`machine.receive()` on `receive`) parameterised over a `fn(&[u8], &mut
Budget) -> Result<Frame>` — deliberately duplicated three times rather than
introducing a fourth crate dependency none of the three briefs offered, per
`AGENT-CONSTRAINTS.md`'s "you own the crates your brief names" rule. It is
exactly the kind of twenty-line preamble the batching note anticipated
repeating a sixth time eventually: every future whole-image or
whole-frame-at-a-time codec (there will be more — TIFF's non-tiled case,
most of the remaining still-image formats the roadmap lists) will either
copy this same pair again or need this factored out. **Proposed seam:** a
generic `SingleShotDecoder<F>`/`SingleShotEncoder<F>` in `vaco-codec-core`
itself (next to `mock.rs`), parameterised the same way, so the fourth crate
that needs it depends on the framework instead of re-deriving it.

### The same mistake in five muxers: header fields derived from the decoded sample format

Fixed, recorded because the *pattern* is the finding rather than any one bug.

Five muxers independently built a container header field from
`AudioParameters::format` — the decoded sample format — instead of from
`CodecId`. The two disagree in exactly the ways `pcm.rs`'s own measured table
has always said:

```text
pcm_s24le   decodes to s32   -> width written as 32-bit
pcm_alaw    decodes to s16   -> one-byte samples tagged as two-byte linear PCM
pcm_s16le   decodes to s16   -> endianness absent from the format entirely
```

`wav`, `w64`, `caf` and `aiff` were found in one session; `au` was found by a
sweep across sixty containers a few hours later, in the *same crate* as the
first four. That is the part worth keeping: the shared helpers
(`pcm::sample_fmt_of`, `coded_bits`, `is_little_endian`, `is_float`) existed by
then, and `au` simply had not been moved onto them. A partial migration reads as
a finished one.

All five produced files that were the right length, had plausible headers, and
decoded to the wrong bytes, with every unit test green. Only a decode-MD5 check
against the source finds this class.

**Now closed:** every header field in these muxers comes from `CodecId`, and a
grep for a muxer reading `format.bits_per_sample()`, `bytes_per_sample()` or
`is_float()` returns nothing. The last three were not wrong — `is_float` agrees
between codec and format — and were changed anyway, because they were the shape
the next person would copy.

**Not gated.** No cheap mechanical check distinguishes "asked the format a
question the codec should answer" from legitimate uses of `SampleFmt`. The
defence is the helper being the obvious thing to reach for, plus the decode-MD5
check in the conformance loop.

### `CodecParameters.video.nal_length_size` means two different things and only one crate can fix it alone

Found closing #647 (MP4's `hvcC` never populated it). `vaco-mux-raw` and
`vaco-mux-mpegts` both key their `-c copy` Annex-B-conversion decision off this
one field for `H264 | Hevc` — reasonable, since it is genuinely "the true
length-prefix width, if any." But `vaco-parse-hevc`'s own `codec_parameters()`
deliberately leaves it `None` for HEVC, because `vaco-probe`'s display code
reads the *same* field, unconditionally, to decide whether to print
`is_avc`/`nal_length_size` — and the reference never prints those for HEVC, in
any container (measured: `ffmpeg -h decoder=hevc` has no such private options
at all).

Populating the field for MP4/HEVC in `vaco-demux-mp4` (this session's fix —
`track::hvcc_length_size` reads `hvcC`'s `lengthSizeMinusOne` directly, the
same relative position `avcC`'s field occupies) closes the actual corruption
bug, confirmed by decode-MD5 on both the raw `hevc` and `mpegts` muxers. It
also makes `vaco-probe` start printing `is_avc`/`nal_length_size` for HEVC,
which is a new, real divergence from the reference — filed as #654 rather than
fixed here, since the fix is in `vaco-probe`, outside this session's crates.

**The seam that's missing:** one field cannot mean "the container's true
length-prefix width" (every codec that has one) and "what a probe should
print for H.264 specifically" (one codec, by design) at the same time. The
clean fix is `vaco-probe` gating those two display fields on
`codec_id == CodecId::H264` explicitly instead of on the field's presence —
then the field itself can mean exactly one thing everywhere it is read.

### RSO's accepted-codec set measured two different answers depending on how the source PCM was produced

Found while widening `vaco-format-audio-simple::rso`'s `add_stream` check for
#651. Feeding the reference muxer `pcm_s24le` via a WAV source (`-i x.wav -c
copy -f rso`) succeeds; feeding it bit-identical `pcm_s24le` samples via the
raw `-f s24le -i x.raw -c copy -f rso` demuxer fails outright ("Could not
write header (incorrect codec parameters?)"), even though `ffprobe` reports
the same `codec_name`/`sample_fmt`/`bits_per_sample`/`channels`/`sample_rate`
for both. Only the `codec_tag` differed (`0x0001` vs `0x0000`), which should
not matter to a raw PCM muxer.

Not root-caused — reading the reference's own source to explain an
AVOption/internal-validation difference is out of bounds here (D7). The table
this session shipped (`rso::accepts`) is built entirely from **container**-
sourced measurements (WAV for little-endian formats, AIFF for the big-endian
ones and `pcm_s8`, matching how the issue itself was reproduced), which agree
internally and match the one directly-reported repro in #651. If a future
agent re-measures this table and gets a different answer through the raw PCM
demuxer specifically, this discrepancy is why — check which input path
produced the disagreement before trusting either result over the other.

### Progressive JPEG AC successive-approximation refinement disagrees with the reference on some multi-block images

Found implementing `vaco-codec-jpeg` (epic #27 / plan 15 §4A.4, issue #296).
`decode.rs`'s `ac_refine` implements Annex G.1.2.3's correction-bit sweep
(skip zero-history coefficients while counting toward a run, apply a free
correction to any nonzero coefficient encountered along the way, place the
new coefficient with its sign bit once the run is exhausted). Verified
correct by hand-trace against a from-scratch reference for a single 8x8
block, including the specific split-band scan pattern this bug needs
(`Ss=1-5` then `Ss=6-63` as separate first scans, refined later by one
`Ss=1-63` scan) — and against a from-scratch, independently-written Python
port of `ac_first` alone, which matched this crate's `ac_first` bit-for-bit
across every real block tested.

Across a multi-block image (a synthetic 64x16 or 64x48 grayscale JPEG built
with the same scan pattern), the same code diverges from `ffmpeg`'s decode
starting around the 4th-8th block, by up to max-abs-deviation ~45 / RMS 3-8.
The trigger is content-dependent, not a clean width threshold: 32/40/56px
wide test images decoded correctly end to end; 48/64px wide did not. A
second, full-pipeline Python reference port (including `ac_refine`, not just
`ac_first`) was built to try to triangulate the bug, but it disagreed with
this crate's Rust even on blocks the Rust output verified correct against
`ffmpeg` — so that port has its own bug and was not useful evidence either
way, and is not included in the crate.

This was not root-caused in the time available. Ruled out: `ac_first`
(independently verified correct), the Huffman table construction for the
specific non-standard DHT used in the failing test image (verified by hand),
and the mechanical execution of `ac_refine`'s skip/correct/place logic
relative to Annex G.1.2.3 as understood from a single-block trace (the
specific failing block in the 64x16 test was hand-traced bit-by-bit and
executes exactly as designed, yet the aggregate multi-scan image result still
disagrees with the reference). Not ruled out: a subtlety in how `eobrun`
carries from one scan to the next across many blocks that a single-block or
short 8-block-no-refinement test never exercises, or a genuine gap in this
implementation's understanding of Annex G.1.2.3 for some multi-block edge
case.

**Not gated** — `tests/roundtrip.rs` only exercises this crate's own encoder
against its own decoder, and the encoder does not emit progressive streams,
so this class of bug has no regression test in the crate today (the encoder
gap is itself listed as a separate row below). The fix needs either a
verified-correct third-party progressive decoder to differentially trace
against (only `ffmpeg`'s compiled binary was available here, which is enough
to detect the divergence but not to single-step alongside it), or a fresh,
careful re-derivation of Annex G.1.2.3 by someone not anchored to this
session's mental model of it. Baseline decode (`SOF0`/`SOF1`, any scan with
`Ah=Al=0` and a single `Ss=0,Se=63` scan) does not use `ac_refine` at all and
is unaffected — measured essentially bit-exact against `ffmpeg` (max-abs-
deviation 1, i.e. IDCT floating-point rounding) across the full
subsampling/restart-interval/optimized-Huffman matrix tested.

**Now closed.** Root cause: `ac_refine`'s skip walk treated "the skip run
has just reached zero" identically for a ZRL and a sized `RS` symbol. A
ZRL has nothing to place, so it must stop the instant its 16-position count
is spent, whatever it lands on; a sized symbol's landing position must be
a coefficient that is genuinely zero (a valid stream never targets an
already-established one), so if the count runs out on a nonzero
coefficient the walk merely passed, that is not the landing spot and the
walk must keep going. The two single-block Python cross-checks above had
both been consistent with one one-sided rule or the other by coincidence;
the actual repro needed a multi-symbol, mixed-kind sequence within one
block, found via a targeted 66x50/4:2:2/restart-interval-4 case pulled out
of a 1296-combination randomized sweep, then confirmed against
libjpeg-turbo's own `jpeg_read_coefficients` ground truth (not just final
pixels) by truncating the file after each scan. Fixed and covered by two
new `ac_refine`-level regression tests, one per symbol kind. The
1296-combination matrix (subsampling x quality x restart interval x
optimized Huffman x image size) now shows zero discrepancies at the pixel
level, and the specific repro matches libjpeg-turbo coefficient-for-
coefficient across all ten of its scans.

### `vaco-codec-jpeg` has no `vaco-codec-vlc` to build its Huffman/entropy layer on

D-01 names a shared VLC/entropy-coding crate as the intended home for this
kind of canonical-Huffman-table-plus-bitstream logic, but no such crate
exists yet in this tree. `bits.rs` (the byte-stuffing-aware entropy
reader/writer) and `huffman.rs` (Annex F.2.2.3 canonical table construction)
were built directly inside `vaco-codec-jpeg` instead. Both are written
generically enough (no JPEG-specific assumptions beyond the byte-stuffing
convention itself, which is genuinely JPEG's own) that they would lift
cleanly into a shared crate if one appears — MJPEG/JFIF-family formats are
the most likely other consumer.

### `vaco-codec-jpeg`'s encoder does not build optimized Huffman tables

`encode.rs` always emits the Annex K.3-K.6 default Huffman tables, never
per-image-optimized ones — correctness-neutral (the output is a valid,
conformant JPEG either way) but costs compression ratio against a
reference encoder at the same quality setting.

**Partially closed:** progressive encode (`EncodeOptions::progressive`)
landed once the `ac_refine` bug above was fixed — one interleaved DC scan
plus one non-interleaved AC scan per component, spectral selection only,
deliberately never successive approximation (see `encode.rs`'s module doc:
that scope avoids writing a mirror of `ac_refine`'s own subtlety on the
write side). Measured against `djpeg`/`ffmpeg`: both accept it, and
`ffmpeg`'s decode of it matches `ffmpeg`'s decode of this crate's baseline
output of the same source bit-for-bit. Optimized Huffman tables remain
unimplemented.

### C-13 update: `EncoderDesc`, `DecoderDesc::make` and CLI dispatch landed; a payload-carrying `CodecId::Ext` did not

The three parts C-13 asked for are done: `vaco-codec-core` has `EncoderDesc`
(mirroring `DecoderDesc`, which now carries a `make: fn(Limits) -> Box<dyn
Decoder>` it did not have either), `xtask/src/registry.rs`'s `KINDS` maps
`"encoder"` to a real typed table, and `vaco-cli`'s `check_codecs`/
`run_pipeline` resolve a named `-c:v`/`-c:a` through `vaco_registry::
encoder_by_name` and build a real decode-then-encode leg instead of only
accepting `"copy"`. QOI, the PNM family and the simple-image repertoire
(PCX/TGA/SGI/XWD/XBM; BMP already had a `CodecId`) are registered as
decoders and encoders. `vaco -i in8.bmp -c:v qoi -f null -` now runs an
actual decode(bmp)-then-encode(qoi) pipeline and exits 0.

The "policy for adding a codec id without a core-crate edit" half did not
survive contact with the rest of the tree. A `CodecId::Ext(&'static
ExtCodec)` payload-carrying variant was built, tested and then reverted: at
least one existing call site (`vaco-bsf-generic`'s noise generator) casts
`CodecId as u64` for a hash seed, and Rust only permits that cast when
*every* variant of the enum is fieldless — confirmed directly (`cargo check
-p vaco-bsf-generic` fails with E0605 the moment any variant carries data,
regardless of which variant a given run actually constructs). Thirteen
`CodecId` variants were hand-added instead (the same shape every prior
codec-family addition to this table already took), which is a real,
un-avoided core-crate edit per codec.

**Proposed seam, unbuilt:** the doc comment on `CodecEntry` already names
plan 15 §1.1's intended fix — generate the enum and table from a
`codecs.toml` the way `vaco-pixfmt` generates its own tables — and that
would still keep every variant fieldless (a generator can emit plain unit
variants same as a human can). It was not attempted here: transcribing the
existing ~150-entry table into a generator's input without introducing a
silent drift risk is a larger, separate piece of work, and the reachability
fix did not depend on it. Whoever picks up plan 15 §1.1 should read this
entry first — the fieldless constraint is the one design fact that rules out
the runtime-registration shortcut that looks obvious otherwise.

### A single still image demuxed with no timeline cannot be muxed to almost any real container

`vaco-demux-image2`'s `SingleSourceDemuxer` (the path a bare `-i in.bmp`
takes, no glob pattern) sets `packet.pts = Timestamp::NONE` deliberately —
measured against the reference, which reports no timeline at all for a lone
still image rather than a synthetic `0`. `vaco-format-core::interleave`
refuses any packet with neither `pts` nor `dts` unless the muxer declares
`FormatFlags::NOTIMESTAMPS`, and only `null` and `ffmetadata` declare it
today. The result: `vaco -i in.bmp -c copy -f image2 out.bmp` (no codec
change at all) fails with "this container needs timestamps and the packet
has none", and so does every other muxer tried (`md5`, `matroska` was not
tried but shares the same `interleave` path). Found verifying #652's
decode-then-encode leg — `-f null -` was the only output that accepted the
same pipeline. `vaco-mux-image2` (and plausibly other single-shot-friendly
muxers) is missing `FormatFlags::NOTIMESTAMPS`; that crate is not owned by
this session.

### `vaco-demux-image2` does not map most image codecs' extensions to a `CodecId`

With QOI/PNM-family/PCX/TGA/SGI/XWD/XBM now registered as decoders (see the
C-13 update above), `vaco -i in.ppm -c:v qoi -f null -` still fails —
"Internal error: a stream being transcoded has no known input codec" — because
`vaco-demux-image2`'s extension table only knows a handful of codecs
(BMP among them) and reports `codec_id: None` for the rest. The decoders
exist and are reachable by name; they are simply never selected as the
*input* side of a transcode until that demuxer's extension table is
extended. Not attempted here: `vaco-demux-image2` is not owned by this
session, and the finding was made verifying #652's CLI dispatch, not while
working in that crate.

### The simple-image codecs' pixel formats do not round-trip through each other without a filter stage

`vaco-cli`'s new decode-then-encode leg (#652) has no scale/pixel-format
conversion between the decoder's output and the encoder's input — a decoded
frame in a format the target encoder does not accept fails with that
encoder's own `Unsupported` message rather than being converted. This is
visible even within `vaco-codec-image-simple` alone: a paletted (1/4/8bpp)
BMP decodes to `Rgb24`, which `vaco-codec-qoi`'s encoder accepts, but
`vaco-codec-image-simple`'s own BMP *encoder* only accepts `bgr24`/`bgra` —
so `bmp(paletted) -> qoi -> bmp` fails on the final step with "bmp: encoder
needs bgr24 or bgra input", even though both codecs are correctly registered
and reachable. Reproducible with a 24bpp source instead: BMP decodes that to
`Bgr24`, which `vaco-codec-qoi`'s encoder then refuses ("qoi: encoder needs
rgb24 or rgba input"). Neither codec is wrong on its own — the gap is the
missing conversion stage between them, which is `vaco-cli`/`vaco-sched`'s to
add (a scale/format filter node between the decoder and encoder legs), not
either codec crate's.

### AC-3 bit-allocation model's masking-curve constants are unverified against the primary spec

`vaco-codec-ac3::tables_bitalloc` (Annex A of ATSC A/52:2018) could not be
checked against the standard's own text in this environment (no network
access to the document, and D7 rules out reading the reference decoder's
source for the same numbers). `BNDSZ`, `HTH`, `LATAB`, `FASTDECAY`/
`SLOWDECAY`/`FASTGAIN`/`SLOWGAIN`/`DBKNEE`/`FLOOR`, and `BAPTAB` are all
reconstructed from the algorithm's well-documented *structure* (a log-power
masking curve with fast/slow leaky-integrator decay, a hearing-threshold
floor, and an SNR-to-`bap` lookup) rather than transcribed values.

This is the dominant measured source of decode error: `bap` is derived, never
transmitted, so a wrong masking-curve constant desyncs mantissa bit reads
rather than merely degrading quality. The crate's own `tests/conformance.rs`
shows the real spread — plain stereo AC-3 at 192k/384k decodes with
`rms_err` around 0.06 (bounded, plausible, still far from bit-exact), while
mono, 5.1, a different dialnorm value, and 44.1 kHz all show `max_abs_err`
well above 1.0 (impossible for valid PCM, meaning actual bitstream desync for
part of the signal). The fix needs a primary or independently-transcribed
copy of Annex A's tables, diffed against `tables_bitalloc.rs`, then
re-measured against the same conformance fixtures to see which rows move.

### Update: AC-3 bit-allocation constants are now verified against ATSC A/52:2012 — the accuracy question is not resolved

The row above ("AC-3 bit-allocation model's masking-curve constants are
unverified against the primary spec") is now out of date on its central
claim. ATSC A/52:2012 (17 December 2012 revision) is reachable from this
tree; `BNDSZ`, `MASKTAB`, `LATAB`, `HTH`, `FASTDECAY`/`SLOWDECAY`/
`FASTGAIN`/`SLOWGAIN`/`DBKNEE`/`FLOOR` and `BAPTAB` in
`vaco-codec-ac3::tables_bitalloc` are now transcribed directly from the
clause text (§7.2.3, Tables 7.6-7.16), not reconstructed from the
algorithm's shape, and `bitalloc::compute_bap` was checked line-by-line
against §7.2.2.1-7.2.2.7's own pseudocode. The bit allocation clause is
§7.2.2/§7.2.3, not "Annex A" — the earlier row's citation was wrong
independently of being unverified (Annex A is the MPEG-2-multiplex mapping,
unrelated to bit allocation). Two real, unrelated bugs turned up doing this
comparison and are fixed: `cplstre` was gated on `nfchans > 1` where the
spec reads it unconditionally (desyncing every mono block), and
`phsflginu` was re-read as a fresh bit in the coupling-coordinates section
instead of reusing the value persisted from the coupling-strategy section
(a spurious bit on any 2/0 stream that sends coupling coordinates).

Despite this, the conformance matrix does not reach bit-exactness and, on
several fixtures, is worse than before this pass, not better — see the
commit that made this change (`fix(codec): verify AC-3 bit-allocation
constants against ATSC A/52:2012 (#367, #368)`) for the measured numbers.
The mono fixture cannot use coupling at all and still shows `max_abs_err`
far above 1.0 after both bug fixes above, which rules out the constants,
the audblk() syntax gates checked in that pass, and coupling as mono's
cause. Per-block mantissa-bit consumption (measured directly from `bap`
sums during decode) exceeds the fixture's fixed CBR frame budget by
roughly 1.5-2x, which says the remaining bug still desyncs the bitstream
somewhere this pass did not isolate — most likely in `compute_bap`'s
address-to-`bap` mapping or in an unverified interaction between exponent
persistence and the mask/psd arithmetic, neither of which was ruled out.
Coupling-channel mantissas remain unimplemented regardless (see the row
below on coupling), which independently accounts for the stereo/5.1
fixtures.

Next step for whoever picks this up: extend the same clause-by-clause
comparison to `mantissa.rs`'s `Quant` dispatch and to whether `exps`/`bap`
array lengths ever silently disagree with `endmant` across a `Reuse`
boundary — both were spot-checked, not exhaustively verified, in this
pass.

### Update 2: grouped-mantissa bugs fixed; mono's desync localised to block 2's bit-allocation output, not yet root-caused

Following the constants-verification update above, pursued the coordinator's
hypothesis that bap 1/2/4's grouped-mantissa handling was the more likely
source of the remaining desync (an arithmetic slip there does not desync
immediately, and shows up as accumulated drift). §7.3.5 confirmed two real,
independent bugs in `mantissa.rs`, now fixed: the decoder equations assign
the *first*-decomposed group digit to the current bin (the code was taking
the *last* one, misordering every group's values without misaligning bit
position); and a group's leftover, not-yet-full members must carry from one
channel's mantissa stream into the next channel's ("the next exponent set
in the block continues filling the partial groups") — `mantissa::decode`
was resetting this state on every call instead of threading one instance
through a whole block's channels-then-LFE sequence.

5.1 improved substantially on this fix (max_abs_err 42.59 -> 16.06, rms
1.54 -> 0.93), consistent with it being real and load-bearing. Mono is
unchanged (6.27 / 0.85): with one channel and no LFE in that fixture there
is no channel boundary for a partial group to hand off to, so this fix
cannot reach mono's own cause.

Localised mono's remaining desync further by independently re-deriving the
raw bitstream (outside this crate, directly from file bytes) at the level
of individual fields: `csnroffst`'s exact bit position, one channel's full
D25 differential-exponent group decode, and the block-to-block bit-position
handoff across all six blocks of frame 0. All matched this crate's own
reader exactly, through block 2's own `snroffset` field — ruling out a
field-order or field-width bug anywhere from the start of the frame up to
that point. What remains: block 2's `csnroffst` (confirmed genuinely large,
not misread) combined with `floorcod == 7`'s `floortab` entry (confirmed
correct against Table 7.10) drives `compute_bap` to assign the maximum bap
to nearly every bin in a block whose signal never exceeds -18 dBFS, and
that over-allocation compounds until block 3 exhausts the frame's actual
6144-bit budget and starts reading past the end of real data. Whether this
is a genuine remaining bug in `compute_bap`'s downstream arithmetic given
those two verified-correct inputs, or a real encoder choice this pass does
not yet understand the interaction of, is the open question for whoever
picks this up next — not yet resolved, and not something to guess at
further without re-reading §7.2.2.5-§7.2.2.7 against a wider set of
`(snroffset, floorcod)` combinations than this pass had time to try.

### Update 3: mono's remaining desync — the snroffset/floor combination arithmetic is eliminated too; not root-caused this pass

One more pass on mono specifically, per the coordinator's framing: the
*fields* (`csnroffst`, `floorcod`, and everything before them in the
frame) are independently confirmed byte-accurate through block 2, so
either a correctly-read field is being *combined* wrongly in
`compute_bap`, or the decoder's response to a genuinely-coded
`(csnroffst, floorcod)` pair is what's wrong. Checked the three places
that arithmetic can go wrong while every field it reads is correct, all
re-verified character-by-character against the raw extracted spec text
(including checking specifically for PDF-extraction dash/minus
ambiguity — the source uses a literal Unicode minus sign throughout,
confirmed at the byte level, no garbling found):

- **§7.2.2.1's `snroffset` composition** — `snroffset[ch] = (((csnroffst
  − 15) << 4) + fsnroffst[ch]) << 2` matches `combine_snroffset` exactly.
  `csnroffst` itself (50, for block 2) was independently re-derived
  directly from the file's raw bytes at its exact bit position, outside
  this crate's own reader — not a misread.
- **The `sdecay`/`fdecay`/`sgain`/`dbknee`/`floor` chain** — all five
  tables (`SLOWDECAY`/`FASTDECAY`/`SLOWGAIN`/`DBKNEE`/`FLOOR`) re-verified
  byte-for-byte against Tables 7.6-7.10, including specifically
  re-confirming `floortab[7] = 0xf800` is meant as two's-complement -2048
  (Table 7.10's own encoding, not an assumption). The combination itself
  — `mask[j] -= snroffset; mask[j] -= floor; if(mask[j]<0){mask[j]=0};
  mask[j] &= 0x1fe0; mask[j] += floor;` — matches `compute_bap` exactly.
  Traced this formula algebraically against block 2's real, byte-verified
  inputs (`snroffset=2296`, `floorcod=7`, so `floor=-2048`): for *any*
  starting mask value in a plausible range, `mask[j] -= snroffset -
  floor` nets to a small number close to zero, survives the `&0x1fe0`
  truncation as itself (or near it), and `+= floor` then drives the final
  mask to roughly -1800 to -2048 regardless of the masking curve's own
  output — which forces `bap` toward its maximum for nearly every bin.
  This is what the pseudocode, executed faithfully, produces from these
  two inputs; no transcription error was found in it.
- **The final `bap`-index clamp** — `address = (psd[i] - mask[j]) >> 5;
  address = min(63, max(0, address));` matches exactly, and `BAPTAB`
  re-verified byte-for-byte against Table 7.16.

None of the three eliminated. What's left open, and not resolved this
pass: block 2's own exponents (independently re-derived from raw bytes
and confirmed to match this crate's decode exactly) are systematically
low — 0 to 18 across 148 bins, implying loud, broadband content — despite
the fixture's global PCM peak being only about -18 dBFS. This is not
proven to be impossible (a coherent, narrow-band transient can produce
transform-domain coefficients well above the time-domain peak; Parseval's
theorem doesn't rule out genuinely broadband, dynamic content producing
this either), so it does not by itself prove a bug — but it also isn't
proof of the opposite, since this pass has no independent way (D7 rules
out the reference decoder's own source) to check what `csnroffst` value
a conformant encoder would actually have chosen for this content. Also
newly found in this pass, unimplemented, and unrelated to this specific
desync (`snroffset` here is nonzero, so it does not apply): §7.2.2.1.1's
special case — "if [csnroffst, fsnroffst[ch], cplfsnroffst, lfefsnroffst]
are all found to be equal to zero, then all elements of bap[] should be
set to zero, and no other bit allocation processing is required" — a real
gap for whoever picks this up next, distinct from the mono desync above.

Per the coordinator's own framing: this is a bounded "here is everything
it is not" rather than a fourth partial fix. #367/#368 stay open.

### AC-3 IMDCT window is a KBD(alpha=5) approximation, not the spec's own table

`vaco-codec-ac3::imdct::kbd_window` approximates AC-3's specific 256-tap
window (ATSC A/52:2018 §7.5.3) with a Kaiser-Bessel-derived window, alpha=5 —
a documented close approximation in the audio-codec literature, not the same
table. The IMDCT itself is exact (general Princen-Bradley transform math,
verified independently of any codec-specific table). Swapping in the real
window needs the same primary-text access the bit-allocation tables need;
until then this is a second, additive source of the measured error above.

### AC-3 coupling and delta-bit-allocation are parsed but not reconstructed

`vaco-codec-ac3::audblk::decode` reads every coupling-related field
(`cplinu`, `chincpl`, `cplcoexp`/`cplcomant`, the coupling-band structure) and
every delta-bit-allocation field (`chdeltbae`, `deltoffst`/`deltlen`/`deltba`)
correctly enough to stay bit-aligned, but does not apply either to
reconstruction: a coupled channel's shared high-frequency spectrum is left
silent rather than rebuilt from the coupling channel, and delta bit
allocation's fine-tuning adjustment to the SNR curve is ignored. Both are
real gaps for content that uses them (coupling especially, for 5.1 content
at lower bitrates than this session's fixtures used). Fixing coupling needs
building the shared coupling-channel spectrum and redistributing it across
`chincpl` channels per `cplcoexp`/`cplcomant`; fixing delta bit allocation
needs applying the transmitted per-segment offset directly to
`bitalloc::compute_bap`'s mask before the SNR lookup.

### AC-3 dual mono (`acmod == 0`) is approximated as stereo, unmeasured

`vaco_format_ac3::tables::acmod_layout` maps `acmod == 0` (two independent
mono programme channels) to `ChannelLayout::STEREO`, on the strength of
`vaco-demux-raw::ac3`'s measurement that the reference also reports it as
`stereo` — but that measurement never actually produced a real `acmod == 0`
encode to check the *samples* against (this `ffmpeg` build's `-c:a ac3`
encoder was not observed emitting dual mono from any input tried this
session). The channel-count and container-level report are probably right;
whether the two decoded channels are independent programmes rather than a
matrixed L/R pair has not been checked against real dual-mono audio.

### `vaco-demux-raw::ac3` and `vaco-format-ac3` both parse the AC-3 syncframe

`vaco-demux-raw::ac3` (landed first, for #653) keeps its own inline
sync/frame-size parser rather than depending on the `vaco-format-ac3` crate
added afterward for the decoder — mirroring `vaco-format-mpegaudio`'s role
for the mp3 family would mean the demuxer used the shared crate too. Not
unified in the same session: `vaco-demux-raw::ac3` was complete, tested and
closing an issue before `vaco-format-ac3` existed, and re-pointing a working,
already-verified demuxer at a brand-new crate risked destabilising closed
work for a cosmetic win. The two parsers were checked against the same real
fixture bytes independently and agree; a future change should make the
demuxer depend on `vaco-format-ac3::syncinfo` and delete its own copy.

### AC-3 IMDCT is a direct O(n²) sum, not a fast transform

`vaco-codec-ac3::imdct::imdct` computes every output sample as a full sum
over all input coefficients rather than a butterfly/FFT-based fast IMDCT.
Deliberate for this session (correctness first, and a fast transform is a
much larger place to introduce a subtle bug than a direct sum), but it is
the reason this crate's own conformance test takes noticeably longer per
fixture than its frame count would suggest. A real-time decoder needs the
fast form; `vaco-tx` (this workspace's existing transform crate, used by
`vaco-codec-mpegaudio`) is the obvious place to look for a reusable kernel
before writing a new one.

### E-AC-3 AHT and spectral extension are not implemented

`vaco-codec-ac3::eac3` walks independent/dependent substream structure and
refuses (`Eac3Error::NotImplemented`, currently unreachable — see the
module's own doc comment) rather than attempts AHT (Adaptive Hybrid
Transform) or spectral extension reconstruction. Both replace substantial
parts of classic AC-3's exponent/bit-allocation/mantissa pipeline for the
bins they cover, and implementing them from specification recall alone,
against fixtures generated by an encoder that may not even exercise them,
was judged higher risk than value for this session — a confidently wrong
reconstruction is a worse outcome than a clear refusal. The module is also
entirely behind the non-default `patent-unverified-eac3-decode` feature
regardless (E-AC-3 decode's patent status is unresolved per D9), so nothing
here reaches a build anyone ships until that is settled separately.

### `Muxer::bind_url`/`Demuxer::bind_url` have no options channel through the registry

Closing gaps 2/7 for `image2` (`vaco-mux-image2`/`vaco-demux-image2`) means
`RegistryMuxer::bind_url`/`RegistryDemuxer::bind_url` construct
`Image2MuxOptions::default()`/`Image2Options::default()` — there is no way
for `-pattern_type`, `-start_number`, `-update`, `-strftime`, `-frame_pts`
or `-atomic_writing` to reach the registry path at all, only
`Image2Demuxer::open_pattern`/`Image2MuxWriter::create` called directly.
This is the same shape gap 5's `Muxer::set_option` closed for muxer-private
options generally; a `Demuxer::set_option` mirror (there is none yet) would
be the natural place to thread these through before `bind_url` constructs
the real demuxer/muxer, rather than inventing a second, `bind_url`-specific
options channel.

### `Discovery<D>`'s `Demuxer` impl does not forward `bind_url`

Consistent with `Demuxer::reconfigure` (gap 4), which `Discovery::run` calls
directly on the owned inner value it still has by concrete type, not
through `Discovery`'s own `impl Demuxer for Discovery<D>` — so neither
method is reachable by calling `.bind_url()`/`.reconfigure()` on a
`Discovery`-wrapped demuxer through the trait object. `vaco-cli::input::open`
calls `bind_url` on the raw registry-constructed demuxer *before* wrapping
it in `Discovery::new`, so this does not affect #649, but it is a real trap
for a future caller who wraps first and expects the trait method to reach
through: unlike `Box<dyn Muxer>`/`Box<dyn Demuxer>` and `vaco-cli`'s
`TallyingMuxer` (which do forward both `add_stream_with`-shaped methods and
`bind_url` explicitly, per those types' own doc comments), `Discovery` was
never meant to be driven that way and its docs do not yet say so.

### `vaco-codec-mpegaudio` Layer III does not decode real content accurately yet

Side-info parsing, the bit reservoir, MS stereo, alias reduction and the
requantisation formula are all implemented, and two real bugs in this path
were found and fixed by comparing decoded PCM to `ffmpeg` (the global-gain
constant, and a silent-granule Huffman-decode loop reading past its
`part2_3_length` budget) — but real-file decode is still measurably wrong:
a 440 Hz test tone reaches only ~0.44 sample correlation against
`ffmpeg`'s own decode after finding the best time alignment, and a 6000 Hz
tone comes out at a measurably wrong output frequency (~4316 Hz instead of
6000 Hz). A dedicated unit test
(`vaco-codec-mpegaudio`'s `layer3::frequency_placement_tests`) proves the
subband-splitting/IMDCT/windowing/overlap-add/synthesis half of the
pipeline places a known spectral line at its correct frequency in
isolation, which narrows the remaining bug to the side-info/Huffman-decode
half without identifying it. Next step: compare `is[]` (post-Huffman,
pre-requantisation) for one hand-constructed granule against a known-good
value, since the transform half is already ruled out.

### `vaco-codec-mpegaudio` Layer III short blocks (`block_type == 2`) decode to silence

Neither the short-block scalefactor layout (band-major, window-minor over
12 bands × 3 windows) nor the per-window 12-point IMDCT reassembly (three
windowed 12-sample blocks overlapped with a 6-sample stride, then padded
with 6 zero samples at each end to reach 36 — worked out from ISO/IEC
11172-3's own prose describing the process, but not yet implemented) is
built. A `block_type == 2` granule's side info parses correctly and the
granule still resynchronises to its declared `part2_3_length`, so this
does not corrupt anything else in the frame, but that granule's own audio
is lost (rendered as silence) rather than decoded. Most likely to matter on
transient/percussive material, which is exactly what triggers short
blocks.

### `vaco-codec-mpegaudio` has no intensity stereo for any layer

Layer I/II's `intensity_stereo` channel mode (only one subband's worth of
allocation/scalefactor/samples transmitted above `bound`, shared by both
output channels with independent scalefactors) and Layer III's
`mode_extension` intensity bit (the "side" channel's scalefactors reused as
`is_ratio = tan(is_pos·π/12)` positions) are both unimplemented. Content
encoded with either falls back to treating both channels as independently
coded, which is wrong for the shared subbands/bands. Plain stereo, dual
channel, mono, and (Layer III only) MS stereo all decode correctly.

### `vaco-codec-mpegaudio` MPEG-2/2.5 (low sample rate) Layer III is `Unsupported`

The low-sample-rate extension's different `scalefac_compress` decomposition
(three ranges of the 9-bit field, further split under intensity stereo) is
not implemented; `MpegAudioDecoder` returns `Error::Unsupported` for any
16000/22050/24000/11025/12000/8000 Hz Layer III packet rather than
attempting a wrong decode. Layer I/II's low-sample-rate bit-allocation
table (`LAYER2_TABLE_LSF`, `provenance/vaco-codec-mpegaudio.toml`) is
transcribed from ISO/IEC 13818-3 but untested against real audio — no
MPEG-2/2.5 encoder was available on this machine to generate a fixture.

### `vaco-codec-mpegaudio` does not apply the demuxer's gapless trim

`vaco-demux-mpegaudio` (issue #644) already produces `SkipSamples` packet
side data from the LAME tag's encoder delay/padding, but nothing consumes
it: `MpegAudioDecoder::send_packet`/`receive_frame` decode every sample the
bitstream carries, including the encoder's priming delay and trailing
padding. Trimming needs a component that owns both the packet's side data
and the decoded frame's sample count — today that is neither this crate
(which only sees one packet/frame at a time, with no stream-level state for
"how many total samples has this stream produced so far") nor `vaco-cli`
(issue #652 — no decoder is reachable from the CLI at all yet).

### `vaco-scale` cannot produce sub-byte-packed or float output — RESOLVED

Both closed the same session, through an integer proxy rather than by
teaching `geometry`/`rowio` a second sample shape:

- **`monowhite`/`monoblack`** (pbm/xbm's only accepted format, 1-bit-per-pixel
  packed): `vaco-scale::special` runs the ordinary pipeline into a `gray8`
  proxy, then packs it 1-bit-per-pixel with an *ordered* dither. The
  threshold table is measured, not invented: a synthetic ramp shaped so
  every `(x mod 8, y mod 8)` position sees every 8-bit value showed exactly
  one flip per position as the source value swept 0..255 (the signature of a
  positional/Class A dither, not error diffusion), cross-checked against an
  unrelated gradient image at 0 mismatches out of 1024 pixels. `monoblack` is
  measured to be `monowhite`'s exact bitwise complement. End-to-end
  (`gray8` -> `monowhite` through the real `Scaler`) matches the table
  exactly. Through an RGB/BGR source the packed bits sometimes differ from
  the reference by a bit here and there (23 of 137 bytes on a 32x32 `bgr24`
  `testsrc`) — that traces to the RGB-to-Gray8 rounding row directly below,
  which shifts a source pixel by 1 luma unit often enough to cross one of
  the dither table's ~3-4-unit-wide thresholds. Not a new deviation; the old
  one amplified by a threshold.
- **The eight float formats** (`grayf32le/be`, `rgbf32le/be`, `grayf16le/be`,
  `rgbf16le/be` — pfm/phm's only accepted formats): proxied through
  `gray16le`/`rgb48le`, linearly rescaled at `f = v / 65535.0` (measured
  against the reference exactly: `gray16le` 1/32768/65535 -> `grayf32le`
  1.5259022e-5/0.5000076/1.0). Both directions work (float source and float
  destination), not only the destination direction the symptom described.
  `f16`'s bit conversion has no reference to measure against — this
  machine's `ffmpeg` lists `grayf16le`/`rgbf16le` decode-only (`I` with no
  `O` in `-pix_fmts`), so it cannot itself write one — and is instead
  checked by its own round-trip property test over all 65536 `u16` inputs.

Left open, found while verifying: `vaco-codec-pnm`'s `encode_pfm` writes the
PFM header's byte-order scale field with the opposite sign from the
reference (`-1.000000` vs `1.000000` for the same `rgbf32le` data) — the
*pixel* bytes are correct (decoding both files back through `ffmpeg` and
comparing agrees to within 1 of 255, the same rounding character as the
row below), only the header's stated endianness disagrees. Not `vaco-scale`'s
file to fix.

### `vaco-scale` RGB-to-Gray8 has a one-off rounding deviation, ~1% of pixels

Measured converting a 32x32 `testsrc` through the new decode-to-encode
converter to two different Gray8-only encoders (`vaco-codec-pnm`'s pgm from
both an `rgb24` and a `bgr24` source): 10 of 1024 pixels differ from the
reference by exactly 1 (out of 255), the rest are bit-exact. Max absolute
deviation 1, RMS 0.099. Looks like a luma-coefficient rounding-direction
difference (round-half-up vs round-half-to-even, or an off-by-one in
`vaco-scale::colour`'s fixed-point scale) rather than a wrong matrix — every
other measured conversion through the same converter (`bgr24`/`gray8` to
`rgb24`/`rgba` for the qoi encoder, four source/target combinations) was
byte-identical. Not chased further here since it is `vaco-scale`'s own
colour path, not the wiring this session added.

### Three image encoders have pre-existing header/output bugs, found while verifying the new converter

Unrelated to the decode-to-encode conversion stage (each pair's pixel data
converts correctly; the divergence is in the encoder's own output), found
while building the `bmp`/`ppm`/`pgm`-to-various-encoders verification table
against `ffmpeg` 8.1:

- `vaco-codec-image-simple`'s pcx encoder: exactly one byte differs from the
  reference (offset 13 in a 1327-byte file), 0x01 vs 0x00 — a header field,
  not pixel data.
- `vaco-codec-image-simple`'s targa encoder: output diverges from byte 3
  (the image-type field) onward.
- `vaco-codec-image-simple`'s xwd encoder and `vaco-codec-pnm`'s (n/a) —
  correction, this one is xwd only: our output is 10 bytes shorter than the
  reference's (3173 vs 3183) and `cmp` hits EOF on ours first.
- `vaco-codec-image-simple`'s sgi encoder: our output is roughly 40% the
  reference's size (1536 vs 2513 bytes) and `cmp` hits EOF on ours first —
  looks like a missing RLE fallback-to-raw case or a truncated scanline
  table, not investigated further.

None of these touch pixel-format conversion; they are encoder-internal and
belong with whoever owns `vaco-codec-image-simple` next.

### `vaco-demux-image2` has no pipe-splitter entry for TGA at all

Separate from the "registered but mapped to no `CodecId`" gap #655 reported
(fixed this session for the eleven formats that already had a `pipe!` row):
TGA has no row in `crates/format/vaco-demux-image2/src/pipe/mod.rs` in the
first place, so `.tga`/`.targa` cannot be opened as an `image2` input at all,
by extension or by content — there is no demuxer to reach. The reference
registers a `tga_pipe` demuxer (`ffmpeg -demuxers` lists it); TGA has no
fixed magic number, so its `pipe!` entry would need `magics = &[]` and rely
on the extension, the same shape already used for `photocd`/`pictor`/`gem`/
`svg`/`vbn`/`qdraw`/`pgmyuv` in that file.

### `hevc_metadata`'s `aud` was not ported alongside `h264_metadata`'s (gap 12)

`h264_metadata`'s `aud=insert`/`remove` shipped this session (interface gap
12); `hevc_metadata` exposes the identical option name and was left at the
bare-name default rather than mirrored. Not an oversight: HEVC's NAL header
is two bytes (`forbidden_zero_bit(1) nal_unit_type(6) nuh_layer_id(6)
nuh_temporal_id_plus1(3)`, `vaco_format_nalu::HeaderKind::H265`) rather than
H.264's one, and the AUD payload/`pic_type` layout for HEVC was not measured
in this pass — porting the H.264 logic unchanged would be guessing the byte
layout, not reproducing it. Whoever picks this up should measure
`ffmpeg 8.1 -bsf:v hevc_metadata=aud=insert/remove` the same way
`h264_metadata`'s module doc did (five-ish adversarial HEVC streams: I-only,
already-AUD'd, non-16-multiple crop, forced VUI, a B-frame GOP) before
reusing any of the H.264 byte offsets.

### `vaco_frame::Frame` has no picture-type field at all

Found while implementing `showinfo` (interface gap 13): the reference's
`type:I`/`P`/`B` field comes from `AVFrame::pict_type`, which this
workspace's `Frame` has no equivalent of — not because it was scoped out for
`showinfo` specifically, but because no decoder in this workspace exists to
set one (D5). `showinfo` hard-codes `type:I`, which happens to be correct
for every frame this workspace's filter graphs can currently produce
(source-generated or filter-transformed, never decoded), and is recorded
as such in that filter's own doc rather than silently guessed. The same gap
will resurface for any future filter or `vaco-probe -show_frames` field that
needs a real picture type — worth a `FrameFlags`-adjacent field (or a fourth
`FrameSideData` variant, if it should stay optional) once a real decoder is
in the tree to populate it, rather than each caller re-discovering the
absence independently.

### `FrameData::Subtitle` has no producer, and its `SubtitleContent::Bitmap` layout is unverified against any real decoder

Landed to close interface gap 17, and honestly incomplete in one way worth
tracking rather than discovering again: the shape (`x`/`y`/`w`/`h`/`forced`
plus `Bitmap`/`Text`/`Ass` content) was designed from the reference's own
documented option surface and `AVSubtitleRect`'s well-known field set, not
fitted to any of the three in-flight T2-13 decoder crates' actual internal
representations, because touching those crates was explicitly out of scope
while they are mid-write. In particular `SubtitleContent::Bitmap`'s
`stride`/`data`/`palette` split is a guess at what a DVB/VobSub/PGS decoder
will find convenient to hand over — the first of those three crates to
actually construct a `FrameData::Subtitle` should treat this shape as a
draft, not a contract, and report back if it does not fit (a palette
ordering mismatch, a stride convention that does not match what RLE
decoding naturally produces, a rect that needs to carry more than these six
fields). Cheap to change now, while nothing depends on it; expensive once a
decoder and a consumer both exist.

### `vaco-codec-subtitle-teletext`'s module doc is stale as of gap 17 closing

That crate's own doc still says `vaco_frame::FrameData` has exactly two
variants — true when it was written, false as of this session's
`FrameData::Subtitle` addition. Not fixed here: the crate is mid-write by
another agent and out of this session's scope by explicit instruction.
Whoever next touches that crate's docs should update the claim (and check
whether the rest of that module's reasoning about "nowhere to put decoded
output" still holds now that there is somewhere).

### `SubtitleContent::Bitmap`'s shape fits `vaco-codec-subtitle-bitmap`'s decode output well -- reporting back per the request above

The entry above ("`FrameData::Subtitle` has no producer...") asked the
first of the three T2-13 decoder crates to construct one to report whether
`stride`/`data`/`palette` fits. It does, closely: DVB/PGS/VobSub decode in
this crate all produce a `vaco_format_subtitle_bitmap::IndexedBitmap` --
`rect` (`x`/`y`/`w`/`h`), a `Palette` of up to 256 `Rgba` entries, and a
row-major `Vec<u8>` of indices with no padding between rows -- so
`stride` is always exactly `w`, never something the decoder has to compute
separately, and `palette.entries().iter().map(|c| [c.r,c.g,c.b,c.a])`
converts directly to `Vec<[u8; 4]>`. Nothing about this session's three
decoders needed a different rect field, a different palette entry count
cap, or a stride wider than the logical width.

Not wired up in this session: `vaco-codec-subtitle-bitmap` was built and
committed as a standalone library before gap 17 closed (its own top-level
doc comment explains why, referencing the gap as still-open), and this
session's remaining scope did not include the `Decoder`/registry/
`vaco-component.toml` plumbing that would actually construct
`FrameData::Subtitle` values from its `SubtitleEvent`/`IndexedBitmap`
output. That plumbing is a well-scoped follow-up now that both halves
exist: a `Decoder` impl per `CodecId` (`DvbSubtitle`/`DvdSubtitle`/
`HdmvPgsSubtitle`) translating this crate's already-working decode
functions' output through `SubtitleRect::bitmap` into `FrameData::Subtitle`.

### Nothing parses a Matroska `S_VOBSUB` track's `CodecPrivate` into a `Palette`

`vaco-demux-matroska`'s codec table maps `S_VOBSUB` to `CodecId::DvdSubtitle`
by name only (`crates/format/vaco-demux-matroska/src/codec.rs`); nothing
reads the track's `CodecPrivate`, which for a real `S_VOBSUB` track is the
literal `.idx`-file text (`size:`/`palette:` lines) per the Matroska
specification's own convention for this codec. `vaco-codec-subtitle-bitmap`'s
`vobsub::decode_spu` takes the 16-entry `Palette` as a plain parameter
specifically to match `VobSubDemuxer::palette()`'s existing shape (see that
function's own doc comment), which covers the `.idx`/`.sub` pair path --
but there is currently no caller on the Matroska path that could produce
that `Palette` at all, since nothing parses `CodecPrivate` into one.
`vaco-subtitle-bitmap::vobsub::idx::parse_palette` already does exactly the
parsing a `CodecPrivate`-reading caller would need; the gap is that nothing
in `vaco-demux-matroska` calls it.

### The registered `dvbsub` demuxer's fixed-chunk framing is not display-set aligned, so its decoder has to buffer

`vaco-subtitle-bitmap::dvbsub::DEMUXER` deliberately matches the measured
reference (`ffmpeg -h demuxer=dvbsub`'s raw chunk reader, see that crate's
own docs) rather than doing any segment-aware framing -- correct against
the measured reference, but it means a packet from that specific demuxer
can split a `region_composition_segment` or an `object_data_segment` right
down the middle. `vaco-codec-subtitle-bitmap::dvb::DvbSubDecoder` copes by
buffering pushed bytes until it can walk a complete chain ending in
`EndOfDisplaySet` before decoding (see its own doc comment) rather than
assuming one push is one epoch. Real DVB delivery over MPEG-TS does not
have this problem (`data_alignment_indicator` means one PES payload is
normally one whole epoch), so this only matters for a caller driving the
raw `dvbsub` format directly -- worth knowing if a future MPEG-TS wiring
ever wants to hand this decoder individual segments instead of whole PES
payloads, since the buffering assumes it may have to reassemble.

## `provenance-check` is red: five orchestrator commits lack `Signed-off-by`

`cd56f8c4`, `b0f4fd15`, `c6ce3870`, `2903fc20` and `0c9369bc` carry the three
`Vaco-*` trailers and no `Signed-off-by:`. CI runs `cargo xtask
provenance-check` (`.github/workflows/ci.yml`), so the workflow fails on every
push until these are fixed. Nothing else in the tree fails that gate.

Cause: `git commit-tree`, which the private-index recipe uses, does not add the
trailer the way `git commit -s` does. The trailer block was copied from
`git log -1 --format='%B' | tail -4`, and four lines was one short — it showed
the three `Vaco-*` lines and cut off the `Signed-off-by:` above them. Later
commits from the same recipe include it.

Fix: rewrite those five commits to add the trailer and force-push. Deliberately
**not** done when found — nine agents were committing into the shared tree, and
a rewrite plus force-push would orphan any commit landing in the window. The
trees are identical either way, so this is metadata-only and safe to do the
moment the tree is quiet. Do it before anything else at the next lull.

Do **not** "fix" this by widening the gate or by moving `provenance/baseline`
forward: the baseline exempts every commit in between, trading one bad record
for a hundred unchecked ones, and `provenance/corrections.toml` maps a citation
to a registered source id, which is a different axis entirely.

### `SubtitleContent::Text` fits `vaco-codec-subtitle-cc`'s decode output too, same as the bitmap crate above

Same shape of gap as the entry above, for the third T2-13 decoder:
`vaco-codec-subtitle-cc`'s `Event::Cea608`/`Event::Cea708` carry a
`Screen` (a sparse, row-sorted set of styled `Cell`s) whose `Screen::text()`
method already produces exactly the plain string `SubtitleContent::Text`
wants. Not wired up in this session, for the same reason: this crate was
built and committed as a standalone library before gap 17 closed (its own
top-level doc comment explains the correction), and this session's scope
did not include the `Decoder`/registry/`vaco-component.toml` plumbing.

Unlike the bitmap crate, this one has an extra design question the bitmap
case does not: `CodecId::Eia608` covers both CEA-608 and CEA-708, and this
crate's real input is a per-frame `cc_data` side-data buffer, not a
bitstream a `Decoder::send_packet` would demux — so "what is a packet
here" needs an answer before the `Decoder` impl can be written, not just a
translation of already-working output through `SubtitleRect::text`.

### `vaco-format-misc-audio`'s `adx`/`g726`/`g726le` state duration at a different tick rate than the reference

`BlockDemuxer`'s `time_base` is always `1/sample_rate`, so a packet's `pts`
counts samples. The reference's own `adx` demuxer instead ticks at `1/250`
(one tick per 32-sample block) and its raw `g726`/`g726le` demuxers tick at
a generic `1/90000`; both agree with this crate's wall-clock duration
exactly (`0.304 s` and `0.3 s` on the measured fixtures) but disagree on
`duration_ts`/`time_base` themselves. `crates/format/vaco-format-misc-audio/tests/differential.rs`
checks duration in microseconds for exactly this reason. Reproducing the
reference's tick rate per format would mean `BlockDemuxer` taking a
caller-supplied `time_base` instead of deriving one from `sample_rate`, and
`adx` additionally reporting `duration_ts` in blocks rather than samples.

### `vaco-format-misc-audio`'s `aptx`/`aptx_hd` estimate a duration the reference declines to state

Both codecs have a fixed 4:1/6:1 byte:frame ratio, so
`crates/format/vaco-format-misc-audio/src/block.rs`'s `BlockDemuxer::duration`
estimates one from the file size — the same policy
`vaco-format-audio-simple::pcm::RawPcmDemuxer` uses for headerless PCM. The
reference's own raw `aptx`/`aptx_hd` demuxers report `N/A` instead. Not
changed, since matching `N/A` would mean discarding a number this crate can
compute exactly; recorded because a future differential pass comparing
`duration_ts` field-for-field will flag it as a divergence and should not
re-litigate the question from scratch.

### The `fuzz` workspace could not be built to actually run `misc_audio_demux`

At the time `vaco-format-misc-audio` landed, `crates/signal/vaco-scale/src/scaler.rs`
had an uncommitted, in-progress edit calling a `plan_spec` function that did
not exist yet (`special.rs` was untracked), which fails
`cargo check`/`cargo fuzz run` for the whole `fuzz` package — every fuzz
target shares one `Cargo.toml`, so one crate's broken mid-edit state blocks
all of them, not just its own. `fuzz/fuzz_targets/misc_audio_demux.rs` is
written and registered (`cargo xtask gen-fuzz` ran cleanly), but was never
actually executed with `cargo +nightly fuzz run` in this session; a
proptest-based stand-in (`tests/properties.rs`'s
`no_demuxer_panics_or_loops_on_arbitrary_bytes`) covers the same
no-panic/terminates property in the meantime. Re-run the real fuzz target
once `vaco-scale` builds again.

### Correction: the `misc_audio_demux` fuzz gap above closed within the same session

`vaco-scale` was fixed by whichever agent owned it shortly after the entry
above was written. `cargo +nightly fuzz run misc_audio_demux --
-max_total_time=60` then ran cleanly: `exit=0 execs=#288767`, and
`find fuzz/artifacts -type f` is empty. Left the original entry in place
rather than deleting it, since the record of *why* it was missing at the
time is still accurate; this note is the resolution.

### `vaco-subtitle-text`'s ASS demuxer emits a bare `Text` field, not the reference's nine-field chunk

Measured while building `vaco-codec-subtitle-text` (C-04). The reference's ASS
demuxer hands its decoder a nine-field chunk with the timestamps stripped —
`ffmpeg -i in.ass -c:s copy -f data -` gives
`0,0,Default,Speaker,5,6,7,fx,{\i1}hi{\i0} there, with, commas`, i.e.
`ReadOrder,Layer,Style,Name,MarginL,MarginR,MarginV,Effect,Text`. This
workspace's `crates/format/vaco-subtitle-text/src/ass.rs` instead puts only the
`Text` field in the packet (`parts.last()`), discarding layer, style, actor,
margins and effect.

Nothing is broken today: the decoder handles both shapes, and the ASS *muxer*
re-emits from `Cue` so a demux/mux round trip is lossless in practice. But a
consumer that wants the style name (any real ASS renderer does) cannot get it
from the packet, and a `-c copy` from ASS into Matroska will not produce the
byte stream Matroska readers expect, because that container carries the
nine-field chunk. Belongs to whoever owns `vaco-subtitle-text` next; the
decoder side is already written to accept the reference shape when it arrives.

### `FrameData::Subtitle` wiring for the text subtitle decoders

`vaco-codec-subtitle-text` is a standalone library with no `Decoder` impl, for
the same reason `vaco-codec-subtitle-bitmap` and `vaco-codec-subtitle-teletext`
are: interface gap 17 was still uncommitted in another agent's tree when it was
written, and a crate at `HEAD` calling into it would not build for anyone else.

Unlike the other two, the target type fits with nothing left over —
`SubtitleContent::Ass` is exactly what every decoder in that crate emits, and
that variant's own docs name ASS/SSA as its reason for existing. Each `to_ass`
returns the `String` that `SubtitleRect::ass(0, 0, 0, 0, false, …)` wants, so
the wiring is a `Decoder` impl and a `vaco-component.toml` fragment per codec
and nothing more. Worth doing in one pass across all three subtitle codec
crates rather than three times.

### The generated `fuzz/Cargo.toml` lists every path dependency unconditionally

So one crate that does not compile blocks **every** fuzz target in the tree, not
just its own. Hit twice in one session: `vaco-codec-ac3` (a call to
`all_snr_offsets_raw_zero`, which did not exist) and then `vaco-codec-vp8` (a
`decode_macroblock` call missing an argument) — both transient mid-edit states
in other agents' crates, both of which made
`cargo fuzz run subtitle_text_decode` fail to build. `--no-default-features
--features <mine>` does not help, because the `[dependencies]` block is not
feature-gated; only waiting does.

`xtask/src/gen_fuzz.rs` already computes `referenced` (the crates each target
actually mentions) and could emit each path dependency as
`optional = true` with the per-crate feature turning it on, which is what the
existing `required-features` on each `[[bin]]` already implies. That would make
a single-target fuzz build independent of the rest of the tree. Not attempted
here — it is a change to a generator every agent depends on, and it wants its
own package rather than riding along with a codec crate.

### `WebVTT` character references: reference and specification have diverged

`vaco-codec-subtitle-text` implements the six names `WebVTT` defined before it
adopted HTML's full table in November 2015 (`&amp;`, `&lt;`, `&gt;`, `&lrm;`,
`&rlm;`, `&nbsp;`), because that is what the reference implements — measured:
`&quot;`, `&apos;`, `&hellip;`, `&#65;` and `&#x42;` all come back verbatim from
`ffmpeg -f ass -`. The current spec (§4.2.2, §6.4) requires HTML's ~2,200-name
table plus numeric references with the missing-semicolon longest-match rule.

Recording it because it is the one place in that crate where "match the
reference" and "match the specification" give different answers, and a future
agent reading only the spec would reasonably think the six-name table is a bug.
If the reference ever gains the full table, the change is confined to
`webvtt::entity`.

### `vaco-format-misc`'s `roq` stream discovery stops at the first packet, not the first audio-or-video pair

`RoqDemuxer::open` (`crates/format/vaco-format-misc/src/roq.rs`) has to decide
whether an audio stream exists before returning `streams()` for the first
time, but the container states nothing about audio up front — the only way to
find out is to read chunks. The loop it uses stops as soon as **any** packet
(`self.pending` gains an entry) is produced, not specifically after a video
packet. For every real-world layout this format's own documentation shows —
audio before the first codebook, repeated every frame — that is the same
thing, because the first chunk that produces a packet at all is the audio
chunk. But a file that opens with video only and interleaves its first audio
chunk starting at, say, frame 50 would report `streams().len() == 1` forever:
discovery already stopped at frame 1's video packet and never looks again.

Not fixed here because no such file exists to verify against — every hand-built
fixture and every RoQ file this crate's author is aware of interleaves audio
from the start. The honest fix is either a larger, unconditional lookahead (at
real cost: `MAX_LOOKAHEAD_CHUNKS` bounds it at 4096 chunks today, chosen for
"generous for a real file," not for "provably scans past a stream nobody
happens to use immediately") or accepting that a late-starting audio stream is
a genuine documented divergence, the way `vaco-demux-flv`'s equivalent
first-tag-creates-a-stream problem (`AGENT-CONSTRAINTS.md`'s "an empty
collection at construction is not an answer") was accepted rather than solved
generically. Whoever next touches `roq` should decide which, with a real
multi-hundred-frame fixture in hand rather than a synthetic three-chunk one.

### Correction: the generated `fuzz/Cargo.toml` blocking-on-one-crate item above is resolved

`xtask/src/gen_fuzz.rs` now emits every path dependency as `optional = true`,
gated behind the feature named after the crate whose target declares it —
exactly the change this entry proposed and did not attempt. `default` still
lists every feature, so a plain `cargo fuzz run <target>` (no flags) keeps
building everything, matching the existing behaviour when the tree is
healthy; `--no-default-features --features <feature>` now genuinely scopes
the build to one target's own crate and whatever it references.

Verified behaviourally: introduced a one-line unclosed delimiter into
`vaco-codec-golomb/src/lib.rs` (idle at the time, unrelated to `vaco-expr`),
then built and ran `expr_parse --no-default-features --features expr` while
`vaco-codec-golomb` stayed broken on disk. It built and ran to completion
(exit=0, execs=4939095) without ever compiling the broken crate; the same
target without `--no-default-features` failed to build, confirming the flag
is load-bearing and `default` alone does not fix the blocking. Reverted the
golomb file immediately after.

`Justfile`'s `fuzz` and `fuzz-all` recipes previously passed neither
`--features` nor `--no-default-features` at all — worse than the incantation
the briefs documented, since they always built the full default set
regardless of target. Both now resolve the target's feature from the
generated manifest and pass `--no-default-features --features <feature>`.

Not done here, reported instead: `planning/AGENT-CONSTRAINTS.md` and
`planning/AGENT-BRIEF-TEMPLATE.md` both still say
`cargo +nightly fuzz run <target> --features <feature> -- ...` with no
`--no-default-features`, which is now the difference between an isolated
build and a build that still depends on every other crate in the tree
compiling. Whoever edits those next should add the flag.

### `SplitMix64` is now duplicated in three filter crates (`vaco-filter-temporal`, `vaco-filter-source`, `vaco-filter-artistic`)

Each of `vaco-filter-temporal::rng`, `vaco-filter-source::rng` and this
crate's `vaco-filter-artistic::rng` carries its own byte-identical copy of
the same small `SplitMix64` PRNG (Vigna, public domain/CC0), each with its
own doc explaining why it exists (a filter with a `seed` option whose actual
generator would need reading the reference's source, so only
*reproducibility*, not bit-identical output, is asked of it: `random`,
`cellauto`/`life`/`sierpinski`-style sources, and now `noise`). Not
consolidated here: the obvious host per `planning/16-filters.md` §4.1 is
`vaco-filter-vdsp` (shared video kernels crossing crate boundaries), and
`planning/ASSIGNMENTS.md` lists it `assigned` to `agent:analysis2` as of
2026-08-28 — this pass does not own it and did not touch it. `dup-check`
will not catch this on its own: it compares type names across crates, and
`SplitMix64` is spelled identically in all three, so it would only surface
if two of them tried to `pub use` the same path, which none do. Whoever next
owns `vaco-filter-vdsp` (or whoever does the filter-crate reconciliation
sweep `planning/FILTER-CRATE-DIVERGENCE.md` already tracks) is the natural
one to move it there and update three `use` statements.

### `vaco-cli`'s `exec.rs` discards a `Decoder::set_extradata` refusal with no counter

Closing interface gap 19 (`INTERFACE-GAPS.md`) added `p.extradata.as_deref()`
offered to the decoder at `exec.rs`'s `decoder_desc.build(limits)` call site,
discarded with `let _ =` on the theory that offering extradata is not a
promise it will be used — the same convention `vaco-format-core::discovery`'s
`build_parser` already uses for `Parser::set_extradata`, and the one
`Decoder::set_extradata`'s own doc states explicitly.

`AGENT-CONSTRAINTS.md`'s rule about a discarded fallible call ("make the
discard countable") applies here in principle, and nothing in `vaco-cli`
currently counts it. In practice the only implementor as of this change
(`VobSubSubtitleDecoder::set_extradata`) always returns `Ok(())` regardless of
whether the bytes parsed to a real palette, so there is no error path being
silently swallowed today — but a future decoder whose `set_extradata`
legitimately fails (a malformed `avcC` it cannot even partially use, say)
would fail exactly as invisibly as `H264Parser::set_extradata` did for
months. `exec.rs` has no logging or warnings channel at all right now
(checked: no `tracing`/`log`/`eprintln!` anywhere in the file), so wiring
this properly means either adding one or growing `RunSpec` a field for
degraded-but-not-fatal configuration — both bigger than this change's scope.
Whoever adds the next `Decoder::set_extradata` override with a real failure
mode should revisit this call site first.

### `vaco-codec-subtitle-bitmap`'s `VobSubSubtitleDecoder` cannot shift `Frame::pts` for a delayed `SP_STA_DSP`

Closing interface gap 20 found its own premise wrong — `Frame::duration` is
`vaco_core::Duration` (always real microseconds), not ticks of the stream's
time base, so the codec's own display *length* converts with no time base at
all and now does (`decoder.rs`'s `display_duration`). What is still true and
still open: `VobSub`'s `SP_STA_DSP` can state a display *start* delayed past
the packet's own PTS (`SubtitleEvent::start` non-zero), and expressing that
delay as a shift on `Frame::pts` (a tick count in the stream's time base)
would need that time base, which no `Decoder` in this tree receives. `Frame::pts`
is therefore left equal to `packet.pts` unconditionally, and a stream that
sets a non-zero start delay will show its subtitle slightly early.

No test can currently fail on this, because nothing in the fixture set states
a non-zero `SP_STA_DSP` delay — `vobsub_spu()` (`decoder.rs`'s own test
helper, and `vobsub.rs`'s private `sample_spu()` it was copied from) always
puts `STA_DSP` at `SP_DCSQ_STM = 0`. Whoever finds or builds a real disc SPU
with a delayed start should add it as a regression fixture; until then this
is a known, undetected gap rather than a verified-absent one. Not chased
further here: a `Decoder::set_time_base` built to fix a case this narrow
would be speculative interface surface (D19) for a fact no fixture currently
demonstrates matters.

### `vaco-codec-vp8` decodes only the first DCT token partition

RFC 6386 §9.5 allows a frame's coefficient tokens to be split across
`2^log2_nbr_of_dct_partitions` (1, 2, 4, or 8) independent partitions
specifically so a decoder can decode macroblock rows in parallel.
`vaco-codec-vp8::decode::decode_frame` (`crates/codec/vaco-codec-vp8/src/decode.rs`)
parses the partition-size table in the header but always decodes every
macroblock's residual from partition 0, regardless of
`log2_nbr_of_dct_partitions`. A stream encoded with more than one token
partition (typically produced by an encoder configured for multi-threaded
encode, e.g. libvpx `--token-parts`) will decode incorrectly from roughly
the second macroblock row onward, because the token bool-decoder never
switches partitions and instead keeps reading past the intended boundary
into whatever partition 0's own decode has not yet consumed. This is C-16d's
threading requirement, not implemented here — every differential-tested
fixture in `docs/codec/vaco-codec-vp8.md`'s Verification table used
`log2_nbr_of_dct_partitions == 0` (single partition), which this decoder
handles correctly and bit-exactly; multi-partition content is untested and
known-wrong. The fix is mechanical once someone needs it: `header.rs`
already exposes each partition's byte range, so `decode_macroblock` needs a
`row -> partition index` mapping (`row % num_partitions` per RFC 6386
§9.5) and one `BoolDecoder` per partition instead of one shared `token_bd`.

### `vaco-codec-vp8`: two details assumed rather than confirmed against RFC 6386's primary prose

Both are implemented with a documented, reasonable choice and have not
produced a wrong pixel across 112 bit-exact-verified frames spanning all
four version profiles, SPLITMV, golden/altref and segmentation (see
`docs/codec/vaco-codec-vp8.md`), but neither was independently located in
the RFC's own text during this crate's spec-extraction pass, only in the
widely-documented decoder convention:

1. The loop-filter *mode* delta's index mapping
   (`decode::mode_delta_index`) — which of the four `mb_lf_adjustments()`
   mode-delta slots applies to `B_PRED`/`ZEROMV`/other-inter/`SPLITMV`.
2. Chroma motion-vector rounding (`decode::round_div8`) — the exact
   rounding RFC 6386 specifies when deriving one chroma MV from the sum of
   four covering luma (eighth-pel) components.

Neither is blocking; recorded so a future reader chasing a rare chroma or
loop-filter mismatch on unusual content knows where to look first.

### `vaco-format-misc`'s `smk` video packets omit whatever palette-state packaging the reference adds

Measured while building the `smk` (Smacker) demuxer: an otherwise-empty
frame (a 4-byte palette chunk, nothing else) produces a **769-byte** video
`AVPacket` from the reference (`ffmpeg -c copy -f framemd5 -`, since
`ffprobe -show_streams` needs `smackvid` to open and it refuses to over a
fixture with anything less than a fully valid packed Huffman tree). Adding
8 real video-chunk bytes made it 777 — consistently `769 + n`, and
`769 = 1 + 256×3` strongly suggests a one-byte flag plus a synthesised
256-entry RGB palette table the demuxer prepends ahead of the real video
bytes, using state it tracks itself by decoding every palette chunk's
change instructions as it goes.

Three independent guesses at the exact byte layout (flag value 0 vs. 1,
prefix vs. suffix, an all-zero vs. a partially-set palette) were checked
against the measured MD5 hash and all failed to reproduce it — pinning the
real construction would mean reverse-engineering an undocumented internal
packet convention rather than reading a public specification, which is
past what black-box measurement of *container framing* is for. This
demuxer's video packets are the raw video-chunk bytes only: correct
per the public file-format spec, and a real, measured, unresolved
divergence in packet `size`/hash from the reference for video packets
specifically. Everything else about `smk` — stream count, dimensions,
frame rate, audio packet content/timing, extradata composition, frame
count — matches. Whoever wants byte-identical `smk` video packets needs to
implement the palette-chunk decode (the block-copy/range-copy/new-RGB-entry
instructions the public spec documents in full) and track running palette
state across frames; the spec support for doing so already exists, it is
just not wired to packet construction here.


### `vaco-format-misc`'s `bink` deliberately does not reproduce the reference's odd-length-frame drift

Measured against hand-built fixtures (`ffmpeg -c copy -f framemd5 -`): the
reference bink demuxer reads an odd-length frame's video chunk one byte
short, and — because it reads sequentially rather than re-seeking to the
next frame's absolute offset from the frame index table — every later frame
inherits that one-byte drift, eventually producing "audio size in header
(…) > size of packet left" and a hard demux error. Confirmed with two
fixtures differing only in whether one frame's total length was even or
odd: the all-even one round-trips exactly via `table[i+1] - (table[i] &
!1) - audio_bytes_consumed`, and the drift starts at exactly the odd frame
in the other.

`crates/format/vaco-format-misc/src/bink.rs` seeks to each frame's own
table offset before reading it (the table exists to make exactly that
possible) rather than reading sequentially, so it neither drifts nor
cascades into the reference's demux error on this input shape. Recorded as
a deliberate, understood divergence rather than a bug: a real Bink encoder
is not expected to produce an odd-length frame, so this only matters for a
hand-corrupted or adversarial file, and this crate's reading is the more
defensible one when the two disagree.

### `vaco-codec-core::CodecId::AdpcmAdx` is missing `CodecProperties::INTRA_ONLY`

Found while probing `ffmpeg -codecs`/`-decoders` for interface gap 21's nine
new game-codec `CodecId` variants — not part of that batch and not fixed
here, since it is an existing row this pass had no reason to touch.

`ffmpeg -hide_banner -codecs` prints `adpcm_adx`'s flags as `DEAIL.`: decode,
encode, audio, **intra-only**, lossy, not lossless. `vaco-codec-core`'s
existing `entry(CodecId::AdpcmAdx, "adpcm_adx", "SEGA CRI ADX ADPCM", A,
CodecProperties::LOSSY)` (`crates/signal/vaco-codec-core/src/lib.rs`) carries
`LOSSY` alone, missing `INTRA_ONLY` — inconsistent with `Aptx`/`AptxHd` right
next to it in the same table, which both correctly carry
`.union(CodecProperties::INTRA_ONLY)` for the identical `I` flag. `CodecProperties::INTRA_ONLY`
has exactly one reader in the tree today (`vaco-cli`/`vaco-probe`'s `-codecs`
listing columns), so the only user-visible effect is a blank column where the
reference prints `I` for `adpcm_adx` specifically — `vaco-codec-core`'s own
`the_codec_table_agrees_with_the_reference` test does not catch this, because
it only diffs `name`/`long_name`, not the flag columns. A one-line fix
(`CodecProperties::LOSSY.union(CodecProperties::INTRA_ONLY)`) for whoever
next touches that row or is auditing the table's properties column against
the reference's flags.

### `vaco-codec-mpeg12`: an unresolved non-intra residual bit-consumption bug, reproducible

`crates/codec/vaco-codec-mpeg12` (T2-01a, epic #36) is a clean-room ITU-T
H.262 decoder built directly on `vaco-parse-mpegvideo` and
`vaco-codec-dsp-idct`'s `mpeg2` IDCT. It decodes I/P/B frame pictures
(frame-based and field-based-within-a-frame-picture prediction) but has a
real, measured, unresolved bug: on any fixture busier than a small
low-motion test clip, a non-intra macroblock's own coefficient/CBP decode
eventually desyncs the bitstream, and — because `ActivePicture::supported`
is one flag shared between "this whole picture's coding mode is
unimplemented" (checked once, at `begin_picture`) and "a decode error
happened somewhere in this picture" (set from many places inside
`macroblock::decode_coded_macroblock`/`reconstruct_macroblock`) — every
slice from that point to the end of the picture is silently skipped
(`decode_slice`'s own `if !ap.supported { return; }` guard), leaving the
rest of the frame at its zero-initialized allocation. Because that
corrupted frame is then used as a motion-compensation reference, the
corruption propagates to every picture in the rest of the GOP.

Measured (via the crate's own differential harness against
`ffmpeg`-decoded reference `yuv420p`, MAD/RMS per frame — see
`docs/codec/vaco-codec-mpeg12.md` for the full table):

- Small (64x48), low-motion I/I-P/I-P-B fixtures, MPEG-2: avg MAD 1.1-1.7,
  max MAD 2 — essentially reference-quality, the residual float-IDCT
  rounding difference documented in that same doc.
- `m2_qcif_ipb` (176x144) and `m2_cif_ipb` (352x288), MPEG-2, and
  `m2_oddsize` (48x64): avg MAD 11-234, with the corruption traced by hand
  to one specific macroblock's `coded_block_pattern` VLC decode failing
  partway through a picture (concretely: `m2_qcif_ipb.m2v`'s second
  P-picture in display order, macroblock address 80 — mb_x=3, mb_y=7 in an
  11x9 macroblock grid — fails at the `coded_block_pattern()` VLC call in
  `macroblock::decode_coded_macroblock`, after a macroblock type and motion
  vector that both decode as plausible, spec-conforming values). Every
  macroblock after address 80 in that picture, and the picture's own
  bottom slice row, is lost; every later picture in the GOP that
  references it inherits the corruption.
- The three MPEG-1 fixtures (`m1_i`/`m1_ip`/`m1_ipb`, 64x48) improved
  drastically this session (avg MAD ~252 -> ~13-54, after fixing the
  MPEG-1 escape-coding bug below) but still show the same class of
  residual-desync failure, not yet root-caused independently — it may be
  the same underlying bug as the MPEG-2 one above, since both involve
  non-intra coefficient/CBP decode past the first few macroblocks.

What was ruled out by hand-tracing (so the next pass doesn't re-derive
this): the `CODED_BLOCK_PATTERN`, `DCT_DC_SIZE_LUMA`/`CHROMA`,
`MACROBLOCK_TYPE` (P-picture "MC, Coded" row), and `MOTION_CODE` tables
all mechanically cross-checked correct against the spec text at the
specific values involved; the failing macroblock's own type decode,
motion-vector decode (f_code=1, no residual-extension bits involved), and
bidirectional-averaging arithmetic for other, *working* nonzero-motion
macroblocks in the same picture all hand-verified pixel-exact against the
reference. The bug is somewhere in coefficient or CBP bit consumption for
a *specific* macroblock content pattern this session did not isolate
further — most likely a rare VLC-table edge (an escape-level boundary, or
a run/level combination near `n` wrapping past 63) exercised only by
busier/more-detailed content, since the small fixtures never trip it. Two
real bugs in this exact area were found and fixed this session (see the
crate's own commit and `docs/codec/vaco-codec-mpeg12.md`'s changelog-style
notes), so a third, rarer one is plausible rather than surprising.

Two structural fixes worth making together, not separately:

1. Split `ActivePicture::supported` into two flags: one set once at
   `begin_picture` for "this picture's coding mode is unimplemented"
   (still correctly gates the whole picture, unsupported-picture counting,
   and `CORRUPT`/neutral-fill), and one reset per **slice**, not per
   picture, for "this slice hit a local decode error" — so a bad
   macroblock loses the rest of *its own slice* (already true today) but
   not the rest of the picture's *other* slices, which today are silently
   dropped even though their own bitstream is untouched by the earlier
   slice's problem.
2. Once (1) exists, re-run the differential matrix in
   `docs/codec/vaco-codec-mpeg12.md` and see whether the visible symptom
   changes from "rest of picture and GOP corrupted" to "one macroblock's
   region wrong" — which would make the underlying coefficient/CBP bug
   much easier to isolate by shrinking the blast radius of each repro run.

Fixtures to reproduce with, already in this session's scratch directory
(not committed — regenerate with `ffmpeg` from a raw YUV source using the
same `-s WxH -pix_fmt yuv420p -c:v mpeg1video|mpeg2video` flags the crate's
`docs/codec/vaco-codec-mpeg12.md` documents): `m2_qcif_ipb.m2v` (176x144,
IBBPBBPBBPBBPBB GOP=15) is the smallest fixture that reproduces the bug.

### `vaco-parse-mpegvideo`: end-of-stream `flush()` reports a spurious `max_alloc_total` budget error on tiny inputs

While building `vaco-codec-mpeg12`'s differential-test harness
(`crates/codec/vaco-codec-mpeg12/examples/decode_dump.rs`), calling
`vaco_parse_mpegvideo`'s `Parser::parse(&[])` (the end-of-stream flush
convention other parsers in this workspace use) on small (5-8 KB) `.m1v`/
`.m2v` test fixtures returned `Err` reporting `max_alloc_total limit
exceeded: requested ~1073741824+<small delta>, cap 1073741824` — a
budget-accounting bug (some code path appears to request/round up to a
fixed ~1 GiB allocation regardless of the tiny actual input) in a crate
this agent does not own and did not fix. Worked around by not depending on
`vaco-parse-mpegvideo` at all for the harness: `decode_dump.rs` implements
its own ~15-line access-unit splitter directly against
`vaco_bitstream::annexb::find_start_code`, documented at length in that
file's own module doc. Whoever owns `vaco-parse-mpegvideo` should add a
regression fixture at this size and trace the flush path's own allocation
request.

### `vaco-codec-mpeg12`: pieces that belong to a shared MPEG-family decoder core (D-22, epic #25), not to this crate specifically

D-22 (a shared decode core for the MPEG-1/2/4-family — motion compensation,
macroblock/slice iteration, IDCT integration, reference-picture management)
does not exist yet; this crate is the first real consumer of what it would
factor out of. Nothing here was *designed* against a shared interface, so
none of it is *wired up* for reuse, but the following pieces are generic to
the family rather than specific to H.262/MPEG-2 syntax, and are exactly
what a future D-22 pass should look at extracting:

- `crates/codec/vaco-codec-mpeg12/src/motion.rs`'s `form_prediction`
  (half-pel interpolation via the spec's `//` round-to-nearest operator,
  generalized over frame/field addressing through `row_scale`/
  `row_parity`) and `average_predictions` (B-picture bidirectional
  averaging) implement §7.6.4/§7.6.7.1 exactly as MPEG-4 part 2's simple
  and advanced-simple profiles reuse them (MPEG-4 part 2 §7.6 cites H.262's
  half-pel scheme directly for non-quarter-pel modes) — this is the
  motion-compensation core.
- The `previous`/`recent`/`held` one-picture-delay reference-management
  scheme in `crates/codec/vaco-codec-mpeg12/src/decoder.rs` (B-picture
  display-order reordering by holding the most recently decoded reference
  picture until the next one is decoded) is the generic MPEG B-picture
  reordering algorithm, not an H.262-specific one.
- `crates/codec/vaco-codec-mpeg12/src/block.rs`'s `inverse_scan`/
  `dequantise`/`inverse_transform` pipeline (inverse zigzag or alternate
  scan, weighting-matrix + quantiser-scale dequantisation, mismatch
  control, IDCT via `vaco-codec-dsp-idct::mpeg2`) is the same three-stage
  shape MPEG-4 part 2 uses, differing only in the mismatch-control formula
  and default matrices (both already parameterized here as function
  arguments, not hardcoded).
- `vaco-codec-mpeg12`'s own `vlc::decode` (generic linear-scan prefix-code
  matcher parameterized over any `(bits, value)` table) is not MPEG-2-
  specific at all — any future MPEG-family VLC table (MPEG-4's own
  macroblock_type/CBPY tables, for instance) could reuse it as-is.

None of this was extracted in this session because there is no D-22 crate
to extract it *into* yet, and speculatively designing an interface for a
second, hypothetical consumer this crate does not have would be exactly
the kind of unrequested abstraction `AGENT-CONSTRAINTS.md` asks agents to
avoid. Recorded here so whoever picks up D-22 has a concrete list of
already-working reference implementations to start from instead of
re-deriving them.

### `vaco-codec-mpeg12`: explicitly unimplemented decode paths

Recorded once, centrally, rather than scattered per-module (each module's
own doc comment cross-references this entry): separate field-coded
pictures (`picture_structure != "Frame picture"` — only field prediction
*within* a frame picture, §7.6.2's common real-world interlaced case, is
implemented), dual-prime prediction, 16x8 motion compensation, MPEG-1's
`full_pel_forward_vector`/`full_pel_backward_vector` modes, and 4:2:2/4:4:4
chroma sampling (T2-01b/#356, explicitly lower priority and not attempted
this session) are all unimplemented. A picture that needs any of the first
three is decoded as a flat `CORRUPT`-flagged mid-grey placeholder rather
than silently producing wrong pixels (`Mpeg12Decoder::unsupported_pictures`
counts these); a stream declaring 4:2:2/4:4:4 chroma is out of scope
entirely (this crate only allocates `Yuv420p` frames). Spatial/SNR/temporal
scalability extensions are not parsed or decoded.

### `vaco-format-misc-audio`'s `BlockDemuxer` batches packets the reference emits one-per-block

Found implementing `vag` (FM-58, issue #620) and comparing its packet
granularity against `ffprobe -show_packets`: the reference emits **one
packet per 16-byte PS-ADPCM block**, ten blocks in a ten-block fixture,
`pts` advancing by 28 samples each time. `vag.rs` was written to match
that directly (see its own module doc). But this crate's shared
`BlockDemuxer` helper — used by `adx`, `pvf`, `nistsphere` and every
`rawcodec.rs` format, all closed under issues #621/#622 — batches many
blocks into one packet up to `TARGET_PACKET_BYTES` (4096 bytes): `adx`'s
own fixture (76 blocks of 18 bytes) demuxes to a **single** 1368-byte
packet in this crate today, where `ffprobe -show_packets` on the same
fixture reports 76 separate 18-byte packets, one per block, `pts`
advancing by 1 each time. This was not caught by `tests/differential.rs`,
which checks stream-level sample_rate/channels/duration and "at least one
packet produced", never packet count or per-packet size.

Not fixed here: `BlockDemuxer` is shared by five already-closed formats,
and changing its packetisation policy is a wider, riskier change than
this session's own dispatch (`vag`/`xwma`) called for — it would need
`adx.rs`'s own committed test (`assert_eq!(pkt.len, 18 * 76)`, which
explicitly encodes the batched-packet assumption) rewritten along with
every other `BlockDemuxer` consumer's expectations, and re-verification
that nothing downstream (a caller doing per-packet seeking or timing
math) depends on the current batching. Whoever picks this up: the fix is
narrow in principle (drop `TARGET_PACKET_BYTES` batching, emit exactly
`bytes_per_block` per packet, matching `vag.rs`'s bespoke loop) but wide
in blast radius (five formats' tests and two closed issues).

### `vaco-format-misc-audio`'s `xwma` has an unreproduced `duration_ts` anomaly when a `dpds` chunk is present

See `xwma.rs`'s own module doc for the full measurement: a `data` chunk's
byte-rate-formula duration (`bytes * sample_rate / avg_bytes_per_sec`) is
exactly what the reference reports when no `dpds` chunk exists, but adding
any `dpds` chunk — one entry or many, any byte content — collapses the
reported `duration_ts` to a fixed, much smaller value unrelated to the
formula, independent of the `dpds` chunk's own size or content. This
crate always uses the plain byte-rate formula and does not reproduce
whatever this is; the working hypothesis (an `ffprobe` generic duration-
estimation fallback reacting to a `dpds`-signalled "real WMA container" by
attempting a codec-probe against this crate's non-decodable synthetic
payload) was not confirmed, since confirming it needs genuinely valid WMA
bitstream data, out of scope for framing work.

### `vaco-codec-core::CodecId`: seven ids where the widened reference audit disagrees and neither answer was forced

Closing interface gap 21's audit (`the_codec_table_agrees_with_the_reference`,
`crates/signal/vaco-codec-core/tests/params.rs`) found 51 property
disagreements against `ffmpeg -codecs`' I/L/S flag columns. 44 were the
unambiguous "an existing row never had its flags checked against the
reference at all" class (27 fixed in the same pass: `dvvideo`, `cljr`,
`g728`, `pcm_vidc`, `avs2`, `avs3`, `jpeg2000`, `flv1`, `flashsv`,
`flashsv2`, `vp6`/`vp6a`/`vp6f`, `nellymoser`, `adpcm_swf`, `gsm`/`gsm_ms`,
`adpcm_g722`/`adpcm_g726`, `g723_1`, `g729`, `qcelp`, `ilbc`, `opus`, `flac`,
`vorbis`, `mp3` — plus `adpcm_adx`, fixed in the commit immediately before
the audit landed since it was the one that motivated writing the audit at
all). Seven were left alone and recorded in the test's own
`KNOWN_PROPERTY_DIVERGENCES` list, because fixing them would mean picking an
answer to a real modelling question this pass had no standing to settle
unilaterally:

* `subrip`, `mov_text`: the reference does not apply the lossy/lossless/
  intra vocabulary to text subtitle codecs at all — those two rows print no
  `I`/`L`/`S` flags whatsoever in `-codecs`, not a specific combination to
  match. This project already made the opposite, defensible choice
  (trivially intra-only and lossless, since a text cue has no compression
  mode at all), and "the reference states nothing" is not evidence that
  choice is wrong.
* `wrapped_avframe`: an internal passthrough pseudo-codec. `-codecs` does
  not flag it intra-only; whether "independently decodable" is even a
  coherent question for a passthrough was not something this pass could
  answer with a `-codecs` diff alone.
* `png`: `-codecs` does not mark it intra-only, unlike every other
  `vaco-format-*-simple`-family image codec sharing the `IMG` constant
  (`pbm`/`pgm`/`ppm`/`pam`/`pfm`/`phm`/`pcx`/`targa`/`sgi`/`xwd`/`xbm`, all
  of which *do* get `I` from the reference). The likely reason — PNG's
  animated form (APNG) can inter-frame-delta the way GIF does, and this
  table already correctly does not call `gif` intra-only either — is a
  plausible hypothesis, not a verified fact; whoever owns `vaco-codec-qoi`/
  image handling should decide whether `Png` needs its own row split from
  the `IMG` constant rather than have this pass strip the flag on a guess.
* `h264`, `hevc`, `av1`: `-codecs` flags all three both lossy **and**
  lossless-capable (each has a real, specified lossless coding mode); this
  table only ever gives them `LOSSY`. Whether a coarse two-state
  lossy-xor-lossless-per-codec-family model should grow a "both" case for
  the handful of codecs that are actually dual-mode is a real design
  question — [`CodecProperties`] is a bitflag set, so "both" is
  representable today, the table just never asserts it for these three.

None of the seven are silently accepted: the test itself would fail loudly
if one's properties ever changed to agree with the reference and the name
were left in `KNOWN_PROPERTY_DIVERGENCES` (the test also asserts the
divergence list stays exactly as wide as it needs to be, not wider).

Also found in the same pass and left alone as out of scope for a `-codecs`
diff: `smk`'s uncompressed audio tracks report `pcm_s16le`/`pcm_u8` in the
reference, not `smackaudio` — this *was* fixed (`vaco-format-misc/src/smk.rs`),
since it directly affects gap 21's own acceptance criterion, but the
underlying fact (Smacker's `AudioRate` `compressed` bit gates whether a
track is coded audio at all, versus raw PCM in the container's clothing)
was found by testing real fixtures through `ffprobe`, not from `-codecs`,
and is recorded here so the next person auditing per-codec properties knows
`-codecs`' flat name-to-flags table cannot see container-conditional codec
identity like this at all.

### Update: the `BlockDemuxer` batching entry above is fixed (2026-08-28)

The entry "`vaco-format-misc-audio`'s `BlockDemuxer` batches packets the
reference emits one-per-block" is resolved for ten of its twelve affected
formats. Measured every `BlockDemuxer` consumer individually against
`ffprobe -show_packets` rather than assuming `vag`'s one-packet-per-block
answer generalised — it does not: `adx`, `gsm` and `g729` do get one
packet per block (18/33/10 bytes), but `g722`, `aptx` and `sln` batch into
1024-byte packets, `g726`/`g726le`/`g728` into 1020-byte packets, `dfpwm`
into 512-byte packets, and `aptx_hd` into 1536-byte packets — each its own
fixed constant with no shared formula (`g722` and `g726` share the same
1:2 byte:frame ratio and fixed sample rate, yet batch differently).
`BlockDemuxer::new` now takes `target_packet_bytes` as a required
parameter instead of picking `4096` itself; `RawCodecSpec` carries the
measured constant for each `rawcodec.rs` entry, and `adx.rs` passes its
own block size directly.

`tests/differential.rs` now asserts the exact per-packet size sequence
for every fixture with a measured answer (previously it checked only
stream-level fields and "at least one packet produced", the gap that let
the original bug through). `adx.rs`'s own test that had encoded the
batched-packet assumption (`assert_eq!(pkt.len, 18 * 76)`) is rewritten
to assert 76 separate 18-byte packets, matching the reference.

Allocation shape checked before shipping: `Packet::alloc` already routes
every packet through `vaco_limits::Budget` regardless of packet count,
and the per-`read_packet`-call iteration count is bounded by the
caller's own `consume_fuel` budget (`vaco-probe/src/packets.rs` charges
one fuel unit per call already) or the fuzz target's `MAX_PACKETS` cap —
neither of which this change touches. A direct release-mode timing check
(a synthetic 180 MB `adx` file, 7.4 million one-block packets) completed
in 443 ms, and a 30-second `misc_audio_demux` fuzz run afterwards found
no crash, no `slow-unit-`/`oom-` artifact, and an empty
`fuzz/artifacts`.

**Not fixed, for a different reason:** `nistsphere` and `pvf`'s raw-PCM
tail still use the old, unmeasured `4096`-byte default
(`block::DEFAULT_TARGET_PACKET_BYTES`, kept under that name specifically
so it reads as "not measured" rather than "the right answer"). See the
next entry.

### `vaco-format-misc-audio`'s `nistsphere`/`pvf` raw-PCM packet batching depends on sample rate, formula not pinned down

While fixing the `BlockDemuxer` entry above, `nistsphere`'s raw-PCM tail
was measured across eighteen sample rates from 250 Hz to 96 kHz (mono,
16-bit) to see whether its batching followed a clean rule the way the
compressed/ADPCM formats did. It does not reduce to one formula found so
far:

- From 250 Hz through 16000 Hz, packet size in frames matches
  `nearest_power_of_two(sample_rate * 0.064)` exactly (64 ms of audio,
  rounded to a power of two) at every rate tried: 250→16, 500→32,
  1000→64, 2000→128, 4000→256, 8000→512, 11025→1024, 16000→1024.
- That formula breaks between 20.4 kHz and 20.6 kHz: it predicts 2048
  frames for both, but the reference switches from 1024 frames (at
  ≤20400 Hz) to 2048 frames (at ≥20600 Hz) — a transition roughly 4.3 kHz
  earlier than the 64 ms rule predicts, for reasons not identified.
- Higher rates (22050, 32000, 44100, 48000, 96000 Hz) then matched a
  `nearest_power_of_two(rate * 0.064)`-shaped curve again, but by then
  the low/mid-range mismatch had already disproved a single global
  formula.

Not fixed: reproducing this without the actual rule would mean guessing,
which risks exactly the outcome the `BlockDemuxer` fix above was trying
to avoid — trading a known, honest approximation for an unverified one
that merely looks more precise. `nistsphere`/`pvf` keep using
`block::DEFAULT_TARGET_PACKET_BYTES` (4096 bytes), explicitly not claimed
to match the reference. Whoever picks this up: the measurement script
(sweeping `sample_rate` over a hand-built NIST SPHERE header and reading
`ffprobe -show_entries packet=size`) is straightforward to rebuild; the
open question is what governs the 16–32 kHz transition specifically,
since everything on either side of it fits the simple 64 ms/power-of-two
rule.
