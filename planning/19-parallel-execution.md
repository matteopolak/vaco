# 19 — Parallel Execution Protocol

**Constraint.** This project is built by many agents working **simultaneously**, in **one working
tree**, on **one branch (`main`)**, with **no worktrees and no feature branches**. Everything except
the initial core must be parallelisable.

That constraint is unusual and it is load-bearing. Ordinary multi-developer practice assumes branch
isolation and human-paced merges; we have neither. This document defines the protocol that makes
concurrent writing safe. It is binding on every agent.

---

## 1. The four hazards

| Hazard | Why it bites here | Mitigation |
|---|---|---|
| **Two agents edit one file** | Last write wins, silently. There is no merge conflict to catch it, because there is no merge. | §2 single-writer ownership. |
| **Shared files everyone must touch** | `Cargo.toml`, `Cargo.lock`, the registry, `Justfile` are natural serialization points that would collapse parallelism. | §3 designed them away. |
| **Concurrent `cargo` invocations** | Cargo takes an exclusive lock on the target directory. Concurrent builds block rather than fail — correct, but throughput drops to serial. | §4 per-agent target directories. |
| **Concurrent `git`** | One agent running `git add -A`, `checkout`, `reset`, `stash` or `pull` corrupts every other agent's in-flight work. | §5 no agent runs git, ever. |

---

## 2. Single-writer ownership

**The rule: every file has exactly one writing agent at any moment. The unit of ownership is the
crate directory.**

- An agent assigned `vaco-codec-flac` owns `crates/codec/vaco-codec-flac/**` and nothing else.
- It may **read** anything in the workspace. It may **write** only inside its own crate.
- Two agents are never assigned the same crate concurrently. If a crate needs two people, it needs
  splitting into two crates first — which is one more reason the architecture favours many small
  crates (`10-architecture.md` §1.7).
- Cross-crate changes are not permitted mid-flight. If crate A needs a change in crate B, the agent
  raises it; it does not reach across. This is what keeps the tree consistent without merges.

Ownership is recorded in `planning/ASSIGNMENTS.md`, which the orchestrator alone maintains — one row
per crate: crate, owner, status, started, finished.

---

## 3. Designing away the shared files

### 3.1 Workspace membership uses a glob — no root edit to add a crate

```toml
# Cargo.toml (root)
[workspace]
resolver = "3"
members  = ["crates/*/*", "xtask"]
exclude  = ["fuzz"]
```

Adding a crate is creating a directory. No agent ever edits the root manifest to register one, so the
single largest contention point disappears.

### 3.2 `[workspace.dependencies]` is pre-populated in Phase 0

Every external dependency the plans anticipate is declared once, in Phase 0, with its version. Crates
then write only:

```toml
[dependencies]
thiserror.workspace = true
vaco-core = { path = "../../core/vaco-core" }
```

An agent needing a dependency that is not pre-declared **stops and requests it** rather than editing
the root. Requests are batched by the orchestrator. This is deliberate friction: D10 makes every
adoption a reviewed decision, so an agent silently adding a dependency would violate policy anyway.

### 3.3 `Cargo.lock`: new *packages* are the orchestrator's, new *edges* are not

Agents never run `cargo add` or `cargo update`, and never edit the root `Cargo.toml`. Adding a
dependency that is not already in `[workspace.dependencies]` is a D10 decision and goes through the
orchestrator.

**But an agent writing `proptest.workspace = true` in its own crate is not that.** Every anticipated
dependency is already declared and already resolved (§3.2), so taking one up adds an *edge* inside an
existing workspace member — no new package, no new version, nothing to review. The lock line it
produces is:

```
 name = "vaco-chlayout"
 dependencies = [
+ "proptest",
```

That is expected churn, and the orchestrator commits it with the crate.

#### Who reconciles the lock, concurrently

Once agents may add edges (above), the lock is reconciled by **whichever agent
runs cargo next**, against whatever manifests exist in the tree at that instant.
The `vaco-color` agent found its own one-line lock change accompanied by five
hunks it had not authored, from four other crates, one of which appeared between
two of its own runs.

That is safe, and it is worth knowing exactly why:

- Cargo serialises writes to the lock, so there is no torn file.
- Agents only ever *add* edges, each inside its own crate's block, so there is
  no region two agents contend for. The reconciliation converges.
