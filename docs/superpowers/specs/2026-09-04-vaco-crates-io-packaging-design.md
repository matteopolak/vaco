# Vaco crates.io packaging design

## Decision

The published package is `vaco`. `cargo install vaco` installs exactly two
executables: `vvmpeg` and `vvprobe`. It installs neither `vaco` nor
`vaco-probe` compatibility aliases. A future player executable is deliberately
outside this decision; adding it requires an explicit new `[[bin]]` target and
release audit.

The public package lives at `crates/app/vaco`. It owns both bin targets and a
library facade. The binaries call the facade's process-entry functions; they do
not assemble separate dependency graphs. The facade re-exports Vaco's public
API under stable namespaces: `media` for core media types, `codec`, `format`,
`filter`, `io`, `registry`, `cli`, and `probe`. It delegates to the separately
testable implementation libraries `vaco-cli` and `vaco-probe` rather than
moving their parser and end-to-end test code. The old application packages
cease to declare binary targets and remain published implementation libraries.

The facade never flattens exports: `vaco::codec::h264` and
`vaco::format::mp4`, for example, are distinct module paths even when their
underlying crates expose common type names. Encumbered component exports are
behind facade features that forward the corresponding registry/component
feature; those features default off. An unencumbered `vaco` install and library
dependency therefore cannot expose patent-gated codec APIs accidentally.

## Alternatives considered

1. Rename `vaco-cli` to package `vaco` and move the probe sources into it.
   This gives one package but makes a high-conflict source move across two busy
   applications and weakens their independently testable boundaries.
2. Keep both existing binary packages and add `vaco` as a meta-package. Cargo
   does not install binaries of dependencies, so `cargo install vaco` would not
   provide either command.
3. Add a `vaco` facade package owning both bins and the namespaced library API.
   This is the selected design: Cargo's package-to-binary relationship is
   correct, users receive a coherent Rust API, existing implementation
   libraries stay independently testable, and the migration is additive until
   the final removal of the old bin declarations.

## Dependency and publication boundary

The release boundary is the union of transitive Cargo graphs reachable from the
two `vaco` bin targets and the facade's supported component features, through
`normal` and `build` edges. Dev-only edges are excluded. Every internal package
in that union is published; every other workspace package is marked
`publish = false`. A dependency that has both a local path and an internal
target must also use an exact version requirement, for example:

```toml
vaco-cli = { path = "../vaco-cli", version = "=0.1.0" }
```

The exact requirement prevents a locally correct path graph from resolving to
an incompatible released crate after publication. Versions stay workspace-owned
and are advanced together by the release automation, so every internal edge
continues to name the version being released.

`release/vaco-public-api.json` is the one source of truth for the facade
namespace, its dependency package names, and its feature forwarding. A generator
reads it plus Cargo metadata to emit both `crates/app/vaco/Cargo.toml` direct
path-plus-exact-version dependencies and `crates/app/vaco/src/lib.rs`
re-exports. The generated manifest and library are never edited by hand. The
same metadata-driven plan supplies the publish audit, so no separate list can
drift between public API, dependency edges, and crates.io publication.

On the current `vaco-cli` + `vaco-probe` graph, the default feature audit finds
200 distinct internal packages and 142 external packages over 342 normal/build
nodes. The all-supported-features audit finds 221 internal and 182 external
packages over 403 nodes. The facade adds one internal package, so the planned
publish union is 222 internal packages; 28 workspace members remain outside it
and must be non-publishable. One currently reachable package
(`vaco-vecheck-macros`) is incorrectly `publish = false`; it must become
publishable or be removed from the graph. The numbers are recorded to size the
work, not hard-coded release policy: the generated migration plan and
`scripts/audit-publish-closure.py` are the release gates of record.

## Release automation and safety gate

Release-plz creates synchronized version PRs and release PRs for the published
closure. It must not publish on merge. A distinct manually dispatched publish
workflow first fetches GitHub's open issue list with pagination (excluding its
own release-tracking issue #665) and separately
calls `scripts/check-crates-io-names.py` for every exact closure package name.
The name preflight fails closed on a conflicting owner, an unexpected registry
response, or a transport failure; it happens before any `cargo publish`, so a
conflict can never leave a partial release. The workflow then runs the closure
audit, packages every release crate, and only then publishes in dependency
order. The credentials remain in the manual environment; no automated push or
release-plz job receives crates.io publish authority.

The manual workflow is an operational gate, not a substitute for review. It
also verifies that `cargo install --path crates/app/vaco` exposes precisely
`vvmpeg` and `vvprobe`, that the facade can compile with default and supported
component feature sets, and that `cargo package --list` contains both bin
sources, the generated public library, the package README, and no unexpected
generated artifacts.

## Package README

The generated `crates/app/vaco/README.md` is the crates.io landing page. It
contains one library import example checked by the facade's documentation test,
one command example per installed binary, and a short factual comparison with
FFmpeg: Vaco's clean-room safe-Rust core, the mature and broader C ecosystem it
is compared against, incomplete compatibility, mixed measured performance with
codec paths still behind, GPL-3.0-or-later versus FFmpeg's configuration-
dependent LGPL/GPL licensing, and experimental status. It makes no performance
or compatibility promise beyond those bounds.

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
