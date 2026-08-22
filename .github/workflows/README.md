# CI

`ci.yml` runs four jobs. The ordering is deliberate: the `policy` job is seconds
long and catches the mistakes that are cheapest to make, so it fails fast before
anything spends a build.

Not yet wired, because the machinery they gate does not exist yet:

- **`patent-check`** (D4) — assert no component carrying `Caps::PATENT_ENCUMBERED`
  is reachable from a default-feature build. Add with the first encumbered codec.
- **`conformance-smoke`** (D6) — differential comparison against the pinned
  reference binary. Add with `vaco-conformance`, in wave 1.
- **`refbin-drift`** — nightly run against a newer reference ffmpeg, non-blocking,
  so upstream behaviour changes are triaged weeks before they can block a release.
- **`fuzz-nightly`** — the ~500 generated fuzz targets.
- **`bench-regression`** — divan results compared against the `bench-results`
  branch with Mann-Whitney gating.