- No package, version or checksum can move, because no agent may edit the root
  `Cargo.toml` where versions are declared. Only edges to already-resolved
  packages can appear.

What is **not** safe is regenerating the lock at a wave boundary from a tree that
is still being edited — you would pin whatever half-written state exists at that
moment. So the orchestrator does not regenerate it. It *verifies* it, which is a
checkable invariant rather than a judgement call:

```sh
just lock-gate
```

That fails if any `name` / `version` / `source` / `checksum` / `[[package]]`
line moved, and then proves the lock is consistent with every manifest in the
tree.

It uses `cargo metadata --locked`, not `cargo check --locked`, deliberately.
`metadata` resolves the dependency graph without compiling anything, so the gate
answers *"is the lock consistent?"* and not *"does the whole workspace build?"* —
which mid-wave it does not, because some crate is always half-written. The first
version of this gate used `check` and failed on a crate that was being actively
constructed at the time, which is exactly the false alarm that trains people to
ignore a gate.

The remaining hazard is unchanged and already recorded: one agent's syntactically
broken manifest fails workspace *parsing* for everyone, which is how a `[[bench]]`
with no bench file once blocked every agent at once.

#### The `--locked` trap

As first written, this section said agents run `cargo check --locked`, which "fails loudly rather than
silently rewriting". It does — including when an agent legitimately takes up a pre-declared
dependency, because the edge is missing from the lock. The rule and the flag together made a permitted
action look forbidden.

The `vaco-color` agent read it correctly and went the conservative way: it dropped `proptest` and
hand-rolled seeded xorshift sweeps instead, losing shrinking on a crate full of table invariants.
Three concurrent agents read it the other way and added `proptest` regardless. Both were defensible
readings of a rule that could not be obeyed as stated.

So: **use `--locked` for `test` and `clippy`, but run the first `cargo check` of a crate without it**,
so a newly taken-up workspace dependency can register its edge. If that run changes anything in
`Cargo.lock` other than lines inside your own crate's `dependencies = [...]` block, stop and report —
that means a *package* moved, which is not yours.

### 3.4 The registry is assembled from per-crate fragments

`vaco-registry` is generated, never hand-edited. Each component crate ships a manifest describing what
it registers:

```toml
# crates/codec/vaco-codec-flac/vaco-component.toml
[[component]]
kind      = "decoder"
name      = "flac"
long_name = "FLA C (Free Lossless Audio Codec)"
media     = "audio"
feature   = "codec-flac"
ctor      = "vaco_codec_flac::FlacDecoder"
```

`cargo xtask gen-registry` walks `crates/**/vaco-component.toml` and emits the registry source. The
generated file is committed and reviewable; CI re-runs the generator and fails if it differs. No agent
ever writes the registry, so ~120 crates register themselves with zero contention.

### 3.4b `fuzz/Cargo.toml` is generated from the target files

The fuzz manifest was the last hand-edited shared file, and the worst of them:
adding one target meant editing **three** regions — `[dependencies]`,
`[features]` twice (the feature, then `default`), and a new `[[bin]]`. Every
agent edits it and none owns it.

One wave produced three separate failures:

- An agent patched `core = []` with a substring replace, which also matched
  `codec-core`, `protocol-core`, `format-core` and `cli-core`. It caught its own
  damage; nothing would have caught it for anyone else.
- The `default` line changed between another agent's read and its write.
- `cli-core` and `conformance` went missing from `default` altogether, so
  `cargo fuzz run cli_specifier` failed with *"requires the features"*. Three
  crates' targets were unrunnable, from lost edits nobody noticed — including by
  me, while reviewing the reports that mentioned them.

Fuzzing needed no new fragment file. `fuzz/fuzz_targets/<name>.rs` already
exists and already has exactly one author: the agent that owns the crate under
test. So the declaration lives in the target's own header —

```rust
//! fuzz-crate: vaco-core
```

— and `cargo xtask gen-fuzz` derives the path dependency, the feature name, the
`[[bin]]` block and the `default` entry from it. The thing an agent writes and
the thing it declares are now the same file, so there is nothing left to
contend for. `default` lists every feature unconditionally, so a target can
never again be silently unrunnable.

CI runs `cargo xtask gen-fuzz --check`.

