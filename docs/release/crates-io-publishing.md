# Crates.io publishing

## What it is

The `vaco` package is the installable distribution of Vaco. It provides the
`vvmpeg` media command and `vvprobe` inspection command; internal crates are
published only when Cargo needs them to resolve this package.

## How it works

The release audit derives the normal/build dependency closure from Cargo
metadata instead of maintaining a hand-written package list. Internal members
outside that closure must opt out with `publish = false`. Internal path
dependencies inside it carry exact release versions, allowing the same manifest
to work in the workspace and on crates.io.

Release-plz prepares version PRs only. Publishing is a manual workflow that
refuses to run while GitHub has an open issue, then audits and packages the
closure before publishing it in dependency order.

## How to change it

After changing a production dependency, run:

```sh
RUSTC_WRAPPER= python3 scripts/audit-publish-closure.py \
  --root vaco-cli --root vaco-probe
```

During the wrapper migration, those roots become `--root vaco`. Do not add a
binary target without updating the wrapper package and the install smoke test.
Do not make a tooling crate publishable merely to silence the audit; it belongs
outside the distribution boundary.

## Configuration

`--root` chooses package roots. `--metadata` supplies a saved `cargo metadata`
document for offline investigation. The release workflow supplies the final
root and checks GitHub issues before calling Cargo publish.

## Dependencies

The audit uses `cargo metadata --locked` and Python's standard library.
Publishing additionally requires a configured
crates.io token in the manually protected GitHub environment.
