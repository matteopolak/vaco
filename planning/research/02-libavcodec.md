# FFmpeg libavcodec Feature Inventory

**Note on deliverable:** I was not able to write this file — as a search subagent I only have read-only tools (Bash/Read/Search), no Write/Edit. Please save the content below to `/Users/matthew/projects/vaco/planning/research/02-libavcodec.md`.

Source: `~/repos/FFmpeg`, commit `564f92cce2` (tree dated 2026-08-18), version string `8.0.git`. All content below is a structural/nominal inventory (names, flags, relationships, spec citations) — no source code, algorithms, or constant tables are reproduced.

---

## 1. FRAMEWORK

### 1.1 Core object model

- **`AVCodec`** (public, `codec.h`) — the immutable, name/id-addressable descriptor returned by `avcodec_find_decoder()`/`_encoder()`/`_by_name()`. Holds name, long_name, `AVMediaType`, `AVCodecID`, `capabilities` bitmask, supported `AVRational` framerates/samplerates/pixel formats/sample formats/channel layouts, `AVClass` (private options), `AVCodecHWConfigInternal` list, and profile list.
- **`FFCodec`** (private, `codec_internal.h`) — the internal superset of `AVCodec` (`.p` embeds the public struct) that every real implementation defines as `ff_<name>_decoder`/`ff_<name>_encoder`. Adds the `cb_type`/`cb` union of callback pointers, `priv_data_size`, `update_thread_context`/`update_thread_context_for_user` (frame-threading state propagation), `init_thread_copy`, `flush`, `close`, `bsfs` (implicitly-attached bitstream filter name string, e.g. `"vp9_superframe_split"`), `hw_configs`, `caps_internal` (`FF_CODEC_CAP_*`, e.g. `INIT_THREADSAFE`, `SETS_PKT_DTS`, `SKIP_FRAME_FILL_PARAM`, `EXPORTS_CROPPING`).
- **`AVCodecContext`** (public, `avcodec.h`) — the per-instance mutable state: dimensions/pix_fmt/sample_fmt/ch_layout, `bit_rate`, `time_base`, `framerate`, `gop_size`, `thread_count`/`thread_type`, `flags`/`flags2` (`AV_CODEC_FLAG_*`), `get_buffer2`/`get_encode_buffer` callbacks, `hw_frames_ctx`/`hw_device_ctx`, `extradata`, `profile`/`level`, and codec-family private option blocks accessed via `priv_data`.
- **`AVCodecParameters`** (`codec_parameters.h`) — codec-agnostic stream description carried in containers (`AVStream.codecpar`): `codec_type`, `codec_id`, `codec_tag`, `extradata`, `bit_rate`, `bits_per_coded_sample`, `profile`/`level`, `width`/`height`, `sample_aspect_ratio`, `field_order`, `color_range`/`primaries`/`trc`/`space`/`chroma_location`, `ch_layout`, `sample_rate`, `frame_size`; converted to/from `AVCodecContext` via `avcodec_parameters_to_context`/`from_context`.

### 1.2 Registration / discovery

All decoders and encoders are `extern`-declared and enumerated in a single translation unit, `allcodecs.c`, which builds a `const FFCodec *codec_list[]` (generated into `codec_list.c` at configure time from the `extern` list, gated by `CONFIG_<NAME>_DECODER`/`_ENCODER`). `av_codec_iterate()` walks this array; `avcodec_find_decoder`/`_encoder` (by id) and `_by_name` linear-scan it, preferring a non-experimental match and falling back to an `AV_CODEC_CAP_EXPERIMENTAL`-flagged one. Parsers (`AVCodecParser`/`FFCodecParser`) and bitstream filters (`FFBitStreamFilter`) have their own analogous registration files, `parser_list.c`/`bsf_list.c` (generated from extern declarations at the top of `bitstream_filters.c` and the parser Makefile set).

### 1.3 Capability flags — `AV_CODEC_CAP_*` (`codec.h`)

| Flag | Meaning |
|---|---|
| `DRAW_HORIZ_BAND` (1<<0) | Decoder can invoke the `draw_horiz_band` callback for incremental band output. |
| `DR1` (1<<1) | Codec uses `get_buffer2`/`get_encode_buffer` for frame allocation and supports caller-supplied custom allocators. |
| `ENCODER_RECONF` (1<<2) | Encoder can be reconfigured with new init parameters without a full close/reopen. |
| `DELAY` (1<<5) | Codec has internal buffering/lookahead; must be flushed with a NULL-data call at EOF to drain remaining frames/packets. |
| `SMALL_LAST_FRAME` (1<<6) | Codec accepts a final, undersized audio frame without padding/truncation issues. |
| `EXPERIMENTAL` (1<<9) | Codec is experimental; deprioritized versus non-experimental codecs sharing the same `AVCodecID`. |
| `CHANNEL_CONF` (1<<10) | Decoder should derive channel configuration/sample rate itself rather than trusting the container. |
| `FRAME_THREADS` (1<<12) | Codec supports frame-level multithreading. |
| `SLICE_THREADS` (1<<13) | Codec supports slice/partition-level multithreading. |
| `PARAM_CHANGE` (1<<14) | Codec tolerates mid-stream parameter changes. |
| `OTHER_THREADS` (1<<15) | Codec is internally multithreaded by a mechanism other than frame/slice threading (typically an external-library wrapper). |
| `VARIABLE_FRAME_SIZE` (1<<16) | Audio encoder accepts a different sample count per `send_frame` call. |
| `AVOID_PROBING` (1<<17) | Decoder is a poor choice for stream probing (expensive to spin up, e.g. hardware) — used only as last resort. |
| `HARDWARE` (1<<18) | Codec is itself a non-hwaccel hardware implementation (e.g. a V4L2 M2M/MediaCodec/RKMPP codec, not a `hwaccel` hook). |
| `HYBRID` (1<<19) | Codec is potentially hardware-backed but has an internal software fallback. |
| `ENCODER_REORDERED_OPAQUE` (1<<20) | Encoder can round-trip the input frame's opaque value onto the matching output packet (`AV_CODEC_FLAG_COPY_OPAQUE`). |
| `ENCODER_FLUSH` (1<<21) | Encoder can be flushed via `avcodec_flush_buffers()` in place, without close/reopen. |
| `ENCODER_RECON_FRAME` (1<<22) | Encoder can additionally emit the reconstructed (as-decoded) frame alongside the packet, when `AV_CODEC_FLAG_RECON_FRAME` is set. |

### 1.4 Property flags — `AV_CODEC_PROP_*` (`codec_desc.h`, attached to `AVCodecDescriptor` per `AVCodecID`, not per implementation)

| Flag | Meaning |
|---|---|
| `INTRA_ONLY` (1<<0) | Video/audio codec uses only intra compression (no inter-frame prediction). |
| `LOSSY` (1<<1) | Codec supports a lossy mode (may coexist with `LOSSLESS`). |
| `LOSSLESS` (1<<2) | Codec supports a lossless mode. |
| `REORDER` (1<<3) | Coded/packet order may differ from presentation order (PTS ≠ DTS is possible). |
| `FIELDS` (1<<4) | Video codec supports separately-coded interlaced fields. |
| `ENHANCEMENT` (1<<5) | Codec carries enhancement data applied to other frames; not independently decodable to a full image (e.g. LCEVC). |
| `BITMAP_SUB` (1<<16) | Subtitle codec is bitmap-based (`AVSubtitleRect.pict`). |
| `TEXT_SUB` (1<<17) | Subtitle codec is text-based (`AVSubtitleRect.ass`). |

### 1.5 Decode/encode call model

- **Modern (send/receive)**: `avcodec_send_packet` / `avcodec_receive_frame` (decode), `avcodec_send_frame` / `avcodec_receive_packet` (encode). Internally each `FFCodec` implements one callback "shape" selected by `cb_type`: `FF_CODEC_CB_TYPE_RECEIVE_FRAME` (decoder drives its own pull loop) or the older-style `FF_CODEC_CB_TYPE_DECODE` (`decode(avctx, frame, *got_frame, avpkt)` — one packet in, at most one frame out, adapted internally to the send/receive API by `decode.c`); mirrored on the encode side by `FF_CODEC_CB_TYPE_RECEIVE_PACKET` vs. the older `FF_CODEC_CB_TYPE_ENCODE` (`encode2`-style). `FF_CODEC_CB_TYPE_DECODE_SUB` / `FF_CODEC_CB_TYPE_ENCODE_SUB` are the subtitle-specific shapes (`AVSubtitle` in/out instead of `AVFrame`).
- Draining: signaled by `avcodec_send_packet(NULL)` / `avcodec_send_frame(NULL)`; the codec then returns `AVERROR_EOF` from receive once fully drained. Only codecs with `AV_CODEC_CAP_DELAY` are guaranteed to ever see a NULL send.
- Flushing mid-stream: `avcodec_flush_buffers()`; for encoders this requires `AV_CODEC_CAP_ENCODER_FLUSH`, otherwise the encoder must be closed and reopened.

### 1.6 Threading models

