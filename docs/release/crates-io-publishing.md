# Crates.io publishing

## What it is

The `vaco` package is Vaco's installable distribution and public Rust facade.
It provides the `vvmpeg` media command, `vvprobe` inspection command, and
namespaced `media`, `codec`, `format`, `filter`, `io`, `registry`, `cli`, and
`probe` APIs. Internal crates are published only when Cargo needs a supported
facade feature or either command.

## How it works

`release/vaco-public-api.json` describes facade namespaces and feature
forwarding. Its generator emits the facade's direct dependencies and re-exports
from that one description. The release audit derives default and all-supported-
features normal/build closures from Cargo metadata instead of maintaining a
second hand-written package list. Internal members outside their union must opt
out with `publish = false`. Internal path dependencies inside it carry exact
release versions, allowing the same manifest to work in the workspace and on
crates.io.

Release-plz prepares version PRs only. Publishing is a manual workflow that
refuses to run while GitHub has an open issue, checks every exact closure name
against crates.io and its expected owner, then audits and packages the closure
before publishing it in dependency order.

## How to change it

After changing a production dependency, run:

```sh
RUSTC_WRAPPER= python3 scripts/plan-publish-migration.py \
  --root vaco-cli --root vaco-probe
```

During the wrapper migration, those roots become `--root vaco`; run both the
default and all-supported-feature plans. Do not add a binary target without
updating the facade descriptor and install smoke test. Do not make a tooling
crate publishable merely to silence the audit; it belongs outside the
distribution boundary.

## Configuration

`--root` chooses package roots. `--metadata` supplies a saved `cargo metadata`
document for offline investigation. `--apply` is a deliberate later mutation
step; it is not used for audit. `check-crates-io-names.py --plan <plan.json>
--expected-owner <user-or-team>` is a separate preflight. The release workflow
supplies the final root and checks GitHub issues before calling Cargo publish.

## Dependencies

The generators and audit use `cargo metadata --locked` and Python's standard
library. Publishing additionally requires a configured
crates.io token in the manually protected GitHub environment.