**The general rule this instance illustrates:** when a shared file's entries
each belong to exactly one owner, do not ask the owners to edit the shared file
— find the file each owner *already* owns and generate from it. A fragment file
is a good answer; an existing file that happens to be a fragment is a better
one.

### 3.5 Documentation follows the same fragment pattern

Each crate owns `docs/<area>/<crate>.md`. `docs/README.md` is generated by
`cargo xtask gen-docs-index` from the front-matter of those files. No agent edits the index.

### 3.6 The orchestrator-only file list

These are **never** written by a task agent:

```
Cargo.toml  Cargo.lock  rust-toolchain.toml  Justfile  deny.toml  about.toml
clippy.toml  rustfmt.toml  .cargo/config.toml  .github/**
crates/registry/**            (generated)
docs/README.md                (generated)
fuzz/Cargo.toml               (generated from target front-matter, §3.4b)
planning/**                   (owned by the orchestrator)
```

---

## 4. Build isolation and caching

This follows the design already measured and documented in `~/projects/lodestone`
(`docs/build-caching.md`, `HANDOFF.md`) rather than inventing a new one. The numbers below are theirs,
measured on a 10-core / 16 GB M-series machine with up to eleven concurrent agents in one checkout.

### 4.1 The problem is cargo's build-directory lock, not compilation

Cargo takes an exclusive lock per target directory and concurrent builds **block** on it. Measured on
a shared `target/`: a single `cargo test` at **42m35s elapsed at 0.0% CPU** — pure lock-wait — with
two further cargo processes blocked 11 and 18 minutes, and a five-crate check taking 10m44s.

sccache does not touch that lock. **Private target dirs dodge the lock; sccache is what makes them
affordable**, because each agent's dependency graph then comes from cache instead of being recompiled
from scratch per agent.

### 4.2 The rule every agent brief carries verbatim

```
Build in a PRIVATE target dir, never the repo's `target/`:

1. Choose your dir once at task start: /tmp/vaco-<crate>-<4 random chars>
   (example: /tmp/vaco-flac-k3f9). Write the LITERAL path in every command —
   shell variables do not survive between tool calls.

2. Add `--target-dir` and `-j 4` to EVERY cargo command:

   cargo check -p vaco-codec-flac --locked -j 4 --target-dir /tmp/vaco-flac-k3f9
   cargo test  -p vaco-codec-flac --locked --no-fail-fast -j 4 --target-dir /tmp/vaco-flac-k3f9

   - The --target-dir FLAG form is MANDATORY. NEVER export CARGO_TARGET_DIR as
     an environment variable: sccache hashes CARGO_* env vars into its cache
     keys, and the env-var form measured 0% cache hits where the flag form
     measured 78-94%.
   - -j 4 bounds rustc parallelism. Without the shared-target lock there is no
     accidental admission control left, and the machine is shared by everyone.

3. sccache is active via .cargo/config.toml — set nothing yourself.

4. Before finishing, delete your dir, by its literal name:
     rm -rf /tmp/vaco-flac-k3f9
   NEVER glob /tmp/vaco-*. Another agent is using one of those.
```

The `Justfile` bakes this so agents need not retype it: every cargo recipe reads a
`VACO_TARGET_DIR` variable and expands to the flag form. Note that a `cargo xtask` **alias** cannot
carry `--target-dir`, which is exactly why the `just xtask` recipe exists — it runs the expanded
`cargo run -q -p xtask -j 4 --target-dir … --` form.

### 4.3 Configuration

```toml
# .cargo/config.toml
[build]
rustc-wrapper = "sccache"   # PATH-relative, never an absolute path

[env]
SCCACHE_CACHE_SIZE = "20G"
```

**The path must be PATH-relative.** An absolute `/opt/homebrew/bin/sccache` is a
macOS Homebrew location; it broke CI on `ubuntu-latest` immediately and would
break any contributor not on a Mac. The failure mode is confusing, too — cargo
reports `could not execute process ... (No such file or directory)` from inside
whatever tool happened to invoke it, not from a step that mentions sccache.

