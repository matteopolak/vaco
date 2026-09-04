# `vaco-bench` results store

## What it is

The benchmark results store preserves JSONL measurements on the orphan
`bench-results` branch and renders a compact, self-contained HTML view of the
latest comparable rows. It is an inspection and regression-triage aid; the
benchmark command remains the authority for whether a measurement is a
regression.

## How it works

Tracked CI writes immutable JSONL files under
`results/filter/<machine>/<yyyy-mm>/<git-sha>.jsonl`. A report job concatenates
those rows, validates the same schema used for baseline comparisons, then
selects the newest row for every complete measurement identity.

Identity includes the benchmark, scope, construction outcome, backend, unit,
and complete machine/toolchain fingerprint. The report asks the shared
trailing-seven baseline policy for each latest row, so CPU or compiler changes
remain incomparable instead of looking like a performance regression. It
renders only that already-classified result: current median/MAD/min/p95,
rolling baseline, ratio, status, source commit, and fingerprint. The HTML has
no runtime dependencies, JavaScript, or remote assets and escapes every value
from JSONL before inserting it into markup.

The report generator receives its generation timestamp explicitly. Given the
same JSONL and timestamp it emits identical bytes, which makes branch updates
reviewable and avoids a clock-dependent diff.

## How to change it

Keep JSONL parsing, identity matching, and the rolling baseline calculation in
one results-store API. The HTML renderer must not independently decide which
rows match or regress; it accepts entries that have already been classified by
that API.

When a new benchmark scope or physical metric is introduced, add it to the
identity before it can enter history. Extend the report entry adapter and its
escaping/determinism tests at the same time. Do not add a web framework or a
client-side charting dependency: the branch report must remain directly
viewable offline.

## Configuration

The eventual command accepts a combined JSONL input, an HTML output path, an
explicit report timestamp, and the same positive `--fail-under` ratio used by
the benchmark gate. CI writes reports beside their corresponding results under
`reports/filter/<machine>/index.html` and promotes a result plus its report in
one `bench-results` branch commit only after the configured gate succeeds.

The GitHub-hosted filter job remains advisory because its VM is not a controlled
benchmark machine. A dedicated runner may make the same comparison blocking
without changing result files or report generation.

## Dependencies

The renderer uses only the Rust standard library. It depends on the
`vaco-bench` JSONL schema and results-store identity/baseline policy, not on
network access, browser libraries, JavaScript, or charting packages.
