# Working in this repository

`planning/00-decisions.md` holds the numbered decision records;
`planning/AGENT-CONSTRAINTS.md` holds rules derived from specific incidents.
This file is what you must know before touching anything.

## Hard constraints

**No `unsafe`.** `unsafe_code = "forbid"` workspace-wide. No inline assembly, no
unsafe intrinsics. SIMD goes through the portable substrate. Only `vaco-hw-*` is
exempt.

**Clean room.** Do not read FFmpeg, libav, VLC, GStreamer, mpv, x264 or x265
source. Ever. Write from published standards, academic papers, and
permissively-licensed references (JM, HM, libopus, libvpx, dav1d, libjxl). Running
the `ffmpeg` *binary* as a black box to generate fixtures and compare output is
fine and encouraged — that is not the same as reading its source.

**Patent gating.** A component with `encumbered = true` must have
`default = false`.

## Verify by measuring, not by reading

The single most common failure here is concluding something works because the code
looks right. It usually looks right.

- **Exit code 0 proves nothing.** A decoder shipped that decoded 2.5% of a file
  and exited 0. Another produced output 2.3x too large. Both passed their tests
  and were reported as working. Check sample counts, byte counts, frame counts.
- **Both directions.** For a format, verify ffmpeg-produced file → we read it,
  *and* our file → ffmpeg reads it. One direction passes while real bugs survive.
- **Check the value, not just that it opened.** A corrupt config box usually still
  opens. Compare the fields the reference reports.
- **Implemented is not reachable.** A decoder can be complete, tested and
  registered while no CLI path reaches it and no demuxer produces its packets.
  Verify end to end through the binary.
- **A refusal is a floor on what's missing, never a ceiling.** Fixing the error a
  stream hits first reveals the next one, not the end.

**An oracle you wrote shares your misreading.** A test derived from the same
assumption as the code will pass while both are wrong. Rotation shipped with two
transpose directions swapped — composed from two conventions that turned out not
to be negations of each other — and its unit test, written from that same
assumption, passed cleanly. Only comparing full-pipeline output against ffmpeg
caught it, on every rotated pixel.

The largest instance so far: FFV1's encoder and decoder round-tripped each other
byte-exactly while both disagreed with the format, so an ffmpeg-written file
decoded to wrong pixels on 99.6% of bytes — on a *lossless* codec — and ffmpeg
read our output as wrong pixels too. The crate's only test was that round-trip.
A self-round-trip test proves the two halves agree, which is not the claim.

So for anything with a reference, check against the reference, not against your
own understanding of it, and never let a round-trip be a codec's only evidence.

**Registered-but-wrong is worse than absent.** Where something is out of scope,
refuse by name via `check_scope`. Never emit wrong pixels or samples.

**First sightings are never the last.** Every defect class found here has gone
1 → many (unvalidated probe fields 1→5, frame-budget overcharge 1→6, image
encoders inferring format 1→13, `Eof`-on-drain violations 1→8). When you fix one,
sweep for its siblings before closing.

## Comments and documentation

Comments should be brief and explain something non-obvious. Code should otherwise
be self-documenting. Do not narrate what the next line does.

Keep: public rustdoc, usage examples, recorded measurements, spec clause
citations, and notes explaining why a non-obvious thing is the way it is.

Cut: restatements of the code, changelog narrative inside source, issue-number
parentheticals, and per-crate docs that walk through the implementation at a level
that will be wrong in a week.

`docs/` is reference material, not a diary. Several pages are generated — they say
so at the top; edit the generator, not the page.

## Tests

Write tests that can fail. Delete tests that cannot.

A test asserting a constant equals its own literal, or that a string appears in a
source file, tests nothing and costs compile time. A test covering one end of a
range where clamping could break asymmetrically is worth keeping.

Prefer one test that decodes a real file and compares bytes over ten that check
internal structure.

## One source of truth

Do not rely on two things agreeing across crates. Prefer types, newtypes, macros
and generated tables that make disagreement impossible.

This is not abstract: a filter was advertised by `-filters` and unusable, because
CLI dispatch kept a hand-maintained list beside the registry. Codecs cannot have
that bug, because dispatch and listing read the same generated table. When you see
two lists that must match, remove one.

Minor divergence from ffmpeg's behaviour is acceptable when it makes the internals
substantially simpler. Say what diverged.

## Performance

Every number must come from a measurement, in this session, on this machine.
`planning/PERF-BASELINE.md` has the protocol; it is binding.

- **Profile before optimising, and profile the callee.** Six recorded
  optimisations here reasoned correctly and measured slower.
- **Report ratios against a same-session `ffmpeg` run.** Absolute times drift ~30%
  under load; two agents have disagreed 30% on absolutes and agreed exactly on the
  ratio.
- **Interleave A/B, alternate order, ≥10 rounds**, and report CPU-seconds beside
  wall clock.
- Build into a private target dir, never the shared `target/`.
- Do not re-propose anything on the do-not-re-propose list without new evidence.

**Optimise the success path; the error path may get arbitrarily slower.**
`#[inline]` is a hint and often does nothing — use `#[inline(always)]` where you
mean it, `#[cold]` on error paths, and branch hints where they help. Performance
only matters when the output is useful.

Byte-exactness is not negotiable for a speedup. A threading change that is fast
and occasionally wrong is worth nothing: verify at 1/2/4/8 threads plus the
determinism fuzz targets.

