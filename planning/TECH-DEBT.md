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
