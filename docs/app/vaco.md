# `vaco` facade and commands

## What it is

The `vaco` package is the installable public facade for Vaco. It owns the
`vvmpeg` media command, `vvprobe` inspection command, and the namespaced Rust
API that exposes supported Vaco implementation crates.

## How it works

`release/vaco-public-api.json` selects the existing CLI and probe roots and
defines stable namespace groups. `scripts/gen-vaco-facade.py` combines that
descriptor with locked default and all-feature Cargo metadata to generate this
package's direct exact-version dependencies, feature forwarding, `src/lib.rs`
re-exports, and package README. The README is included in the facade's crate
documentation, so its Rust examples are doctests. `vvmpeg` calls
`vaco::cli::run`; `vvprobe` calls `vaco::probe::run`. Legacy `vaco` and
`vaco-probe` binary targets are absent.

## How to change it

Edit the descriptor or generator template, regenerate the facade into a private
directory, inspect the generated manifest, namespaces, and README, then copy it
into `crates/app/vaco` in one complete operation. Do not hand-edit generated
facade files. Keep README library examples against facade paths rather than
implementation crates; they are part of the public API contract. Add a feature
forward when an opt-in registry component becomes public, preserving
`default = false` for encumbered components.

## Configuration

The public library is feature-gated. Default features remain unencumbered;
facade features such as `patent-encumbered-h264-decode` forward the matching
`vaco-registry` feature and are opt-in. The package README is the crates.io
landing page and documents the commands and API scope.

## Dependencies

The generated direct dependencies are the supported normal/build closure of
the CLI and probe roots. They use both `path` and exact `version` requirements
so the same facade resolves locally and after publication. Release tooling
checks that boundary and crates.io name ownership before a manual publish.