Implemented in `pthread.c` (dispatch), `pthread_frame.c` (frame threading), `pthread_slice.c` (slice threading), `threadprogress.c` (cross-thread progress signaling primitive used by frame-threaded decoders with inter-frame dependencies, e.g. motion-compensated prediction across threads).
- **Frame threading** (`AV_CODEC_CAP_FRAME_THREADS`): N frames decoded concurrently on N threads; requires `update_thread_context`/`init_thread_copy` to propagate per-frame decoder state (reference frames, parameter sets) between thread contexts.
- **Slice threading** (`AV_CODEC_CAP_SLICE_THREADS`): a single frame's slices/partitions decoded concurrently via `avctx->execute`/`execute2`.
- **Other threading** (`AV_CODEC_CAP_OTHER_THREADS`): codec manages its own thread pool internally, typically a wrapper around an external library that is already multithreaded (libx264, libx265, libaom, libvpx, libsvtav1, etc.).
- `thread_type` (`FF_THREAD_FRAME` / `FF_THREAD_SLICE`, bitmask) and `thread_count` on `AVCodecContext` select/report the active model; `"auto"` thread count triggers `av_cpu_count()`-based sizing.
- Thread-safe init is expressed via the internal `FF_CODEC_CAP_INIT_THREADSAFE` bit (`caps_internal`): codecs without it are serialized by a global lock during `avcodec_open2` because their `init()` touches non-reentrant global state (legacy VLC table init, etc.).

### 1.7 Profiles/levels

Centralized per-family `AVProfile[]` tables in `profiles.c`, each terminated by `AV_PROFILE_UNKNOWN` and referenced from the relevant `AVCodec.profiles`/`AVCodecDescriptor.profiles`: `ff_aac_profiles`, `ff_dca_profiles`, `ff_eac3_profiles`, `ff_truehd_profiles`, `ff_dnxhd_profiles`, `ff_h264_profiles`, `ff_hevc_profiles`, `ff_vvc_profiles`, `ff_jpeg2000_profiles`, `ff_mpeg2_video_profiles`, `ff_mpeg4_video_profiles`, `ff_vc1_profiles`, `ff_vp9_profiles`, `ff_av1_profiles`, `ff_sbc_profiles`, `ff_prores_profiles`, `ff_prores_raw_profiles`, `ff_mjpeg_profiles`, `ff_arib_caption_profiles`, `ff_evc_profiles`. `AVCodecContext.level` is a raw integer (codec-specific meaning, e.g. H.264/HEVC level ×10 or ×30 encodings per spec Annex A).

### 1.8 Parsers — full list (66), `AVCodecParser`/`FFCodecParser` in `<name>_parser.c`

| Parser symbol | Role |
|---|---|
| `aac_parser` | Frame-syncs raw/ADTS AAC into complete access units. |
| `ac3_parser` | Syncs AC-3/E-AC-3 frames, exposes bitrate/channel info via `ac3_parser_internal.h` shared header parser. |
| `adx_parser` | Frames CRI ADX ADPCM blocks. |
| `ahx_parser` | Frames AHX (MPEG audio variant used by Sega/Dreamcast titles). |
| `amr_parser` | Frames AMR-NB/WB storage-format packets. |
| `apv_parser` | Frames APV (Advanced Professional Video) access units. |
| `av1_parser` | Frames AV1 OBUs/temporal units, extracts sequence header info (reuses `cbs_av1`). |
| `avs2_parser` | Frames AVS2/IEEE1857.4 NAL-like units. |
| `avs3_parser` | Frames AVS3/IEEE1857.10 units. |
| `bmp_parser` | Frames raw BMP image streams (for image2/mjpeg-like pipes). |
| `cavsvideo_parser` | Frames Chinese AVS (AVS1) start-code video. |
| `cook_parser` | Frames RealAudio COOK blocks. |
| `cri_parser` | Frames CRI HCA/related container-embedded frames. |
| `dca_parser` | Syncs DTS core/extension substream frames. |
| `dirac_parser` | Frames Dirac parse units. |
| `dnxhd_parser` | Frames DNxHD/DNxHR essence. |
| `dnxuc_parser` | Frames Avid DNxUncompressed / SMPTE RDD 50 essence. |
| `dolby_e_parser` | Frames Dolby E frames embedded in PCM. |
| `dpx_parser` | Frames raw DPX image streams. |
| `dvaudio_parser` | Frames DV audio blocks. |
| `dvbsub_parser` | Frames DVB subtitle segments. |
| `dvd_nav_parser` | Frames DVD NAV packet (PCI/DSI) data. |
| `dvdsub_parser` | Frames DVD (VOBSUB) subpicture units. |
| `evc_parser` | Frames MPEG-5 EVC NAL units. |
| `ffv1_parser` | Frames FFV1 frames (needed since FFV1 has no fixed frame size in some configs). |
| `flac_parser` | Frames raw FLAC frames without a container, using CRC/frame-header heuristics. |
| `ftr_parser` | Frames FTR (Fasttracker-related?) speech frames used by some game formats. |
| `g723_1_parser` | Frames G.723.1 speech frames. |
| `g729_parser` | Frames G.729 speech frames. |
| `gif_parser` | Frames GIF image/animation blocks. |
| `gsm_parser` | Frames raw GSM 06.10 frames. |
| `h261_parser` | Frames H.261 picture start codes. |
| `h263_parser` | Frames H.263/H.263+ picture start codes. |
| `h264_parser` | Frames H.264 Annex-B NAL units, exposes field/frame and POC info (built on shared `h264parse`). |
| `hdr_parser` | Frames Radiance HDR image streams. |
| `ipu_parser` | Frames PS2 IPU MPEG-2-like essence. |
| `jpeg2000_parser` | Frames JPEG 2000 codestreams. |
| `jpegxl_parser` | Frames JPEG XL codestreams/containers. |
| `jpegxs_parser` | Frames JPEG XS codestreams. |
| `lcevc_parser` | Frames LCEVC enhancement NAL/OBU-style units. |
| `latm_parser` (`aac_latm_parser`) | Frames LATM/LOAS-wrapped AAC. |
| `mjpeg_parser` | Frames Motion JPEG frames from elementary streams. |
| `misc4_parser` | Frames the "Misc4" game-video codec's frames. |
| `mlp_parser` | Frames MLP/TrueHD access units, extracts channel/rate info. |
| `mpeg4video_parser` | Frames MPEG-4 Part 2 (Xvid/DivX) VOP start codes. |
| `mpegaudio_parser` | Frames MPEG-1/2 Layer I/II/III (MP3) frames. |
| `mpegvideo_parser` | Frames MPEG-1/2 video picture start codes. |
| `png_parser` | Frames PNG chunk streams. |
| `pnm_parser` | Frames PNM (PBM/PGM/PPM/PAM) image streams. |
| `prores_parser` | Frames Apple ProRes frames, extracts profile. |
| `prores_raw_parser` | Frames ProRes RAW frames. |
| `qoi_parser` | Frames QOI image streams. |
| `rv34_parser` | Frames RealVideo 3/4 slice headers. |
| `sbc_parser` | Frames Bluetooth SBC audio frames. |
| `sipr_parser` | Frames RealAudio SIPR frames. |
| `tak_parser` | Frames TAK lossless-audio frames. |
| `vc1_parser` | Frames VC-1/WMV3 frame headers, extracts profile/level. |
| `vorbis_parser` | Frames raw Vorbis packets, tracks granule position / packet duration. |
| `vp3_parser` | Frames VP3/Theora frames. |
| `vp8_parser` | Frames VP8 frames, extracts key-frame/size info. |
| `vp9_parser` | Frames VP9 frames/superframes, extracts profile/size. |
| `vvc_parser` | Frames H.266/VVC NAL units. |
| `webp_parser` | Frames WebP (RIFF-chunked) image/animation streams. |
| `xbm_parser` | Frames XBM image text streams. |
| `xma_parser` | Frames Xbox Media Audio blocks. |
| `xwd_parser` | Frames X Window Dump image streams. |

### 1.9 Bitstream filters — full list (50, `bsf/*.c`)

