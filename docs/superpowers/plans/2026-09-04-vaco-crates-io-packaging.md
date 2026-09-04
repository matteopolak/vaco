# Implementation plan: crates.io package and CLI rename

## Scope and ownership

This plan implements the approved package `vaco` with exactly `vvmpeg` and
`vvprobe`. It does not add a player binary and does not publish to crates.io.
The current CENC owner retains `crates/app/vaco-cli/Cargo.toml`, its sources,
and MP4 tests until its work lands. The root-manifest, generated docs index,
Justfile, and existing workflows also have active owners; coordinate before
editing them.

## Steps

1. Add the package `crates/app/vaco` with a library containing the two shared
   process-entry functions and explicit `[[bin]]` targets `vvmpeg` and
   `vvprobe`. Depend on `vaco-cli` and `vaco-probe` using path-plus-exact-version
   dependencies. Test the wrappers with process invocation tests.
2. After the CENC owner releases its manifest, remove the legacy bin targets
   from `vaco-cli` and `vaco-probe`; keep their existing library APIs. Update
   command names in their user-facing docs and all checked-in command fixtures.
3. Update every internal normal/build path dependency in the calculated closure
   to carry `version = "=<workspace release version>"`. Do this in small,
   non-overlapping batches, using the audit script after each batch. Do not add
   version constraints to dev-only edges.
4. Mark all workspace packages outside the calculated closure `publish = false`.
   Resolve the one current contradiction: `vaco-vecheck-macros` is in the
   closure, so publish it or remove the production edge before this step.
5. Add `release-plz.toml` and a version-PR workflow. Configure it to update the
   synchronized workspace release version and path dependency requirements, but
   never grant it a crates.io token or publish command.
6. Add a manually dispatched publish workflow. It checks the full paginated
   open-issue list, runs `scripts/audit-publish-closure.py`, packages the
   closure, installs the staged `vaco` package to a private root, confirms the
   two expected executable names, and publishes in dependency order only after
   all gates pass.
7. Update release scripts, reproducibility targets, docs index, CI examples,
   and SBOM/package references from the old two-package/two-command shape to
   one package/two-command shape.
8. Run `cargo package --list` for each closure package with a private Cargo
   target and `CARGO_INCREMENTAL=0`; run local install smoke tests and targeted
   CLI end-to-end tests. Push release preparation evidence, but leave actual
   publication held until all GitHub issues are closed.

## Completion criteria

- `cargo install vaco` installs `vvmpeg` and `vvprobe`, and not the old names.
- The audit reports no closure or path-version violations.
- All out-of-closure workspace packages are `publish = false`.
- Release-plz can open a version PR without publishing.
- The manual publish workflow refuses a non-empty open-issues result.
- Packaging and local-install smoke tests pass; crates.io remains unchanged.
