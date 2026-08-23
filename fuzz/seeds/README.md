# Fuzz seeds — inputs that once crashed

**This directory is committed. `fuzz/corpus/` is not.**

`.gitignore` excludes `fuzz/corpus`, and rightly: a corpus is large, generated,
and cached in CI (D6). But that means an input which *once crashed* has nowhere
durable to live, and the fact that it no longer crashes is precisely what makes
it worth keeping.

So: when a fuzz target produces a `crash-`, `slow-unit-` or `oom-` artifact and
the underlying bug is then fixed, the input goes here rather than being deleted.
The brief template's rule is "diagnose it, do not delete it", and "already
fixed" is a diagnosis that turns evidence into a regression seed.

## Layout

One directory per fuzz target, matching the target's name. `cargo fuzz run`
takes a path, so a seed can be replayed directly:

```bash
cargo +nightly fuzz run apetag_tag --features format-apetag \
    fuzz/seeds/apetag_tag/regression-apetag-crash-1c5c9313
```

Name a seed after what it found, keeping enough of the original artifact hash to
trace it back.

## Why not just leave them in `fuzz/artifacts/`

Because `find fuzz/artifacts -type f` being empty is part of every fuzz report,
and a **stale** artifact fails that check forever. One did, and the cost is not
the false alarm itself — it is that a check which cries wolf is one people learn
to skip, which is the same reasoning that keeps `owner-gate` to a named list and
`patent-gate` separate from `default = false`.

An artifact directory means "something is wrong now". This directory means
"something was wrong once, and here is the proof it is not any more".
