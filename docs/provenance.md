# Provenance and the clean-room evidence trail

## What it is

The machine-checked half of Vaco's clean-room claim (D15, plan 13 §6). Two
artefacts carry it: `provenance/*.toml`, which records where every large
constant table came from, and the commit trailers, which record how each change
was produced. `cargo xtask provenance-check` enforces both.

A clean-room claim is worth exactly what its record can show. "We did not read
their source" is unfalsifiable on its own; "this table is ITU-T H.264 Table
9-44, transcribed, and here is the file that has said so since the day it
landed" is not.

## How it works

### Tables

Documents are declared **once**, in `provenance/sources.toml`, and cited from
anywhere — a `[[table]]` row in any crate's file, or a commit's `Vaco-Spec-Ref`
trailer. ISO/IEC 14496-12 backs the MP4 demuxer, the MP4 muxer and the shared
ISOBMFF crate; three copies of that one acquisition record would be three
chances for them to disagree, which is the failure mode this directory exists to
prevent (D19). Declaring the same id twice is an error.

Every `static`/`const` array of **32 or more elements** in a `codec`, `format`,
`filter`, `signal` or `model` crate must have a `[[table]]` entry in
`provenance/<crate>.toml`. Below 32 elements a table is a handful of magic
numbers that the surrounding code explains; above it, it is a transcription of
*something*, and which something is the whole question.

The check runs both ways. A table with no entry fails, **and an entry naming a
table that no longer exists fails too** — a record that quietly rots into
fiction is worse than no record, because it still reads as evidence.

Two fields answer two different questions:

- `kind` on the `[[source]]` — what the document is: `spec`, `rfc`, `paper`,
  `blackbox`, `original`.
- `method` on the `[[table]]` — what we did with it: `transcribed`, `derived`,
  `probed`, `original`.

"Transcribed from Table 9-44" and "derived by evaluating the standard's
equation" are both `spec`, and only one of them survives somebody finding an
arithmetic error in the other. `vaco-codec-dsp-idct` is the live example: its
HEVC matrix is `derived`, and the reason is written into its file — the literal
reading of equation 8-317 reproduces the well-known integer cores and still
computes the wrong transform.

`blackbox` means the values were measured from the reference binary rather than
read from a document. Recording the observed behaviour of a shipped binary is
not copying its expression (D6), but it is a different kind of evidence and it
is labelled as one.

### Trailers

Commits touching `crates/{codec,format,filter,signal}/` must carry:

```
Signed-off-by: Name <email>
Vaco-Provenance: spec
Vaco-Spec-Ref: itu-t-h264-202108 Table 9-44, clause 9.3.3.2.1.1
Vaco-Clean-Room: yes
Vaco-AI-Assisted: yes
```

**Both trailers may repeat.** A single-value rule was the first design and it
broke on the first commit that aggregated a wave: fifteen crates implemented
from a dozen documents do not have *one* provenance, and forcing them to pick
one would have made the record less true rather than more.

`Vaco-Spec-Ref` must **start with a source id declared in `provenance/*.toml`**.
Plan 13 §6.2 wrote the reference as free text; requiring the id first is what
makes it checkable, and a citation to a document nobody recorded acquiring looks
authoritative while proving nothing.

Everything else needs only `Signed-off-by`.

History before `provenance/baseline` is exempt. The alternative was to rewrite
every existing commit message, which would have meant writing trailers from
memory long after the fact — exactly the false record §6 warns against. The
baseline says, honestly, that the machine-checked trail starts there.

## How to change it

- **Adding a table**: add `[[table]]` to `provenance/<crate>.toml`. If its
  source is not yet declared, add a `[[source]]` first — the table's `source`
  key must resolve within the same file.
- **Changing the threshold**: `TABLE_THRESHOLD` in `xtask/src/provenance.rs`.
  Lowering it will surface a batch of small tables; each needs a real answer,
  not a placeholder.
- **Adding a crate area**: `AREAS` in the same file. `app`, `io`, `tool` and
  `registry` are excluded because their tables are our own option lists and
  generated output, not transcriptions of anyone's document.
- **Moving the baseline forward** is not something to do casually. It erases
  the checked range behind it.

The gotcha, recorded because it cost the largest table in the repository: the
scanner walks **byte** indices. The first version mixed a `Vec<char>` scan with
`text.get(i..)` lookahead, which agrees with itself until a file contains a
non-ASCII character and then goes silently blind to everything after it —
`vaco-pixfmt`'s 267 descriptors were reported as absent while the gate passed
on everything else. A gate whose failure mode is a quiet false negative is
worse than no gate. If you extend the scanner, extend its falsification tests
too.

## Configuration

- `provenance/sources.toml` — the document register.
- `provenance/baseline` — the commit from which trailers are checked.
- `.git/vaco-attestation` — per-clone, written by `.githooks/attest`, read by
  the `prepare-commit-msg` hook. Not committed.
- `git config core.hooksPath .githooks` — installed by `just setup`. Three
  hooks: `prepare-commit-msg` fills the trailer block, `commit-msg` **blocks**
  on a bad one (a bad trailer is cheap to fix at that moment and expensive at
  every later one — the first version had no such hook, and the
  `TODO-source-id` placeholder went straight into a pushed commit), and
  `pre-commit` reports table findings without blocking.

  A note for anyone editing `prepare-commit-msg`: `git commit -F` defaults to
  `--cleanup=whitespace`, which does **not** strip `#` comment lines. A helpful
  guidance block written into the message file lands verbatim in the commit
  body. It did, once.

## Dependencies

None. `xtask` is deliberately dependency-free, and reads the array-of-tables
TOML subset through `xtask/src/toml.rs`, shared with the component fragments.
Plan 13 §6.4 wrote `provenance/*.yaml`; these are TOML because that parser
already existed and one metadata dialect beats two (D19).