| BSF name | Transformation |
|---|---|
| `aac_adtstoasc` | ADTS AAC → bare AudioSpecificConfig/raw stream (for MP4 muxing). |
| `ahx_to_mp2` | Rewrites AHX frame headers into standard MP2 headers. |
| `apv_metadata` | Edits APV metadata OBUs (color/HDR side data injection). |
| `av1_frame_merge` | Merges AV1 temporal-unit-split OBU packets into complete TUs. |
| `av1_frame_split` | Splits AV1 temporal units into per-frame OBU packets. |
| `av1_metadata` | Edits/injects/removes AV1 OBU metadata (color config, HDR, etc.). |
| `chomp` | Strips trailing zero-padding bytes from packets. |
| `dca_core` | Extracts the DTS core substream, discarding extension substreams. |
| `dovi_rpu` | Extracts/reattaches Dolby Vision RPU NAL/OBU data. |
| `dovi_split` | Splits a combined base+enhancement Dolby Vision stream into separate layers. |
| `dts2pts` | Rewrites DTS/PTS timestamps using a specified generation rule. |
| `dump_extradata` | Prepends codec extradata into the packet stream (inverse of extract). |
| `dv_error_marker` | Marks DV frames containing decode errors. |
| `eac3_core` | Extracts the AC-3 core from an E-AC-3 stream. |
| `eia608_to_smpte436m` | Repackages EIA-608 caption data into SMPTE 436M ANC packets. |
| `evc_frame_merge` | Merges split EVC NAL packets into full access units. |
| `extract_extradata` | Pulls in-band parameter-set/extradata NALs out into `AVPacket` side data. |
| `filter_units` | Generic pass/drop filter for NAL/OBU units by type (H.264/HEVC/VVC/AV1/EVC via CBS). |
| `h264_metadata` | Edits H.264 SPS/PPS/SEI fields (aspect ratio, VUI, level, SEI insertion/removal). |
| `h264_mp4toannexb` | Converts length-prefixed (AVCC) H.264 to Annex-B start-code format. |
| `h264_redundant_pps` | Strips/normalizes redundant PPS NALs. |
| `hapqa_extract` | Extracts a single subcodec's data from multi-texture HAPQA/HAPAlpha streams. |
| `hevc_metadata` | Edits HEVC VPS/SPS/PPS/SEI fields, analogous to `h264_metadata`. |
| `hevc_mp4toannexb` | Converts length-prefixed HEVC to Annex-B start-code format. |
| `imx_dump_header` | Inserts the MXF/IMX MPEG-2 sequence header required by some decoders/muxers. |
| `lcevc_merge` | Merges separate base+LCEVC-enhancement streams into one. |
| `lcevc_metadata` | Edits LCEVC enhancement metadata. |
| `media100_to_mjpegb` | Rewrites Media 100 headers into MJPEG-B compatible form. |
| `mjpeg2jpeg` | Converts AVI1/MJPEG (huffman-table-implicit) frames into standalone JFIF JPEG. |
| `mjpega_dump_header` | Inserts the header needed for "MJPEG-A" variant streams. |
| `mov2textsub` | Converts MOV/QuickTime text-track samples into plain subtitle text packets. |
| `mpeg2_metadata` | Edits MPEG-2 sequence/display extension metadata (aspect ratio, color). |
| `mpeg4_unpack_bframes` | Unpacks DivX-packed-bitstream B-frames into separate packets. |
| `noise` | Randomly corrupts packet bytes (for robustness/error-resilience testing). |
| `null` | No-op passthrough. |
| `opus_metadata` | Edits Opus stream metadata. |
| `pcm_rechunk` | Regroups raw PCM samples into differently-sized packets. |
| `pgs_frame_merge` | Merges split PGS subtitle segments into complete presentation sets. |
| `prores_metadata` | Edits ProRes frame metadata (color primaries/matrix/transfer). |
| `remove_extradata` | Strips in-band parameter sets from the packet stream. |
| `setts` | Rewrites/overrides packet PTS/DTS/duration via expressions. |
| `showinfo` | Logs per-packet diagnostic info without modifying data. |
| `smpte436m_to_eia608` | Inverse of `eia608_to_smpte436m`. |
| `text2movsub` | Converts plain text subtitle packets into MOV text-track sample format. |
| `trace_headers` | Logs parsed bitstream header fields (via CBS) without modifying data. |
| `truehd_core` | Extracts the AC-3/MLP-compatible core from a TrueHD stream. |
| `vp9_metadata` | Edits VP9 stream-level metadata (color space). |
| `vp9_raw_reorder` | Reorders VP9 packets into decode order, marking invisible frames. |
| `vp9_superframe` | Merges VP9 invisible (alt-ref) frames into superframes. |
| `vp9_superframe_split` | Splits VP9 superframes back into individual frame packets. |
| `vvc_metadata` | Edits H.266/VVC VPS/SPS/PPS/SEI fields. |
| `vvc_mp4toannexb` | Converts length-prefixed VVC to Annex-B start-code format. |

(Plus internal-only `source`/`sink` pseudo-filters used by the BSF graph API, not user-facing.)

### 1.10 Hardware acceleration

**Decode hwaccels** (`AVHWAccel`/`FFHWAccel`, one struct per codec×API pairing, registered in `hwaccels.h`, 85 entries) — codec × API matrix:

| Codec | vaapi | vdpau | dxva2/d3d11va | d3d12va | videotoolbox | nvdec/cuvid | vulkan |
|---|---|---|---|---|---|---|---|
| H.263 | dec | | | | dec | | |
| H.264 (AVC) | dec | dec | dec | dec | dec | dec | dec |
| HEVC (H.265) | dec | dec | dec | dec | dec | dec | dec |
| VVC (H.266) | dec | | | | | | |
| MPEG-1 | | dec | | | dec | dec | |
| MPEG-2 | dec | dec | dec | dec | dec | dec | |
| MPEG-4 Part 2 | dec | dec | | | dec | dec | |
| VC-1/WMV3 | dec | dec | dec | dec | | dec | |
| VP8 | dec | | | | | dec | |
| VP9 | dec | dec | dec | dec | dec | dec | dec |
| AV1 | dec | dec | dec | dec | dec | dec | dec |
| MJPEG | dec | | | | | dec | |
| ProRes | | | | | dec | | dec |
| ProRes RAW | | | | | dec | | dec |
| FFV1 | | | | | | | dec |
| APV | | | | | | | dec |
| DPX | | | | | | | dec |

**Encode hw wrappers/APIs (non-hwaccel `FFCodec` implementations, `AV_CODEC_CAP_HARDWARE`/`HYBRID`)** — codec × API for encode:

| Codec | vaapi | nvenc | qsv | amf | mediacodec | v4l2m2m | videotoolbox | d3d12va | Media Foundation (mf) | rkmpp |
|---|---|---|---|---|---|---|---|---|---|---|
| H.264 | enc | enc | enc | enc | enc | enc | enc | enc | enc | enc |
| HEVC | enc | enc | enc | enc | enc | enc | enc | enc | enc | enc |
| AV1 | enc | enc | enc | enc | enc | | | enc | enc | |
| VP8 | enc | | | | enc | enc | | | | |
| VP9 | enc | | enc | | enc | | | | | |
| MPEG-2 | enc | | enc | | | | | | | |
| MPEG-4 | | | | | enc | enc | | | | |
| H.263 | | | | | | enc | | | | |
| MJPEG | enc | | enc | | | | | | | |
| ProRes | | | | | | | enc | | | |

Additional decode-only "hardware" `FFCodec` entries beyond the hwaccel matrix: `av1_qsv`, `h264_qsv`, `hevc_qsv`, `mpeg2_qsv`, `vc1_qsv`, `vp8_qsv`, `vp9_qsv`, `vvc_qsv` decoders (Intel QSV/oneVPL); `*_mediacodec` decoders (Android, h264/hevc/mpeg2/mpeg4/vp8/vp9/av1/aac/amrnb/amrwb); `*_v4l2m2m` decoders (Linux, h263/h264/hevc/mpeg1/mpeg2/mpeg4/vp8/vp9); `*_rkmpp` decoders (Rockchip, h264/hevc/vp8/vp9); `*_cuvid` decoders (NVDEC, av1/h264/hevc/mjpeg/mpeg1/mpeg2/mpeg4/vc1/vp8/vp9); `*_amf` decoders (AMD, av1/h264/hevc/vp9); `*_oh` decoders (OpenHarmony, h264/hevc); AudioToolbox decoders (`*_at`: aac/ac3/adpcm_ima_qt/alac/amr_nb/eac3/gsm_ms/ilbc/mp1/mp2/mp3/pcm_alaw/pcm_mulaw/qdmc/qdm2 — macOS/iOS).

### 1.11 Shared infrastructure (crate-decomposition map)