Because the wrapper is configured repo-wide, **`sccache` must be on PATH for any
cargo invocation here**. Every CI job that compiles installs it. Container actions
that cannot (`cargo-deny-action`, `audit-check`) set `RUSTC_WRAPPER: ""`, which
takes precedence over the config and disables it; neither compiles anything, so
nothing is lost. Contributors without sccache can export the same empty value,
but should expect much slower cold builds — and should pick one setting and keep
it, since flipping the wrapper invalidates every fingerprint in a target dir.

CI uses `mozilla-actions/sccache-action` pinned to an exact version, matching the local
`sccache` release.

### 4.4 Measured behaviour and traps

- **~87% of rustc invocations are cacheable**; a warm workspace check measured a **94.28%** hit rate.
  The non-cacheable remainder is proc-macro dylibs, build-script executables, and incrementally-
  compiled workspace crates.
- **Do NOT set `CARGO_INCREMENTAL=0`.** sccache marks workspace crates non-cacheable and passes them
  through, so incremental compilation is preserved. The edit-check loop measured at parity with and
  without the wrapper.
- **Trap — the wrapper is hashed into every fingerprint.** Adding or removing `rustc-wrapper` forces a
  full rebuild in every target directory. Set it once, in Phase 0, before any parallel work starts.
- **Trap — disk.** Per-agent target dirs accumulated to **137 GB** on the reference project. The
  cleanup-on-finish rule in §4.2 step 4 is not housekeeping; it is a hard requirement, and the
  orchestrator sweeps orphaned dirs at each wave boundary.
- **`/usr/bin/time` user-time is meaningless under the wrapper** — rustc work moves into the sccache
  daemon, outside the measured process tree. Compare wall time, or read `sccache --show-stats`.
- **One gap does not apply to us.** On the reference project, build scripts compiling vendored C
  (~1,500 translation units in one `-sys` crate) bypassed sccache entirely and were rebuilt in every
  target dir. D10 Gate 1 bans FFI and native build scripts outright, so **Vaco has no C to miss** —
  an unplanned but real benefit of the pure-Rust rule.

### 4.5 Run cargo in the foreground

A cargo run that exceeds the tool timeout should be handled by polling the agent's own log with a
cheap `grep` for `Finished` / `test result:` — **not** by backgrounding it and waiting for a
notification, which reads like progress and is not. And confirm a run has finished before reading any
count out of its log: a count read from a log cargo is still writing looks exactly like a pass.

---

## 5. Git protocol

**No task agent runs `git add -A`, `checkout`, `reset`, `stash`, `clean`, `pull`, `rebase` or
`merge`.** In a shared working tree each of those can destroy other agents' uncommitted work. This is
absolute.

Two workable commit models; we use the second:

1. *Orchestrator-only commits.* Agents report completion; the orchestrator does scoped adds and
   commits. Simple, but the orchestrator becomes a throughput bottleneck.
2. **Private-index commits (preferred).** An agent commits its own crate without ever touching the
   shared index, by using a private index file and building the commit in one step:

   ```bash
   export GIT_INDEX_FILE=/tmp/vaco-flac-k3f9.index
   git read-tree HEAD
   git add crates/codec/vaco-codec-flac docs/codec/vaco-codec-flac.md
   TREE=$(git write-tree)
   git commit-tree "$TREE" -p HEAD -m "codec-flac: initial implementation"
   ```

   The shared `.git/index` is never written, so a concurrent agent's staged state cannot be
   clobbered. The orchestrator advances the branch ref.

**A red tree mid-session is usually someone's in-flight edit, not a regression.** Before blaming a
commit, check whether the offending symbol exists at `HEAD`.

## 6. Contract-first: the one genuinely serial phase

Parallel work is only safe if the interfaces it targets already exist and do not move. Phase 0 is
therefore serial, and it is the only part that is.

**Phase 0 delivers a workspace that compiles, with every public interface present and every body
`todo!()`.** Concretely:

1. Root manifest, toolchain pin, lint configuration, profiles, `xtask`, `Justfile`, `deny.toml`.
2. Every crate directory in `10-architecture.md` created, with its manifest and dependency edges — so
   the crate graph is real and acyclic from day one.
3. Every **public** type, trait and function signature written out, compiling, unimplemented:
   `vaco-core` errors and rationals; `vaco-frame`/`vaco-packet`; the format, codec and filter traits;
   `vaco-opts`'s derive; the `KernelSet` shape.
