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
