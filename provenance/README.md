# Provenance

One file per crate that carries a large constant table, recording where the
numbers came from. `cargo xtask provenance-check` reads these and fails when a
table of 32 or more elements has no entry — **and when an entry names a table
that no longer exists**, because a record that quietly rots into fiction is
worse than none.

```toml
[[source]]
id       = "itu-t-h264-202108"
kind     = "spec"          # spec | rfc | paper | blackbox | original
title     = "ITU-T Rec. H.264 (08/2021)"
acquired  = "2026-08-21"
where     = "https://www.itu.int/rec/T-REC-H.264"

[[table]]
name   = "RANGE_TAB_LPS"
file   = "crates/signal/vaco-codec-cabac/src/tables.rs"
source = "itu-t-h264-202108"
clause = "Table 9-44, §9.3.3.2.1.1"   # required when the source is a document
method = "transcribed"                 # transcribed | derived | probed | original
```

`kind` says what the document is. `method` says what we did with it, and the two
answer different questions — "transcribed from Table 9-44" and "derived by
evaluating the standard's equation" are both `spec`, and only one of them
survives somebody finding an arithmetic error in the other.

`blackbox` means the values were measured from the reference binary's observable
output rather than read from a document. Recording the observed behaviour of a
shipped binary is not copying its expression (D6), but it is a different kind of
evidence from a specification clause and it is labelled as one.

Plan 13 §6.4 wrote `provenance/*.yaml`. These are TOML: xtask is
dependency-free and already had a parser for this subset, and one metadata
dialect in the repository beats two (D19).
