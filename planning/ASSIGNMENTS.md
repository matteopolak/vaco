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
| vaco-filter-* T3 audio long tail | 4 | #485 (closes epic #58) | agent:audio-tail | active | 2026-08-27 | — | last open child of FT-4.13 |
| vaco-cli stderr reporting | 7 | #641 #208 | agent:cli-report | done | 2026-08-27 | 2026-08-28 | the Input #0/Output #0 dump and -stats landed 08-27; -progress/-report finished by agent:cli-batch 08-28 (see next row) |
| vaco-protocol-dial (new) + vaco-protocol-rtmp (epic #61) | 2 | TECH-DEBT row 1, #552 | agent:rtmp | done | 2026-08-27 | — | pays down the five-copy dial duplication first, then RTMP is the test of whether the seam was right |
| mp3 demux/mux + vaco-codec-mpegaudio (epic #38 decode half) | 3 | #644 #362 (#363/#364 open) | agent:mpeg-audio | done | 2026-08-27 | — | `.mp3` cannot be opened at all today; demuxer first, then Layers I/II/III |
| vaco-codec-mpeg12 (epic #36) | 3 | MPEG-1/2 video decode | agent:mpeg2 | active | 2026-08-28 | — | decoder landed; three bugs fixed, one non-intra coefficient desync open on busy content, #355 open |
| vaco-codec-vp9 + vaco-codec-msac (epics #28, #32) | 3 | VP8 + VP9 decode; #329 encode | agent:vpx | free | 2026-08-28 | 2026-08-28 | No crate is named `vaco-codec-vpx` — the real crates are `vaco-codec-vp9` and `vaco-codec-msac`, which have their own rows below. Decode and #329 (all-intra key-frame encode) are done; #330–#334 (RDO, motion estimation, rate control) remain open and unclaimed. |
| vaco-codec-subtitle (epic #44) | 4 | DVB, DVD, PGS, CEA-608/708, Teletext | agent:subtitles | active | 2026-08-28 | — | first leaf batch of the fan-out; CEA-608/708 depends on the FrameSideData gaps |
| vaco-codec-h264 (epics #35, #37) | 3 | #418 #419 #420 #421 #422 #423 #424 | agent:h264-cabac | active | 2026-08-28 | — | #425 CLOSED (deblocking, new bar). #421/#422/#423 NOT closed after four rounds on the same defect: `cabac_ip_simple.264` at 56.14% (up from 36.15%), two real bugs fixed (P_Skip mv-grid gap, P_8x8 fill), a real interp clipping bug fixed (`c3606ba`, not the cause). Splitting measurement this round: integer-pel blocks in rows 1+ are ALSO wrong 62.9% of the time (vs 90.7% fractional) -- exonerates interpolation as primary cause, narrows to something below row 0 regardless of prediction path, not isolated. Leading unverified hypothesis for next agent: coded_block_flag/cbp CABAC context for inter macroblocks with a real above neighbour (same shape as two already-fixed same-macroblock timing hazards, at mb granularity). Full elimination list on #422. `receive_frame` still returns `NeedMoreInput`. #418 stays open (inter context coverage). Stopping per this round's own stop condition -- do not re-open this defect without new instrumented evidence, not re-reasoning from the same data. |
| vaco-codec-dsp-deblock | 3 | #127 then #425 | agent:h264-cabac | active | 2026-08-28 | — | Landed scalar (no vaco-simd dep yet, matching vaco-codec-dsp-idct's own precedent), correctness-first: Table 8-16/8-17 plus per-edge filter_luma_line/filter_chroma_line, edge/line interface so a masked-lane kernel can slot in later. vaco-checkasm confirmed a todo!() stub (does not deliver #92); vaco-simd::testing has #92's real mechanics but no ternary/select driver and vaco-simd::ops has no select primitive, so #127's own spike is deferred until a masked-select primitive + driver exist — not blocking, since a spike needs this scalar reference to measure against anyway. Wired into vaco-codec-h264::deblock (luma-only, all-intra bS); cabac_i_only.264 now 98.97% byte-exact against ffmpeg's real deblocked output (was 63.77%), several frames fully byte-exact. Remaining ~1% hand-traced to one tC0-clipped branch, not yet root-caused (no working primary-text copy found this session). |
| vaco-simd | 0 | #90, #127 | agent:simd | done | 2026-08-21 | 2026-08-28 | x86 re-run outstanding. Extended 2026-08-28 for #127's spike: fearless_simd already has masked-lane select natively (`S::mask8s: Select<S::u8s>`), so added `ops::select_u8`/`ops::simd::select_u8` (thin pass-through, not a composition) plus `ops::dispatched_select_u8_row` so callers never need to name `fearless_simd` directly. Measured (`benches/adoption.rs` Group 7): native select and a hand-composed bitwise blend tie on NEON, both ~10x the branchy scalar loop. Did NOT touch vaco-codec-dsp-deblock (owned by agent:h264-cabac, active) — wiring this into the deblock filter is that crate's own next step. |
| vaco-checkasm | 10 | #92, #127 | agent:checkasm | done | 2026-08-28 | 2026-08-28 | Kernel trait + Differential<K> + edge generators (vector-width tails, integer saturation, float specials) + CLI verify/list; wired vaco-scale::affine_row (a real production kernel) end to end, 92 cases clean; two synthetic seeded-bug kernels prove the harness catches an induced mismatch. Cross-tier-in-one-run is out of scope (needs unsafe assume_supported); coverage accumulates per-CI-machine instead. Unblocks #423, #127 — see comments there. Extended 2026-08-28: kernels::masked_select wires #127's spike (vaco-simd::ops::select_u8) through Differential, 506 cases clean; `verify`/`list` show both wired kernels. |
| vaco-codec-dsp-mc | 3 | #23, #258 | agent:mc-fir | assigned | 2026-08-28 | — | D-08a: const-generic separable FIR engine (arbitrary tap count), tap-set traits, edge emulation (border replication), scalar reference. New crate, no row previously existed. #258's own listed dependency on #125 (PF-3.2, the Decoder<->KernelSet batched-dispatch contract) is NOT settled — #125 is still open and owned by no one — so this crate ships as a standalone library consumers build against, without integrating into vaco-codec-h264's decoder (that crate is agent:h264-cabac's, active, and out of scope here regardless). D-08b (#259: full SIMD tier matrix, 100% checkasm coverage across SSE2..AVX-512/NEON) is explicitly cut from this pass. |
| pixel-format conversion + image2 codec mapping | 4 | #655 | agent:transcode-core | active | 2026-08-28 | — | core: no codec pair whose formats differ can transcode |
| INTERFACE-GAPS 12/13/14/15 | 4 | BSF options, FrameSideData log + motion vectors, float pixel formats | agent:filter-gaps | active | 2026-08-28 | — | core: unblocks the filter fan-out |
| vaco-format-core interface gaps | 4 | gaps 2, 7, 16 + #649 | agent:iface-gaps | done | 2026-08-28 | — | a demuxer/muxer that must know about more than one file; unblocks image2 sequences and the segmenting muxers |
| vaco-format-ac3 + vaco-codec-ac3 (epic #39) | 3 | #653 #367 #368 #369 #370 | agent:ac3 | active | 2026-08-27 | — | demuxer first; `.ac3` cannot be opened today |
| vaco-codec-core + vaco-registry + xtask + vaco-cli (codec path) | 3 | #652 | agent:codec-path | active | 2026-08-27 | — | P0: no leaf decoder or encoder is reachable from the CLI; blocks three codec agents' output |
| container sweep follow-ups | 4 | #647 #648 #650 #651 #643 | agent:sweep-followup | done | 2026-08-27 | — | six issues the sweep filed |
| vaco-demux-avi + vaco-mux-avi (finish AVI) | 4 | #642 + finding 50's three open items | agent:avi-finish | active | 2026-08-27 | — | extradata to stream info, length-prefixed H.264, JUNK sizing, the audio grid gap |
| vaco-codec-jpeg (C-15, epic #27) | 3 | #295 #296 #297 | agent:jpeg | done | 2026-08-27 | — | baseline, progressive, MJPEG framing + encoder |
| vaco-codec-qoi/-pnm/-image-simple (C-13, epic #26) | 3 | #291 #292 #293 | agent:image-codecs | done | 2026-08-27 | — | ~15 codecs, one dispatch: encode byte-identity + decode MD5 table |
| container sweep (~60 formats, excl. the nine under active ownership) | 4 | #643 + whatever it files | agent:container-sweep | done | 2026-08-27 | — | one comparison loop over every both-direction format |
| vaco-mux-avi + vaco-mux-flv | 4 | #639 #640 | agent:avi-flv | done | 2026-08-27 | — | AVI's 600 Hz slot grid (correctness, not byte-identity); FLV metadata forwarding + end-of-sequence |
| vaco-mux-mpegts + vaco-demux-mpegts + probe dump | 4 | #636 #635 | agent:ts-conformance | done | 2026-08-27 | 2026-08-27 | PCR low bytes, data_alignment, PTS/DTS; ts_id/ts_packetsize; the invented TAG:ts_codec |
| vaco-mux-mp4 + vaco-mux-matroska | 4 | #637 #638 | agent:remux-detail | done | 2026-08-27 | 2026-08-27 | btrt/elst/32-bit mdat; Duration, TrackUID width, Colour, TrackEntry order |
| vaco-parse-vpx / -mpeg12 / image + codec-core profile tables | 2 | #275 #276 #277 #278 | agent:parsers | active | 2026-08-27 | — | P-05..P-08. Footprint in vaco-codec-core is ONE new module file + its `mod` line |
| vaco-mux-hash + Muxer::add_stream_with | 4 | #634 | agent:framecrc-tb | active | 2026-08-27 | — | finding 32: framecrc `#tb` follows the frame rate, not the input; also `#software:`/`#extradata`. Owns the `Muxer` trait + its `Box` impl, not other muxers |
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
| vaco-demux-hls | 4 | #600 | agent:adaptive | done | 2026-08-23 | 2026-08-28 | Work package CLOSED; row was never updated. Corrected 2026-08-28. |
| vaco-mux-hls | 4 | #601 | agent:adaptive | done | 2026-08-23 | 2026-08-28 | Work package CLOSED; row was never updated. Corrected 2026-08-28. |
| vaco-demux-dash | 4 | #602 | agent:adaptive | done | 2026-08-23 | 2026-08-28 | quick-xml — Work package CLOSED; row was never updated. Corrected 2026-08-28. |
| vaco-mux-dash | 4 | #603 | agent:adaptive | done | 2026-08-23 | 2026-08-28 | Work package CLOSED; row was never updated. Corrected 2026-08-28. |
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
| vaco-mux-avi | 4 | — | agent:muxfix | free | 2026-08-23 |  | CONFORMANCE-FINDINGS 14,16-22 — RECLAIMED 2026-08-28: assigned 08-23, never finished, owner gone. Free to claim. |
| vaco-mux-flv | 4 | — | agent:muxfix | free | 2026-08-23 |  | RECLAIMED 2026-08-28: assigned 08-23, never finished, owner gone. Free to claim. |
| vaco-format-rtp | 4 | #597-599 | agent:rtp | done | 2026-08-23 | 2026-08-28 | RTP/RTCP model + SDP — Work package CLOSED; row was never updated. Corrected 2026-08-28. |
| vaco-demux-rtsp | 4 | #597 | agent:rtp | done | 2026-08-23 | 2026-08-28 | Work package CLOSED; row was never updated. Corrected 2026-08-28. |
| vaco-mux-rtp | 4 | #599 | agent:rtp | done | 2026-08-23 | 2026-08-28 | Work package CLOSED; row was never updated. Corrected 2026-08-28. |
| vaco-filter-video-geometry | 5 | #54 | agent:vfilt | done | 2026-08-23 | 2026-08-28 | scale crop pad transpose flips — Work package CLOSED; row was never updated. Corrected 2026-08-28. |
| vaco-filter-video-format | 5 | #54 | agent:vfilt | done | 2026-08-23 | 2026-08-28 | format setsar setdar fps — Work package CLOSED; row was never updated. Corrected 2026-08-28. |
| vaco-filter-video-source | 5 | #54 | agent:vfilt | done | 2026-08-23 | 2026-08-28 | color testsrc smptebars sinks — Work package CLOSED; row was never updated. Corrected 2026-08-28. |
| vaco-filter-core | 5 | — | agent:filter-core | done | 2026-08-22 | 2026-08-22 | frozen only |
| vaco-filter-framesync | 5 | — | agent:filter-graph | done | 2026-08-22 | 2026-08-22 |  |
| vaco-filter-graph | 5 | — | agent:filter-graph | done | 2026-08-22 | 2026-08-22 |  |
| vaco-registry | 6 | — | agent:probe | done | 2026-08-22 | 2026-08-22 |  |
| vaco-textformat | 7 | #188,#189 | agent:textformat | done | 2026-08-22 | 2026-08-22 |  |
| vaco-cli-core | 7 | — | agent:cli-core | done | 2026-08-22 | 2026-08-22 | needs vaco-expr edge for `-b:v 2*1000` |
| vaco-sched | 7 | — | agent:cli-mux | done | 2026-08-23 | 2026-08-23 | build_work now calls Muxer::init() before reading stream_time_base |
| vaco-probe | 7 | — | agent:probe | done | 2026-08-22 | 2026-08-22 |  |
| vaco-cli | 7 | — | agent:cli-mux | done | 2026-08-23 | 2026-08-23 | muxers wired: -c copy remux writes a real file |
| vaco-cli (CL-04/CL-16/CL-17/CL-22/CL-25/CL-11) | 7 | #187 #207 #208 #223 #226 #198 | agent:cli-batch | done | 2026-08-28 | 2026-08-28 | #187/#208/#223/#226/#198 closed; #207 left OPEN (-disposition/-program blocked on a vaco-format-core channel that does not exist; -timestamp/-timecode/-streamid/-dump_attachment not attempted). CL-25's StreamPick enum refactor + real -map [label] wiring is the headline change; commits 71adfbc/630d30f/bd7f167/383716e/add7323/d28246e |
| vaco-conformance | 10 | #172,#173 | agent:conformance | done | 2026-08-22 | 2026-08-22 |  |
| vaco-checkasm | 10 | #92 | agent:checkasm | done | 2026-08-28 | 2026-08-28 | Kernel trait + Differential<K> + edge generators (vector-width tails, integer saturation, float specials) + CLI verify/list; wired vaco-scale::affine_row (a real production kernel) end to end, 92 cases clean; two synthetic seeded-bug kernels prove the harness catches an induced mismatch. Cross-tier-in-one-run is out of scope (needs unsafe assume_supported); coverage accumulates per-CI-machine instead. Unblocks #423, #127 — see comments there. |
| vaco-filter-blur | 5 | #468 | agent:blur2 | done | 2026-08-23 | 2026-08-23 | gblur boxblur unsharp smartblur convolution sobel and the rest of FT-4.6a |
| vaco-filter-denoise | 5 | #469 | agent:denoise | done | 2026-08-23 | 2026-08-23 | hqdn3d atadenoise removegrain nlmeans owdenoise |
| vaco-filter-geometry | 5 | #470 | agent:geom2 | done | 2026-08-23 | 2026-08-23 | T2 geometry (~28) — distinct from vaco-filter-video-geometry's T1 set |
| vaco-filter-color | 5 | #476 | agent:component | done | 2026-08-23 | 2026-08-28 | redirected from the invented vaco-filter-component to plan 16 §4.3 rows — Work package CLOSED; row was never updated. Corrected 2026-08-28. |
| vaco-filter-key | 5 | #476 | agent:component | done | 2026-08-23 | 2026-08-28 | Work package CLOSED; row was never updated. Corrected 2026-08-28. |
| vaco-filter-lut | 5 | #476 | agent:component | done | 2026-08-23 | 2026-08-28 | Work package CLOSED; row was never updated. Corrected 2026-08-28. |
| vaco-filter-achannel | 5 | #482 | agent:achannel | done | 2026-08-23 | 2026-08-23 | T3 channel, layout and mixing filters (~14) |
| vaco-filter-ameasure | 5 | #483 | agent:ameasure | done | 2026-08-23 | 2026-08-23 | NAME DIVERGES: plan 16 §4.3 calls this vaco-filter-aanalysis |
| vaco-demux-mpegts | 4 | #632 | agent:tspkt | done | 2026-08-23 | 2026-08-23 | part 2 residual: PES_packet_length==0 release timing, characterised not root-caused |
| vaco-bsf-core | 3 | #349 | agent:extradata | done | 2026-08-23 | 2026-08-23 | M6 stage is reachable but inert until this exists |
| vaco-bsf-generic | 3 | #349 | agent:extradata | done | 2026-08-23 | 2026-08-23 | extract_extradata closes CONFORMANCE-FINDINGS 26 |
| vaco-bsf-h2645 | 3 | #350,#351,#352 | agent:extradata | done | 2026-08-23 | 2026-08-23 | *_mp4toannexb; dedups vaco-mux-avi/mpegts converters |
| vaco-filter-temporal | 5 | #475 | agent:temporal | done | 2026-08-23 | 2026-08-23 | plan 16 §4.3 row; fps already taken by vaco-filter-video-format |
| vaco-filter-convolve | 5 | #468 | agent:blur2 | done | 2026-08-23 | 2026-08-23 | remainder of the plan row: morpho inflate deflate edgedetect blurdetect convolve deconvolve corr xcorrelate |
| vaco-filter-core | 5 | — | agent:adapt | done | 2026-08-23 | 2026-08-23 | INTERFACE-GAPS 10: Paired + Fanout adapters, then the multi-input filters three agents declined |
| vaco-mux-matroska | 4 | — | agent:mkv | done | 2026-08-23 | 2026-08-23 | CONFORMANCE-FINDINGS 15: CRC-32 on every level-1 element, then SeekHead |
| vaco-filter-vdsp | 5 | — | agent:analysis2 | free | 2026-08-23 |  | created by agent:temporal for scene_sad; extend, do not duplicate — RECLAIMED 2026-08-28: assigned 08-23, never finished, owner gone. Free to claim. |
| vaco-filter-source | 5 | #474 | agent:src | done | 2026-08-23 | 2026-08-23 | plan 16 §4.3 row; nullsrc/color already taken by vaco-filter-video-source |
| vaco-filter-asource | 5 | #481 | agent:src | done | 2026-08-23 | 2026-08-23 | anullsrc already taken |
| vaco-filter-deinterlace | 5 | #106 | agent:deint | done | 2026-08-23 | 2026-08-23 | plan 16 §4.3 row; idet/vfrdet were blocked on INTERFACE-GAPS 11, since closed |
| vaco-frame | 2 | — | agent:framemeta | done | 2026-08-23 | 2026-08-23 | INTERFACE-GAPS 11: per-frame metadata dictionary |
| vaco-probe | 7 | — | agent:framemeta | done | 2026-08-23 | 2026-08-23 | -show_frames FRAME_TAGS consumer side |
| vaco-filter-aeffects | 5 | #484 | agent:audiotidy | done | 2026-08-23 | 2026-08-23 | renamed from vaco-filter-achannel per plan 16 §4.3 |
| vaco-filter-color | 5 | #476 | agent:analysis2 | done | 2026-08-23 | 2026-08-28 | remainder of the 29-filter row — Work package CLOSED; row was never updated. Corrected 2026-08-28. |
| vaco-filter-key | 5 | #476 | agent:color2 | done | 2026-08-23 | 2026-08-23 | remainder of the 20-filter row |
| vaco-filter-lut | 5 | #476 | agent:color2 | done | 2026-08-23 | 2026-08-23 | lut1d, haldclutsrc, the .3dl/.dat/.m3d parsers |
| vaco-filter-analysis | 5 | #477 | agent:analysis2 | done | 2026-08-23 | 2026-08-28 | #477 closed; may extend vaco-filter-vdsp |
| vaco-filter-adsp | 5 | — | agent:audiotidy | done | 2026-08-23 | 2026-08-23 | D19: one biquad design, not five; plan 16 §4.2 says it lives here |
| vaco-filter-aeq | 5 | — | agent:audiotidy | done | 2026-08-23 | 2026-08-23 | renamed from vaco-filter-audio-eq |
| vaco-filter-audio-dynamics | 5 | — | agent:audiotidy | done | 2026-08-23 | 2026-08-23  | renamed |
| vaco-filter-ameasure | 5 | — | agent:audiotidy | done | 2026-08-23 | 2026-08-23  | renamed |
| vaco-bsf-av1 | 3 | #351 | agent:extradata | done | 2026-08-23 | 2026-08-23 | av1_frame_split/merge, av1_metadata |
| vaco-bsf-vpx | 3 | #351 | agent:extradata | done | 2026-08-23 | 2026-08-23 | VP9 only — no VP8 bsf exists in the reference |
| vaco-bsf-audio | 3 | #352 | agent:extradata | done | 2026-08-23 | 2026-08-23 | aac_adtstoasc, opus_metadata, pcm_rechunk |
| vaco-format-nalu | 4 | — | agent:extradata | done | 2026-08-23 | 2026-08-23 | now owns the one extradata-assembly rule (D19) |
| vaco-bsf-* (all six) | 3 | #353,#354 | agent:bsf3 | done | 2026-08-23 | 2026-08-28 | *_metadata needs a CBS write path; INTERFACE-GAPS 12 blocks options — Work package CLOSED; row was never updated. Corrected 2026-08-28. |
| vaco-format-mpjpeg | 4 | #596 | agent:smallfmt | done | 2026-08-23 | 2026-08-28 | Work package CLOSED; row was never updated. Corrected 2026-08-28. |
| vaco-format-spdif | 4 | #612 | agent:smallfmt | done | 2026-08-23 | 2026-08-28 | S/PDIF + s337m — Work package CLOSED; row was never updated. Corrected 2026-08-28. |
| vaco-format-swf | 4 | #616 | agent:smallfmt | done | 2026-08-23 | 2026-08-28 | Work package CLOSED; row was never updated. Corrected 2026-08-28. |
| vaco-format-nut | 4 | #594 | agent:smallfmt | done | 2026-08-23 | 2026-08-28 | fully specified, so byte-identity is reachable — Work package CLOSED; row was never updated. Corrected 2026-08-28. |
| vaco-protocol-crypto | 2 | #546 | agent:proto | done | 2026-08-23 | 2026-08-28 | AES-CTR over a nested URL; D10 dependency decision — Work package CLOSED; row was never updated. Corrected 2026-08-28. |
| crates/io (all) | 2 | #550 | agent:proto | done | 2026-08-23 | 2026-08-28 | httpproxy ftp gopher gophers icecast ipfs/ipns gateways — Work package CLOSED; row was never updated. Corrected 2026-08-28. |
| vaco-bsf-legacy | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-bsf-subtitle | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-aac | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-ac3 | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-adpcm | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-alac | 4 | — | agent:codec-batch | active | 2026-08-28 | — | in flight; backfilled row, was missing entirely |
| vaco-codec-dsp-sinewin | 3 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-exr | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-ffv1 | 4 | — | agent:codec-batch | active | 2026-08-28 | — | in flight; backfilled row, was missing entirely |
| vaco-codec-flac | 4 | — | agent:codec-batch | active | 2026-08-28 | — | in flight; backfilled row, was missing entirely |
| vaco-codec-gif | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-h263 | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-image-simple | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-jpegxl | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-mpegaudio | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-msac | 3 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-null | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-opus | 4 | — | agent:codec-batch | active | 2026-08-28 | — | in flight; backfilled row, was missing entirely |
| vaco-codec-pcm | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-png | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-pnm | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-qoi | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-rawvideo | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-subtitle-bitmap | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-subtitle-cc | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-subtitle-teletext | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-subtitle-text | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-tiff | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-vlc | 3 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-vorbis | 4 | — | agent:codec-batch | active | 2026-08-28 | — | in flight; backfilled row, was missing entirely |
| vaco-codec-vp8 | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-vp9 | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-webp | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-crypto | 0 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-demux-mpegaudio | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-filter-aanalysis | 5 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-filter-adynamics | 5 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-filter-artistic | 5 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-filter-draw-vf | 5 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-filter-mm | 5 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-filter-overlay | 5 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-filter-scope | 5 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-filter-stack | 5 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-filter-video-composite | 5 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-format-adaptive | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-format-gxf | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-format-imf | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-format-misc | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-format-misc-audio | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-format-mpegaudio | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-mux-hds | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-mux-mpegaudio | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-mux-mxf | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-mux-smoothstreaming | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-parse-image | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-parse-mpegvideo | 4 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-protocol-ftp | 2 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-protocol-gopher | 2 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-protocol-httpproxy | 2 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-protocol-icecast | 2 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-protocol-ipfs | 2 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-protocol-rist | 2 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-protocol-rtmp | 2 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-protocol-rtp | 2 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-protocol-srt | 2 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-protocol-srtp | 2 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-rtp | 1 | — | (backfilled) | done | — | 2026-08-28 | Row was missing entirely; crate exists with real code. Backfilled 2026-08-28 — verify by evidence before claiming. |
| vaco-codec-dsp-fmtconvert | 3 | #122 (D-05) | agent:dsp-shared | done | 2026-08-28 | 2026-08-28 | format conversion DSP, no current caller yet (like D-07/D-09) |
| vaco-codec-dsp-lpc | 3 | #257 (D-07) | agent:dsp-shared | done | 2026-08-28 | 2026-08-28 | LPC analysis/synthesis; FLAC/ALAC/Opus-SILK each still have their own local implementation, not touched -- this is the shared primitive for future consolidation |
| vaco-codec-dsp-idct (blockdsp/pixblockdsp extension) | 3 | #123 (D-11) | agent:dsp-shared | done | 2026-08-28 | 2026-08-28 | reassigned from agent:idct (done, IDCT-only) to add the blockdsp/pixblockdsp modules D-11 also names; new files only, existing h264/hevc/mpeg2/vp9 modules untouched |
| vaco-codec-dsp-intrapred | 3 | #126 (D-09) | agent:dsp-shared | done | 2026-08-28 | 2026-08-28 | generic DC/planar/angular-projection primitives shared by HEVC-family and AV1-family intra pred; H.264/VP8/VP9 already ship their own local intra pred and were not touched |
| vaco-codec-dsp-mecmp | 3 | #144 (D-12) | agent:me-rc | done | 2026-08-28 | 2026-08-28 | SAD/SSD/variance vectorised via vaco-simd, checked in vaco-checkasm's kernels::mecmp (575 cases/kernel); SATD scalar-only (Hadamard transpose not yet vectorised, documented) |
| vaco-codec-dsp-me | 3 | #260 (D-13) | agent:me-rc | done | 2026-08-28 | 2026-08-28 | full/diamond/three-step search verified against a full-search oracle; TSS's coarse-grid limitation is real and documented, not a bug. #304/#305 commented with a call-site sketch |
| vaco-codec-dsp-ratecontrol | 3 | #146 (D-14) | agent:me-rc | done | 2026-08-28 | 2026-08-28 | CBR/VBR/CQ, validated by simulation (no real encoder to test against yet); a real VBR-compounds-to-runaway bug found and fixed along the way, kept as a documented lesson in the crate doc |
| vaco-corpus (new) | 10 | #180 #175 | agent:test-infra | done | 2026-08-28 | 2026-08-28 | content-addressed SHA-256 object store (BLAKE3 substituted, no workspace dep), vaco-media.lock with verified PngSuite/VP8/VP9-test-vectors/flac-test-files entries, network-gated fetch via vaco-protocol-http, generic mutate + ddmin minimise; Argon and JVT/JCT-VC recorded as documented gaps, no stable public single-file source found |
| vaco-fuzz-support (new) | 10 | #174 #176 | agent:test-infra | done | 2026-08-28 | 2026-08-28 | Guard (wall-clock deadline) plus a re-export of vaco-limits::ProgressGuard (not a second copy -- dup-check correctly refused one); Dim/BoundedBytes/FuzzPacket structured arbitrary inputs; replay_dir/replay_dir_or_panic. Per-target #[test] wiring against fuzz/seeds/<target>/ for all ~190 existing targets is NOT done, left as a named follow-up |
| vaco-fuzz-alloc (new) | 10 | #176 | agent:test-infra | done | 2026-08-28 | 2026-08-28 | counting GlobalAlloc backstop per plan 13 S2.2.3; needs one xtask/src/unsafe_audit.rs ALLOWED_PREFIX line to clear `cargo xtask unsafe-audit` -- not made because xtask was under agent:codec-path's active ownership all session; verified the gate correctly flags the gap |
| vaco-conformance (metrics + suites.toml extension) | 10 | #253 #181 | agent:test-infra | done | 2026-08-28 | 2026-08-28 | added PSNR/SSIM/spectral-lsd Metric impls and wired compare::quality::compare to use them via Pair::with_signals; added suites.toml joining vaco-corpus suites to codecs. VMAF cut (no pure-Rust impl found). Decoding a bitstream back to a raw Signal for a real case is still a seam -- nothing upstream does that yet |
| vaco-parse-mpegaudio | 4 | #273 (P-03) | agent:audio-headers | done | 2026-08-28 | 2026-08-28 | MP1/2/3 + AC-3/E-AC-3 sync-frame Parsers; wraps vaco-format-mpegaudio/vaco-format-ac3 rather than re-deriving their tables (D19). Fuzz-found and fixed a two-byte sync-word scan that dropped a syncframe split across a chunk boundary. |
| vaco-format-metadata (new) | 4 | #206 (FW-12) | agent:audio-headers | done | 2026-08-28 | 2026-08-28 | canonical metadata key set + MetadataConv table/driver + StreamGroup model. Program/Chapter re-exported from vaco-format-core (FW-01), not redefined. StreamGroup has no Demuxer-trait wiring yet -- that edit belongs to vaco-format-core's owner. |
| vaco-format-vorbiscomment (new) | 4 | #540 (SH-07) | agent:audio-headers | done | 2026-08-28 | 2026-08-28 | Vorbis comment (vendor+tag list) + FLAC METADATA_BLOCK_PICTURE, shared with #274's Vorbis/FLAC header parsing per D19. vaco-parse-opus's CommentHeader parses the identical OpusTags shape and is a known, recorded duplicate (KNOWN_DUPLICATE in xtask/src/dup_check.rs), not merged since that crate is out of scope here. |
| vaco-parse-audio-misc (new) | 4 | #274 (P-04) | agent:audio-headers | done | 2026-08-28 | 2026-08-28 | Vorbis identification header + FLAC STREAMINFO + ALAC ALACSpecificConfig Parsers, closing epic #13 (8/8 children). AlacSpecificConfig duplicates vaco-codec-alac's own struct byte-for-byte (concurrent agent); recorded in dup-check's KNOWN_DUPLICATE with the fix direction, not merged -- that edit belongs to vaco-codec-alac's owner. unpack_headers duplicates vaco-demux-ogg's pack/split_xiph_headers by necessity (D14.1: a codec-level parse crate may not depend on a container demuxer crate). |

| vaco-codec-av1 | 4 | #136 #137 #138 #139 #140 #335 | agent:av1-decode | active | 2026-08-28 | — | complete intra-only AV1 decode as one crate (OBU/seq/frame header, symbol decoder+CDF, tile/partition/mode-info loop, transforms, intra prediction); depends on vaco-parse-av1 (OBU/av1C/seq header) per D14, inter/deblock/CDEF/superres/restoration/film-grain explicitly out of scope (later issues, other agents) |
# Fan-out plan: core first, then leaves in batches

## Why this order

A leaf crate — a codec, a container, a filter — already touches nothing another
agent owns. Measured across every new crate landed on 2026-08-27/28, the only
files outside its own directory are `docs/<layer>/<crate>.md`,
`fuzz/fuzz_targets/<name>.rs` and `provenance/<crate>.toml`, all uniquely named
per crate. Shared core files run at roughly two commits a day. **Contention is
not the constraint.**

The constraint is that leaf work is not yet *usable*. Fifteen image codecs, a
JPEG codec, an MP3 decoder and an AC-3 decoder have landed and none can perform
a transcode, because the layer between decode and encode has three gaps. Writing
more codecs before that closes produces more inert crates.

So: finish the core, then fan out wide.

## Core (in flight)

| Package | What it unblocks |
|---|---|
| #655 — pixel-format conversion, image2 extension mapping (done 2026-08-28) | every codec pair whose formats differ; before this, only pairs that happened to agree worked |
| INTERFACE-GAPS 2, 7, 16 + #649 | image2 sequences, the segmenting muxers behind HLS and DASH, raw MPEG-1/2 input |
| INTERFACE-GAPS 12, 13, 14, 15 | every BSF that takes an option, `showinfo`, `codecview`, every filter on float formats |
| #652 (done) | `-c:v <name>` resolving at all |
| INTERFACE-GAPS 17 — `FrameData` has no `Subtitle` variant | every subtitle decoder, bitmap and text alike: `-c:s <name>` cannot reach a live decoder for *any* codec today |

Gap 17 is the orchestrator's, and it waits on the gap 12/13/14/15 package
landing because both touch `crates/filter/`. Its blast radius is smaller than
gap 17's own text implies: of 224 `FrameData::` references across 126 files,
only **14 are match arms, in 10 files** — the rest are constructors, which a
new variant does not disturb. The 10: `vaco-sched/src/wire.rs`,
`vaco-frame/src/alloc.rs`, `vaco-filter-core/src/{adapt,timeline}.rs`,
`vaco-filter-video-format/src/setdar.rs`,
`vaco-filter-video-geometry/src/pad.rs`,
`vaco-filter-mm/src/{setpts,segment,looping}.rs`,
`vaco-filter-audio/src/amix.rs`. Note `FrameData` is not `#[non_exhaustive]`
where `FrameSideData` is; that is why this one is not purely additive the way
gap 11's `Metadata` variant was.

## Then: leaves, 3–5 per agent

Each batch is one agent, one comparison loop, one table at the end. The pattern
that has worked: implement the family, build the loop once, run every item
through it, falsify each formula against it so a broken fix shows as a changed
row.

Candidate batches, roughly in value order:

- **VP8 + VP9 decode** — epics #28, #32. Both already have parsers.
- **MPEG-1/2 video decode** — epic #36, with `vaco-parse-mpegvideo` already landed.
- **Subtitle decoders** — epic #44: DVB, DVD, PGS, CEA-608/708, Teletext. Five
  formats, one crate family, and the containers already carry them.
- **T3 audio containers** — epics #58, #76. *In flight from 2026-08-28 as
  `agent:misc-audio` (#620/#621/#622, one crate `vaco-format-misc-audio`).*
  All three issues name the same crate, so they are one dispatch, not three —
  D11 allows one writer per crate.
- **T3 video and metadata containers** — epic #77. *In flight from 2026-08-28
  as `agent:misc-video` (#623/#624/#625, one crate `vaco-format-misc`).* Same
  single-crate reasoning. `ivf` and `ffmetadata` are the two worth more than
  their size: the first is what every AV1/VP9 test vector ships in, the second
  is the first real consumer of gap 1's `Muxer::set_metadata`.
- **The T3 video filter long tail** — epic #57, ~150 filters, explicitly the most
  parallelisable work in the project; several agents can run here at once.
- **Remaining protocols** — SRT (#62), RIST (#63), SCTP/DTLS (#64), now that
  `vaco-protocol-dial` exists.
- **MXF, GXF, IMF** — epics #72, #73, #74, #75.

A filter batch and a codec batch can run concurrently without touching each
other; two codec batches can too, as long as they are different codecs.

## What would break the fan-out

- A new codec still needs a `CodecId` variant, which is a `vaco-codec-core` edit.
  Thirteen were added at once for the image codecs and that was fine; a *batch*
  adding its variants in one commit is fine, an agent adding one per codec is
  not. Plan 15 §1.1's generated table is the seam if this ever bites.
- `xtask/src/dup_check.rs`'s `DISTINCT` list and `xtask/src/wasm.rs`'s
  `NATIVE_ONLY` list are both hand-maintained and both take an entry per crate
  occasionally. Neither is hot enough to fix yet.
