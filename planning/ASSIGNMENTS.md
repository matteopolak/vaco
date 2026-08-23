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
| vaco-demux-matroska | 4 | #570 | agent:demux-finish | done | 2026-08-23 | 2026-08-23 | cues, tags, chapters, attachments, delay/preroll/padding |
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
| vaco-format-isom | 4 | #210,#573,#574 | agent:mux-mp4 | done | 2026-08-23 | 2026-08-23 | reassigned from agent:demux-mp4 (done) to add box writers |
| vaco-mux-mp4 | 4 | #210,#573,#574 | agent:mux-mp4 | done | 2026-08-23 | 2026-08-23 |  |
| vaco-format-ebml | 4 | #575 | agent:mux-matroska | done | 2026-08-23 | 2026-08-23 | new: EBML reader extracted from the demuxer + writer |
| vaco-demux-matroska | 4 | #570 | agent:demux-finish | done | 2026-08-23 | 2026-08-23 | cues, tags, chapters, attachments, delay/preroll/padding |
| vaco-mux-matroska | 4 | #575 | agent:mux-matroska | done | 2026-08-23 | 2026-08-23 | matroska webm matroska_audio webm_chunk |
| vaco-format-asf | 4 | #586,#587 | agent:asf | done | 2026-08-23 | 2026-08-23 | new: shared object model |
| vaco-demux-asf | 4 | #586 | agent:asf | done | 2026-08-23 | 2026-08-23 |  |
| vaco-mux-asf | 4 | #587 | agent:asf | done | 2026-08-23 | 2026-08-23 |  |
| vaco-mux-hash | 4 | #572 | agent:mux-hash | done | 2026-08-23 | 2026-08-23 | crc framecrc framemd5 framehash hash md5 streamhash uncodedframecrc — the differential oracle |
| vaco-demux-image2 | 4 | #592 | agent:image2 | done | 2026-08-23 | 2026-08-23 | glob/sequence patterns + 42 pipe splitters |
| vaco-mux-image2 | 4 | #593 | agent:image2 | done | 2026-08-23 | 2026-08-23 | filename patterns, -update, strftime, atomic write |
| vaco-format-mpegts-tables | 4 | #576 | agent:mux-mpegts | done | 2026-08-23 | 2026-08-23 | reassigned from agent:mpegts (done) to add table writers |
| vaco-mux-mpegts | 4 | #576 | agent:mux-mpegts | done | 2026-08-23 | 2026-08-23 |  |
| vaco-format-mpegts-tables | 4 | #576 | agent:mux-mpegts | done | 2026-08-23 | 2026-08-23 | reassigned from agent:mpegts (done) to add table writers |
| vaco-mux-mpegts | 4 | #576 | agent:mux-mpegts | done | 2026-08-23 | 2026-08-23 |  |
| vaco-mux-utility | 4 | #572 | agent:mux-util | done | 2026-08-23 | 2026-08-23 | null, mkvtimestamp_v2 — the last two of FM-20 bar uncodedframecrc |
| vaco-mux-stream | 4 | #590 | agent:mux-util | done | 2026-08-23 | 2026-08-23 | concat ffmetadata segment stream_segment tee fifo — meta-muxers |
| vaco-demux-mp4 | 4 | #565,#566,#567 | agent:demux-finish | done | 2026-08-23 | 2026-08-23 | fragmented, metadata, CENC reporting, HEIF items |
| vaco-format-subtitle | 4 | #591 | agent:subs | done | 2026-08-23 | 2026-08-23 | new: shared cue model |
| vaco-subtitle-text | 4 | #591 | agent:subs | done | 2026-08-23 | 2026-08-23 | 15 demux / 6 mux, count to be verified |
| vaco-demux-hls | 4 | #600 | agent:adaptive | assigned | 2026-08-23 |  |  |
| vaco-mux-hls | 4 | #601 | agent:adaptive | assigned | 2026-08-23 |  |  |
| vaco-demux-dash | 4 | #602 | agent:adaptive | assigned | 2026-08-23 |  | quick-xml |
| vaco-mux-dash | 4 | #603 | agent:adaptive | assigned | 2026-08-23 |  |  |
| vaco-protocol-local | 3 | #544 | agent:protocols | done | 2026-08-23 | 2026-08-23 | data:, md5:. fd: ruled out by D16 — needs unsafe FromRawFd |
| vaco-protocol-wrap | 3 | #545 | agent:protocols | done | 2026-08-23 | 2026-08-23 | subfile concat concatf cache tee async |
| vaco-protocol-file | 3 | #544 | agent:protocols | done | 2026-08-23 | 2026-08-23 | had NO vaco-component.toml — file:/pipe: were never registered |
| vaco-hash | 0 | — | orchestrator | done | 2026-08-23 | 2026-08-23 | D11 merge of crc/md-5/sha1/sha2 |
| vaco-format-core | 4 | — | agent:iface | done | 2026-08-23 | 2026-08-23 | INTERFACE-GAPS 1/4/5/6: metadata channel, options on open, MuxerDesc flags |
| vaco-demux-mxf | 4 | #604-607 | agent:mxf | done | 2026-08-23 | 2026-08-23 | KLV, structural metadata, essence, index tables |
| vaco-protocol-socket | 3 | #547 | agent:net | done | 2026-08-23 | 2026-08-23 | tcp udp udplite unix |
| vaco-protocol-tls | 3 | #548 | agent:net | done | 2026-08-23 | 2026-08-23 | rustls + D14.2 root store |
| vaco-protocol-http | 3 | #549 | agent:net | done | 2026-08-23 | 2026-08-23 | range/seek/reconnect/ICY/chunked POST |
| vaco-conformance | 10 | #196,#211 | agent:conf | done | 2026-08-23 | 2026-08-23 | the remux byte-identity matrix — possible now the CLI writes files |
| vaco-demux-raw | 4 | — | agent:probefix | done | 2026-08-23 | 2026-08-23 | CONFORMANCE-FINDINGS 3: start-code identifiers per format |
| vaco-format-isom | 4 | — | agent:isom-codec | done | 2026-08-23 | 2026-08-23 | PCM resolution needs fourcc + bits + enda; 53-row ESDS table |
| vaco-format-subtitle-bitmap | 4 | #611 | agent:subbmp | done | 2026-08-23 | 2026-08-23 |  |
| vaco-subtitle-bitmap | 4 | #611 | agent:subbmp | done | 2026-08-23 | 2026-08-23 | dvbsub dvbtxt sup vobsub |
| vaco-mux-avi | 4 | — | agent:muxfix | assigned | 2026-08-23 |  | CONFORMANCE-FINDINGS 14,16-22 |
| vaco-mux-flv | 4 | — | agent:muxfix | assigned | 2026-08-23 |  |  |
| vaco-format-rtp | 4 | #597-599 | agent:rtp | assigned | 2026-08-23 |  | RTP/RTCP model + SDP |
| vaco-demux-rtsp | 4 | #597 | agent:rtp | assigned | 2026-08-23 |  |  |
| vaco-mux-rtp | 4 | #599 | agent:rtp | assigned | 2026-08-23 |  |  |
| vaco-filter-video-geometry | 5 | #54 | agent:vfilt | assigned | 2026-08-23 |  | scale crop pad transpose flips |
| vaco-filter-video-format | 5 | #54 | agent:vfilt | assigned | 2026-08-23 |  | format setsar setdar fps |
| vaco-filter-video-source | 5 | #54 | agent:vfilt | assigned | 2026-08-23 |  | color testsrc smptebars sinks |
| vaco-filter-core | 5 | — | agent:filter-core | done | 2026-08-22 | 2026-08-22 | frozen only |
| vaco-filter-framesync | 5 | — | agent:filter-graph | done | 2026-08-22 | 2026-08-22 |  |
| vaco-filter-graph | 5 | — | agent:filter-graph | done | 2026-08-22 | 2026-08-22 |  |
| vaco-registry | 6 | — | agent:probe | done | 2026-08-22 | 2026-08-22 |  |
| vaco-textformat | 7 | #188,#189 | agent:textformat | done | 2026-08-22 | 2026-08-22 |  |
| vaco-cli-core | 7 | — | agent:cli-core | done | 2026-08-22 | 2026-08-22 | needs vaco-expr edge for `-b:v 2*1000` |
| vaco-sched | 7 | — | agent:cli-mux | done | 2026-08-23 | 2026-08-23 | build_work now calls Muxer::init() before reading stream_time_base |
| vaco-probe | 7 | — | agent:probe | done | 2026-08-22 | 2026-08-22 |  |
| vaco-cli | 7 | — | agent:cli-mux | done | 2026-08-23 | 2026-08-23 | muxers wired: -c copy remux writes a real file |
| vaco-conformance | 10 | #172,#173 | agent:conformance | done | 2026-08-22 | 2026-08-22 |  |
| vaco-checkasm | 10 | — | — | free |  |  |
| vaco-filter-blur | 5 | #468 | agent:blur | assigned | 2026-08-23 |  | gblur boxblur unsharp smartblur convolution sobel and the rest of FT-4.6a |
| vaco-filter-denoise | 5 | #469 | agent:denoise | assigned | 2026-08-23 |  | hqdn3d atadenoise removegrain nlmeans owdenoise |
| vaco-filter-geometry | 5 | #470 | agent:geom2 | assigned | 2026-08-23 |  | T2 geometry (~28) — distinct from vaco-filter-video-geometry's T1 set |
| vaco-filter-component | 5 | #476 | agent:component | assigned | 2026-08-23 |  | T3 pixel-format, bit-depth and component filters (~20) |
| vaco-filter-achannel | 5 | #482 | agent:achannel | done | 2026-08-23 | 2026-08-23 | T3 channel, layout and mixing filters (~14) |
| vaco-filter-ameasure | 5 | #483 | agent:ameasure | assigned | 2026-08-23 |  | T3 audio analysis and measurement filters (~14) |
| vaco-demux-mpegts | 4 | #632 | agent:tspkt | assigned | 2026-08-23 |  | packet duration halved + packet ordering + MPEGTS Stream ID side data |
