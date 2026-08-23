# The filter crate partition drifted from plan 16 §4.3, and it was the orchestrator's doing

`planning/16-filters.md` §4.3 and §4.4 carry an authoritative table: one row per
`vaco-filter-*` crate, with the exact filter list, the shared kernels it leans
on, and its tier. It is a deliberate partition — crates are grouped by the
kernels they share (`vdsp`, `adsp`, `draw`, `framesync`), not by topic — and it
is the single source of truth D19 asks for.

Several dispatch briefs invented crate names and memberships instead of reading
it. That is an orchestrator error, not an agent one: every agent built exactly
what its brief asked for, and three of them checked the *reference* carefully
enough to correct my filter lists while having no reason to question the crate
name.

## What diverged

| Built | Plan says | Note |
|---|---|---|
| `vaco-filter-audio-eq` | `vaco-filter-aeq` | membership matches; name only |
| `vaco-filter-audio-dynamics` | `vaco-filter-adynamics` | membership matches; name only |
| `vaco-filter-ameasure` | `vaco-filter-aanalysis` | membership matches exactly; name only |
| ~~`vaco-filter-achannel`~~ `vaco-filter-aeffects` | `vaco-filter-aeffects` | **Done** (FT-4.13d, GitHub #484): renamed, and the crate now implements 22 of the row's 25 filters — `surround`/`headphone` remain deferred as disproportionately large (flagged by this crate's original author), and `hdcd`'s proprietary bit-level decode is out of reach for black-box probing at this project's clean-room standard. |
| `vaco-filter-video-geometry` | `vaco-filter-geometry` | holds the T1 subset of one plan row |
| `vaco-filter-video-format` | `vaco-filter-scale` | |
| `vaco-filter-video-source` | `vaco-filter-source` | |
| `vaco-filter-component` | *(no such row)* | its filters belong to `geometry`, `color`, `key` and `lut` |
| `vaco-filter-audio` | *(no such row)* | spans `aformat`, `amix`, `adynamics` |
| `vaco-filter-plumbing` | `vaco-filter-mm` | |
| `vaco-filter-blur` | `vaco-filter-blur` | name right; brief added the `convolve` row's filters to it |
| `vaco-filter-denoise` | `vaco-filter-denoise` | correct, both name and membership |

## The concrete cost

`axcorrelate` was listed in two briefs at once (#482 and #483). The second agent
implemented it in full, tested it, and then deleted it when `gen-registry`'s
duplicate-name check surfaced the collision. Wasted work that the plan's table
would have prevented, because `axcorrelate` appears in exactly one row.

## The rule going forward

**A dispatch brief for a filter crate must quote its row from
`planning/16-filters.md` §4.3/§4.4 and must not name a crate the table does not
have.** If the reference has a filter the table places nowhere, that is a change
to the table — proposed in a commit that says why — not a new crate invented at
dispatch time.

## Reconciliation, still to do

Renaming is mechanical but touches registry fragments, `docs/`, fuzz targets and
`provenance/` for each crate, so it wants one sweep with no filter agents in
flight rather than six concurrent renames. The rows above are the work list.

Two of them are more than a rename and need a decision first:

- `vaco-filter-video-geometry` holds the T1 subset (`crop`, `pad`, `transpose`,
  the flips) of the plan's single `vaco-filter-geometry` row, whose T2 remainder
  is being written separately. Either they merge, or the plan grows an explicit
  T1/T2 split. Merging is truer to §4.3.
- `vaco-filter-audio` spans three plan rows and predates all of this. Splitting
  it is the largest single piece of the reconciliation.

## Resolved during the wave

`vaco-filter-component` never landed. The agent building it was redirected
mid-flight and shipped `vaco-filter-color`, `vaco-filter-key` and
`vaco-filter-lut` instead — three real rows from §4.3. `vaco-filter-blur`'s
author likewise split the convolution and morphology family out into
`vaco-filter-convolve`, its own row, rather than leaving it misfiled.

So the table above is now three rows shorter in the "built a crate the plan
does not have" category, and the remaining work is renames plus the two
structural decisions.

## What the redirect cost, recorded honestly

The `component` agent had working, tested implementations of `extractplanes`,
`mergeplanes`, `alphamerge` and `maskedmerge` and deleted them rather than push
into crates it did not own — which was the right call under a single-writer
rule, and was still four filters of wasted work caused by the brief, not by the
agent. `maskedmerge` survived in `vaco-filter-key`; the other three did not.

Their shape is worth keeping even though the code is gone, because it is
independent evidence about `INTERFACE-GAPS.md` gap 10: `extractplanes` is
dynamic-output V→N, `mergeplanes` is **dynamic-input N→V**, and `alphamerge`
runs through `framesync` directly. The gap-10 entry originally named only a
two-input adapter and a dynamic-output one; the N-input case is a third shape
and neither covers it.
