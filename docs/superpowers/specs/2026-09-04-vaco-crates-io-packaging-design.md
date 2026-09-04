# Vaco crates.io packaging design

## Decision

The published package is `vaco`. `cargo install vaco` installs exactly two
executables: `vvmpeg` and `vvprobe`. It installs neither `vaco` nor
`vaco-probe` compatibility aliases. A future player executable is deliberately
outside this decision; adding it requires an explicit new `[[bin]]` target and
release audit.

The public package is a small application wrapper at `crates/app/vaco`. It owns
both bin targets and delegates to separately testable implementation libraries:
`vaco-cli` for `vvmpeg` and `vaco-probe` for `vvprobe`. This avoids moving their
existing parser and end-to-end test code while making the install surface a
single package. The old application packages cease to declare binary targets;
they remain libraries in the normal dependency closure and are published only
because `vaco` needs them.

## Alternatives considered

1. Rename `vaco-cli` to package `vaco` and move the probe sources into it.
   This gives one package but makes a high-conflict source move across two busy
   applications and weakens their independently testable boundaries.
2. Keep both existing binary packages and add `vaco` as a meta-package. Cargo
   does not install binaries of dependencies, so `cargo install vaco` would not
   provide either command.
3. Add a thin `vaco` wrapper package owning both bins. This is the selected
   design: Cargo's package-to-binary relationship is correct, the existing
   libraries stay independently testable, and the migration is additive until
   the final removal of the old bin declarations.

## Dependency and publication boundary

The release boundary is the transitive Cargo dependency graph reachable from
the two `vaco` bin targets through `normal` and `build` edges. Dev-only edges
are excluded. Every internal package in that graph is published; every other
workspace package is marked `publish = false`. A dependency that has both a
local path and an internal target must also use an exact version requirement,
for example:

```toml
vaco-cli = { path = "../vaco-cli", version = "=0.1.0" }
```

The exact requirement prevents a locally correct path graph from resolving to
an incompatible released crate after publication. Versions stay workspace-owned
and are advanced together by the release automation, so every internal edge
continues to name the version being released.

On the current `vaco-cli` + `vaco-probe` graph, the audit finds 200 distinct
internal packages and 142 external packages over 342 normal/build nodes. The
new wrapper increases the internal count to 201. One currently reachable
package (`vaco-vecheck-macros`) is incorrectly `publish = false`; it must become
publishable or be removed from the graph. Of 249 workspace members, 49 are
outside the current closure and 46 of those are presently publishable; the
migration makes all 49 non-publishable. The numbers are recorded to size the
work, not hard-coded release policy: `scripts/audit-publish-closure.py` is the
release gate of record.

## Release automation and safety gate

Release-plz creates synchronized version PRs and release PRs for the published
closure. It must not publish on merge. A distinct manually dispatched publish
workflow first fetches GitHub's open issue list with pagination, refuses when
any issue is open, runs the closure audit, packages every release crate, and
only then publishes in dependency order. The credentials remain in the manual
environment; no automated push or release-plz job receives crates.io publish
authority.

The manual workflow is an operational gate, not a substitute for review. It
also verifies that `cargo install --path crates/app/vaco` exposes precisely
`vvmpeg` and `vvprobe`, and that `cargo package --list` contains both bin
sources and no unexpected generated artifacts.

## Migration compatibility

The command rename is intentionally breaking for source installs and packaged
artifacts: scripts must change `vaco` to `vvmpeg` and `vaco-probe` to
`vvprobe`. There are no legacy aliases, because aliases would make `cargo
install vaco` install more binaries than the chosen contract. Package names
`vaco-cli` and `vaco-probe` remain implementation crates and are not CLI
compatibility promises.

## Validation

The closure audit rejects an out-of-closure publishable workspace member, a
reachable `publish = false` package, a missing exact version on an internal
path edge, or a path target outside the closure. Packaging validation runs
`cargo package --list` for every publishable internal crate and then performs a
local `cargo install --path` of `vaco` into an empty private root. The publish
workflow runs the same checks after confirming that GitHub reports zero open
issues. No crates.io publish happens until every project issue is closed.