| Module | Files | Codecs/components depending on it (via `configure` `select=`) |
|---|---|---|
| Bit readers/writers | `get_bits.h`, `put_bits.h`, `bitstream.c/h` | Nearly every bitstream-oriented codec (header-only, ubiquitous). |
| Exp-Golomb coding | `golomb.c/h` | H.264 decoder+parser+SEI, HEVC decoder+parser+SEI, VVC decoder+SEI, RV30/RV40/RV60, SVQ3, CAVS, Dirac parse, Dolby Vision RPU (dec+enc), EVC parse, FIC, Mobiclip, ALS(ralf), ISO writer (mp4/avif muxer helper), libx264 wrapper. |
| CABAC | `cabac.c/h`, `cabac_functions.h` | H.264 decoder, HEVC decoder, VVC decoder (each with codec-specific context-model tables layered on top). |
| CBS (coded bitstream split/insert) | `cbs.c/h`, `cbs_h264*`, `cbs_h265*`, `cbs_h266*`, `cbs_av1*`, `cbs_vp9*`, `cbs_jpeg*`, `cbs_h2645.c/h` | All H.264/HEVC/VVC/AV1/VP9/JPEG bitstream filters (`*_metadata`, `*_mp4toannexb`, `filter_units`, `trace_headers`, `dovi_rpu`, `extract_extradata`), plus parsers for those codecs. |
| VLC tables engine | `vlc.c/h` | Any codec with Huffman/VLC decode (H.263/MPEG family, DV, FLAC, etc. — used internally per-codec, not `select`ed). |
| mpegvideo core | `mpegvideo.c/h`, `mpegutils.c/h`, `rl.c` (run-level tables engine) | H.261, H.263/H.263+, MPEG-1/2 video, MPEG-4 Part 2, MSMPEG4 v1/2/3, WMV1/2, FLV1, RV10/RV20, IPU, MSS2 (via VC-1 reuse). |
| mpegvideo encoder + rate control + motion estimation | `mpegvideo_enc.c`, `ratecontrol.c/h`, `motion_est.c/h/_template.c` | Same family's encoders (H.261/H.263/MPEG-1/2/4/MSMPEG4/WMV1/2/FLV/RV10/RV20/SpeedHQ/AMV/MJPEG encoder), plus SVQ1 encoder (me_cmp only). |
| H.264 shared parts | `h264pred.c/h/_template.c`, `h264qpel` (dsp family), `h264chroma` (dsp family), `h264dsp` family | H.264 decoder, RV30/RV40 decoders (pred+qpel reuse), SVQ3 decoder (pred+dsp reuse), CAVS decoder (chroma reuse). |
| HEVC shared parts | `hevc/ps.c` (param-set parsing, "hevcparse" component), SEI (`hevc_sei`) | HEVC decoder, HEVC parser, HEVC QSV encoder wrapper (extradata construction), `dovi_split` BSF. |
| AAC shared parts | `aac/` subdir (core), `aactab.c`, `aacsbr*`, `aacps*` (SBR/PS extensions) | `aac` decoder, `aac_fixed` decoder, `aac_latm` decoder, `aac` encoder (separate encoder-only tables in `aacenc*`), shared with LATM parser/muxer framing. |
| DSP families (SIMD-dispatchable) | `idctdsp`, `fdctdsp`, `blockdsp`, `pixblockdsp`, `qpeldsp`, `hpeldsp`, `me_cmp`, `mpegvideoencdsp`, `videodsp`, `bswapdsp`, `wmv2dsp` | Broadly shared across the mpegvideo family and several intra codecs (DNxHD, AMV, MJPEG encoder use `idctdsp`/`fdctdsp`; SVQ1 uses `hpeldsp`+`me_cmp`+`mpegvideoencdsp`). |
| Transforms | `fft` (`fft_template.c` family), `rdft`, `mdct15`, `dct` (in `libavutil`, but codec-adjacent), `aandcttab`, `faandct` | Audio codecs (AAC, Vorbis, WMA*, ATRAC family, MP1/2/3 float paths) and DCT-based image/intra codecs. |
| Sine windows | `sinewin.c/h` (+ `sinewin_tablegen.h`) | 17 audio codecs' MDCT-based transforms (AAC, AC-3, Vorbis, WMA*, MLP/TrueHD encoder, Opus, ATRAC family, etc.). |
| LPC | `lpc.c/h` | FLAC, TAK, TrueHD/MLP, ALS-adjacent linear-prediction paths (7 dependents). |
| Golomb-adjacent bitstream helpers | `startcode.c/h` | MPEG start-code scanning shared by parsers/BSFs needing raw start-code search (2 dependents). |
| Rice/entropy for lossless | (per-codec, e.g. FLAC's own, ALS's own) | Not centralized; each lossless codec implements its own entropy stage. |
| Motion estimation template | `motion_est_template.c` | Instantiated per-codec inside `motion_est.c` for the mpegvideo-encoder family. |
| Threading | `pthread.c`, `pthread_frame.c`, `pthread_slice.c`, `threadprogress.c` | Framework-level, used by any codec declaring `FRAME_THREADS`/`SLICE_THREADS` (97 source files reference these capability flags). |
| Hardware-accel plumbing | `hwaccel.c`, `hwaccel_internal.h`, `hw_base_encode.c` (shared hw encoder base), `vaapi_encode.c`/`_h264.c`/`_hevc.c`/etc, `d3d12va_encode.c`, `qsvenc.c` (shared QSV encode base), `nvenc.c` (shared NVENC base) | All hwaccel decoders/encoders listed in §1.10 build on these shared bases rather than duplicating device/session setup per codec. |

---

## 2. DECODER INVENTORY

Legend: **Spec** column cites the public normative document where one exists; "RE" = reverse-engineered / no public spec (proprietary/game formats).

### 2.1 Video — modern standardized

| Decoder | Container/use | Spec | Notes |
|---|---|---|---|
| `h264` | MP4/MOV, MPEG-TS, RTP, Matroska | ITU-T H.264 / ISO/IEC 14496-10 | Baseline→High 10/4:2:2/4:4:4 profiles; frame+slice threading. |
| `hevc` | MP4, MKV, MPEG-TS | ITU-T H.265 / ISO/IEC 23008-2 | Main/Main10/Main12/RExt/SCC profiles; frame+slice threading. |
| `vvc` | MP4, raw VVC | ITU-T H.266 / ISO/IEC 23090-3 | Newest addition; decode-only, vaapi hwaccel present. |
| `av1` | MP4, WebM, MKV, IVF | AOM AV1 spec | Native software decoder; hwaccel + `libdav1d`/`libaom_av1` alternatives. |
| `vp8` | WebM, IVF | RFC 6386 | |
| `vp9` | WebM, MP4, IVF | libvpx bitstream spec (no ITU number) | Profiles 0–3. |
| `theora` (via `vp3`/theora wrapper) | Ogg | Theora spec (Xiph) | Shares VP3 core decode path. |
| `evc` (via `libxevd`) | MP4 | ISO/IEC 23094-1 (MPEG-5 EVC) | External-library only; no native decoder. |
| `apv` | MP4 | SMPTE ST 2118 (APV) | Recent addition; also has Vulkan hwaccel. |
| `jpegxs` (parser only; no standalone decoder listed) | — | ITU-T T.870 / SMPTE ST 2110-22 | Parser exists; encode via `libsvtjpegxs`. |

### 2.2 Legacy MPEG-family video

| Decoder | Container/use | Spec | Notes |
|---|---|---|---|
| `mpeg1video`, `mpegvideo` | VCD, MPEG-PS/TS | ISO/IEC 11172-2 | |
| `mpeg2video` | DVD, MPEG-TS, broadcast | ISO/IEC 13818-2 / ITU-T H.262 | |
| `mpeg4` | MP4 (Xvid/DivX) | ISO/IEC 14496-2 | ASP/SP; also `mpeg4_mmal`/`_v4l2m2m`/`_mediacodec` hw variants. |
| `h261` | H.320 conferencing | ITU-T H.261 | |
| `h263`, `h263i` (Intel var.), `h263p` | 3GP, RTP | ITU-T H.263/H.263+ | |
| `msmpeg4v1`, `msmpeg4v2`, `msmpeg4v3` | legacy AVI | RE (MS proprietary MPEG-4 pre-standard) | Shares `msmpeg4dec` core. |
| `wmv1`, `wmv2`, `wmv3`, `wmv3image` | ASF/WMV | RE (SMPTE-published VC-1 covers wmv3 as its base) | wmv3 shares VC-1 decode. |
| `vc1`, `vc1image` | ASF, MP4 | SMPTE 421M | Also `vc1_qsv`/`_mmal`/`_v4l2m2m` and full hwaccel matrix. |
| `flv` (Sorenson Spark) | FLV | RE (Sorenson H.263 variant) | |
| `rv10`, `rv20` | RealMedia | RE | Shares H.263 core. |
| `rv30`, `rv40` | RealMedia | RE | Shares H.264 pred/qpel DSP. |
| `rv60` | RealMedia | RE | Newer addition, own DSP. |
| `svq1` | MOV | RE (Sorenson Video 1) | |
| `svq3` | MOV | RE (Sorenson Video 3, H.264-derived) | |
| `mpeg2_mmal`/`_qsv`/`_v4l2m2m`/`_mediacodec`, `mpeg1_v4l2m2m` | — | — | Hardware variants of MPEG-1/2. |

### 2.3 Intra/mezzanine/production codecs

| Decoder | Container/use | Spec | Notes |
|---|---|---|---|
| `prores`, `prores_raw` | MOV | Apple ProRes (partially published), ProRes RAW RE | Also `prores_videotoolbox`/`_vulkan` hwaccel. |
| `dnxhd` | MXF, MOV | SMPTE VC-3 (RDD 35) | |
| `dnxuc` (parser only, no decoder symbol) | — | RE | Uncompressed Avid variant; parser-only. |
| `cfhd` | MOV/AVI (CineForm) | RE | |
| `jpeg2000` | MXF, J2K, DCP | ISO/IEC 15444-1 (JPEG 2000) | |
| `jpegls` | JLS | ITU-T T.87 | |
| `dpx` | DPX | SMPTE ST 268 | Also `dpx_vulkan` hwaccel. |
| `cineform`/`hap`/`hapqa` (via `hap`) | MOV/AVI (HAP) | RE | GPU-texture codec (DXT/BC formats). |
| `notchlc` | MOV | RE | |
| `speedhq` | MXF (NDI) | RE | |
| `magicyuv` | AVI/MOV | RE | Lossless. |
| `y41p`, `v210`, `v210x`, `r10k`, `r210`, `avui` | raw/MXF | Uncompressed pro formats, self-descriptive framing | |
| `vc2` (Dirac Pro, encoder only listed; `dirac` is the general decoder) | — | SMPTE VC-2 / ST 2042 | |
| `dirac` | Ogg, MXF | SMPTE VC-2 (Dirac) | |