4. `cargo check --workspace` green, `cargo doc` builds, CI pipeline running.

**Then the interfaces freeze.** After Phase 0, a signature change is a coordinated event handled by
the orchestrator across all affected crates at a wave boundary — never an ad-hoc edit by one agent.

The estimated cost is 2–3 weeks of serial work. It is worth every day of it: it converts ~120 crates
from a dependency-ordered chain into a set of independent tasks, because from that moment every agent
codes against a stable, already-compiling contract rather than against another agent's moving target.

---

## 7. Waves

Work proceeds in waves. Within a wave, everything runs concurrently; between waves, the orchestrator
commits, runs the full workspace build and test suite, resolves interface requests, and reassigns.

| Wave | Content | Concurrency | Gate to exit |
|---|---|---|---|
| **0 — Contract** | §6. Serial. | 1 | `cargo check --workspace` green; interfaces frozen. |
| **1 — Foundations** | `vaco-core`, `vaco-opts` (+derive), `vaco-simd`, `vaco-bitstream`, `vaco-expr`, `vaco-pixfmt`, `vaco-sampfmt`, `vaco-chlayout`, `vaco-color`, `vaco-frame`, `vaco-packet`, `vaco-pool`, `vaco-limits`, plus the conformance and bench harnesses. | 14 ready, **4–6 at once** | Foundations tested; harness runs against reference ffmpeg. |
| **2 — Substrate** | `vaco-io`, protocols, `vaco-format-core`, `vaco-codec-core`, `vaco-filter-core`, `vaco-tx`, `vaco-scale`, `vaco-resample`, `vaco-textformat`, `vaco-cli-core`. | 12 ready, **4–6 at once** | v0.1 dependencies met. |
| **3 — v0.1** | MP4, Matroska, MPEG-TS demuxers; H.264/HEVC/AV1/AAC/Opus header parsers; `vaco-probe`. | 10 ready, **4–6 at once** | **Byte-identical ffprobe output.** D5 met. |
| **4 — Wide** | All remaining codecs, formats, filters, muxers, protocols. This is where the crate-per-agent model pays off. | 100+ ready, **4–8 at once** | Per-crate fidelity grades recorded. |
| **5 — Integration** | `vaco-sched`, `vaco`, `vaco-play`, PGO, release engineering. | 8 ready, **3–4 at once** | Full CLI parity for the supported feature set. |

Wave 4 is the bulk of the project and is almost perfectly parallel *logically*: a codec crate depends
on `vaco-codec-core` and its DSP crates, both frozen since Wave 2, and on nothing its peers are
writing.

**But logical parallelism is not machine parallelism.** The two columns above are deliberately
separate. "Ready" is how many crates have no unmet dependency and could be started; "at once" is how
many agents should actually run, and it is bounded by the hardware, not the architecture. On a
10-core / 16 GB machine the reference project found **4–6 concurrent agents works if wiring files are
brokered, and three collide if they are not** — with eleven reached only under the full protocol in
this document. Each agent runs `-j 4`, so six agents already oversubscribe ten cores; memory, not
CPU, tends to bind first.

So the queue is deep and the throughput window is narrow. The architecture's job is to make sure
there is *always* an unblocked crate to hand the next free agent — which it does — not to run a
hundred at once.

---

## 8. The agent task contract

Every assignment carries exactly this, and an agent that cannot satisfy it stops rather than improvises:

1. **Scope** — the one crate it owns. Write nowhere else.
2. **Interface** — the traits to implement, already frozen and compiling.
3. **Specification** — the public spec document and sections to implement from. Per D7, the agent
   works from the spec and **never** opens `~/repos/FFmpeg`.
4. **Tests** — the unit tests, property tests, fuzz target and differential cases it must deliver. A
   crate without a fuzz target is not done (D6).
5. **Docs** — `docs/<area>/<crate>.md` with the five required sections.
6. **Registration** — its `vaco-component.toml`.
7. **Verification** — `cargo check -p X --locked`, `cargo test -p X --locked`, `cargo clippy -p X`,
   all green in its own target directory.
8. **Report** — what it built, what it deferred, what it needs from other crates, and its D11 fidelity
   grade if it wraps an external crate.

