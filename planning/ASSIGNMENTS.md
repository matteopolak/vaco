# Assignments

One row per crate. **The orchestrator is the only writer of this file.**

Ownership is spatial (plan 19 §2): an agent owns one crate directory and writes
nowhere else. Two agents are never assigned the same crate concurrently — if a
crate needs two people it needs splitting into two crates first.

Status: `free` · `assigned` · `in-review` · `done`

**`frozen` is not `done`.** Wave 0 froze every public signature with a `todo!()`
body. A crate whose interface is frozen but whose bodies are unimplemented is
`free`, not `done` — recording it as done cost real work: the `vaco-opts` agent
found `vaco-core`'s `Rational` methods still unimplemented and had to reimplement
`Dict`, `escape` and `parse` locally to make progress.

| Crate | Layer | Issue | Owner | Status | Started | Finished | Note |
|---|---|---|---|---|---|---|---|
| vaco-core | 0 | — | agent:core | done | 2026-08-21 | 2026-08-21 | tables unvalidated pending reference binary |
| vaco-limits | 0 | — | agent:bitstream | done | 2026-08-21 | 2026-08-21 |  |
| vaco-simd | 0 | #90 | agent:simd | done | 2026-08-21 | 2026-08-21 | x86 re-run outstanding |
| vaco-opts | 0 | — | agent:opts | done | 2026-08-21 | 2026-08-21 |  |
| vaco-opts-derive | 0 | — | agent:opts | done | 2026-08-21 | 2026-08-21 |  |
| vaco-time | 0 | — | orchestrator | done | 2026-08-22 | 2026-08-22 | D18: the clock, behind one door |
| vaco-expr | 0 | — | agent:expr | done | 2026-08-22 | 2026-08-22 |  |
| vaco-bitstream | 0 | — | agent:bitstream | done | 2026-08-21 | 2026-08-21 |  |
| vaco-pixfmt | 1 | — | agent:pixfmt | done | 2026-08-21 | 2026-08-21 |
| vaco-sampfmt | 1 | — | agent:audio-desc | done | 2026-08-22 | 2026-08-22 |  |
| vaco-chlayout | 1 | — | agent:audio-desc | done | 2026-08-22 | 2026-08-22 |  |
| vaco-color | 1 | — | agent:color | done | 2026-08-22 | 2026-08-22 |  |
| vaco-pool | 1 | — | agent:model | done | 2026-08-22 | 2026-08-22 |
| vaco-frame | 1 | — | agent:model | done | 2026-08-22 | 2026-08-22 |
| vaco-packet | 1 | — | agent:model | done | 2026-08-22 | 2026-08-22 |
| vaco-io | 2 | #199,#200 | agent:io | done | 2026-08-22 | 2026-08-22 |  |
| vaco-protocol-core | 2 | #535 | agent:io | done | 2026-08-22 | 2026-08-22 |  |
| vaco-protocol-file | 2 | — | agent:io | done | 2026-08-22 | 2026-08-22 |  |
| vaco-protocol-http | 2 | — | — | free |  |  |
| vaco-tx | 3 | #243-#246 | agent:tx | done | 2026-08-22 | 2026-08-22 |  |
| vaco-scale | 3 | — | agent:scale | done | 2026-08-22 | 2026-08-22 |  |
| vaco-resample | 3 | — | agent:resample | done | 2026-08-22 | 2026-08-22 |  |
| vaco-codec-core | 3 | #170,#251 | agent:codec-core | done | 2026-08-22 | 2026-08-22 |  |
| vaco-codec-golomb | 3 | — | agent:codec-bits | done | 2026-08-22 | 2026-08-22 |  |
| vaco-codec-cabac | 3 | — | agent:codec-bits | done | 2026-08-22 | 2026-08-22 |  |
| vaco-codec-cbs | 3 | — | agent:hevc | assigned | 2026-08-22 |  |  |
| vaco-codec-dsp-idct | 3 | — | — | free |  |  |
| vaco-format-core | 4 | — | agent:format-core | done | 2026-08-22 | 2026-08-22 | unblocked by vaco-io |
| vaco-format-riff | 4 | — | — | free |  |  |
| vaco-format-isom | 4 | — | agent:isom | done | 2026-08-22 | 2026-08-22 |  |
| vaco-format-mpegts-tables | 4 | — | agent:mpegts | assigned | 2026-08-22 |  |  |
| vaco-format-id3 | 4 | — | — | free |  |  |
| vaco-format-nalu | 4 | — | agent:h264 | done | 2026-08-22 | 2026-08-22 |  |
| vaco-parse-h264 | 4 | — | agent:h264 | done | 2026-08-22 | 2026-08-22 |  |
| vaco-parse-hevc | 4 | — | agent:hevc | assigned | 2026-08-22 |  |  |
| vaco-parse-av1 | 4 | — | — | free |  |  |
| vaco-parse-aac | 4 | — | agent:audio-parse | done | 2026-08-22 | 2026-08-22 |  |
| vaco-parse-opus | 4 | — | agent:audio-parse | done | 2026-08-22 | 2026-08-22 |  |
| vaco-demux-mp4 | 4 | — | agent:demux-mp4 | assigned | 2026-08-22 |  |  |
| vaco-demux-matroska | 4 | — | agent:matroska | assigned | 2026-08-22 |  |  |
| vaco-demux-mpegts | 4 | — | agent:mpegts | assigned | 2026-08-22 |  |  |
| vaco-filter-core | 5 | — | agent:filter-core | done | 2026-08-22 | 2026-08-22 | frozen only |
| vaco-filter-framesync | 5 | — | agent:filter-graph | assigned | 2026-08-22 |  |  |
| vaco-filter-graph | 5 | — | agent:filter-graph | assigned | 2026-08-22 |  |  |
| vaco-registry | 6 | — | agent:probe | assigned | 2026-08-22 |  |  |
| vaco-textformat | 7 | #188,#189 | agent:textformat | done | 2026-08-22 | 2026-08-22 |  |
| vaco-cli-core | 7 | — | agent:cli-core | done | 2026-08-22 | 2026-08-22 | needs vaco-expr edge for `-b:v 2*1000` |
| vaco-sched | 7 | — | — | free |  |  |
| vaco-probe | 7 | — | agent:probe | assigned | 2026-08-22 |  |  |
| vaco-cli | 7 | — | — | free |  |  |
| vaco-conformance | 10 | #172,#173 | agent:conformance | done | 2026-08-22 | 2026-08-22 |  |
| vaco-checkasm | 10 | — | — | free |  |  |