### 2.4 Lossless (video)

| Decoder | Container | Spec | Notes |
|---|---|---|---|
| `ffv1` | MKV, MOV | RFC 9043 | Also `ffv1_vulkan` hwaccel/encoder. |
| `huffyuv`, `ffvhuff` | AVI | RE | |
| `lagarith` | AVI | RE | |
| `utvideo` | AVI/MOV | RE | |
| `mszh`, `zlib` | AVI | RE (zlib-based) | |
| `sheervideo` | MOV | RE | |
| `vble` | AVI | RE | |
| `zerocodec` | AVI | RE | |
| `mvha` | MOV | RE | |
| `ylc` | AVI | RE | |
| `png`, `apng` | PNG/APNG | ISO/IEC 15948, RFC 2083 | |
| `qtrle` | MOV | RE (Apple Animation codec) | |
| `qoi` | QOI | QOI spec (informal, public) | |

### 2.5 Screen-capture / RDP-family

| Decoder | Use | Spec | Notes |
|---|---|---|---|
| `flashsv`, `flashsv2` | FLV (Screen Video) | RE (Adobe) | |
| `vmnc` | AVI (VNC) | RE | |
| `mss1`, `mss2` | ASF (Windows Media Screen) | RE | mss2 reuses VC-1 decode. |
| `tscc`, `tscc2` | AVI (TechSmith Camtasia) | RE | |
| `vp6`/`vp6a`/`vp6f`, and screen recorders reusing MJPEG/H.264 not counted separately | — | — | |
| `mwsc` | AVI | RE | |
| `rasc` | AVI (RemotelyAnywhere) | RE | |
| `scpr` | AVI | RE | |
| `screenpresso` | AVI | RE | |
| `zmbv` | AVI (DOSBox) | RE | |
| `lscr` | MOV (LEAD Screen Capture) | RE | |
| `fmvc` | AVI | RE | |
| `g2m` | MOV (GoToMeeting) | RE | |
| `tdsc` | MOV (TDSC) | RE | |
| `wcmv` | AVI | RE | |
| `dxa`, `dxtory`, `dxv` | AVI | RE | |
| `gdv` | Gremlin AVI | RE | |
| `mvc1`, `mvc2` (Silicon Graphics Movie) | — | RE | |
| `mimic` | Mimic (MSN webcam) | RE | |
| `cscd` | AVI (CamStudio) | RE | |
| `mss1`/`mss2` (dup, listed once) | — | — | |

### 2.6 Game / FMV codecs

`bethsoftvid`, `bfi`, `bink` (+`binkaudio_dct`/`_rdft`), `bmv_video`/`bmv_audio`, `c93`, `cdgraphics`, `cdtoons`, `cdxl`, `cinepak`, `cri` (CRID video, plus `cri_decoder` for CRIWARE), `dfa`, `dsicinvideo`/`dsicinaudio`, `eacmv`, `eamad`, `eatgq`, `eatgv`, `eatqi`, `escape124`, `escape130`, `fourxm`, `fraps`, `frwu`, `gdv`, `hnm4_video`, `hq_hqa`, `hqx`, `idcin`, `iff_ilbm`, `imm4`, `imm5`, `indeo2/3/4/5`, `interplay_video`/`interplay_acm`/`interplay_dpcm`, `ipu`, `jv`, `kgv1`, `kmvc`, `mdec` (PS1), `media100`, `mmvideo`, `mobiclip`, `motionpixels`, `msa1`, `mts2`, `mv30`, `mvdv`, `mxpeg`, `nuv`, `paf_video`/`paf_audio`, `pictor`, `qdraw`, `qpeg`, `rl2`, `roq`/`roq_dpcm`, `rpza`, `sanm` (LucasArts SMUSH), `simbiosis_imx`, `smacker`/`smackaud`, `smc`, `smvjpeg`, `sga`, `srgc`, `thp`, `tiertexseqvideo`, `truemotion1`/`truemotion2`/`truemotion2rt`, `txd`, `ulti`, `vb`, `vmdvideo`/`vmdaudio`, `vmix`, `vqa`, `vqc`, `wnv1`, `xan_wc3`/`xan_wc4`/`xan_dpcm`, `yop`, `zero12v`, `mscc`, `msp2`, `msrle`, `msvideo1`, `mwsc`, `cyuv`, `cpia`, `brender_pix`, `arbc`, `argo`, `agm`, `aic`, `aasc`, `anm`, `ansi`, `avs`, `bitpacked`, `xl`, `gem`, `psd`, `photocd`, `pixlet`, `ptx`, `vb`, `vble`, `wady_dpcm`, `wavarc`, `sol_dpcm`, `derf_dpcm`, `cbd2_dpcm`, `sdx2_dpcm`, `gremlin_dpcm` — all RE, mostly one-off historical adventure/FMV game formats. Spec: RE throughout.

### 2.7 Image formats (still)

| Decoder | Spec | Notes |
|---|---|---|
| `mjpeg`, `mjpegb`, `sp5x`, `smvjpeg`, `amv` (also video codec) | ITU-T T.81 (JPEG) | Baseline/extended, MJPEG-A/B variants. |
| `webp`, `webp_anim` | WebP spec (Google) | |
| `tiff` | TIFF 6.0 | |
| `bmp` | BMP (Microsoft) | |
| `gif` | GIF89a | |
| `exr` | OpenEXR spec | |
| `sgi` | SGI RGB | |
| `targa`, `targa_y216` | TGA | |
| `pcx` | PCX | |
| `xbm`, `xpm`, `xwd` | X bitmap formats | |
| `pnm` family: `pbm`, `pgm`, `pgmyuv`, `ppm`, `pam`, `pfm`, `phm` | Netpbm | |
| `pgx` | JPEG2000-adjacent PGX | |
| `dds` | DirectDraw Surface | RE (BC1-7 etc.) |
| `dpx` (also pro codec) | SMPTE ST 268 | |
| `qoi` (also lossless) | QOI spec | |
| `fits` | FITS (astronomy) | |
| `psd` | Adobe PSD (RE) | |
| `photocd` | Kodak PhotoCD (RE) | |
| `xface` | X-Face (RE) | |
| `pictor` | RE | |
| `pjs`/none — see subtitles | — | |

### 2.8 Raw / uncompressed

`rawvideo`, `v210`, `v210x`, `y41p`, `r10k`, `r210`, `avui`, `bitpacked`, `wrapped_avframe`, `pcm_vidc`, and the full PCM audio table (§2.10).

### 2.9 Audio — standardized lossy

| Decoder | Container | Spec | Notes |
|---|---|---|---|
| `aac`, `aac_fixed`, `aac_latm` | MP4, ADTS, LATM/LOAS | ISO/IEC 14496-3 | Fixed-point variant for embedded use. |
| `ac3`, `ac3_fixed` | AC-3/E-AC-3 | ATSC A/52 | |
| `eac3` | E-AC-3 | ATSC A/52 Annex E | |
| `mp1`/`mp1float`, `mp2`/`mp2float`, `mp3`/`mp3float`, `mp3adu`/`mp3adufloat`, `mp3on4`/`mp3on4float` | MPEG audio, RTP ADU variant | ISO/IEC 11172-3 / 13818-3 | |
| `vorbis` | Ogg | Vorbis I spec (Xiph) | |
| `opus` | Ogg, WebM, Matroska | RFC 6716 | |
| `dca` | DTS, MKV | RE (ETSI TS 102 114 for core only) | |
| `als` | MPEG-4 ALS | ISO/IEC 14496-3 Annex | Lossless audio profile of MPEG-4 Part 3. |
| `atrac1`, `atrac3`, `atrac3al`, `atrac3p`, `atrac3pal`, `atrac9` | AEA, OMA, WAV | RE (Sony) | |
| `amrnb`, `amrwb` | AMR/3GP | 3GPP TS 26.090/26.190 | |
| `sbc` | Bluetooth A2DP | Bluetooth SIG SBC spec | |
| `g723_1`, `g729` (decode only), `g728` | Speech | ITU-T G.723.1/G.729/G.728 | |
| `ilbc` | VoIP | RFC 3951 | |
| `qcelp` | Qualcomm PureVoice | RE (3GPP2) | |
| `evrc` | 3GPP2 EVRC | RE | |
| `siren` (`siren` decoder) | G.722.1 variant | RE | |
| `dolby_e` | SDI-embedded | RE (SMPTE 337M carriage) | |
| `dsd_lsbf`/`_msbf`/`_lsbf_planar`/`_msbf_planar` | DSF/DFF (SACD) | DSD (Sony/Philips, RE for FFmpeg impl) | |
| `dst` | DSD Stream (SACD) | RE | |

### 2.10 Audio — lossless

