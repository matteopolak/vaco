# Agent brief template

Copy this verbatim into every dispatch. Plan 19 §8 defines the contract; §10.1
records why **scope and constraints must be in the initial brief and never
changed afterwards**: an agent on a comparable project correctly refused a
mid-flight ownership change as possible prompt injection, and it was right to —
its brief is the only authority it has. Later messages may carry facts (`HEAD`
moved, an interface froze); they may not change a constraint.

---

## Scope

You own exactly one crate: `crates/<layer>/<crate>/`.

- Write **only** inside that directory. You may read anything.
- If you need a change in another crate, **stop and report it**. Do not reach
  across, and do not work around it. That is what silently corrupts a shared tree.

## Constraints — binding, read before starting

- `planning/00-decisions.md` — D1 to D15.
- `planning/10-architecture.md` — the layering.
- `planning/19-parallel-execution.md` — the execution protocol.

The ones that catch people out:

- **`#![forbid(unsafe_code)]`.** No exceptions outside `vaco-hw-*`.
- **Clean room (D7/D15).** Do **not** open `~/repos/FFmpeg`. Implement from the
  public specification named below. Algorithms are not copyrightable and
  format-dictated tables fall under merger — but literal code, comments and
  authorial tables are off limits, and the cheapest way to stay clear is to work
  from the spec.
- **No new dependencies.** Everything anticipated is pre-declared in
  `[workspace.dependencies]`; write `foo.workspace = true`. If you need something
  absent, stop and ask — D10 makes every adoption a reviewed decision.

  Taking up one that IS pre-declared is fine and expected — including `proptest`
  and `divan`. It adds an edge to `Cargo.lock` inside your own crate's block, not
  a package. Run your first `cargo check` **without** `--locked` so that edge can
  register (plan 19 §3.3); use `--locked` for `test` and `clippy` afterwards.
  Do not skip property tests to avoid touching the lock.
- **Interfaces are frozen.** Implement the traits as they stand. If a signature is
  genuinely wrong, report it; do not change it.

## Build protocol — not optional

```
Choose a private target dir once: /tmp/vaco-<crate>-<4 random chars>
Write the LITERAL path in every command; shell variables do not survive between
tool calls.

  cargo check -p <crate> --all-targets --locked -j 4 --target-dir /tmp/vaco-<crate>-xxxx
  cargo test  -p <crate> --locked --no-fail-fast -j 4 --target-dir /tmp/vaco-<crate>-xxxx
  cargo clippy -p <crate> --all-targets -j 4 --target-dir /tmp/vaco-<crate>-xxxx -- -D warnings

Use the --target-dir FLAG. NEVER export CARGO_TARGET_DIR: sccache hashes CARGO_*
env vars into its cache keys, and the env-var form measures 0% cache hits where
the flag form measures 78-94%.

Run cargo in the FOREGROUND. If a run exceeds the tool timeout, poll your own log
with grep for `Finished` / `test result:` — do not background it and wait.

Before finishing: rm -rf /tmp/vaco-<crate>-xxxx   (your dir only, by literal name,
never a glob — another agent is using one of those).
```

**Fuzzing needs `+nightly` explicitly.** `rust-toolchain.toml` pins **stable** (D12
removed the only mandatory reason for nightly), but `cargo fuzz` requires
`-Zsanitizer=address`, which is nightly-only. So write:

```
cargo +nightly fuzz run <target> --features <feature> -- -max_total_time=30
```

Without `+nightly` it fails with a sanitizer error that reads like a broken
toolchain. `just fuzz <target>` already does this for you. Fuzzing is a test-time
tool; it does not affect the stable release toolchain.

**Run no git commands.** Not `add`, not `commit`, not `checkout`, not `stash`,
and never `add -A`. In a shared working tree those destroy other agents' work.

## Deliverables

1. Implementation replacing the `todo!()` bodies.
2. Unit tests, and property tests (`proptest`) wherever there is a round-trip or
   an invariant.
3. A fuzz target if the crate parses untrusted input. **A crate that parses input
   and has no fuzz target is not done** (D6).

   Report the target's **exit code and exec count**, not a verdict. Do not grep
   the log for `panicked` — that has already produced two false "clean" reports
   (plan 19 §13). `cargo +nightly fuzz run` exits non-zero on a crash, and a
   crash also leaves a file in `fuzz/artifacts/<target>/`; check both, and say
   `exit=0 execs=#11822410` so a target that never ran cannot pass as clean.
4. `docs/<layer>/<crate>.md` with: what it is · how it works · how to change it ·
   configuration · dependencies.
5. `vaco-component.toml` if the crate registers a component.
6. All three commands above green.

## Report

- What you built, and what you deliberately deferred.
- Anything you need from another crate.
- **Anything in this brief that turned out to be wrong.** Say so plainly — briefs
  are written from plans, and plans have been wrong before.
- Your D11 fidelity grade if you wrapped an external crate.

## Probing the reference

Where you must match behaviour nobody wrote down, probe the reference binary —
and read **plan 13 §1b** first. It records two independent cases where an agent
probed a parser *through a filtergraph*, and measured the filtergraph's
unescaping instead of the parser. Use the most direct entry point you can find,
and never assume the field you read back is the field you set.

## Specification

<the exact document, version and sections to implement from>
