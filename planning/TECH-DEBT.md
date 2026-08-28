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

### `AviMuxer`'s new slot-grid budgets hardcode `Limits::permissive()`

`convert_budget` (pre-existing) and `grid_budget` (new, for the 600 Hz
grid's empty-slot backfill) are both constructed with
`Budget::new(Limits::permissive())` inside `AviMuxer::new`, ignoring the
`FormatOptions` the caller passed in. Consistent with the crate's existing
pattern, and permissive's 1 GiB/2^32-fuel caps are generous enough that no
real recording should ever hit them — but an embedder who wants a stricter
bound (the `Limits::strict()`/library-embedding case
`vaco_limits::Limits`'s own docs describe) cannot get one without a
`vaco-mux-avi` code change, since nothing threads a caller-supplied
`Limits` through `Muxer::new`/`FormatOptions` today. Not fixed here: it is
the same shape as `convert_budget`'s pre-existing choice, not a regression,
and widening it is a `FormatOptions`/`Muxer` interface question bigger than
one crate.

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

### `vaco-codec-jpeg`'s encoder has no progressive mode and does not build optimized Huffman tables

`encode.rs` always emits a single baseline (`SOF0`) scan and the Annex
K.3-K.6 default Huffman tables, never per-image-optimized ones. Both are
correctness-neutral (the output is a valid, conformant JPEG either way) but
cost compression ratio against a reference encoder at the same quality
setting. Neither was implemented because issue #297's acceptance bar was
"encoder output re-decodes within a quality bound", which the current
encoder meets; extending it to progressive output would also need the
`ac_refine`-side bug above resolved first, since a progressive encoder is
only useful if this crate's own progressive decoder can be trusted to check
it against.

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