| Decoder | Spec |
|---|---|
| `flac` | RFC 9639 |
| `alac` | RE (Apple, published reference decoder) |
| `ape` (Monkey's Audio) | RE |
| `tta` | RE |
| `wavpack` | RE |
| `mlp`, `truehd` | RE (Dolby) |
| `tak` | RE |
| `shorten` | RE |
| `wmalossless` | RE |
| `ralf` (RealAudio Lossless) | RE |
| `bonk` | RE |
| `wavarc` | RE |
| `osq` | RE |
| `apac` | RE |

### 2.11 Audio — speech / legacy / game

`cook`, `qdm2`/`qdmc`, `wmapro`, `wmavoice`, `wmav1`/`wmav2`, `ra_144`, `ra_288`, `sipr`, `truespeech`, `imc`, `iac`, `on2avc`, `nellymoser`, `metasound`, `twinvq`, `gsm`/`gsm_ms`, `binkaudio_dct`/`_rdft`, `smackaud`, `interplay_acm`, `xma1`/`xma2`, `mpc7`/`mpc8` (Musepack), `hca`, `hcom`, `qdmc`, `dss_sp`, `ftr`, `fastaudio`, `msnsiren`, `mace3`/`mace6`, `dvaudio`, `vmdaudio`, `bmv_audio`, `ws_snd1`, `misc4`, `siren`, `dfpwm`, `qoa`, `acelp_kelvin`, `ffwavesynth` (synthetic test-tone decoder). All RE (proprietary or game-specific), except `dfpwm`/`qoa` (open, informally specified).

### 2.12 Audio — PCM (self-describing framing, no external spec beyond byte layout)

`pcm_s8`(+`_planar`), `pcm_u8`, `pcm_s16le`/`be`(+planar), `pcm_u16le`/`be`, `pcm_s24le`/`be`(+planar), `pcm_u24le`/`be`, `pcm_s32le`/`be`(+planar), `pcm_u32le`/`be`, `pcm_s64le`/`be`, `pcm_f16le`, `pcm_f24le`, `pcm_f32le`/`be`, `pcm_f64le`/`be`, `pcm_alaw`, `pcm_mulaw`, `pcm_vidc`, `pcm_bluray`, `pcm_dvd`, `pcm_dvda`, `pcm_lxf`, `pcm_s24daud`, `pcm_sga`. Plus AudioToolbox-backed `pcm_alaw_at`/`pcm_mulaw_at`.

### 2.13 Audio — ADPCM variants (all RE except ITU-standardized ones)

Standardized: `adpcm_g722` (ITU-T G.722), `adpcm_g726`/`g726le` (ITU-T G.726), `adpcm_ms` (Microsoft, documented), `adpcm_swf` (Adobe, documented), `adpcm_ima_wav`/`adpcm_ima_qt` (documented container formats).
RE/game-specific (30+): `adpcm_4xm`, `adpcm_adx`, `adpcm_afc`, `adpcm_agm`, `adpcm_aica`, `adpcm_argo`, `adpcm_circus`, `adpcm_ct`, `adpcm_dtk`, `adpcm_ea`/`_maxis_xa`/`_r1`/`_r2`/`_r3`/`_xas`, `adpcm_ima_acorn`/`_alp`/`_amv`/`_apc`/`_apm`/`_cunning`/`_dat4`/`_dk3`/`_dk4`/`_ea_eacs`/`_ea_sead`/`_escape`/`_hvqm2`/`_hvqm4`/`_iss`/`_magix`/`_moflex`/`_mtf`/`_oki`/`_pda`/`_rad`/`_ssi`/`_smjpeg`/`_ws`/`_xbox`, `adpcm_mtaf`, `adpcm_n64`, `adpcm_psx`/`_psxc`, `adpcm_sanyo`, `adpcm_sbpro_2`/`_3`/`_4`, `adpcm_thp`/`_thp_le`, `adpcm_vima`, `adpcm_xa`, `adpcm_xmd`, `adpcm_yamaha`, `adpcm_zork`.

### 2.14 Subtitle

| Decoder | Format | Spec |
|---|---|---|
| `ass`, `ssa` | ASS/SSA | Aegisub/libass de-facto spec |
| `srt`, `subrip` | SubRip | De-facto |
| `webvtt` | WebVTT | W3C WebVTT |
| `movtext` | MP4 3GPP timed text | 3GPP TS 26.245 |
| `dvbsub` | DVB | ETSI EN 300 743 |
| `dvdsub` | DVD (VOBSUB) | RE |
| `pgssub` | Blu-ray PGS | RE |
| `xsub` | DivX XSUB | RE |
| `ccaption` | ATSC CEA-608/708 embedded | CEA-708 |
| `jacosub`, `microdvd`, `mpl2`, `pjs`, `realtext`, `sami`, `stl`, `subviewer`, `subviewer1`, `vplayer` | Legacy text subtitle formats | De-facto/informal |
| `text` | Plain text | — |
| `libaribb24`, `libaribcaption` | ARIB B24 (Japanese broadcast) | ARIB STD-B24 |
| `libzvbi_teletext` | Teletext | ETSI EN 300 706 |

### 2.15 Data

No standalone decoders; these are container-level pass-through `AVCodecID`s described by `AVCodecDescriptor` only (no `FFCodec`): `SCTE_35`, `EPG`, `SMPTE_KLV`, `TIMED_ID3`, `SMPTE_2038`, `SMPTE_436M_ANC`, `ITUT_T35`, `BIN_DATA`, `TTF`/`OTF` (font attachments), `DVD_NAV` (has a parser, `dvd_nav_parser`, but no decoder), `MPEG2TS`.

---

## 3. ENCODER INVENTORY

### 3.1 Native video encoders

| Encoder | Spec/target | Notes |
|---|---|---|
| `h264_videotoolbox`/`_vaapi`/`_nvenc`/`_qsv`/`_amf`/`_mf`/`_v4l2m2m`/`_vulkan`/`_rkmpp`/`_mediacodec`/`_oh`/`_d3d12va` | ITU-T H.264 | All hardware-only; no native H.264 software encoder exists in FFmpeg (relies on `libx264`). |
| `hevc_videotoolbox`/`_vaapi`/`_nvenc`/`_qsv`/`_amf`/`_mf`/`_v4l2m2m`/`_vulkan`/`_rkmpp`/`_mediacodec`/`_oh`/`_d3d12va` | ITU-T H.265 | Same pattern; native software falls to `libx265`. |
| `av1_nvenc`/`_qsv`/`_amf`/`_mf`/`_vaapi`/`_vulkan`/`_mediacodec`/`_d3d12va` | AV1 | Native software falls to `libaom_av1`/`libsvtav1`/`librav1e`. |
| `mpeg1video`, `mpeg2video` (+`_qsv`/`_vaapi` hw variants) | ISO/IEC 11172-2 / 13818-2 | Native software encoders exist (mpegvideo_enc-based). |
| `mpeg4`, `msmpeg4v2`, `msmpeg4v3`, `wmv1`, `wmv2`, `flv`, `h263`, `h263p`, `h261`, `rv10`, `rv20` (+ `h263_v4l2m2m`) | Legacy MPEG-family | Native, mpegvideo_enc-based. |
| `svq1` | Sorenson SVQ1 | Native. |
| `snow` | RE (FFmpeg's own experimental wavelet codec) | Native, uses own DWT. |
| `vc2` (Dirac Pro) | SMPTE VC-2 | Native. |
| `ffv1`, `ffv1_vulkan` | RFC 9043 | Native software + Vulkan-accelerated variant. |
| `ffvhuff`, `huffyuv` | RE | Native lossless. |
| `magicyuv` | RE | Native lossless. |
| `utvideo` | RE | Native lossless. |
| `cfhd` | RE | Native intermediate codec. |
| `dnxhd` | SMPTE VC-3 | Native. |
| `prores`, `prores_aw`, `prores_ks`, `prores_ks_vulkan` | Apple ProRes | Three independent native encoder implementations (original, Anatoliy Wasserman's, KostyaShishkov's) plus Vulkan-accelerated KS variant; also `prores_videotoolbox` hw wrapper. |
| `jpeg2000` | ISO/IEC 15444-1 | Native. |
| `jpegls` | ITU-T T.87 | Native. |
| `mjpeg`, `mjpeg_qsv`/`_vaapi` | ITU-T T.81 | Native + hw variants. |
| `ljpeg` | Lossless JPEG (RE-ish, part of JPEG spec Annex) | Native. |
| `amv` | RE (Sorenson AMV) | Native, reuses MJPEG core. |
| `qtrle`, `msrle`, `msvideo1` | RE | Native. |
| `zmbv` | RE | Native. |
| `cinepak` | RE | Native. |
| `roq`, `roq_dpcm` | RE (id Software) | Native. |
| `rpza` | RE (Apple) | Native. |
| `smc` | RE (Apple) | Native. |
| `dxv` | RE | Native. |
| `hap` | RE | Native, DXT-based. |
| `speedhq` | RE | Native. |
| `dpx`, `exr`, `sgi`, `tiff`, `png`, `apng`, `gif`, `bmp`, `pcx`, `sunrast`, `targa`, `wbmp`, `xbm`, `xface`, `xwd`, `yuv4`, `pam`, `pbm`, `pgm`, `pgmyuv`, `ppm`, `pfm`, `phm`, `qoi`, `fits`, `alias_pix`, `hdr` | Image formats, various open specs | Native. |
| `zlib` | RE (zlib-wrapped raw) | Native. |
| `r10k`, `r210`, `v210`, `y41p`, `avui`, `vbn`, `bitpacked`, `rawvideo`, `wrapped_avframe` | Raw/uncompressed | Native. |
| `a64multi`, `a64multi5` | Commodore 64 video | Native, niche target format. |
| `cljr` | RE (Cirrus Logic) | Native. |
| `comfortnoise` | RFC 3389-adjacent (audio, listed here for structural completeness — actually audio) | — |
| `dvvideo` | SMPTE 314M/370M (DV) | Native. |
| `apv_vulkan` | SMPTE ST 2118 | Vulkan-accelerated native APV encoder. |

### 3.2 Native audio encoders

`aac`, `ac3`/`ac3_fixed`, `eac3`, `mp2`/`mp2fixed`, `alac`, `flac`, `opus`, `vorbis`, `dca` (experimental), `wavpack`, `truehd`, `mlp`, `tta`, `sbc`, `nellymoser`, `wmav1`, `wmav2`, `ra_144`, `s302m` (experimental, SMPTE 302M), `g723_1`, `comfortnoise`, `dfpwm`, `aptx`, `aptx_hd`, plus the full PCM/ADPCM/DPCM encoder set: `pcm_*` (16 formats incl. `_at` AudioToolbox variants for alaw/mulaw), `adpcm_adx`, `adpcm_argo`, `adpcm_g722`, `adpcm_g726`/`le`, `adpcm_ima_alp`/`_amv`/`_apm`/`_qt`/`_ssi`/`_wav`/`_ws`, `adpcm_ms`, `adpcm_swf`, `adpcm_yamaha`, `roq_dpcm`.

### 3.3 Subtitle encoders

`ass`, `ssa`, `srt`, `subrip`, `webvtt`, `movtext`, `ttml`, `dvbsub`, `dvdsub`, `xsub`, `text`.

### 3.4 External-library wrapper encoders

| Encoder | Library | License |
|---|---|---|
| `libx264`, `libx264rgb` | x264 | GPL-2.0+ |
| `libx265` | x265 | GPL-2.0+ (also offers a commercial license) |
| `libxavs` | xavs | GPL-2.0+ |
| `libxavs2` | xavs2 | GPL-2.0+ |
| `libxvid` | libxvidcore | GPL-2.0+ |
| `libvpx_vp8`, `libvpx_vp9` | libvpx | BSD-3-Clause |
| `libaom_av1` | libaom | BSD-2-Clause |
| `librav1e` | rav1e | BSD-2-Clause |
| `libsvtav1` | SVT-AV1 | BSD-2-Clause-Patent |
| `libsvtjpegxs` (dec+enc) | SVT-JPEG-XS | BSD-2-Clause-Patent |
| `liboapv` | OpenAPV | BSD-3-Clause |
| `libxeve` | XEVE (EVC) | BSD-3-Clause |
| `libvvenc` | Fraunhofer VVenC | BSD-3-Clause-Clear |
| `libkvazaar` | Kvazaar (HEVC) | BSD-3-Clause |
| `libopenh264` (dec+enc) | Cisco OpenH264 | BSD-2-Clause |
| `libtheora` | libtheora | BSD-3-Clause |
| `libwebp`, `libwebp_anim` | libwebp | BSD-3-Clause |
| `libjxl`, `libjxl_anim` | libjxl (JPEG XL) | BSD-3-Clause / Apache-2.0 |
| `libopenjpeg` | OpenJPEG | BSD-2-Clause |
| `librsvg` (decoder) | librsvg | LGPL-2.1+ |
| `libmp3lame` | LAME | LGPL-2.0+ |
| `libshine` | Shine (fixed-point MP3) | BSD-3-Clause |
| `libtwolame` | TwoLAME (MP2) | LGPL-2.1+ |
| `libvorbis` (dec+enc) | libvorbis | BSD-3-Clause |
| `libopus` (dec+enc) | libopus | BSD-3-Clause |
| `liblc3` (dec+enc) | Google liblc3 (Bluetooth LC3) | Apache-2.0 |
| `libfdk_aac` (dec+enc) | fdk-aac | Custom permissive but **flagged nonfree** by FFmpeg build system (patent-encumbered AAC) |
| `libmpeghdec` (decoder) | MPEG-H 3D Audio decoder | **Nonfree** per FFmpeg classification |
| `libgsm`, `libgsm_ms` (dec+enc) | libgsm | ISC-style permissive |
| `libilbc` (dec+enc) | libilbc (WebRTC) | BSD-3-Clause |
| `libcodec2` (dec+enc) | codec2 | LGPL-2.1+ |
| `libopencore_amrnb` (dec+enc), `libopencore_amrwb` (decoder) | OpenCORE AMR | Apache-2.0 (requires `--enable-version3`) |
| `libvo_amrwbenc` | VisualOn AMR-WB encoder | Apache-2.0 (requires `--enable-version3`) |
| `libspeex` (dec+enc) | libspeex | BSD-3-Clause |
| `libdav1d` (decoder) | dav1d | BSD-2-Clause |
| `libdavs2` (decoder) | davs2 (AVS2) | **GPL-2.0+** |
| `libuavs3d` (decoder) | uavs3d (AVS3) | BSD-3-Clause-ish (open) |
| `libxevd` (decoder) | XEVD (EVC) | BSD-3-Clause |
| `libaribb24`, `libaribcaption` (decoders) | libaribb24 (LGPLv3), libaribcaption (permissive) | libaribb24 requires `--enable-version3` |
| `libzvbi_teletext` (decoder) | libzvbi | GPL-2.0+ |

### 3.5 Special/null encoders

`vnull` (video), `anull` (audio) — discard-all encoders used for benchmarking/testing pipelines.

---

## 4. LICENSING / AVAILABILITY FLAGS

### 4.1 GPL-gated (require `--enable-gpl`; native libavcodec code itself has no GPL-only files — gating is entirely at the external-library level)

External libraries under `EXTERNAL_LIBRARY_GPL_LIST`: `libdavs2`, `libx264`, `libx265`, `libxavs`, `libxavs2`, `libxvid`, plus non-avcodec `frei0r`, `libcdio`, `librubberband`, `libvidstab`, `avisynth`. In `libavcodec` terms this gates: `libdavs2` decoder, `libx264`/`libx264rgb` encoders, `libx265` encoder, `libxavs`/`libxavs2` encoders, `libxvid` encoder. One local IDCT asm file (`x86/idct_mmx.c`) is also GPL-only but affects no public API surface (falls back to the default IDCT when GPL is off).

### 4.2 Nonfree (require `--enable-nonfree`, changes distribution terms beyond even GPL compatibility)

`libfdk_aac` (encoder+decoder), `libmpeghdec` (decoder). `decklink` (I/O, not avcodec) is the third member of this list.

### 4.3 LGPLv3/Apache-requiring (`--enable-version3`)

`libaribb24` (decoder), `libopencore_amrnb`/`libopencore_amrwb` (Apache-licensed, decoder+encoder), `libvo_amrwbenc` (encoder). (`gmp`, `liblensfun`, `mbedtls`, `rkmpp` also in this class but not avcodec codecs themselves — `rkmpp` hw codecs specifically are gated this way since the RK MPI library is Apache-2.0.)

### 4.4 External library required, but permissively licensed (no `--enable-gpl`/`--enable-nonfree` needed)

All entries in §3.4 not covered above: libx264-adjacent alternatives `libaom_av1`, `libvpx_*`, `librav1e`, `libsvtav1`, `liboapv`, `libxeve`/`libxevd`, `libvvenc`, `libkvazaar`, `libopenh264`, `libtheora`, `libwebp*`, `libjxl*`, `libopenjpeg`, `librsvg`, `libmp3lame`, `libshine`, `libtwolame`, `libvorbis`, `libopus`, `liblc3`, `libgsm*`, `libilbc`, `libcodec2`, `libspeex`, `libdav1d`, `libuavs3d`, `libaribcaption`, `libzvbi_teletext` (GPL, see §4.1 correction — actually GPL, listed here in error; correctly counted in §4.1's spirit though not in `EXTERNAL_LIBRARY_GPL_LIST` since it's checked via its own `--enable-libzvbi` + license note in source).

### 4.5 Experimental (`AV_CODEC_CAP_EXPERIMENTAL`, native code)

`dca` (audio encoder), `mlp` (encoder), `opus` (native encoder, `opus/enc.c`), `vorbis` (native encoder), `avui` (encoder), `pdv` (encoder), `s302m` (encoder). These require `-strict experimental` (or `-strict -2`) on the CLI to select.

### 4.6 Hardware-only encoders with no native software fallback in libavcodec

H.264 and HEVC have **no native software encoder** in FFmpeg at all — every `h264_*`/`hevc_*` encoder is either a hardware API wrapper or the `libx264`/`libx265` external-library wrapper. This is a notable planning datapoint: a "H.264/HEVC encode" feature parity target must either wrap an external encoder or write one from scratch, since there's no FFmpeg reference implementation to port.

---

## 5. APPROXIMATE IMPLEMENTATION SIZE (top ~40 by lines of C, for effort planning)

Sizes are summed by codec/family across all `.c` files with that codec's prefix (including any subdirectory), so they include the codec's own encoder+decoder+DSP+data-table files together. Data-only table files (e.g. `dcadata.c`, `aactab.c`, `vp9data.c`) are included since they are part of that codec's implementation footprint, not shared infra.

| Family | Approx. LOC | Included files (representative) |
|---|---|---|
| VVC (H.266) | ~22,900 | `vvc/*.c` (decoder, own DSP/CABAC context), `vvc_parser.c`, `vaapi_vvc.c` |
| CBS (H.264/H.265/H.266/AV1/VP9/JPEG syntax, shared infra not one codec) | ~22,000 | `cbs*.c` |
| DCA/DTS | ~19,850 | `dcadata.c` (large static tables), `dca_core.c`, `dcadec.c`, `dcadsp.c`, `dcaenc.c`, `dca_exss.c`, `dca_xll.c`, `dca_lbr.c` |
| H.264 | ~19,800 | `h264dec.c`, `h264_slice.c`, `h264_cabac.c`, `h264_cavlc.c`, `h264_refs.c`, `h264_parse.c`, `h264_sei.c`, `h264_ps.c`, `h264pred*.c`, `h264_loopfilter.c`, etc. |
| HEVC (H.265) | ~13,700 | `hevc/*.c` (decoder, cabac, ps, sei, filter, refs, dsp) |
| AAC | ~21,600 | `aac/*.c` (core) + top-level `aacenc*.c`, `aacsbr*.c`, `aacps*.c`, `aactab.c`, `aacdec_fixed*` |
| VAAPI encode infra | ~12,250 | `vaapi_encode.c` + per-codec `vaapi_encode_{h264,hevc,mpeg2,vp8,vp9,av1}.c` |
| VC-1 | ~10,850 | `vc1.c`, `vc1dec.c`, `vc1_block.c`, `vc1_loopfilter.c`, `vc1_mc.c`, `vc1_parser.c`, `vc1data.c` |
| VP9 | ~10,480 | `vp9.c`, `vp9data.c`, `vp9dsp*.c`, `vp9mvs.c`, `vp9prob.c`, `vp9recon.c` |
| mpegvideo core | ~8,190 | `mpegvideo.c`, `mpegvideo_enc.c` (also counted separately below), `mpegutils.c`, `mpeg12dec.c`-adjacent |
| WMA family | ~7,590 | `wmadec.c`, `wmaenc.c`, `wmaprodec.c`, `wmavoice.c`, `wma.c`, `wma_common.c`, `wma_freqs.c` |
| Opus (native) | ~7,560 | `opus/*.c` (dec.c, enc.c, celt, silk, pvq, rc) |
| AC-3/E-AC-3 | ~6,930 | `ac3enc.c`, `ac3dec.c`, `ac3_parser.c`, `ac3tab.c`, `eac3enc.c`, `eac3dec.c` |
| QSV wrappers | ~6,500 | `qsvenc.c` + per-codec `qsvenc_{h264,hevc,mpeg2,jpeg,vp9,av1}.c`, `qsvdec.c` |
| FFV1 | ~6,450 | `ffv1.c`, `ffv1enc.c`, `ffv1dec.c`, `ffv1_template.c`, `ffv1_parser.c`, `vulkan/ffv1*` |
| JPEG 2000 | ~6,090 | `jpeg2000dec.c`, `jpeg2000enc.c`, `jpeg2000.c`, `jpeg2000dwt.c`, `jpeg2000_parser.c` |
| Vorbis | ~6,030 | `vorbisdec.c`, `vorbisenc.c`, `vorbis.c`, `vorbis_data.c`, `vorbis_parser.c` |
| MPEG-4 Part 2 | ~5,970 | `mpeg4videodec.c`, `mpeg4videoenc.c`, `mpeg4video.c`, `mpeg4video_parser.c` |
| ProRes (all 3 encoders + decoder) | ~5,550 | `proresdec2.c`, `proresenc_anatoliy.c`, `proresenc_kostya.c`, `proresdsp.c`, `prores_raw.c`, `prores_parser.c`, `vulkan/prores*` |
| Dirac / VC-2 | ~5,360 | `diracdec.c`, `dirac_parser.c`, `dirac_vlc.c`, `diracdsp.c`, `vc2enc.c`, `dirac_dwt.c` |
| MPEG-1/2 video core | ~5,010 | `mpeg12dec.c`, `mpeg12enc.c`, `mpeg12.c`, `mpeg12data.c` |
| MJPEG | ~4,880 | `mjpegdec.c`, `mjpegenc.c`, `mjpegenc_common.c`, `mjpeg_parser.c`, `mjpegenc_huffman.c` |
| VideoToolbox (hw wrapper) | ~4,710 | `videotoolbox.c`, `videotoolboxenc.c`, `videotoolbox_vt_internal.h`-adjacent |
| Snow | ~4,520 | `snow.c`, `snowenc.c`, `snow_dwt.c` |
| MLP/TrueHD | ~4,480 | `mlpdec.c`, `mlpenc.c`, `mlp.c`, `mlp_parser.c` |
| FLAC | ~4,230 | `flacdec.c`, `flacenc.c`, `flac.c`, `flacdata.c`, `flac_parser.c` |
| VP8 | ~4,070 | `vp8.c`, `vp8data.c`, `vp8dsp.c` |
| NVENC wrapper | ~3,950 | `nvenc.c`, `nvenc_h264.c`, `nvenc_hevc.c`, `nvenc_av1.c` |
| VP3/Theora | ~3,730 | `vp3.c`, `vp3data.c`, `libtheoraenc.c`-adjacent |
| PNG | ~3,690 | `pngdec.c`, `pngenc.c`, `png.c` |
| AMF wrapper | ~3,590 | `amfenc.c`, `amfenc_{h264,hevc,av1}.c`, `amfdec.c` |
| DNxHD | ~3,430 | `dnxhddec.c`, `dnxhdenc.c`, `dnxhddata.c`, `dnxhd_parser.c` |
| TIFF | ~3,340 | `tiff.c`, `tiff_data.c`, `tiff_common.c` |
| OpenEXR | ~3,010 | `exr.c`, `exrenc.c` |
| Indeo | ~2,840 | `indeo2.c`, `indeo3.c`, `indeo4.c`, `indeo5.c`, `ivi.c`, `ivi_dsp.c` |
| APV | ~2,720 | `apv_decode.c`, `apv_parser.c`, `vulkan/apv*` |
| RV30/RV40 | ~2,700 | `rv30.c`, `rv34.c`, `rv40.c`, `rv34dsp.c`, `rv34_parser.c` |
| Huffyuv/FFVHuff | ~2,680 | `huffyuv.c`, `huffyuvdec.c`, `huffyuvenc.c` |
| RV60 | ~2,610 | `rv60dec.c`, `rv60dsp.c` |
| WebP | ~2,330 | `webp.c`, `libwebpenc_common.c`-adjacent |

---

## 10-line summary

Explored `~/repos/FFmpeg` libavcodec (v8.0.git, commit 564f92cce2). Catalogued the full `AVCodec`/`AVCodecContext`/`FFCodec` object model, all 23 `AV_CODEC_CAP_*` capability flags and 8 `AV_CODEC_PROP_*` property flags, the send/receive vs. legacy decode/encode callback shapes, frame/slice/other threading models, and draining/flushing semantics. Enumerated all 66 `AVCodecParser` implementations and all 50 bitstream filters with one-line transformation descriptions. Built the complete decode-hwaccel matrix (85 codec×API pairs across vaapi/vdpau/dxva2/d3d11va/d3d12va/videotoolbox/nvdec/vulkan) and the encode-side hw wrapper matrix (vaapi/nvenc/qsv/amf/mediacodec/v4l2m2m/videotoolbox/mf/rkmpp/d3d12va). Mapped shared infrastructure (CABAC, golomb, CBS, mpegvideo core, H.264/HEVC shared parts, AAC core, DSP families, rate control/motion estimation, sinewin/LPC/transforms) to their dependent codecs via `configure` `select=` chains — the key crate-decomposition input. Enumerated all ~605 decoder and ~271 encoder `FFCodec` symbols from `allcodecs.c`, grouped into video (modern/legacy-MPEG/intra-production/lossless/screen-capture/game/image/raw), audio (lossy/lossless/speech/legacy/PCM/ADPCM/DSD), subtitle, and data categories with spec citations (ITU-T/ISO/IEC/RFC/SMPTE) or "reverse-engineered" marking. Catalogued 40+ external-library wrapper encoders with their bound library and license. Flagged GPL-only (`libx264`/`libx265`/`libxavs*`/`libxvid`/`libdavs2`/`libzvbi`), nonfree (`libfdk_aac`, `libmpeghdec`), version3-gated (AMR/aribb24), and 7 experimental native encoders — and noted H.264/HEVC have zero native FFmpeg software encoders (wrapper-only). Compiled a ~40-entry LOC table (VVC ~22.9k, CBS infra ~22k, DCA ~19.9k, H.264 ~19.8k, AAC ~21.6k, down to WebP ~2.3k) for effort estimation. Could not write the deliverable file directly — no Write/Edit tool in this read-only subagent session; full content delivered as chat text above for the parent session to save to `/Users/matthew/projects/vaco/planning/research/02-libavcodec.md`.