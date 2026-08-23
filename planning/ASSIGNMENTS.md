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
| vaco-hash | 0 | — | orchestrator | done | 2026-08-23 | 2026-08-23 | D11 merge: the single owner of crc/md-5/sha1/sha2, split out of vaco-probe and vaco-mux-hash |
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
| vaco-demux-raw | 4 | — | agent:raw | done | 2026-08-22 | 2026-08-23 | 48 registrations; PCM, rawvideo, bitstream |
| vaco-mux-raw | 4 | — | agent:raw | done | 2026-08-22 | 2026-08-23 | 40 registrations |
| vaco-format-audio-simple | 4 | — | agent:audio-simple | done | 2026-08-22 | 2026-08-23 | wav w64 aiff caf au voc sox ircam rso |
| vaco-format-apetag | 4 | — | agent:audio-simple | done | 2026-08-22 | 2026-08-23 | APE tag + ReplayGain |
| vaco-format-avlanguage | 4 | — | agent:audio-simple | done | 2026-08-22 | 2026-08-23 | language-code tables |
| vaco-demux-avi | 4 | — | agent:avi-flv | done | 2026-08-22 | 2026-08-23 | RIFF walk, idx1/OpenDML |
| vaco-mux-avi | 4 | — | agent:avi-flv | done | 2026-08-22 | 2026-08-23 |  |
| vaco-demux-flv | 4 | — | agent:avi-flv | done | 2026-08-22 | 2026-08-23 | tag walk, AMF metadata |
| vaco-mux-flv | 4 | — | agent:avi-flv | done | 2026-08-22 | 2026-08-23 |  |
| vaco-demux-ogg | 4 | — | agent:ogg | done | 2026-08-22 | 2026-08-23 | page/packet layer, per-codec granule mapping |
| vaco-mux-ogg | 4 | — | agent:ogg | done | 2026-08-22 | 2026-08-23 |  |
| vaco-demux-mpegps | 4 | — | agent:mpegps-dv | done | 2026-08-22 | 2026-08-23 | shares PES with mpegts; D19 question open |
| vaco-mux-mpegps | 4 | — | agent:mpegps-dv | done | 2026-08-22 | 2026-08-23 | mpeg vob svcd vcd dvd |
| vaco-format-dv | 4 | — | agent:mpegps-dv | done | 2026-08-22 | 2026-08-23 | frame format, not really a container |
| vaco-filter-audio | 5 | #466 | agent:filters-t1a | done | 2026-08-22 | 2026-08-23 | aresample aformat volume amix amerge channelmap channelsplit join pan asetnsamples asetrate |
| vaco-filter-plumbing | 5 | #467 | agent:filters-t1a | done | 2026-08-22 | 2026-08-23 | trim/atrim, setpts, settb, fifo family |
| vaco-format-isom | 4 | #210,#573,#574 | agent:mux-mp4 | assigned | 2026-08-23 |  | reassigned from agent:demux-mp4 (done) to add box writers |
| vaco-mux-mp4 | 4 | #210,#573,#574 | agent:mux-mp4 | assigned | 2026-08-23 |  |  |
| vaco-format-ebml | 4 | #575 | agent:mux-matroska | assigned | 2026-08-23 |  | new: EBML reader extracted from the demuxer + writer |
| vaco-demux-matroska | 4 | #575 | agent:mux-matroska | assigned | 2026-08-23 |  | reassigned for the EBML extraction only; behaviour must not change |
| vaco-mux-matroska | 4 | #575 | agent:mux-matroska | assigned | 2026-08-23 |  | matroska webm matroska_audio webm_chunk |
| vaco-format-asf | 4 | #586,#587 | agent:asf | assigned | 2026-08-23 |  | new: shared object model |
| vaco-demux-asf | 4 | #586 | agent:asf | assigned | 2026-08-23 |  |  |
| vaco-mux-asf | 4 | #587 | agent:asf | assigned | 2026-08-23 |  |  |
| vaco-mux-hash | 4 | #572 | agent:mux-hash | done | 2026-08-23 | 2026-08-23 | crc framecrc framemd5 framehash hash md5 streamhash uncodedframecrc — the differential oracle |
| vaco-demux-image2 | 4 | #592 | agent:image2 | assigned | 2026-08-23 |  | glob/sequence patterns + 42 pipe splitters |
| vaco-mux-image2 | 4 | #593 | agent:image2 | assigned | 2026-08-23 |  | filename patterns, -update, strftime, atomic write |
| vaco-format-mpegts-tables | 4 | #576 | agent:mux-mpegts | assigned | 2026-08-23 |  | reassigned from agent:mpegts (done) to add table writers |
| vaco-mux-mpegts | 4 | #576 | agent:mux-mpegts | assigned | 2026-08-23 |  |  |
| vaco-format-mpegts-tables | 4 | #576 | agent:mux-mpegts | assigned | 2026-08-23 |  | reassigned from agent:mpegts (done) to add table writers |
| vaco-mux-mpegts | 4 | #576 | agent:mux-mpegts | assigned | 2026-08-23 |  |  |
| vaco-mux-utility | 4 | #572 | agent:mux-util | assigned | 2026-08-23 |  | null, mkvtimestamp_v2 — the last two of FM-20 bar uncodedframecrc |
| vaco-mux-stream | 4 | #590 | agent:mux-util | assigned | 2026-08-23 |  | concat ffmetadata segment stream_segment tee fifo — meta-muxers |
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