### Escalation, not improvisation
An agent **stops and reports** — it does not work around — when it needs a signature change in another
crate, a new external dependency, a change to a shared file, or a decision that contradicts
`00-decisions.md`. Working around any of these is what silently corrupts a shared tree.

---

## 9. Why this holds together

The whole protocol reduces to three ideas:

1. **Ownership is spatial** — one writer per directory, enforced by assignment rather than by locking.
2. **Shared state is generated, not edited** — the registry, the docs index, and workspace membership
   are all derived from per-crate fragments, so the files everyone would otherwise contend over have
   no human writers at all.
3. **Interfaces freeze before the fan-out** — which is why Phase 0 is serial and why nothing else has
   to be.

Get those three right and ~120 crates can genuinely be built at once in one tree. Get any one wrong
and the tree corrupts quietly, which is far worse than failing loudly.

---

## 10. Operational lessons

Carried over from the reference project's `HANDOFF.md`, which recorded these the expensive way.

### 10.1 Put ownership and constraints in the *initial* brief

A subagent there correctly **refused a mid-flight ownership change as possible prompt injection** — it
cannot verify who a later message is from, and its original brief is the only authority it has. That
refusal was the right behaviour, not a malfunction.

The rule: an agent's scope, its file ownership, and the constraints it works under go in the dispatch
prompt and are never changed afterwards. Later messages may carry **facts** — `HEAD` moved, an
interface was frozen, a file was released, a decision document was updated — but never a change to a
constraint the brief already stated. If a constraint genuinely must change, expect a refusal and
re-dispatch the task with a corrected brief.

### 10.2 Brokering the wiring files is the orchestrator's main job

On the reference project, measured over 200 commits, a handful of files absorbed most collisions: a
docs index (50 commits), a few registration and wiring modules (13–26 each). Every feature needs one
line in some of them — an index entry, a `pub mod`, a registration — which is why they collide
constantly.

Vaco's answer is stronger than brokering: §3 **generates** every such file from per-crate fragments,
so they have no human writers at all. Where something slips through that model, the orchestrator is
its only writer, and agents send an anchor plus the exact lines rather than editing it.

### 10.3 Brief with evidence, not conclusions

Hand an agent the evidence and the candidate explanations, not a conclusion to implement. Mark which
facts were verified and which are being passed on faith. Ask explicitly for "anything in this brief
that turned out wrong", and read that part of the report first. When an agent contradicts the brief
and is right, accept it and move on — that has already happened repeatedly during planning, and the
agents were right each time.

### 10.4 The tracker lags the tree

Before dispatching, check whether the work already exists. "Nothing exists for X" is the least
trustworthy claim available; grep for the symbol first.

### 10.5 Verify at wave boundaries, integrated

A count taken while agents are mid-edit is a sample, not a measurement — the invariant is **zero
failures**, never a number. Run the full integrated check when a group lands, and feed failures back
to the owning agent by name.

### 10.6 Review agents must be verifiably read-only

Brief a review or planning agent explicitly as read-only, require it to state what it did *not*
examine, and verify with `git status --short` that it wrote nothing. A review agent that edits is
indistinguishable from an implementing one after the fact.

---

## 11. Gate 1 verification: use the build graph, not `Cargo.lock`

Discovered while executing P0-01.

`Cargo.lock` lists a package's **optional** dependencies whether or not the enabling feature is
active. A naive `grep '^name = "ring"' Cargo.lock` therefore reports a Gate 1 violation for a build
that never compiles `ring` at all — which is exactly what happened on the first check of this
workspace.

`cargo xtask dep-gate` must query the **resolved build graph**:

```bash
cargo tree --workspace -e normal   # what actually links
cargo tree --workspace -e build    # where cc/cmake/bindgen would appear
```

and additionally fail on any `links` key or third-party `build.rs`, which is the property Gate 1
actually cares about. Checking the lock file would produce false positives that train people to
ignore the gate — worse than not having one.

Note also that `cargo tree -i <crate>` returning empty output *is* the pass signal, not a broken
command.

---

## 12. Private-index commits leave the main index stale

Discovered on the first wave-1 commit.

The `GIT_INDEX_FILE` + `commit-tree` recipe in §5 works and does keep the shared
`.git/index` untouched — which is the point. But after `git update-ref` moves the
branch, the main index still describes the *previous* commit, so `git status`
reports every file in the new commit as modified (`MM`), which looks alarmingly
like an agent having overwritten it.

