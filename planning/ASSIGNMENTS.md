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
| vaco-protocol-http | 2 | — | agent:http | done | 2026-08-22 | 2026-08-22 | ureq+rustls-rustcrypto; NATIVE_ONLY for wasm by design |
| vaco-tx | 3 | #243-#246 | agent:tx | done | 2026-08-22 | 2026-08-22 |  |
| vaco-scale | 3 | — | agent:scale | done | 2026-08-22 | 2026-08-22 |  |
| vaco-resample | 3 | — | agent:resample | done | 2026-08-22 | 2026-08-22 |  |
| vaco-codec-core | 3 | #170,#251 | agent:codec-core | done | 2026-08-22 | 2026-08-22 |  |
| vaco-codec-golomb | 3 | — | agent:codec-bits | done | 2026-08-22 | 2026-08-22 |  |
| vaco-codec-cabac | 3 | — | agent:codec-bits | done | 2026-08-22 | 2026-08-22 |  |
| vaco-codec-cbs | 3 | — | agent:hevc | done | 2026-08-22 | 2026-08-22 |  |
| vaco-codec-dsp-idct | 3 | — | agent:idct | done | 2026-08-22 | 2026-08-22 | HEVC eq. 8-317 misread by two agreeing oracles; see plan 13 §2b |
| vaco-format-core | 4 | — | agent:format-core | done | 2026-08-22 | 2026-08-22 | unblocked by vaco-io |
| vaco-format-riff | 4 | — | agent:riff-id3 | done | 2026-08-22 | 2026-08-22 | declared chunk sizes clamped, never trusted |
| vaco-format-isom | 4 | — | agent:isom | done | 2026-08-22 | 2026-08-22 |  |
| vaco-format-mpegts-tables | 4 | — | agent:mpegts | done | 2026-08-22 | 2026-08-22 |  |
| vaco-format-id3 | 4 | — | agent:riff-id3 | done | 2026-08-22 | 2026-08-22 | clean-room; issue #539's "wraps id3" premise was wrong |
| vaco-format-nalu | 4 | — | agent:h264 | done | 2026-08-22 | 2026-08-22 |  |
| vaco-parse-h264 | 4 | — | agent:h264 | done | 2026-08-22 | 2026-08-22 |  |
| vaco-parse-hevc | 4 | — | agent:hevc | done | 2026-08-22 | 2026-08-22 |  |
| vaco-parse-av1 | 4 | — | agent:av1 | done | 2026-08-22 | 2026-08-22 | cbs fits a non-NAL codec; Annex B framing does not round-trip |
| vaco-parse-aac | 4 | — | agent:audio-parse | done | 2026-08-22 | 2026-08-22 |  |
| vaco-parse-opus | 4 | — | agent:audio-parse | done | 2026-08-22 | 2026-08-22 |  |
| vaco-demux-mp4 | 4 | — | agent:demux-mp4 | done | 2026-08-22 | 2026-08-22 |  |
| vaco-demux-matroska | 4 | — | agent:matroska | done | 2026-08-22 | 2026-08-22 |  |
| vaco-demux-mpegts | 4 | — | agent:mpegts | done | 2026-08-22 | 2026-08-22 |  |
| vaco-demux-raw | 4 | — | agent:raw | assigned | 2026-08-22 |  | 48 registrations; PCM, rawvideo, bitstream |
| vaco-mux-raw | 4 | — | agent:raw | assigned | 2026-08-22 |  | 40 registrations |
| vaco-format-audio-simple | 4 | — | agent:audio-simple | assigned | 2026-08-22 |  | wav w64 aiff caf au voc sox ircam rso |
| vaco-format-apetag | 4 | — | agent:audio-simple | assigned | 2026-08-22 |  | APE tag + ReplayGain |
| vaco-format-avlanguage | 4 | — | agent:audio-simple | assigned | 2026-08-22 |  | language-code tables |
| vaco-demux-avi | 4 | — | agent:avi-flv | assigned | 2026-08-22 |  | RIFF walk, idx1/OpenDML |
| vaco-mux-avi | 4 | — | agent:avi-flv | assigned | 2026-08-22 |  |  |
| vaco-demux-flv | 4 | — | agent:avi-flv | assigned | 2026-08-22 |  | tag walk, AMF metadata |
| vaco-mux-flv | 4 | — | agent:avi-flv | assigned | 2026-08-22 |  |  |
| vaco-demux-ogg | 4 | — | agent:ogg | assigned | 2026-08-22 |  | page/packet layer, per-codec granule mapping |
| vaco-mux-ogg | 4 | — | agent:ogg | assigned | 2026-08-22 |  |  |
| vaco-demux-mpegps | 4 | — | agent:mpegps-dv | assigned | 2026-08-22 |  | shares PES with mpegts; D19 question open |
| vaco-mux-mpegps | 4 | — | agent:mpegps-dv | assigned | 2026-08-22 |  | mpeg vob svcd vcd dvd |
| vaco-format-dv | 4 | — | agent:mpegps-dv | assigned | 2026-08-22 |  | frame format, not really a container |
| vaco-filter-core | 5 | — | agent:filter-core | done | 2026-08-22 | 2026-08-22 | frozen only |
| vaco-filter-framesync | 5 | — | agent:filter-graph | done | 2026-08-22 | 2026-08-22 |  |
| vaco-filter-graph | 5 | — | agent:filter-graph | done | 2026-08-22 | 2026-08-22 |  |
| vaco-registry | 6 | — | agent:probe | done | 2026-08-22 | 2026-08-22 |  |
| vaco-textformat | 7 | #188,#189 | agent:textformat | done | 2026-08-22 | 2026-08-22 |  |
| vaco-cli-core | 7 | — | agent:cli-core | done | 2026-08-22 | 2026-08-22 | needs vaco-expr edge for `-b:v 2*1000` |
| vaco-sched | 7 | — | agent:sched | done | 2026-08-22 | 2026-08-22 | step function; threads are a driver choice, break-even ~20us/job |
| vaco-probe | 7 | — | agent:probe | done | 2026-08-22 | 2026-08-22 |  |
| vaco-cli | 7 | — | agent:cli | done | 2026-08-22 | 2026-08-22 | spine only; CL-16+ open. No muxers, so -f null is the observable path |
| vaco-conformance | 10 | #172,#173 | agent:conformance | done | 2026-08-22 | 2026-08-22 |  |
| vaco-checkasm | 10 | — | — | free |  |  |
