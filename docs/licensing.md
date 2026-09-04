# Licensing

## What it is

Vaco-owned source code and workspace packages are licensed under
`GPL-3.0-or-later`. The complete GNU General Public License version 3 text is in
the repository-root [`LICENSE`](../LICENSE).

## How it works

The root `[workspace.package]` declaration in `Cargo.toml` is the single source
of package-license metadata. Member manifests use `license.workspace = true`,
so published package metadata and the project-level license cannot drift.

`LICENSES/` and `THIRD_PARTY_LICENSES.html` are different: they preserve the
licenses and attribution required for third-party dependencies and reference
material. Those records do not change the license of Vaco-owned code.

## How to change it

Change the root manifest, this document, the root README, command `license`
output, and any generated command template together. Regenerate generated
facade files after changing `scripts/gen-vaco-facade.py`; do not edit generated
files as an independent source of truth. Keep third-party notice text under
`LICENSES/` unchanged unless the associated dependency or attribution record
changes.

## Configuration

`Cargo.toml` `[workspace.package].license` contains the SPDX identifier:
`GPL-3.0-or-later`.

## Dependencies

This policy relies on Cargo workspace metadata for crate packages and on
`provenance/third-party-notices.toml` plus
`scripts/gen_third_party_notices.py` for third-party attribution.