Refresh the main index after moving the ref:

```bash
git update-ref refs/heads/main "$COMMIT"
git read-tree HEAD          # index only; the working tree is untouched
```

`git read-tree` is safe with agents mid-write because it never touches working
files. Do **not** reach for `git reset` here out of habit — `--hard` would destroy
every in-flight agent's work, and the `MM` display is exactly the kind of scary
symptom that invites that reflex.

## 13. Fuzzing: check the exit code, never grep the log

A fuzz run was reported as "six targets clean" when two of them had in fact
found crashes. The report came from grepping the captured output for `panicked`
in a loop. The grep missed them — libFuzzer's crash report is interleaved with
the target's own stderr and the loop's redirection dropped part of it — and
nothing else in the pipeline disagreed, so "clean" went into the ledger. The
crash artifacts sat in `fuzz/artifacts/` for two waves before an agent working
on `vaco-frame` noticed the directory was not empty and said so.

Both were real:

- `Rational::reduced()` saturated at `i32::MIN`, mapping `-1/i32::MIN` onto a
  genuinely different rational, so `cmp` disagreed with itself before and after
  reduction.
- `parse::video_rate("00:0")` returned `Some(0/0)`; the reference rejects it.

A third, `image_size` accepting a zero dimension, was found on the very next run
after the fix — the target had never got past the first crash to reach it.

### The rule

`cargo +nightly fuzz run` **exits non-zero on a crash**. That is the oracle.

```sh
cargo +nightly fuzz run "$t" -- -max_total_time=120 > "$log" 2>&1
rc=$?                       # <-- this, not `grep panicked "$log"`
```

Two further checks, both cheap, both of which would independently have caught
this:

1. **`find fuzz/artifacts -type f` must be empty afterwards.** An artifact on
   disk is a crash, whatever the log says. Check it in CI, not just by eye.
2. **Report the exec count, not the verdict.** A target that fails to build, or
   dies in the first millisecond, also produces no `panicked` line. `#11822410`
   is evidence of fuzzing; "clean" is not. Pull the last `#<n>` out of the log
   and put it in the ledger next to the target name.

### The wider point

This is the same failure as §11 (grepping `Cargo.lock` for Gate 1 instead of
asking the build graph) and the `unsafe-audit` rewrite (grepping source text
instead of asking the compiler). Three times now, a text search over a tool's
*output* has been wrong where the tool's own *result* was right there. When a
tool already computes the answer — an exit code, a resolved graph, a lint —
that answer is the oracle. Grep is for finding things to look at, not for
deciding whether something passed.

Reporting a check as passed when it was never really run is worse than reporting
a failure: a failure gets fixed, a false pass gets built on.

## 14. The orchestrator's pre-commit gate

Agents run their own three commands (`check`, `test`, `clippy`) on their own
crate. That is not sufficient, because two classes of breakage are invisible
from inside a single crate:

- **Workspace-wide gates.** `vaco-tx` was committed failing
  `cargo fmt --all -- --check`, which CI enforces. The agent never ran it —
  correctly, since `--all` reaches into crates it does not own. Only the
  orchestrator can run it, and only at commit time.
- **Unowned files.** `fuzz/fuzz_targets/` belongs to no crate. When
  `vaco-codec-core` renamed `next()` to `next_unit()`, two fuzz targets kept
  calling the old name and nobody's `cargo check -p <crate>` covered them.
  Anything outside `crates/*/*/` needs an explicit owner at integration time,
  and that owner is the orchestrator.

So before every commit, from the repository root:

```sh
cargo fmt --all -- --check          # CI enforces this; agents cannot run it
cargo xtask layer-check             # graph acyclic and downward
cargo xtask dep-gate                # D10 Gate 1: no FFI, no vendored C
cargo xtask unsafe-audit            # D2
cargo +nightly fuzz build           # the unowned directory still compiles
find fuzz/artifacts -type f         # must be empty (§13)
just lock-gate                      # Cargo.lock moved by edges only (§3.3)
```

Run `cargo fmt --all` (not `--check`) with care: it will reformat crates other
agents are actively editing. Do it immediately before committing, never in the
middle of a wave, and expect to commit the reformatting as its own change.
