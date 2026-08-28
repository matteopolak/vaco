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
