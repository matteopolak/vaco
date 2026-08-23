## What this changes

<!-- One paragraph. What behaviour is different afterwards? -->

## How it was verified

<!-- Measured numbers, not adjectives. If you benchmarked, give ratios. If you
     probed the reference, say which command and what it printed. If you fuzzed,
     give `exit=0 execs=#…` and confirm `fuzz/artifacts` is empty. -->

## Clean-room checklist

- [ ] I have **not** read FFmpeg / libav / x264 / x265 / VLC / GStreamer source for the module(s) this PR touches.
- [ ] Every constant table added cites the specification clause it was transcribed from, in `provenance/<crate>.toml`.
- [ ] No table was copied from another implementation's source — including permissively-licensed ones — without being recorded in `THIRD_PARTY.md` with its licence.
- [ ] No text (comments, help strings, docs) was copied from FFmpeg or from a standards document.
- [ ] Tests compare against spec-defined output, a round-trip invariant, or a freshly-run reference binary — **not** against checksums copied from another project's repository.
- [ ] `Vaco-Provenance:` is present on every commit that touches implementation code, and `cargo xtask provenance-check` passes.
- [ ] If any Tier-B material was consulted: I am the dirty-team member for this module and I have **not** authored implementation code here.
- [ ] If this PR adds or changes an external dependency: `docs/dependencies.md` records the D10 gate assessment, and the crate is reachable from exactly one Vaco crate (D11).

<!-- The checklist is not ceremony. A clean-room claim is worth exactly what its
     record can show, and the record is these boxes plus provenance/*.toml. -->