## Working in a shared tree

Several agents work in this one checkout on `main` simultaneously. Assume every
file you did not just write belongs to someone else.

**Only additive operations are safe.** `git checkout -- <path>`, `git restore`,
`git reset --hard` and `git stash` overwrite tracked files with HEAD's content and
destroy other agents' uncommitted work, with no reflog entry and nothing to
recover. Never run them over a path you do not own. Need a clean tree to build
against? Use a private worktree or a fresh clone in your scratchpad.

**Never `git commit -F <msg> -- <pathspec>`.** That form commits the *working
tree's* content for those paths, including other agents' edits.

**`git add` writes to the shared index, so never pair it with a bare `git
commit`.** The index belongs to the whole checkout, not to you. A correctly
scoped `add` followed by a bare `commit` ships *everything* staged, including
whatever another agent left there — that has already published a stale README
that silently reverted a page of corrected measurements, and the scoped `add`
made it look safe. A scoped `add` does not imply a scoped `commit`. If you find
someone's stale content staged, `git add` your path's correct working-tree
content to clear it; that touches the index only.

Use the private-index recipe in `planning/AGENT-CONSTRAINTS.md` instead — it
never touches the shared index — and finish it with both guards:

```sh
git update-ref refs/heads/main "$commit" "$BASE" || { echo "HEAD moved; restart"; exit 1; }
git merge-base --is-ancestor "$commit" refs/heads/main || echo "ORPHANED"
```

The old-value argument is a compare-and-swap; without it you can silently orphan
someone else's commit. An orphaned commit looks perfectly healthy — `git show`
prints it — so the ancestry check is the only thing that catches it.

The `reference-transaction` hook now refuses any move of `main` off its own
history, so this fails loudly rather than silently. If you hit it, rebuild on the
new tip; don't reach for the `VACO_ALLOW_NONFF=1` override, which exists for the
planned history rewrite.

Before editing a crate, check `git log` for it and `git blame` for
"Not Committed Yet". Prefer crates nobody is in. If the code already does what you
came to do, stop and pick something else.

Commit small and often. Land a compiling partial state rather than holding a
broken tree; gate the feature off if it isn't ready. "Not ready to enable" and
"not ready to commit" are different things.

**A build failure in a crate you did not touch is probably not yours.** Someone is
mid-edit: a call site written before the function it calls. Diagnose before you
react — `git status --porcelain -- <that crate>` shows whether it is dirty, and
building that crate at `HEAD` in a private worktree shows whether the failure is
committed. Uncommitted and not yours means wait or build around it; it usually
clears in minutes. Never "fix" it by reverting someone's file, and never conclude
a failure is pre-existing by stashing — if your own change is already committed,
stashing leaves it in place and proves nothing. Bisect instead.

**Do a long refactor in a private worktree.** A multi-file change that leaves the
tree non-compiling for an hour blocks every other agent's verification. Edit,
build and verify in the worktree, then apply the finished change to the shared
tree and commit it immediately, so the shared tree only ever sees compiling
states. Small edits are fine in place.

## Commits

Conventional messages with a bracketed scope: `fix(demux-mp4): ...`. Never a bare
`git commit -m`. No attribution or `Signed-off-by` trailers.

A commit touching `crates/{codec,format,filter,signal}/` must carry the clean-room
trailers:

```
Vaco-Provenance: spec
Vaco-Spec-Ref: <declared-source-id> <clause>
Vaco-Clean-Room: yes
```

`Vaco-Provenance` is exactly one of `spec`, `rfc`, `paper`, `blackbox`,
`original`, `cleanroom-doc:<path>`. Free text fails the gate — "measured via
ffmpeg 9.0.1" is `blackbox`.

`Vaco-Spec-Ref` must *start with* a source id declared in `provenance/*.toml`:
`aom-av1-spec`, not "AV1 spec"; `atsc-a52-2018`, not "RFC 3686". A citation to a
document we never recorded acquiring proves nothing. If your source isn't
declared, declare it first.

The `prepare-commit-msg` hook writes these for you — but **`commit-tree` bypasses
hooks**, so with the private-index recipe you must put them in the message
yourself. Check with `cargo run -p xtask -- provenance-check` before you move on.

After a ref race, also check the commit is not empty:

```sh
git show --numstat --format= "$commit" | grep -q . || echo "EMPTY — committed nothing"
```

Retrying a commit against a moved `HEAD` can produce a commit whose tree equals
its parent's. It passes the ancestry check and reads as healthy, while claiming
work it does not contain.

Constant tables of 32+ elements need a `[[table]]` entry in
`provenance/<crate>.toml`. Renaming or moving a table breaks its entry; update it
in the same commit.

## Fix it, don't file it

If you find a bug, fix it. File an issue only when the fix is genuinely out of
scope for what you are doing.

When closing a GitHub issue, put the measured evidence in the closing comment.
Do not close partial work to move a number — say what remains. If the acceptance
criteria depend on infrastructure that does not exist, say so and re-scope.

## Report honestly

Say what you measured and what you assumed. If you could not verify something,
say that rather than implying you did. If a task's premise turns out to be wrong —
already done, already fixed, or someone else is mid-edit — stop and say so instead
of executing it anyway. That has been the most valuable thing agents do here.
