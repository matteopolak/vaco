# FFmpeg libavformat Feature Inventory

Source: `~/repos/FFmpeg`, commit `564f92cce2` (2026-08-19), version string `8.0.git`. This is a structural/nominal inventory — names, option strings, flag enumerations, struct field lists, and cross-file dependency relationships — for a clean-room Rust reimplementation. No algorithm bodies, parsing logic, or constant/lookup tables are reproduced; only the public/semi-public shape (identifiers, types, defaults, ranges, one-line semantics taken from doc comments) is catalogued. Where a container/protocol is standardized, the governing spec is cited; otherwise it is marked "reverse-engineered" (FFmpeg's own historical characterization for formats it decodes without a public spec).

---

## 1. FRAMEWORK

### 1.1 Core object model

- **`AVFormatContext`** (`avformat.h`) — top-level container handle for both muxing and demuxing. Key fields: `iformat`/`oformat` (format descriptors, mutually exclusive), `priv_data`, `pb` (`AVIOContext *`), `ctx_flags` (`AVFMTCTX_*`), `streams`/`nb_streams` (`AVStream **`), `nb_programs`/`programs` (`AVProgram **`), `nb_chapters`/`chapters` (`AVChapter **`), `url`/`filename`, `start_time`/`duration`/`bit_rate`, `packet_size`, `max_delay`, `flags` (`AVFMT_FLAG_*`), `probesize`/`format_probesize`, `max_analyze_duration`, `key`/`keylen` (decryption key), `video_codec_id`/`audio_codec_id`/`subtitle_codec_id`/`data_codec_id` (forced codec overrides), `metadata`, `start_time_realtime`, `fps_probe_size`, `error_recognition`, `interrupt_callback` (`AVIOInterruptCB`), `debug`, `max_streams`, `max_index_size`, `max_picture_buffer`, `max_interleave_delta`, `max_ts_probe`, `max_chunk_duration`/`max_chunk_size`, `max_probe_packets`, `strict_std_compliance`, `event_flags` (`AVFMT_EVENT_FLAG_*`), `avoid_negative_ts`, `audio_preload`, `use_wallclock_as_timestamps`, `skip_estimate_duration_from_pts`, `avio_flags`, `duration_estimation_method` (`AVDurationEstimationMethod`), `skip_initial_bytes`, `correct_ts_overflow`, `seek2any`, `flush_packets`, `probe_score`, `codec_whitelist`/`format_whitelist`/`protocol_whitelist`/`protocol_blacklist` (comma-separated), `io_open`/`io_close2` callbacks, `url_open_cb` equivalents, `output_ts_offset`, `dump_separator`, `data_codec_id`, `metadata_header_padding`, `opaque`/`control_message_cb`, `output_timestamp_offset`. Internal counterpart `FFFormatContext` (`internal.h`) holds packet_buffer lists, `id3v2_meta`, `parse_queue`, probe-retry state, etc., not exposed publicly.
- **`AVStream`** (`avformat.h`) — one elementary stream. Public fields: `index`, `id`, `codecpar` (`AVCodecParameters *`), `priv_data`, `time_base`, `start_time`, `duration`, `nb_frames`, `disposition` (`AV_DISPOSITION_*`), `discard` (`AVDiscard`), `sample_aspect_ratio`, `metadata`, `avg_frame_rate`, `attached_pic` (embedded `AVPacket` for `AV_DISPOSITION_ATTACHED_PIC` streams, e.g. cover art), `event_flags` (`AVSTREAM_EVENT_FLAG_*`), `r_frame_rate`, `pts_wrap_bits`. Internal counterpart `FFStream` (`internal.h`) carries the demuxer-private parsing/index state (`index_entries`, `parser`, `cur_dts`, `probe_data`, `pts_buffer`, `last_IP_pts`, `stream_identifier`, `interleaver_chunk_size/duration`, `bsfc` auto-BSF chain, `mux_ts_offset`, `need_context_update`).
- **`AVProgram`** — logical grouping of stream indices (e.g. MPEG-TS/HDHomeRun-style multiplex): `id`, `flags`, `discard`, `stream_index[]`/`nb_stream_indexes`, `metadata`, `program_num`, `pmt_pid`, `pcr_pid`, `pmt_version`; internal-only `start_time`/`end_time`, `pts_wrap_reference`, `pts_wrap_behavior`.
- **`AVChapter`** — `id` (int64), `time_base`, `start`/`end`, `metadata`.
- **`AVPacket`** (`libavcodec/packet.h`, shared with libavcodec) — `buf` (refcounted `AVBufferRef`), `pts`/`dts`, `data`/`size`, `stream_index`, `flags` (`AV_PKT_FLAG_KEY`/`AV_PKT_FLAG_DISCARD`/`AV_PKT_FLAG_CORRUPT`/`AV_PKT_FLAG_DISPOSABLE`/`AV_PKT_FLAG_TRUSTED`), `side_data`/`side_data_elems` (array of typed `AVPacketSideData`, e.g. palette, replaygain, stereo3d, skip-samples, new-extradata, timecode, matrix, encryption-info), `duration`, `pos`, `opaque`/`opaque_ref`, `time_base`.
- **`AVProbeData`** (`avformat.h`) — `filename`, `buf` (padded with `AVPROBE_PADDING_SIZE` zero bytes), `buf_size`, `mime_type`.
- **`AVIndexEntry`** — `pos`, `timestamp`, `flags:2` (`AVINDEX_KEYFRAME`, `AVINDEX_DISCARD_FRAME`), `size:30`, `min_distance`.

### 1.2 Stream discovery / probing model

- **Container-level probing**: `av_probe_input_format2`/`av_probe_input_format3` iterate every registered `AVInputFormat`, calling each demuxer's `read_probe(AVProbeData*)` (via internal `FFInputFormat.read_probe`), which returns a confidence score. A demuxer with no `read_probe` but non-NULL `extensions` is scored purely by filename-extension match. Highest score wins; ties are broken by registration order.
- **Score constants** (`AVPROBE_SCORE_*`): `MAX` = 100 (maximal confidence, exact magic/structural match), `EXTENSION` = 50 (extension-only match), `MIME_BONUS` = 30 (added when demuxer's `mime_type` list matches probe data's mime type), `RETRY` = MAX/4, `STREAM_RETRY` = MAX/4 − 1 (thresholds below which `avformat_open_input` re-probes with a larger buffer).
- **Buffer growth**: initial probe buffer is `format_probesize`-bounded (`PROBE_BUF_MIN` doubling up to `PROBE_BUF_MAX`, default via `formatprobesize` option); if the best score is at/below `AVPROBE_SCORE_RETRY`, the demuxer core doubles the buffer and retries up to the configured probe size ceiling.
- **`avformat_find_stream_info()`**: after `avformat_open_input()` opens the container and calls its `read_header`, this second pass reads/parses/decodes leading packets (bounded by `probesize` bytes, `max_analyze_duration` µs, `fps_probe_size` frames, `max_ts_probe` packets, `max_probe_packets` packets) to fill in per-stream codec parameters (dimensions, sample rate, channel layout), `avg_frame_rate`/`r_frame_rate`, and to run `estimate_timings()` for `duration`/`bit_rate` when the container didn't supply them directly.

### 1.3 Timestamp model

- **Time bases**: every `AVStream.time_base` is an `AVRational`; demuxers set it, muxers may overwrite the caller's hint with the value actually written. `AVFormatContext` itself has no global time base — `AV_TIME_BASE`/`AV_TIME_BASE_Q` (1/1,000,000, defined in `libavutil/avutil.h`) is the fixed rescale target used by `av_seek_frame`'s `AVSEEK_FLAG` semantics and by `output_ts_offset`.
- **PTS/DTS**: packet-level `pts` (presentation) and `dts` (decode) are in stream time base; `AV_NOPTS_VALUE` denotes unknown. Reordering codecs (B-frames) produce `dts` ≤ `pts` per frame but a monotonic `dts` sequence.
- **Wrapping**: `AVStream.pts_wrap_bits` records the bit width of the container's native timestamp field (e.g. 33-bit MPEG-PES). Wrap-around handling is controlled per-program via `AVProgram.pts_wrap_behavior`, one of `AV_PTS_WRAP_IGNORE` (0), `AV_PTS_WRAP_ADD_OFFSET` (1), `AV_PTS_WRAP_SUB_OFFSET` (−1), driven by `correct_ts_overflow`.
- **Discontinuity correction**: demuxer core (`demux.c`) tracks a per-stream `cur_dts`/`last_IP_pts` state machine; the generic `AVFMT_FLAG_IGNDTS`/`AVFMT_FLAG_GENPTS`/`AVFMT_FLAG_SORT_DTS` options and format-level `AVFMT_TS_DISCONT` capability flag govern whether jumps are treated as errors, tolerated, or actively re-sorted. `use_wallclock_as_timestamps` substitutes local wall-clock reads for missing container timestamps (capture/live sources).
- **`start_time`**: `AVFormatContext.start_time` (global) and `AVStream.start_time` (per stream) hold the PTS of the first presented frame in the corresponding time base; demuxers only set the per-stream value when certain (documented exception: ASF, whose header value is unreliable and is deliberately not surfaced).
- **Duration estimation** (`enum AVDurationEstimationMethod`, read-only, reported via `AVFormatContext.duration_estimation_method`): `AVFMT_DURATION_FROM_PTS` (accurate, derived from observed packet PTS range), `AVFMT_DURATION_FROM_STREAM` (copied from a stream whose container-native duration field is trusted), `AVFMT_DURATION_FROM_BITRATE` (least accurate — file size ÷ bit rate fallback). Controlled by `skip_estimate_duration_from_pts` and `duration_probesize`.

### 1.4 Seeking model

- **Generic entry points**: `av_seek_frame(s, stream_index, timestamp, flags)` (legacy, single target timestamp) and `avformat_seek_file(s, stream_index, min_ts, ts, max_ts, flags)` (range-bounded, preferred).
- **`AVSEEK_FLAG_*`**: `BACKWARD` (1, prefer keyframe ≤ target), `BYTE` (2, `timestamp` is a byte offset, not a PTS), `ANY` (4, allow landing on non-keyframes — demuxer must additionally support this via `seek2any`/`AVFMT_FLAG_FAST_SEEK` for it to be honored on some formats), `FRAME` (8, `timestamp` is a frame number). `AVSEEK_SIZE` (`avio.h`, 0x10000) is a pseudo-whence value passed to a custom `AVIOContext.seek` callback to query total stream size without moving the position; `AVSEEK_FORCE` (0x20000) forces a seek even on a protocol that reports itself unseekable.
- **Dispatch**: if the format defines `read_seek2` (new-style, timestamp range + stream index), it is called directly; else if `AVFMT_TS_DISCONT` is not set and an index exists, `ff_seek_frame_binary()` does a binary search using `read_timestamp()` callback probes; else `seek_frame_generic()` falls back to linear/index-assisted scanning with `AVIndexEntry` records, calling the format's `read_seek` (old-style, single timestamp) if present, or performing pure generic-index seeking (`AVFMT_GENERIC_INDEX` capability) directly against `av_index_search_timestamp()`.
- **Format-level opt-outs**: `AVFMT_NOBINSEARCH` (binary search via `read_timestamp` unavailable), `AVFMT_NOGENSEARCH` (generic index-based search unavailable), `AVFMT_NO_BYTE_SEEK` (byte-offset seeking unsupported), `AVFMT_SEEK_TO_PTS` (seeking is PTS-based even when the format is otherwise byte-oriented).
- **Index building**: populated incrementally by demuxers via `av_add_index_entry()` (dedups/merges by position, capped by `max_index_size` bytes per stream — LRU-style eviction of old entries once exceeded) or read wholesale from a container-native index (MOV `stco`/`stsz`, MKV Cues, AVI `idx1`, etc.) during `read_header`.

### 1.5 Interleaving model (muxers)

- `av_interleaved_write_frame()` buffers packets per stream in a linked packet list (`FFFormatContext.packet_buffer`) and only flushes once every active stream has at least one queued packet (or `max_interleave_delta` µs of buffering has been exceeded for "sparse" streams), guaranteeing output packets are emitted in non-decreasing DTS order across streams.
- Default comparator interleaves strictly by rescaled DTS (`ff_interleave_packet_per_dts`) when the format has more than one stream needing interleaving, or is pass-through (`ff_interleave_packet_passthrough`, single-stream/unbuffered) otherwise; a format can override via `FFOutputFormat.interleave_packet` for custom ordering (e.g. NUT, MOV fragment boundaries).
- `av_write_frame()` is the non-interleaving alternative — caller is responsible for DTS ordering.
- `audio_preload`/`max_chunk_duration`/`max_chunk_size` bias/limit interleaving granularity for formats that support chunked output (e.g. MOV/AVI).

### 1.6 Bitstream-filter-in-muxer mechanism

`AVFMT_FLAG_AUTO_BSF` (default-on) makes `av_interleaved_write_frame`/`av_write_frame` consult `FFOutputFormat.check_bitstream(s, st, pkt)` once per stream before the first packet is written; the muxer inspects the stream's codec parameters and, if the incoming bitstream isn't already in the container-required form, attaches the appropriate filter (e.g. `h264_mp4toannexb`↔`h264_mp4toannexb` conversions, `aac_adtstoasc`, `extract_extradata`) to the stream's internal `FFStream.bsfc` chain, transparently rewriting every subsequent packet for that stream. This is distinct from user-specified `-bsf:v` chains, which are applied by the caller (or `fftools/ffmpeg`) before packets ever reach the muxer.

### 1.7 AVIO layer

- **`AVIOContext`** (`avio.h`) — buffered I/O abstraction sitting on top of a `URLContext` or user callbacks: `buffer`/`buffer_size`/`buf_ptr`/`buf_end`, `opaque`, `read_packet`/`write_packet`/`seek` function pointers (legacy signatures; superseded by `read_packet2`-style internal variants), `pos`, `eof_reached`, `error`, `write_flag`, `max_packet_size`/`min_packet_size`, `checksum`/`checksum_ptr` (running checksum over data read), `read_pause`/`read_seek` (for live/broadcast sources supporting pause/resume and time-based seek), `seekable` (`AVIO_SEEKABLE_NORMAL` = byte seek, `AVIO_SEEKABLE_TIME` = time-based seek, e.g. RTSP), `direct` (mirrors `AVIO_FLAG_DIRECT`), `protocol_whitelist`/`protocol_blacklist`, `write_data_type` (typed-write variant distinguishing header/trailer/sync-point/unknown data via `AVIODataMarkerType`), `ignore_boundary_point`, `bytes_read`/`bytes_written` (statistics).
- **Construction**: `avio_open`/`avio_open2` (URL-backed, via `ffio_open_whitelist`→`URLProtocol`), `avio_alloc_context` (fully custom read/write/seek callbacks — used for in-memory or embedded-stream I/O), `avio_context_free`.
- **Dynamic buffers**: `avio_open_dyn_buf`/`avio_close_dyn_buf`/`avio_get_dyn_buf` create a growable in-memory `AVIOContext` sink (no backing URL) used internally by muxers that must know a box/element's size before emitting it (MOV `moov`, Matroska master elements, NUT sync points) and by two-pass "write to memory, measure, then flush" patterns.
- **Direct/custom IO**: `AVFMT_FLAG_CUSTOM_IO` tells the format layer the caller supplied `AVFormatContext.pb` itself and must not have it closed automatically. `AVIO_FLAG_DIRECT` requests minimal internal buffering (passthrough to the OS/protocol layer).
- **Seekability query**: `avio_seek`, `avio_size`, and the `AVSEEK_SIZE` whence trick for user callbacks; `avio_feof`.

### 1.8 URLProtocol layer

- **`URLContext`** (`url.h`) — per-open-connection state: `prot` (`URLProtocol*`), `priv_data`, `filename`, `flags` (`AVIO_FLAG_*`), `max_packet_size`/`min_packet_size`, `is_streamed`, `is_connected`, `interrupt_callback`, `rw_timeout`, `protocol_whitelist`/`protocol_blacklist`, `avfc` (owning `AVFormatContext`, or NULL for standalone `avio_open` use), `prefer_libcurl`.
- **`URLProtocol`** — one entry per scheme: `name`; `url_open`/`url_open2` (the latter dictionary-options-aware, used by protocols that themselves open nested protocols, e.g. `crypto`, `tls`, `concat`); `url_accept`/`url_handshake` (server-mode); `url_read`/`url_write`/`url_seek`/`url_close`; `url_read_pause`/`url_read_seek` (live-source pause/seek, mirrors `AVIOContext`); `url_get_file_handle`/`url_get_multi_file_handle` (expose native fd(s), e.g. for `select()`-based multiplexing); `url_get_short_seek`; `url_shutdown` (half-close, e.g. TCP `SHUT_WR`); `priv_data_class`/`priv_data_size` (AVOption-driven private context); `flags` (`URL_PROTOCOL_FLAG_NESTED_SCHEME`, `URL_PROTOCOL_FLAG_NETWORK`); `url_check`; `url_open_dir`/`url_read_dir`/`url_close_dir` (directory listing, e.g. `ftp`, `smb`); `url_delete`/`url_move`; `default_whitelist` (nested-protocol whitelist a protocol implicitly grants, e.g. `hls` → `http,https,tls,tcp,file,crypto`).
- **Whitelisting**: `ffurl_open_whitelist()` is the gate all nested opens pass through; `protocol_whitelist`/`protocol_blacklist` (per-`AVFormatContext` or per-`URLContext`) restrict which schemes may be transitively opened — the mechanism that stops a malicious playlist/manifest from pivoting into an unintended protocol (e.g. `file://` from a remote `concat` list).


### 1.9 Format-level flags — `AVFMT_*` (on `AVInputFormat.flags` / `AVOutputFormat.flags`)

| Flag | Value | Applies to | Meaning |
|---|---|---|---|
| `AVFMT_NOFILE` | 0x0001 | both | Format doesn't need/use an `AVIOContext`/URL (e.g. device formats managing their own I/O). |
| `AVFMT_NEEDNUMBER` | 0x0002 | both | Needs a `%d` placeholder in the filename (image sequences). |
| `AVFMT_EXPERIMENTAL` | 0x0004 | both | Format is experimental. |
| `AVFMT_SHOW_IDS` | 0x0008 | demux | Show numeric stream IDs in default dump output. |
| `AVFMT_GLOBALHEADER` | 0x0040 | mux | Format wants codec extradata as a single global header rather than in-band. |
| `AVFMT_NOTIMESTAMPS` | 0x0080 | both | Format has no timestamps at all. |
| `AVFMT_GENERIC_INDEX` | 0x0100 | demux | Use the generic index-building code path. |
| `AVFMT_TS_DISCONT` | 0x0200 | demux | Format allows timestamp discontinuities (muxing still requires monotone timestamps). |
| `AVFMT_VARIABLE_FPS` | 0x0400 | mux | Format allows variable frame rate. |
| `AVFMT_NODIMENSIONS` | 0x0800 | mux | Format doesn't need width/height. |
| `AVFMT_NOSTREAMS` | 0x1000 | mux | Format doesn't require any streams (e.g. metadata-only). |
| `AVFMT_NOBINSEARCH` | 0x2000 | demux | No binary-search seek fallback via `read_timestamp`. |
| `AVFMT_NOGENSEARCH` | 0x4000 | demux | No generic-index seek fallback. |
| `AVFMT_NO_BYTE_SEEK` | 0x8000 | demux | No byte-offset seeking. |
| `AVFMT_TS_NONSTRICT` | 0x20000 | mux | Format doesn't require strictly increasing timestamps, just non-decreasing. |
| `AVFMT_TS_NEGATIVE` | 0x40000 | mux | Format supports muxing negative timestamps. |
| `AVFMT_FIXED_FRAMESIZE` | 0x80000 | mux | Format wants fixed-size audio frames (`AVCodecParameters.frame_size`). |
| `AVFMT_SEEK_TO_PTS` | 0x4000000 | demux | Seeking is PTS-based. |

### 1.10 Context-level runtime flags — `AVFMT_FLAG_*` (`AVFormatContext.flags`, set via the `fflags` AVOption)

| Flag | Value | Meaning |
|---|---|---|
| `GENPTS` | 0x0001 | Generate missing PTS even if it requires parsing ahead. |
| `IGNIDX` | 0x0002 | Ignore the container's index. |
| `NONBLOCK` | 0x0004 | Don't block reading packets from input. |
| `IGNDTS` | 0x0008 | Ignore DTS on packets that carry both DTS and PTS. |
| `NOFILLIN` | 0x0010 | Don't infer values from other values — only return what the container stored. |
| `NOPARSE` | 0x0020 | Disable `AVParser` use (requires `NOFILLIN` too — frame-boundary detection depends on parsing). |
| `NOBUFFER` | 0x0040 | Reduce latency from optional internal buffering. |
| `CUSTOM_IO` | 0x0080 | Caller supplied `AVIOContext`; don't `avio_close()` it automatically. |
| `DISCARD_CORRUPT` | 0x0100 | Discard packets marked corrupted. |
| `FLUSH_PACKETS` | 0x0200 | Flush the `AVIOContext` after every packet (muxing). |
| `BITEXACT` | 0x0400 | Don't write random/volatile data (muxer ID strings, timestamps) — used for regression-test reproducibility. |
| `SORT_DTS` | 0x10000 | Try to interleave output packets by DTS (demuxing; slower). |
| `FAST_SEEK` | 0x80000 | Enable fast-but-inaccurate seeks for formats that support it. |
| `AUTO_BSF` | 0x200000 | Add muxer-required bitstream filters automatically (see §1.6); default on. |
| `LEGACY_ID3V2_COMM_KEYS` | 0x400000 | Deprecated: also export ID3v2 `COMM` frames as bare metadata keys. |

### 1.11 Other flag families

- **`AVIO_FLAG_*`** (`avio.h`): `READ` (1), `WRITE` (2), `READ_WRITE` (3), `NONBLOCK` (8), `DIRECT` (0x8000, minimize internal buffering).
- **`AVIO_SEEKABLE_*`**: `NORMAL` (1<<0, byte seek), `TIME` (1<<1, time-based seek).
- **`URL_PROTOCOL_FLAG_*`** (`url.h`): `NESTED_SCHEME` (1, name can prefix a nested scheme), `NETWORK` (2, protocol uses the network — governs `--enable-network` gating).
- **Internal capability flags** — `FFInputFormat.flags_internal` (`demux.h`): `FF_INFMT_FLAG_INIT_CLEANUP` (1<<0, `read_close` must run after a failed `read_header`), `FF_INFMT_FLAG_PREFER_CODEC_FRAMERATE` (1<<1), `FF_INFMT_FLAG_ID3V2_AUTO` (1<<2, auto-parse leading ID3v2). `FFOutputFormat.flags_internal` (`mux.h`): `FF_OFMT_FLAG_ALLOW_FLUSH` (1<<1, `write_packet` accepts a NULL packet to flush internally buffered data), `FF_OFMT_FLAG_MAX_ONE_OF_EACH` (1<<2, at most one stream per default-codec media type), `FF_OFMT_FLAG_ONLY_DEFAULT_CODECS` (1<<3, only the format's declared default codec IDs are accepted).
- **`AV_DISPOSITION_*`** (stream role/purpose, `AVStream.disposition`): `DEFAULT`, `DUB`, `ORIGINAL`, `COMMENT`, `LYRICS`, `KARAOKE`, `FORCED`, `HEARING_IMPAIRED`, `VISUAL_IMPAIRED`, `CLEAN_EFFECTS`, `ATTACHED_PIC`, `TIMED_THUMBNAILS`, `NON_DIEGETIC`, `CAPTIONS`, `DESCRIPTIONS`, `METADATA`, `DEPENDENT`, `STILL_IMAGE`, `MULTILAYER`.
- **`AVSTREAM_PARSE_*`** (`AVStreamParseType`, demux-internal parser mode): `NONE`, `FULL` (parse + repack), `HEADERS` (headers only), `TIMESTAMPS` (full parse + timestamp interpolation for frames not starting on packet boundaries), `FULL_ONCE`, `FULL_RAW` (raw elementary streams, no demux-level headers).
- **`AVFMTCTX_*`** (`AVFormatContext.ctx_flags`): `NOHEADER` (0x0001, streams added dynamically, no header parsed up front), `UNSEEKABLE` (0x0002, definitely not seekable — some network formats, e.g. HLS, can flip this at runtime).
- **`AVFMT_AVOID_NEG_TS_*`** (`avoid_negative_ts` option values): `AUTO` (−1, enabled when target format requires it), `DISABLED` (0), `MAKE_NON_NEGATIVE` (1), `MAKE_ZERO` (2).
- **`AVFMT_EVENT_FLAG_METADATA_UPDATED`** / **`AVSTREAM_EVENT_FLAG_METADATA_UPDATED`** / **`AVSTREAM_EVENT_FLAG_NEW_PACKETS`** — change-notification flags on `AVFormatContext.event_flags` / `AVStream.event_flags`.
- **`AVPROBE_SCORE_*`**, **`AVPROBE_PADDING_SIZE`** (32) — see §1.2.
- **`AVSEEK_FLAG_*`**, **`AVSEEK_SIZE`**, **`AVSEEK_FORCE`** — see §1.4.
- **`AVFMT_PROGCOPY_*`** (`avformat_program_copy` behavior): `MATCH_BY_ID` (1<<0), `MATCH_BY_INDEX` (1<<1), `OVERWRITE` (1<<8).

### 1.12 Generic `AVFormatContext` AVOptions (`avformat_options[]`, `options_table.h`)

All flagged `D` (decoding), `E` (encoding), or both. Unit-grouped constants are nested under their parent flag/enum option.

| Option | Type | Default | Flags | Semantics |
|---|---|---|---|---|
| `avioflags` (unit `avioflags`) | FLAGS | 0 | D\|E | Const `direct` → `AVIO_FLAG_DIRECT` (reduce buffering). |
| `probesize` | INT64 | 5,000,000 | D | Max bytes read while probing format/stream info. |
| `formatprobesize` | INT | `PROBE_BUF_MAX` | D | Max bytes read purely to identify the container format (before explicit format is trusted). |
| `packetsize` | INT | 0 | E | Set fixed packet size. |
| `fflags` (unit `fflags`) | FLAGS | `AUTO_BSF` | D\|E | See §1.10 for every const value (`flush_packets`, `ignidx`, `genpts`, `nofillin`, `noparse`, `igndts`, `discardcorrupt`, `sortdts`, `fastseek`, `nobuffer`, `bitexact`, `autobsf`, deprecated `legacy_id3v2_comm_keys`). |
| `seek2any` | BOOL | 0 | D | Allow seeking to non-keyframes at the demuxer level when supported. |
| `analyzeduration` | INT64 | 0 (→ internal default) | D | Microseconds of stream analyzed during probing. |
| `cryptokey` | BINARY | — | D | Decryption key (raw bytes). |
| `indexmem` | INT | 1<<20 | D | Max bytes of per-stream timestamp index memory. |
| `rtbufsize` | INT | 3,041,280 | D | Max bytes buffered for realtime-capture frames (≈1s of 15fps 352×288 YUYV422). |
| `fdebug` (unit `fdebug`) | FLAGS | 0 | D\|E | Consts `ts` → `AV_FDEBUG_TS`, `id3v2` → `AV_FDEBUG_ID3V2`. |
| `max_delay` | INT | −1 | D\|E | Max muxing/demuxing delay, µs. |
| `start_time_realtime` | INT64 | `AV_NOPTS_VALUE` | E | Wall-clock time corresponding to PTS 0. |
| `fpsprobesize` | INT | −1 | D | Number of frames used to probe FPS. |
| `audio_preload` | INT | 0 | E | µs by which audio packets are interleaved earlier. |
| `chunk_duration` | INT | 0 | E | Max chunk duration, µs. |
| `chunk_size` | INT | 0 | E | Max chunk size, bytes. |
| `f_err_detect` / `err_detect` (unit `err_detect`) | FLAGS | `AV_EF_CRCCHECK` | D | Consts `crccheck`, `bitstream`, `buffer`, `explode`, `ignore_err`, `careful`, `compliant`, `aggressive`. `f_err_detect` is the deprecated alias. |
| `use_wallclock_as_timestamps` | BOOL | 0 | D | Use local wall-clock as packet timestamps. |
| `skip_initial_bytes` | INT64 | 0 | D | Bytes to skip before reading header/frames. |
| `correct_ts_overflow` | BOOL | 1 | D | Correct single timestamp-overflow events. |
| `flush_packets` | INT | −1 | E | Flush `AVIOContext` after each packet. |
| `metadata_header_padding` | INT | −1 | E | Padding bytes reserved in written metadata header. |
| `output_ts_offset` | DURATION | 0 | E | Output timestamp offset. |
| `max_interleave_delta` | INT64 | 10,000,000 | E | Max interleaving buffering duration, µs. |
| `f_strict` / `strict` (unit `strict`) | INT | 0 | D\|E | Consts `very`, `strict`, `normal`, `unofficial`, `experimental` (`FF_COMPLIANCE_*`). `f_strict` is the deprecated alias. |
| `max_ts_probe` | INT | 50 | D | Max packets read while waiting for the first timestamp. |
| `avoid_negative_ts` (unit `avoid_negative_ts`) | INT | −1 | E | Consts `auto`, `disabled`, `make_non_negative`, `make_zero`. |
| `dump_separator` | STRING | `", "` | D\|E | Field separator used by `-dump`-style info output. |
| `codec_whitelist` | STRING | NULL | D | Comma-separated allowed decoders. |
| `format_whitelist` | STRING | NULL | D | Comma-separated allowed demuxers. |
| `protocol_whitelist` | STRING | NULL | D | Comma-separated allowed protocols. |
| `protocol_blacklist` | STRING | NULL | D | Comma-separated disallowed protocols. |
| `max_streams` | INT | 1000 | D | Max number of streams accepted. |
| `skip_estimate_duration_from_pts` | BOOL | 0 | D | Skip PTS-based duration estimation pass. |
| `max_probe_packets` | INT | 2500 | D | Max packets probed per codec. |
| `duration_probesize` | INT64 | 0 | D | Max bytes probed for duration estimation from PTS. |
| `recursion_limit` | INT | 10 | D | Max recursive demuxer-reopen depth (nested playlists/concat). |


---

## 2. DEMUXER INVENTORY

**Total registered demuxers: 368** (`libavformat/allformats.c`). Grouped below; extensions/MIME types are read directly from each `AVInputFormat`/`FFInputFormat` struct literal (or, for macro-generated families, from the macro invocation site) — `–` means the demuxer declares no `extensions` field (selected by content probing and/or explicit `-f`).

### 2.1 General-purpose containers

mp4/mov, matroska/webm, mpegts, mpegps, avi, asf, flv, ogg, rtp/rtsp/sdp, hls, dash, and related session/stream formats. (23 demuxers)

| Name | Source file | Extensions | Spec / status |
|---|---|---|---|
| `asf` | `asfdec_f.c` | – | ASF spec (Microsoft, de facto) |
| `asf_o` | `asfdec_o.c` | – | ASF spec (Microsoft, de facto) |
| `avi` | `avidec.c` | avi | RIFF/AVI (Microsoft, de facto) |
| `dash` | `dashdec.c` | – | ISO/IEC 23009-1 (MPEG-DASH) |
| `flv` | `flvdec.c` | flv | Adobe FLV File Format Spec |
| `hls` | `hls.c` | – | RFC 8216 (HLS) |
| `ivr` | `rmdec.c` | ivr | reverse-engineered (RealMedia/Internet Video Recording) |
| `live_flv` | `flvdec.c` | flv | Adobe FLV File Format Spec |
| `matroska` | `matroskadec.c` | mkv,mk3d,mka,mks,webm | Matroska/WebM spec (matroska.org, EBML-based) |
| `mov` | `mov.c` | mov,mp4,m4a,3gp,3g2,mj2,psp,m4v,m4b,ism,ismv,isma,f4v,avif,heic,heif | ISO/IEC 14496-12/14 (MP4/QuickTime File Format) |
| `mpegps` | `mpeg.c` | – | ISO/IEC 13818-1 / ISO/IEC 11172-1 (MPEG-1/2 PS) |
| `mpegts` | `mpegts.c` | – | ISO/IEC 13818-1 (MPEG-2 TS) |
| `mpegtsraw` | `mpegts.c` | – | ISO/IEC 13818-1 (MPEG-2 TS) |
| `mpjpeg` | `mpjpegdec.c` | mjpg | RFC 2046 multipart/x-mixed-replace (MJPEG-over-HTTP, de facto) |
| `nut` | `nutdec.c` | nut | NUT open container spec (Ffmpeg/NUT project) |
| `ogg` | `oggdec.c` | ogg | RFC 3533 (Ogg), Xiph.org |
| `rm` | `rmdec.c` | – | reverse-engineered (RealMedia, proprietary) |
| `rtp` | `rtsp.c` | – | RFC 3550 (RTP) |
| `rtsp` | `rtspdec.c` | – | RFC 2326 / RFC 7826 (RTSP 1.0/2.0) |
| `sap` | `sapdec.c` | – | reverse-engineered (Session Announcement-like ad hoc; not RFC 2974 SAP) |
| `sdp` | `rtsp.c` | – | RFC 8866 (SDP) |
| `swf` | `swfdec.c` | – | Adobe SWF File Format Spec |
| `webm_dash_manifest` | `matroskadec.c` | – | ISO/IEC 23009-1 (MPEG-DASH) |

### 2.2 Broadcast / professional formats

Studio and broadcast interchange containers. (7 demuxers)

| Name | Source file | Extensions | Spec / status |
|---|---|---|---|
| `dv` | `dv.c` | dv,dif | SMPTE 314M / IEC 61834 (DV) |
| `dvdvideo` | `dvdvideodec.c` | – | DVD-Video (proprietary, via libdvdread/libdvdnav) |
| `gxf` | `gxf.c` | – | SMPTE 360M (GXF) |
| `imf` | `imfdec.c` | – | SMPTE ST 2067 (IMF) |
| `lxf` | `lxfdec.c` | – | reverse-engineered (Leitch/Harmonic LXF) |
| `mxf` | `mxfdec.c` | – | SMPTE 377M (MXF) |
| `wtv` | `wtvdec.c` | – | reverse-engineered (Microsoft Windows TV Recording) |

### 2.3 Audio containers / codecs-as-container

Dedicated audio file formats and self-delimiting compressed-audio elementary streams treated as demuxable containers. (91 demuxers)

| Name | Source file | Extensions | Spec / status |
|---|---|---|---|
| `aac` | `aacdec.c` | aac | ISO/IEC 13818-7 / 14496-3 (ADTS/LOAS) |
| `ac3` | `ac3dec.c` | ac3 | ATSC A/52 (AC-3) |
| `act` | `act.c` | – | reverse-engineered |
| `aea` | `aeadec.c` | aea | reverse-engineered |
| `aiff` | `aiffdec.c` | – | EA IFF 85 / AIFF (Apple) |
| `aix` | `aixdec.c` | aix | reverse-engineered |
| `amr` | `amr.c` | – | 3GPP TS 26.101 (AMR) |
| `amrnb` | `amr.c` | – | 3GPP TS 26.101 (AMR-NB) |
| `amrwb` | `amr.c` | – | 3GPP TS 26.204 (AMR-WB) |
| `apac` | `apac.c` | apc | reverse-engineered |
| `ape` | `ape.c` | ape,apl,mac | reverse-engineered (Monkey's Audio, proprietary) |
| `apm` | `apm.c` | – | reverse-engineered |
| `aptx` | `aptxdec.c` | aptx | reverse-engineered (Qualcomm aptX, proprietary) |
| `aptx_hd` | `aptxdec.c` | aptxhd | reverse-engineered (Qualcomm aptX HD, proprietary) |
| `ast` | `astdec.c` | ast | reverse-engineered |
| `au` | `au.c` | – | Sun/NeXT .au (de facto) |
| `avr` | `avr.c` | avr | reverse-engineered |
| `bfstm` | `brstm.c` | bfstm,bcstm | reverse-engineered |
| `binka` | `binka.c` | binka | reverse-engineered |
| `bonk` | `bonk.c` | bonk | reverse-engineered |
| `brstm` | `brstm.c` | brstm | reverse-engineered |
| `caf` | `cafdec.c` | – | Apple Core Audio Format spec |
| `codec2` | `codec2.c` | c2 | reverse-engineered (Codec 2 project) |
| `codec2raw` | `codec2.c` | – | reverse-engineered (Codec 2 project) |
| `dfpwm` | `dfpwmdec.c` | dfpwm | reverse-engineered |
| `dsf` | `dsfdec.c` | – | DSF (Sony/Philips DSD Stream File, de facto) |
| `dss` | `dss.c` | dss | reverse-engineered (Olympus DSS dictation) |
| `dts` | `dtsdec.c` | dts | ETSI TS 102 114 (DTS Coherent Acoustics, partial public spec) |
| `dtshd` | `dtshddec.c` | dtshd | ETSI TS 102 114 (DTS-HD, partial public spec) |
| `eac3` | `ac3dec.c` | eac3,ec3 | ATSC A/52 (E-AC-3) |
| `epaf` | `epafdec.c` | paf,fap | reverse-engineered |
| `flac` | `flacdec.c` | flac | IETF RFC 9639 (FLAC), Xiph.org |
| `fsb` | `fsb.c` | fsb | reverse-engineered |
| `fwse` | `fwse.c` | fwse | reverse-engineered |
| `g722` | `g722.c` | g722,722 | ITU-T G.722 |
| `g723_1` | `g723_1.c` | tco,rco,g723_1 | ITU-T G.723.1 |
| `g726` | `g726.c` | – | ITU-T G.726 |
| `g726le` | `g726.c` | – | ITU-T G.726 |
| `g728` | `g728dec.c` | g728 | ITU-T G.728 |
| `g729` | `g729dec.c` | g729 | ITU-T G.729 |
| `genh` | `genh.c` | genh | reverse-engineered |
| `gsm` | `gsmdec.c` | gsm | ETSI GSM 06.10 |
| `hca` | `hca.c` | hca | reverse-engineered |
| `iamf` | `iamfdec.c` | iamf | IAMF (AOM Immersive Audio Model and Formats) spec |
| `ilbc` | `ilbc.c` | – | RFC 3952 (iLBC) |
| `ircam` | `ircamdec.c` | sf,ircam | Berkeley/IRCAM soundfile spec (de facto) |
| `kvag` | `kvag.c` | – | reverse-engineered |
| `lc3` | `lc3.c` | lc3 | Bluetooth SIG LC3 (LE Audio) spec |
| `luodat` | `luodatdec.c` | dat | reverse-engineered |
| `mca` | `mca.c` | mca | reverse-engineered |
| `mlp` | `mlpdec.c` | mlp | reverse-engineered (MLP, Dolby proprietary) |
| `mmf` | `mmf.c` | – | reverse-engineered |
| `mp3` | `mp3dec.c` | mp2,mp3,m2a,mpa | reverse-engineered |
| `mpc` | `mpc.c` | mpc | reverse-engineered (Musepack) |
| `mpc8` | `mpc8.c` | – | reverse-engineered (Musepack SV8) |
| `msf` | `msf.c` | msf | reverse-engineered |
| `mtaf` | `mtaf.c` | mtaf | reverse-engineered |
| `musx` | `musx.c` | musx | reverse-engineered |
| `nistsphere` | `nistspheredec.c` | nist,sph | reverse-engineered |
| `oma` | `omadec.c` | oma,omg,aa3 | reverse-engineered (Sony OpenMG/ATRAC3) |
| `osq` | `osq.c` | osq | reverse-engineered |
| `pvf` | `pvfdec.c` | pvf | reverse-engineered |
| `qcp` | `qcp.c` | – | 3GPP2 / QCP (Qualcomm PureVoice, de facto) |
| `qoa` | `qoadec.c` | qoa | Quite OK Audio open spec |
| `rka` | `rka.c` | rka | reverse-engineered |
| `rsd` | `rsd.c` | rsd | reverse-engineered |
| `rso` | `rsodec.c` | rso | reverse-engineered (RSO) |
| `sbc` | `sbcdec.c` | sbc,msbc | Bluetooth SIG SBC (A2DP) spec |
| `sds` | `sdsdec.c` | sds | reverse-engineered |
| `sdx` | `sdxdec.c` | sdx | reverse-engineered |
| `shorten` | `shortendec.c` | shn | reverse-engineered (Shorten) |
| `sln` | `pcmdec.c` | sln | reverse-engineered |
| `sox` | `soxdec.c` | – | SoX native format (de facto) |
| `spdif` | `spdifdec.c` | – | IEC 61937 |
| `svag` | `svag.c` | svag | reverse-engineered |
| `svs` | `svs.c` | svs | reverse-engineered |
| `tak` | `takdec.c` | tak | reverse-engineered (TAK, proprietary) |
| `truehd` | `mlpdec.c` | thd | reverse-engineered (Dolby TrueHD, proprietary) |
| `tta` | `tta.c` | tta | True Audio (TTA) open spec |
| `vag` | `vag.c` | vag | reverse-engineered |
| `voc` | `vocdec.c` | – | Creative Voice File (de facto) |
| `vqf` | `vqf.c` | vqf,vql,vqe | reverse-engineered |
| `w64` | `wavdec.c` | – | Sony Wave64 spec |
| `wady` | `wady.c` | way | reverse-engineered |
| `wav` | `wavdec.c` | – | RIFF/WAVE (de facto, ITU/EBU profiles) |
| `wavarc` | `wavarc.c` | wa | reverse-engineered |
| `wsaud` | `westwood_aud.c` | – | reverse-engineered |
| `wv` | `wvdec.c` | – | WavPack open spec (wavpack.com) |
| `xmd` | `xmd.c` | xmd | reverse-engineered |
| `xvag` | `xvag.c` | xvag | reverse-engineered |
| `xwma` | `xwma.c` | – | reverse-engineered |

### 2.4 Image / image2 sequence handling

Single-image and animated-image formats, plus the generic numbered-sequence (`image2`) and per-codec auto pipe demuxers (`-f <codec>_pipe`, selected via `IMAGEAUTO_DEMUXER` registration when the corresponding image decoder is enabled). (46 demuxers)

| Name | Source file | Extensions | Spec / status |
|---|---|---|---|
| `apng` | `apngdec.c` | – | APNG spec (Mozilla/community) |
| `gif` | `gifdec.c` | gif | GIF89a (CompuServe) |
| `ico` | `icodec.c` | – | Microsoft ICO/CUR format (de facto) |
| `image2` | `img2dec.c` | – | FFmpeg-specific image-sequence pseudo-format |
| `image2_alias_pix` | `img2_alias_pix.c` | – | reverse-engineered |
| `image2_brender_pix` | `img2_brender_pix.c` | – | reverse-engineered |
| `image2pipe` | `img2dec.c` | – | FFmpeg-specific image-sequence pseudo-format |
| `image_bmp_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_cri_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_dds_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_dpx_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_exr_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_gem_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_gif_pipe` | `img2dec.c` | – | GIF89a (CompuServe) |
| `image_hdr_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_j2k_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_jpeg_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_jpegls_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_jpegxl_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_jpegxs_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_pam_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_pbm_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_pcx_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_pfm_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_pgm_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_pgmyuv_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_pgx_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_phm_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_photocd_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_pictor_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_png_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_ppm_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_psd_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_qdraw_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_qoi_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_sgi_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_sunrast_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_svg_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_tiff_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_vbn_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_webp_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_xbm_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_xpm_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `image_xwd_pipe` | `img2dec.c` | – | see corresponding image codec spec |
| `jpegxl_anim` | `jpegxl_anim_dec.c` | jxl | ISO/IEC 18181 (JPEG XL) |
| `webp_anim` | `webp_anim_dec.c` | – | WebP spec (Google) |

### 2.5 Subtitle formats

Every registered subtitle demuxer. (23 demuxers)

| Name | Source file | Extensions | Spec / status |
|---|---|---|---|
| `aqtitle` | `aqtitledec.c` | aqt | no formal spec (AQTitle, de facto) |
| `ass` | `assdec.c` | – | no formal spec (SSA/ASS, community-documented via Aegisub/VSFilter) |
| `dvbsub` | `dvbsub.c` | – | ETSI EN 300 743 (DVB Subtitling) |
| `dvbtxt` | `dvbtxt.c` | – | ETSI EN 300 472 / EN 300 706 (DVB Teletext) |
| `jacosub` | `jacosubdec.c` | – | no formal spec (JACOsub, community-documented) |
| `lrc` | `lrcdec.c` | – | no formal spec (LRC lyrics, de facto) |
| `mcc` | `mccdec.c` | mcc | reverse-engineered (MacCaption MCC) |
| `microdvd` | `microdvddec.c` | – | no formal spec (MicroDVD, de facto) |
| `mpl2` | `mpl2dec.c` | txt,mpl2 | no formal spec (MPL2, de facto) |
| `mpsub` | `mpsubdec.c` | sub | no formal spec (MPlayer subtitle format) |
| `pjs` | `pjsdec.c` | pjs | no formal spec (Phoenix Japanimation Society subtitles) |
| `realtext` | `realtextdec.c` | rt | RealText spec (RealNetworks) |
| `sami` | `samidec.c` | smi,sami | Microsoft SAMI spec |
| `scc` | `sccdec.c` | scc | CEA-608 (Scenarist Closed Caption) |
| `srt` | `srtdec.c` | – | no formal spec (SubRip, de facto/community) |
| `stl` | `stldec.c` | stl | EBU Tech 3264 (EBU STL) |
| `subviewer` | `subviewerdec.c` | sub | no formal spec (SubViewer, de facto) |
| `subviewer1` | `subviewer1dec.c` | sub | no formal spec (SubViewer, de facto) |
| `sup` | `supdec.c` | sup | reverse-engineered (Blu-ray/HDMV PGS) |
| `tedcaptions` | `tedcaptionsdec.c` | – | reverse-engineered (TED closed-caption XML) |
| `vobsub` | `mpeg.c` | idx | reverse-engineered (DVD VobSub/SPU) |
| `vplayer` | `vplayerdec.c` | txt | no formal spec (VPlayer subtitles) |
| `webvtt` | `webvttdec.c` | vtt,webvtt | W3C WebVTT spec |

### 2.6 Game / FMV / legacy formats (long tail)

Proprietary game engine, adaptive/interactive-movie, and legacy multimedia formats — almost all reverse-engineered from samples, no public spec. (123 demuxers)

| Name | Source file | Extensions | Spec / status |
|---|---|---|---|
| `aa` | `aadec.c` | aa | reverse-engineered |
| `aax` | `aaxdec.c` | aax | reverse-engineered |
| `ac4` | `ac4dec.c` | ac4 | ETSI TS 103 190 (Dolby AC-4) |
| `ace` | `acedec.c` | – | reverse-engineered |
| `acm` | `acm.c` | acm | reverse-engineered |
| `adf` | `bintext.c` | adf | reverse-engineered |
| `adp` | `adp.c` | adp,dtk | reverse-engineered |
| `ads` | `ads.c` | ads,ss2 | reverse-engineered |
| `adx` | `adxdec.c` | adx | reverse-engineered |
| `afc` | `afc.c` | afc | reverse-engineered |
| `alp` | `alp.c` | – | reverse-engineered |
| `anm` | `anm.c` | – | reverse-engineered |
| `apc` | `apc.c` | – | reverse-engineered |
| `apv` | `apvdec.c` | apv | ISO/IEC 23001-25 (APV, Advanced Professional Video) |
| `argo_asf` | `argo_asf.c` | – | reverse-engineered |
| `argo_brp` | `argo_brp.c` | – | reverse-engineered |
| `argo_cvg` | `argo_cvg.c` | – | reverse-engineered |
| `avs` | `avs.c` | – | reverse-engineered |
| `bethsoftvid` | `bethsoftvid.c` | – | reverse-engineered |
| `bfi` | `bfi.c` | – | reverse-engineered |
| `bink` | `bink.c` | – | reverse-engineered |
| `bintext` | `bintext.c` | – | reverse-engineered |
| `bmv` | `bmv.c` | bmv | reverse-engineered |
| `boa` | `boadec.c` | – | reverse-engineered |
| `c93` | `c93.c` | – | reverse-engineered |
| `cdg` | `cdg.c` | cdg | reverse-engineered |
| `cdxl` | `cdxl.c` | cdxl,xl | reverse-engineered |
| `cine` | `cinedec.c` | – | reverse-engineered |
| `daud` | `dauddec.c` | 302,daud | reverse-engineered |
| `dcstr` | `dcstr.c` | str | reverse-engineered |
| `derf` | `derf.c` | adp | reverse-engineered |
| `dfa` | `dfa.c` | – | reverse-engineered |
| `dhav` | `dhav.c` | dav | reverse-engineered |
| `dsicin` | `dsicin.c` | – | reverse-engineered |
| `dxa` | `dxa.c` | – | reverse-engineered |
| `ea` | `electronicarts.c` | – | reverse-engineered |
| `ea_cdata` | `eacdata.c` | cdata | reverse-engineered |
| `filmstrip` | `filmstripdec.c` | flm | reverse-engineered |
| `fits` | `fitsdec.c` | – | reverse-engineered |
| `flic` | `flic.c` | – | reverse-engineered |
| `fourxm` | `4xm.c` | – | reverse-engineered |
| `frm` | `frmdec.c` | – | reverse-engineered |
| `gdv` | `gdv.c` | – | reverse-engineered |
| `hcom` | `hcom.c` | – | reverse-engineered |
| `hnm` | `hnm.c` | – | reverse-engineered |
| `hxvs` | `hxvs.c` | 264,265 | reverse-engineered |
| `idcin` | `idcin.c` | – | reverse-engineered |
| `idf` | `bintext.c` | idf | reverse-engineered |
| `iff` | `iff.c` | – | reverse-engineered |
| `ifv` | `ifv.c` | ifv | reverse-engineered |
| `ingenient` | `ingenientdec.c` | cgi | reverse-engineered |
| `ipmovie` | `ipmovie.c` | – | reverse-engineered |
| `ipu` | `ipudec.c` | ipu | reverse-engineered |
| `iss` | `iss.c` | – | reverse-engineered |
| `iv8` | `iv8.c` | – | reverse-engineered |
| `ivf` | `ivfdec.c` | – | reverse-engineered |
| `jv` | `jvdec.c` | – | reverse-engineered |
| `kux` | `flvdec.c` | kux | reverse-engineered |
| `laf` | `lafdec.c` | laf | reverse-engineered |
| `lmlm4` | `lmlm4.c` | – | reverse-engineered |
| `lvf` | `lvfdec.c` | lvf | reverse-engineered |
| `mgsts` | `mgsts.c` | – | reverse-engineered |
| `mlv` | `mlvdec.c` | – | reverse-engineered |
| `mm` | `mm.c` | – | reverse-engineered |
| `mods` | `mods.c` | mods | reverse-engineered |
| `moflex` | `moflex.c` | moflex | reverse-engineered |
| `msnwc_tcp` | `msnwc_tcp.c` | – | reverse-engineered |
| `msp` | `mspdec.c` | – | reverse-engineered |
| `mtv` | `mtv.c` | – | reverse-engineered |
| `mv` | `mvdec.c` | – | reverse-engineered |
| `mvi` | `mvi.c` | mvi | reverse-engineered |
| `mvr` | `mvrdec.c` | mvr | reverse-engineered |
| `mxg` | `mxg.c` | mxg | reverse-engineered |
| `nc` | `ncdec.c` | v | reverse-engineered |
| `nsp` | `nspdec.c` | nsp | reverse-engineered |
| `nsv` | `nsvdec.c` | – | reverse-engineered |
| `nuv` | `nuv.c` | – | reverse-engineered |
| `paf` | `paf.c` | – | reverse-engineered |
| `pdv` | `pdvdec.c` | pdv | reverse-engineered |
| `pmp` | `pmpdec.c` | – | reverse-engineered |
| `pp_bnk` | `pp_bnk.c` | – | reverse-engineered |
| `pva` | `pva.c` | – | reverse-engineered |
| `r3d` | `r3d.c` | – | reverse-engineered |
| `rcwt` | `rcwtdec.c` | – | reverse-engineered |
| `redspark` | `redspark.c` | rsd | reverse-engineered |
| `rl2` | `rl2.c` | – | reverse-engineered |
| `roq` | `idroqdec.c` | – | reverse-engineered |
| `rpl` | `rpl.c` | – | reverse-engineered |
| `sbg` | `sbgdec.c` | sbg | reverse-engineered |
| `scd` | `scd.c` | – | reverse-engineered |
| `sdns` | `sdns.c` | sdns | reverse-engineered |
| `sdr2` | `sdr2.c` | sdr2 | reverse-engineered |
| `segafilm` | `segafilm.c` | – | reverse-engineered |
| `ser` | `serdec.c` | ser | reverse-engineered |
| `sga` | `sga.c` | sga | reverse-engineered |
| `siff` | `siff.c` | vb,son | reverse-engineered |
| `simbiosis_imx` | `imx.c` | imx | reverse-engineered |
| `smacker` | `smacker.c` | – | reverse-engineered |
| `smjpeg` | `smjpegdec.c` | mjpg | reverse-engineered |
| `smush` | `smush.c` | – | reverse-engineered |
| `sol` | `sol.c` | – | reverse-engineered |
| `str` | `psxstr.c` | – | reverse-engineered |
| `thp` | `thp.c` | – | reverse-engineered |
| `threedostr` | `3dostr.c` | str | reverse-engineered |
| `tiertexseq` | `tiertexseq.c` | – | reverse-engineered |
| `tmv` | `tmv.c` | – | reverse-engineered |
| `tty` | `tty.c` | – | reverse-engineered |
| `txd` | `txd.c` | – | reverse-engineered |
| `ty` | `ty.c` | ty,ty+ | reverse-engineered |
| `usm` | `usmdec.c` | usm | reverse-engineered |
| `vc1t` | `vc1test.c` | rcv | reverse-engineered |
| `vividas` | `vividas.c` | – | reverse-engineered |
| `vivo` | `vivo.c` | viv | reverse-engineered |
| `vmd` | `sierravmd.c` | – | reverse-engineered |
| `vpk` | `vpk.c` | vpk | reverse-engineered |
| `wc3` | `wc3movie.c` | – | reverse-engineered |
| `wsd` | `wsddec.c` | wsd | reverse-engineered |
| `wsvqa` | `westwood_vqa.c` | – | reverse-engineered |
| `wve` | `wvedec.c` | – | reverse-engineered |
| `xa` | `xa.c` | – | reverse-engineered |
| `xbin` | `bintext.c` | – | reverse-engineered |
| `xmv` | `xmv.c` | xmv | reverse-engineered |
| `yop` | `yop.c` | yop | reverse-engineered |

### 2.7 Scripting-frontend / external-library "device-ish" demuxers

Demuxers that shell out to an external scripting engine or tracker-module library rather than parsing a container themselves. (5 demuxers)

| Name | Source file | Extensions | Spec / status |
|---|---|---|---|
| `avisynth` | `avisynth.c` | avs | AviSynth/AviSynth+ scripting frontend (external library) |
| `libgme` | `libgme.c` | – | Game Music Emu library formats (external library) |
| `libmodplug` | `libmodplug.c` | – | libmodplug tracker-module formats (external library) |
| `libopenmpt` | `libopenmpt.c` | 669,amf,ams,dbm,digi,dmf,dsm,dtm,far,gdm,ice,imf,it,j2b,m15,mdl,med,mmcmp,mms,mo3,mod,mptm,mt2,mtm,nst,okt,plm,ppm,psm,pt36,ptm,s3m,sfx,sfx2,st26,stk,stm,stp,ult,umx,wow,xm,xpk | libopenmpt tracker-module formats (external library) |
| `vapoursynth` | `vapoursynth.c` | – | VapourSynth scripting frontend (external library) |

### 2.8 Raw / elementary-stream demuxers

Headerless or minimally-framed elementary bitstreams identified by content sniffing (`read_probe`) and/or extension, plus the linear-PCM family generated from a single macro (`PCMDEF`, `pcmdec.c`) and the raw-video family generated from `FF_DEF_RAWVIDEO_DEMUXER` (`rawdec.c` + per-codec `*dec.c`). (48 demuxers)

| Name | Source file | Extensions | Spec / status |
|---|---|---|---|
| `av1` | `av1dec.c` | obu | AV1 low-overhead bitstream format (AOMedia) |
| `avs2` | `avs2dec.c` | avs,avs2 | GY/T 299.1 / IEEE 1857.4 (AVS2) |
| `avs3` | `avs3dec.c` | avs3 | GY/T 358 / IEEE 1857.10 (AVS3) |
| `bit` | `bit.c` | bit | reverse-engineered (G.729/G.723 bit-exact test format) |
| `bitpacked` | `rawvideodec.c` | bitpacked | FFmpeg-specific bit-packed raw pixel format |
| `cavsvideo` | `cavsvideodec.c` | – | GB/T 20090 (Chinese AVS) |
| `data` | `rawdec.c` | – | FFmpeg-specific raw-data passthrough |
| `dirac` | `diracdec.c` | – | SMPTE 2042 (Dirac / VC-2 base) |
| `dnxhd` | `dnxhddec.c` | – | SMPTE VC-3 (DNxHD) |
| `evc` | `evcdec.c` | evc | MPEG-5 EVC (ISO/IEC 23094-1) Annex B |
| `h261` | `h261dec.c` | h261 | ITU-T H.261 |
| `h263` | `h263dec.c` | – | ITU-T H.263 |
| `h264` | `h264dec.c` | h26l,h264,264,avc | ITU-T H.264 / ISO/IEC 14496-10 Annex B |
| `hevc` | `hevcdec.c` | hevc,h265,265 | ITU-T H.265 / ISO/IEC 23008-2 Annex B |
| `loas` | `loasdec.c` | – | ISO/IEC 14496-3 (MPEG-4 LOAS/LATM) |
| `m4v` | `m4vdec.c` | m4v | ISO/IEC 14496-2 (MPEG-4 Part 2 video) |
| `mjpeg` | `rawdec.c` | mjpg,mjpeg,mpo | ITU-T T.81 (JPEG) |
| `mjpeg_2000` | `mj2kdec.c` | j2k | ISO/IEC 15444 (JPEG 2000) |
| `mpegvideo` | `mpegvideodec.c` | – | ISO/IEC 11172-2 / 13818-2 (MPEG-1/2 Video) |
| `obu` | `av1dec.c` | obu | AV1 Open Bitstream Unit spec (AOMedia) |
| `pcm_alaw` | `pcmdec.c` | al | raw linear PCM (no container spec) |
| `pcm_f32be` | `pcmdec.c` | – | raw linear PCM (no container spec) |
| `pcm_f32le` | `pcmdec.c` | – | raw linear PCM (no container spec) |
| `pcm_f64be` | `pcmdec.c` | – | raw linear PCM (no container spec) |
| `pcm_f64le` | `pcmdec.c` | – | raw linear PCM (no container spec) |
| `pcm_mulaw` | `pcmdec.c` | ul | raw linear PCM (no container spec) |
| `pcm_s16be` | `pcmdec.c` | sw (BE) | raw linear PCM (no container spec) |
| `pcm_s16le` | `pcmdec.c` | sw (LE, native) | raw linear PCM (no container spec) |
| `pcm_s24be` | `pcmdec.c` | – | raw linear PCM (no container spec) |
| `pcm_s24le` | `pcmdec.c` | – | raw linear PCM (no container spec) |
| `pcm_s32be` | `pcmdec.c` | – | raw linear PCM (no container spec) |
| `pcm_s32le` | `pcmdec.c` | – | raw linear PCM (no container spec) |
| `pcm_s8` | `pcmdec.c` | sb | raw linear PCM (no container spec) |
| `pcm_u16be` | `pcmdec.c` | uw (BE, native) | raw linear PCM (no container spec) |
| `pcm_u16le` | `pcmdec.c` | uw (LE, native) | raw linear PCM (no container spec) |
| `pcm_u24be` | `pcmdec.c` | – | raw linear PCM (no container spec) |
| `pcm_u24le` | `pcmdec.c` | – | raw linear PCM (no container spec) |
| `pcm_u32be` | `pcmdec.c` | – | raw linear PCM (no container spec) |
| `pcm_u32le` | `pcmdec.c` | – | raw linear PCM (no container spec) |
| `pcm_u8` | `pcmdec.c` | ub | raw linear PCM (no container spec) |
| `pcm_vidc` | `pcmdec.c` | – | raw linear PCM (no container spec) |
| `rawvideo` | `rawvideodec.c` | yuv,cif,qcif,rgb | FFmpeg-specific raw pixel format container |
| `s337m` | `s337m.c` | – | SMPTE 337M (non-PCM over AES3) |
| `v210` | `rawvideodec.c` | v210 | SMPTE 292M/424M pixel packing (v210, de facto) |
| `v210x` | `rawvideodec.c` | yuv10 | reverse-engineered (v210x variant) |
| `vc1` | `vc1dec.c` | vc1 | SMPTE 421M (VC-1) |
| `vvc` | `vvcdec.c` | h266,266,vvc | ITU-T H.266 / ISO/IEC 23090-3 Annex B |
| `yuv4mpegpipe` | `yuv4mpegdec.c` | y4m | YUV4MPEG2 (mjpegtools de facto) |

### 2.9 Playlist / concat / segment-style demuxers

Meta-demuxers that stitch together other inputs or reopen the format layer recursively. (2 demuxers)

| Name | Source file | Extensions | Spec / status |
|---|---|---|---|
| `concat` | `concatdec.c` | – | FFmpeg-specific pseudo-format (concat protocol/demuxer list) |
| `ffmetadata` | `ffmetadec.c` | – | FFmpeg-specific metadata text format |

*Note: `hls`, `dash`, `sdp`, `concat`, `webm_dash_manifest` are simultaneously playlist/segment-style demuxers (§2.9 conceptually) and general-purpose network containers (§2.1); they are listed once, under §2.1, to avoid duplication.*


---

## 3. MUXER INVENTORY

**Total registered muxers: 186** (`libavformat/allformats.c`).

### 3.1 General-purpose containers

(12 muxers)

| Name | Source file | Extensions | MIME type | Spec / status |
|---|---|---|---|---|
| `asf` | `asfenc.c` | asf,wmv,wma | video/x-ms-asf | ASF spec (Microsoft, de facto) |
| `asf_stream` | `asfenc.c` | asf,wmv,wma | video/x-ms-asf | ASF spec (Microsoft, de facto) |
| `avi` | `avienc.c` | avi | video/x-msvideo | RIFF/AVI (Microsoft, de facto) |
| `flv` | `flvenc.c` | flv | video/x-flv | Adobe FLV File Format Spec |
| `matroska` | `matroskaenc.c` | mkv | video/x-matroska | Matroska/WebM spec (matroska.org, EBML-based) |
| `mov` | `movenc.c` | mov | – | ISO/IEC 14496-12/14 (MP4/QuickTime File Format) |
| `mpjpeg` | `mpjpeg.c` | mjpg | multipart/x-mixed-replace;boundary= | RFC 2046 multipart/x-mixed-replace (MJPEG-over-HTTP, de facto) |
| `nut` | `nutenc.c` | nut | video/x-nut | NUT open container spec (Ffmpeg/NUT project) |
| `ogg` | `oggenc.c` | ogg | application/ogg | RFC 3533 (Ogg), Xiph.org |
| `rm` | `rmenc.c` | rm,ra | application/vnd.rn-realmedia | reverse-engineered (RealMedia, proprietary) |
| `sap` | `sapenc.c` | – | – | reverse-engineered (Session Announcement-like ad hoc; not RFC 2974 SAP) |
| `swf` | `swfenc.c` | swf | application/x-shockwave-flash | Adobe SWF File Format Spec |

### 3.2 Streaming / segmented / adaptive-bitrate muxers

HLS, DASH, Smooth Streaming, and the generic `segment`/`tee`/`fifo` meta-muxers plus MPEG-4-family streaming variants (`ipod`, `ismv`, `f4v`, `psp`, `3gp`/`3g2`, `hds`). (22 muxers)

| Name | Source file | Extensions | MIME type | Spec / status |
|---|---|---|---|---|
| `dash` | `dashenc.c` | mpd | – | ISO/IEC 23009-1 (MPEG-DASH) |
| `f4v` | `movenc.c` | f4v | application/f4v | ISO/IEC 14496-12/14 (MP4 family, Adobe F4V profile) |
| `fifo` | `fifo.c` | – | – | FFmpeg-specific pseudo-muxer (buffering wrapper) |
| `hds` | `hdsenc.c` | – | – | Adobe HTTP Dynamic Streaming (F4F) spec |
| `hls` | `hlsenc.c` | m3u8 | – | RFC 8216 (HLS) |
| `ipod` | `movenc.c` | m4v,m4a,m4b | video/mp4 | ISO/IEC 14496-12/14 (MP4 family, iPod profile) |
| `ismv` | `movenc.c` | ismv,isma | video/mp4 | ISO/IEC 14496-12/14 (MP4 family, MS Smooth Streaming profile) |
| `iterm2` | `iterm2enc.c` | – | – | iTerm2 inline-image escape-sequence protocol |
| `mpegts` | `mpegtsenc.c` | ts,m2t,m2ts,mts | video/MP2T | ISO/IEC 13818-1 (MPEG-2 TS) |
| `psp` | `movenc.c` | mp4,psp | – | ISO/IEC 14496-12/14 (MP4 family, PSP profile) |
| `rtp` | `rtpenc.c` | – | – | RFC 3550 (RTP) |
| `rtp_mpegts` | `rtpenc_mpegts.c` | – | – | RFC 2250 / RFC 3550 (RTP payload for MPEG-TS) |
| `rtsp` | `rtspenc.c` | – | – | RFC 2326 / RFC 7826 (RTSP 1.0/2.0) |
| `segment` | `segment.c` | – | – | FFmpeg-specific pseudo-muxer (segmenter) |
| `smoothstreaming` | `smoothstreamingenc.c` | – | – | MS-SSTR (IIS Smooth Streaming) spec |
| `stream_segment` | `segment.c` | – | – | FFmpeg-specific pseudo-muxer (segmenter) |
| `tee` | `tee.c` | – | – | FFmpeg-specific pseudo-muxer (fan-out) |
| `tg2` | `movenc.c` | 3g2 | – | ISO/IEC 14496-12/14 (MP4 family, 3GPP2 profile) / 3GPP2 C.S0050 |
| `tgp` | `movenc.c` | 3gp | – | ISO/IEC 14496-12/14 (MP4 family, 3GPP profile) / 3GPP TS 26.244 |
| `webm_chunk` | `webm_chunk.c` | chk | video/webm | WebM spec (matroska.org profile), chunked |
| `webm_dash_manifest` | `webmdashenc.c` | xml | application/xml | ISO/IEC 23009-1 (MPEG-DASH) |
| `whip` | `whip.c` | – | – | IETF draft-ietf-wish-whip (WHIP) |

### 3.3 Broadcast / professional formats

(4 muxers)

| Name | Source file | Extensions | MIME type | Spec / status |
|---|---|---|---|---|
| `dv` | `dvenc.c` | dv | – | SMPTE 314M / IEC 61834 (DV) |
| `gxf` | `gxfenc.c` | gxf | – | SMPTE 360M (GXF) |
| `mxf` | `mxfenc.c` | mxf | application/mxf | SMPTE 377M (MXF) |
| `wtv` | `wtvenc.c` | wtv | – | reverse-engineered (Microsoft Windows TV Recording) |

### 3.4 Audio containers / raw audio muxers

(41 muxers)

| Name | Source file | Extensions | MIME type | Spec / status |
|---|---|---|---|---|
| `ac3` | `rawenc.c` | ac3 | audio/x-ac3 | ATSC A/52 (AC-3) |
| `aea` | `aeaenc.c` | aea | – | reverse-engineered / FFmpeg-defined |
| `aiff` | `aiffenc.c` | aif,aiff,afc,aifc | audio/aiff | EA IFF 85 / AIFF (Apple) |
| `amr` | `amr.c` | amr | audio/amr | 3GPP TS 26.101 (AMR) |
| `apm` | `apm.c` | apm | – | reverse-engineered / FFmpeg-defined |
| `aptx` | `rawenc.c` | aptx | – | reverse-engineered (Qualcomm aptX, proprietary) |
| `aptx_hd` | `rawenc.c` | aptxhd | – | reverse-engineered (Qualcomm aptX HD, proprietary) |
| `ast` | `astenc.c` | ast | – | reverse-engineered / FFmpeg-defined |
| `au` | `au.c` | au | audio/basic | Sun/NeXT .au (de facto) |
| `caf` | `cafenc.c` | caf | audio/x-caf | Apple Core Audio Format spec |
| `codec2` | `codec2.c` | c2 | – | reverse-engineered (Codec 2 project) |
| `codec2raw` | `rawenc.c` | – | – | reverse-engineered (Codec 2 project) |
| `dfpwm` | `rawenc.c` | dfpwm | – | reverse-engineered / FFmpeg-defined |
| `dts` | `rawenc.c` | dts | audio/x-dca | ETSI TS 102 114 (DTS Coherent Acoustics, partial public spec) |
| `eac3` | `rawenc.c` | eac3,ec3 | audio/x-eac3 | ATSC A/52 (E-AC-3) |
| `flac` | `flacenc.c` | flac | audio/x-flac | IETF RFC 9639 (FLAC), Xiph.org |
| `g722` | `rawenc.c` | g722 | audio/G722 | ITU-T G.722 |
| `g723_1` | `rawenc.c` | tco,rco | audio/g723 | ITU-T G.723.1 |
| `g726` | `rawenc.c` | – | – | ITU-T G.726 |
| `g726le` | `rawenc.c` | – | – | ITU-T G.726 |
| `gsm` | `rawenc.c` | gsm | audio/x-gsm | ETSI GSM 06.10 |
| `iamf` | `iamfenc.c` | iamf | – | IAMF (AOM Immersive Audio Model and Formats) spec |
| `ilbc` | `ilbc.c` | lbc | audio/iLBC | RFC 3952 (iLBC) |
| `ircam` | `ircamenc.c` | sf,ircam | – | Berkeley/IRCAM soundfile spec (de facto) |
| `kvag` | `kvag.c` | vag | – | reverse-engineered / FFmpeg-defined |
| `lc3` | `lc3.c` | lc3 | – | Bluetooth SIG LC3 (LE Audio) spec |
| `mlp` | `rawenc.c` | mlp | – | reverse-engineered (MLP, Dolby proprietary) |
| `mmf` | `mmf.c` | mmf | application/vnd.smaf | reverse-engineered / FFmpeg-defined |
| `mp3` | `mp3enc.c` | mp3 | audio/mpeg | reverse-engineered / FFmpeg-defined |
| `oma` | `omaenc.c` | oma | audio/x-oma | reverse-engineered (Sony OpenMG/ATRAC3) |
| `rso` | `rsoenc.c` | rso | – | reverse-engineered (RSO) |
| `sbc` | `rawenc.c` | sbc,msbc | audio/x-sbc | Bluetooth SIG SBC (A2DP) spec |
| `sox` | `soxenc.c` | sox | – | SoX native format (de facto) |
| `spdif` | `spdifenc.c` | spdif | – | IEC 61937 |
| `truehd` | `rawenc.c` | thd | – | reverse-engineered (Dolby TrueHD, proprietary) |
| `tta` | `ttaenc.c` | tta | audio/x-tta | True Audio (TTA) open spec |
| `voc` | `vocenc.c` | voc | audio/x-voc | Creative Voice File (de facto) |
| `w64` | `wavenc.c` | w64 | – | Sony Wave64 spec |
| `wav` | `wavenc.c` | wav | audio/x-wav | RIFF/WAVE (de facto, ITU/EBU profiles) |
| `wsaud` | `westwood_audenc.c` | aud | – | reverse-engineered / FFmpeg-defined |
| `wv` | `wvenc.c` | wv | audio/x-wavpack | WavPack open spec (wavpack.com) |

### 3.5 Image / image2 sequence muxers

(5 muxers)

| Name | Source file | Extensions | MIME type | Spec / status |
|---|---|---|---|---|
| `apng` | `apngenc.c` | apng | image/png | APNG spec (Mozilla/community) |
| `gif` | `gif.c` | gif | image/gif | GIF89a (CompuServe) |
| `ico` | `icoenc.c` | ico | image/vnd.microsoft.icon | Microsoft ICO/CUR format (de facto) |
| `image2` | `img2enc.c` | bmp,dpx,exr,jls,jpeg,jpg,jxs,jxl,ljpg,pam,pbm,pcx,pfm,pgm,pgmyuv,phm, | – | FFmpeg-specific image-sequence pseudo-format |
| `image2pipe` | `img2enc.c` | – | – | FFmpeg-specific image-sequence pseudo-format |

### 3.6 Subtitle muxers

(9 muxers)

| Name | Source file | Extensions | MIME type | Spec / status |
|---|---|---|---|---|
| `ass` | `assenc.c` | ass,ssa | text/x-ass | no formal spec (SSA/ASS, community-documented via Aegisub/VSFilter) |
| `jacosub` | `jacosubenc.c` | jss,js | text/x-jacosub | no formal spec (JACOsub, community-documented) |
| `lrc` | `lrcenc.c` | lrc | – | no formal spec (LRC lyrics, de facto) |
| `mcc` | `mccenc.c` | mcc | – | reverse-engineered (MacCaption MCC) |
| `microdvd` | `microdvdenc.c` | sub | text/x-microdvd | no formal spec (MicroDVD, de facto) |
| `scc` | `sccenc.c` | scc | – | CEA-608 (Scenarist Closed Caption) |
| `srt` | `srtenc.c` | srt | application/x-subrip | no formal spec (SubRip, de facto/community) |
| `sup` | `supenc.c` | sup | application/x-pgs | reverse-engineered (Blu-ray/HDMV PGS) |
| `webvtt` | `webvttenc.c` | vtt | text/vtt | W3C WebVTT spec |

### 3.7 Raw / elementary-stream muxers

Includes the linear-PCM family (`rawenc.c`, `PCMDEF` macro) and other headerless bitstream dumps. (40 muxers)

| Name | Source file | Extensions | MIME type | Spec / status |
|---|---|---|---|---|
| `avs2` | `rawenc.c` | avs,avs2 | – | GY/T 299.1 / IEEE 1857.4 (AVS2) |
| `avs3` | `rawenc.c` | avs3 | – | GY/T 358 / IEEE 1857.10 (AVS3) |
| `bit` | `bit.c` | bit | audio/bit | reverse-engineered (G.729/G.723 bit-exact test format) |
| `cavsvideo` | `rawenc.c` | cavs | – | GB/T 20090 (Chinese AVS) |
| `data` | `rawenc.c` | – | – | FFmpeg-specific raw-data passthrough |
| `dirac` | `rawenc.c` | drc,vc2 | – | SMPTE 2042 (Dirac / VC-2 base) |
| `dnxhd` | `rawenc.c` | dnxhd,dnxhr | – | SMPTE VC-3 (DNxHD) |
| `evc` | `rawenc.c` | evc | – | MPEG-5 EVC (ISO/IEC 23094-1) Annex B |
| `h261` | `rawenc.c` | h261 | video/x-h261 | ITU-T H.261 |
| `h263` | `rawenc.c` | h263 | video/x-h263 | ITU-T H.263 |
| `h264` | `rawenc.c` | h264,264 | – | ITU-T H.264 / ISO/IEC 14496-10 Annex B |
| `hevc` | `rawenc.c` | hevc,h265,265 | – | ITU-T H.265 / ISO/IEC 23008-2 Annex B |
| `m4v` | `rawenc.c` | m4v | – | ISO/IEC 14496-2 (MPEG-4 Part 2 video) |
| `mjpeg` | `rawenc.c` | mjpg,mjpeg | video/x-mjpeg | ITU-T T.81 (JPEG) |
| `obu` | `rawenc.c` | obu | – | AV1 Open Bitstream Unit spec (AOMedia) |
| `pcm_alaw` | `rawenc.c` | – | – | raw linear PCM (no container spec) |
| `pcm_f32be` | `rawenc.c` | – | – | raw linear PCM (no container spec) |
| `pcm_f32le` | `rawenc.c` | – | – | raw linear PCM (no container spec) |
| `pcm_f64be` | `rawenc.c` | – | – | raw linear PCM (no container spec) |
| `pcm_f64le` | `rawenc.c` | – | – | raw linear PCM (no container spec) |
| `pcm_mulaw` | `rawenc.c` | – | – | raw linear PCM (no container spec) |
| `pcm_s16be` | `rawenc.c` | – | – | raw linear PCM (no container spec) |
| `pcm_s16le` | `rawenc.c` | – | – | raw linear PCM (no container spec) |
| `pcm_s24be` | `rawenc.c` | – | – | raw linear PCM (no container spec) |
| `pcm_s24le` | `rawenc.c` | – | – | raw linear PCM (no container spec) |
| `pcm_s32be` | `rawenc.c` | – | – | raw linear PCM (no container spec) |
| `pcm_s32le` | `rawenc.c` | – | – | raw linear PCM (no container spec) |
| `pcm_s8` | `rawenc.c` | – | – | raw linear PCM (no container spec) |
| `pcm_u16be` | `rawenc.c` | – | – | raw linear PCM (no container spec) |
| `pcm_u16le` | `rawenc.c` | – | – | raw linear PCM (no container spec) |
| `pcm_u24be` | `rawenc.c` | – | – | raw linear PCM (no container spec) |
| `pcm_u24le` | `rawenc.c` | – | – | raw linear PCM (no container spec) |
| `pcm_u32be` | `rawenc.c` | – | – | raw linear PCM (no container spec) |
| `pcm_u32le` | `rawenc.c` | – | – | raw linear PCM (no container spec) |
| `pcm_u8` | `rawenc.c` | – | – | raw linear PCM (no container spec) |
| `pcm_vidc` | `rawenc.c` | – | – | raw linear PCM (no container spec) |
| `rawvideo` | `rawenc.c` | yuv,rgb | – | FFmpeg-specific raw pixel format container |
| `vc1` | `rawenc.c` | vc1 | – | SMPTE 421M (VC-1) Annex E elementary stream |
| `vvc` | `rawenc.c` | vvc,h266,266 | – | ITU-T H.266 / ISO/IEC 23090-3 Annex B |
| `yuv4mpegpipe` | `yuv4mpegenc.c` | y4m | – | YUV4MPEG2 (mjpegtools de facto) |

### 3.8 Utility / integrity / test muxers

Not real containers — checksum, hash, and null sinks used for regression testing and fingerprinting, plus the metadata-only sidecar muxer. (12 muxers)

| Name | Source file | Extensions | MIME type | Spec / status |
|---|---|---|---|---|
| `chromaprint` | `chromaprint.c` | – | – | Chromaprint/AcoustID fingerprint (external library) |
| `crc` | `crcenc.c` | – | – | FFmpeg-specific test muxer |
| `framecrc` | `framecrcenc.c` | – | – | FFmpeg-specific test muxer |
| `framehash` | `hashenc.c` | – | – | FFmpeg-specific test muxer |
| `ffmetadata` | `ffmetaenc.c` | ffmeta | – | FFmpeg-specific metadata text format |
| `framemd5` | `hashenc.c` | – | – | FFmpeg-specific test muxer |
| `hash` | `hashenc.c` | – | – | FFmpeg-specific test muxer |
| `md5` | `hashenc.c` | – | – | FFmpeg-specific test muxer |
| `mkvtimestamp_v2` | `mkvtimestamp_v2.c` | – | – | FFmpeg-specific test muxer (mkvmerge-compatible timecode file) |
| `null` | `nullenc.c` | – | – | FFmpeg-specific null sink |
| `streamhash` | `hashenc.c` | – | – | FFmpeg-specific test muxer |
| `uncodedframecrc` | `uncodedframecrcenc.c` | – | – | FFmpeg-specific test muxer |

### 3.9 Game / legacy / misc muxers

(41 muxers)

| Name | Source file | Extensions | MIME type | Spec / status |
|---|---|---|---|---|
| `a64` | `a64.c` | a64, A64 | – | reverse-engineered / FFmpeg-defined |
| `ac4` | `ac4enc.c` | ac4 | audio/ac4 | ETSI TS 103 190 (Dolby AC-4) |
| `adts` | `adtsenc.c` | aac,adts | audio/aac | reverse-engineered / FFmpeg-defined |
| `adx` | `rawenc.c` | adx | – | reverse-engineered / FFmpeg-defined |
| `alp` | `alp.c` | tun,pcm | – | reverse-engineered / FFmpeg-defined |
| `amv` | `amvenc.c` | amv | video/amv | reverse-engineered / FFmpeg-defined |
| `apv` | `apvenc.c` | apv | – | ISO/IEC 23001-25 (APV, Advanced Professional Video) |
| `argo_asf` | `argo_asf.c` | asf | – | reverse-engineered / FFmpeg-defined |
| `argo_cvg` | `argo_cvg.c` | cvg | – | reverse-engineered / FFmpeg-defined |
| `avif` | `movenc.c` | avif | image/avif | ISO/IEC 23008-12 (AVIF, HEIF-based) |
| `avm2` | `swfenc.c` | – | application/x-shockwave-flash | reverse-engineered / FFmpeg-defined |
| `daud` | `daudenc.c` | 302 | – | reverse-engineered / FFmpeg-defined |
| `filmstrip` | `filmstripenc.c` | flm | – | reverse-engineered / FFmpeg-defined |
| `fits` | `fitsenc.c` | fits | – | reverse-engineered / FFmpeg-defined |
| `ivf` | `ivfenc.c` | ivf | – | reverse-engineered / FFmpeg-defined |
| `latm` | `latmenc.c` | latm,loas | audio/MP4A-LATM | reverse-engineered / FFmpeg-defined |
| `matroska_audio` | `matroskaenc.c` | mka | audio/x-matroska | Matroska spec (matroska.org, audio-only profile) |
| `mp2` | `rawenc.c` | mp2,m2a,mpa | audio/mpeg | reverse-engineered / FFmpeg-defined |
| `mp4` | `movenc.c` | mp4 | video/mp4 | ISO/IEC 14496-12/14 (MP4) |
| `mpeg1system` | `mpegenc.c` | mpg,mpeg | video/mpeg | reverse-engineered / FFmpeg-defined |
| `mpeg1vcd` | `mpegenc.c` | – | video/mpeg | reverse-engineered / FFmpeg-defined |
| `mpeg1video` | `rawenc.c` | mpg,mpeg,m1v | video/mpeg | reverse-engineered / FFmpeg-defined |
| `mpeg2dvd` | `mpegenc.c` | dvd | video/mpeg | reverse-engineered / FFmpeg-defined |
| `mpeg2svcd` | `mpegenc.c` | vob | video/mpeg | reverse-engineered / FFmpeg-defined |
| `mpeg2video` | `rawenc.c` | m2v | – | reverse-engineered / FFmpeg-defined |
| `mpeg2vob` | `mpegenc.c` | vob | video/mpeg | reverse-engineered / FFmpeg-defined |
| `mxf_d10` | `mxfenc.c` | – | application/mxf | reverse-engineered / FFmpeg-defined |
| `mxf_opatom` | `mxfenc.c` | mxf | application/mxf | reverse-engineered / FFmpeg-defined |
| `oga` | `oggenc.c` | oga | audio/ogg | reverse-engineered / FFmpeg-defined |
| `ogv` | `oggenc.c` | ogv | video/ogg | reverse-engineered / FFmpeg-defined |
| `opus` | `oggenc.c` | opus | audio/ogg | reverse-engineered / FFmpeg-defined |
| `pdv` | `pdvenc.c` | pdv | – | reverse-engineered / FFmpeg-defined |
| `rcwt` | `rcwtenc.c` | – | – | reverse-engineered / FFmpeg-defined |
| `roq` | `idroqenc.c` | roq | – | reverse-engineered / FFmpeg-defined |
| `segafilm` | `segafilmenc.c` | cpk | – | reverse-engineered / FFmpeg-defined |
| `smjpeg` | `smjpegenc.c` | – | – | reverse-engineered / FFmpeg-defined |
| `spx` | `oggenc.c` | spx | audio/ogg | reverse-engineered / FFmpeg-defined |
| `ttml` | `ttmlenc.c` | ttml | text/ttml | reverse-engineered / FFmpeg-defined |
| `vc1t` | `vc1testenc.c` | rcv | – | reverse-engineered (VC-1 test-bitstream .rcv wrapper) |
| `webm` | `matroskaenc.c` | webm | video/webm | WebM spec (matroska.org profile) |
| `webp` | `webpenc.c` | webp | – | reverse-engineered / FFmpeg-defined |


### 3.10 Notable muxer option surfaces

**MOV/MP4 (`movenc.c`, shared by `mov`/`mp4`/`ipod`/`ismv`/`f4v`/`3gp`/`3g2`/`psp`)** — top-level `AVOption`s: `brand`, `empty_hdlr_name`, `encryption_key`, `encryption_kid`, `encryption_scheme` (`none` | `cenc-aes-ctr`), `frag_duration`, `frag_interleave`, `frag_size`, `fragment_index`, `iods_audio_profile`, `iods_video_profile`, `ism_lookahead`, `min_frag_duration`, `mov_gamma`, `movie_timescale`, `rtpflags` (shared RTP-packetizer flag set), `skip_iods`, `use_editlist`, `use_stream_ids_as_track_ids`, `video_track_timescale`, `write_btrt`, `write_prft` (unit `pts`/`wallclock`), `write_tmcd`.
`movflags` (bitmask, unit `movflags`) constants: `cmaf`, `dash`, `default_base_moof`, `delay_moov`, `disable_chpl`, `empty_moov`, `faststart`, `frag_custom`, `frag_discont`, `frag_every_frame`, `frag_keyframe`, `global_sidx`, `isml`, `moov_size` (int, not a bit flag despite living in the unit group), `negative_cts_offsets`, `omit_tfhd_offset`, `prefer_icc`, `rtphint`, `separate_moof`, `skip_sidx`, `skip_trailer`, `use_metadata_tags`, `write_colr`, `write_gama`, `hybrid_fragmented`.

**Matroska/WebM (`matroskaenc.c`)**: `reserve_index_space`, `cues_to_front`, `cluster_size_limit`, `cluster_time_limit`, `dash` (bool), `dash_track_number`, `live`, `allow_raw_vfw`, `flipped_raw_rgb`, `write_crc32`, `default_mode` (unit `default_mode`: `infer`, `infer_no_subs`, `passthrough`).

**MPEG-TS muxer (`mpegtsenc.c`)**: `mpegts_transport_stream_id`, `mpegts_original_network_id`, `mpegts_service_id`, `mpegts_service_type` (unit `mpegts_service_type`: `digital_tv`, `digital_radio`, `teletext`, `advanced_codec_digital_radio`, `mpeg2_digital_hdtv`, `advanced_codec_digital_sdtv`, `advanced_codec_digital_hdtv`, `hevc_digital_hdtv`), `mpegts_pmt_start_pid`, `mpegts_start_pid`, `mpegts_m2ts_mode`, `muxrate`, `pes_payload_size`, `resend_headers`, `latm`, `pat_pmt_at_frames`, `system_b`, `initial_discontinuity`, `nit`, `omit_rai`, `mpegts_copyts`, `tables_version`, `omit_video_pes_length`, `pcr_period`, `mpegts_pcr_pid`, `pat_period`, `sdt_period`, `nit_period`.
**MPEG-TS demuxer (`mpegts.c`)**: `resync_size`, `ts_id`, `ts_packetsize`, `fix_teletext_pts`, `scan_all_pmts`, `skip_unknown_pmt`, `merge_pmt_versions`, `skip_changes`, `skip_clear`, `max_packet_size`, `compute_pcr`.

**HLS muxer (`hlsenc.c`)**: `start_number`, `hls_time`, `hls_init_time`, `hls_list_size`, `hls_delete_threshold`, `hls_vtt_options`, `hls_allow_cache`, `hls_base_url`, `hls_segment_filename`, `hls_segment_options`, `hls_segment_size`, `hls_key_info_file`, `hls_enc`, `hls_enc_key`, `hls_enc_key_url`, `hls_enc_iv`, `hls_subtitle_path`, `hls_segment_type` (unit: `mpegts`, `fmp4`), `hls_fmp4_init_filename`, `hls_fmp4_init_resend`, `hls_flags` (bitmask, unit `hls_flags`: `single_file`, `temp_file`, `delete_segments`, `round_durations`, `discont_start`, `omit_endlist`, `split_by_time`, `append_list`, `program_date_time`, `second_level_segment_index`, `second_level_segment_duration`, `second_level_segment_size`, `periodic_rekey`, `independent_segments`, `iframes_only`, `split_by_time`), `strftime`, `strftime_mkdir`, `hls_playlist_type` (unit: `event`, `vod`), `method`, `hls_start_number_source` (unit: `generic`, `epoch`, `epoch_us`, `datetime`), `http_user_agent`, `var_stream_map`, `cc_stream_map`, `master_pl_name`, `master_pl_publish_rate`, `http_persistent`, `timeout`, `ignore_io_errors`, `headers`.
**HLS demuxer (`hls.c`)**: `allowed_extensions`, `allowed_segment_extensions`, `extension_picky`, `http_multiple`, `http_persistent`, `http_seekable`, `live_start_index`, `m3u8_hold_counters`, `max_reload`, `prefer_x_start`, `seg_format_options`, `seg_max_retry`.

**DASH muxer (`dashenc.c`)**: `adaptation_sets`, `dash_segment_type` (unit: `auto`, `mp4`, `webm`), `extra_window_size`, `format_options`, `frag_duration`, `frag_type` (unit: `none`, `every_frame`, `duration`, `pframes`), `global_sidx`, `hls_master_name`, `hls_playlist`, `http_opts`, `http_persistent`, `http_user_agent`, `ignore_io_errors`, `index_correction`, `init_seg_name`, `ldash`, `lhls`, `master_m3u8_publish_rate`, `max_playback_rate`, `media_seg_name`, `method`, `min_playback_rate`, `mpd_profile` (unit: `dash`, `dvb_dash`), `remove_at_exit`, `seg_duration`, `single_file`, `single_file_name`, `availability_start_time_ms` (renamed from `streaming`-adjacent internal field), `streaming`, `suggested_presentation_delay`, `target_latency`, `timeout`, `update_period`, `use_template`, `use_timeline`, `utc_timing_url`, `window_size`, `write_prft`.
**DASH demuxer (`dashdec.c`)**: `allowed_extensions`, `cenc_decryption_key`, `cenc_decryption_keys`, `max_reload`.

**`segment` muxer (`segment.c`)**: `reference_stream`, `segment_format`, `segment_format_options`, `segment_list`, `segment_header_filename`, `segment_list_flags` (unit: `cache`, `live`), `segment_list_size`, `segment_list_type` (unit: `flat`, `csv`, `ext`, `ffconcat`, `m3u8`, `hls`), `segment_atclocktime`, `segment_clocktime_offset`, `segment_clocktime_wrap_duration`, `segment_time`, `segment_time_delta`, `min_seg_duration`, `segment_times`, `segment_frames`, `segment_wrap`, `segment_list_entry_prefix`, `segment_start_number`, `segment_wrap_number`, `strftime`, `increment_tc`, `break_non_keyframes`, `individual_header_trailer`, `write_header_trailer`, `reset_timestamps`, `initial_offset`, `write_empty_segments` (also registered as alias `stream_segment`/`ssegment` for per-packet, non-reopening segmentation).

**`tee` muxer (`tee.c`)**: pseudo-URL syntax `"[options]output1|[options]output2|..."` parsed by the tee *protocol* layer, not `AVOption`s in the usual sense — recognized per-output directives: `select` (stream-selection specifier, stolen before the muxer opens), `onfail` (`ignore` | `abort`), `use_fifo` (bool — wrap this output in the `fifo` pseudo-muxer), `fifo_options` (dictionary forwarded to `fifo`), and `bsfs[/<stream-spec>]` (per-stream bitstream-filter chain).

**`fifo` muxer (`fifo.c`)**: `attempt_recovery`, `drop_pkts_on_overflow`, `fifo_format`, `format_opts`, `max_recovery_attempts`, `queue_size`, `recovery_wait_streamtime`, `recovery_wait_time`, `recover_any_error`, `restart_with_keyframe`, `timeshift`. Wraps a real muxer in a background thread + bounded packet queue so a slow/failing downstream (network) muxer cannot stall the encoder.

### 3.11 Notable RTSP/RTP demuxer options
`rtsp.c`: `audio`/`video`/`data`/`subtitle` (per-media-type transport override), `block`, `buffer_size`, `custom_io`, `filter_src`, `http`/`https` (unit values for `rtsp_transport`), `initial_pause`, `listen`, `listen_timeout`, `localaddr`, `max_port`/`min_port`, `pkt_size`, `prefer_tcp`, `reorder_queue_size`, `rtcp_to_source`, `rtsp_transport` (unit: `udp`, `tcp`, `udp_multicast`, `http`, `https`), `satip_raw`, `timeout`, `user_agent`.


---

## 4. PROTOCOL INVENTORY

**Total registered `URLProtocol`s: 57** (`libavformat/protocols.c`). Each entry below is the internal `AVOption` surface of the protocol's private context (from its `priv_class`/`priv_data_class`) plus build-time dependency (from `configure`'s `*_protocol_deps`/`*_protocol_select`) and licensing notes.

| Protocol | Source file | Notable options | External dependency | License / notes |
|---|---|---|---|---|
| `android_content` | androidcontent.c | – | JNI (Android) | Content-resolver URI access on Android. |
| `async` | async.c | – | pthreads/win32 threads | Read-ahead buffering wrapper around any nested URL. |
| `bluray` | bluray.c | – | `libbluray` | BD-ROM disc structure access. |
| `cache` | cache.c | `read_ahead_limit` | none | Transparent local-file read-through/seek cache around any URL. |
| `concat` | concat.c | – | none | `concat:url1\|url2\|...` — sequential virtual concatenation of byte streams. |
| `concatf` | concat.c | – | none | Concat list read from a file, one URL per line. |
| `crypto` | crypto.c | `key`, `iv`, `decrypt` | none (uses `libavutil/aes.h`) | AES-128/256-CTR encrypt/decrypt filter around a nested URL. |
| `data` | data_uri.c | – | none | RFC 2397 `data:` URI (base64/percent-encoded inline payload). |
| `fd` | file.c | `fd` | POSIX | Wrap an already-open OS file descriptor. |
| `ffrtmpcrypt` | rtmpcrypt.c | (shares `rtmp_*` options) | gcrypt / gmp / openssl / mbedtls (any one) | Native RTMPE (encrypted RTMP) key exchange, used internally by `rtmpe`/`rtmpte`. |
| `ffrtmphttp` | rtmphttp.c | – | `http_protocol` | RTMPT (RTMP tunneled over HTTP), used internally by `rtmpt`/`rtmpts`/`rtmpte`. |
| `file` | file.c | `truncate`, `blocksize`, `follow` | POSIX/Win32 | Local filesystem access. |
| `ftp` | ftp.c | `timeout`, `ftp-anonymous-password`, `ftp-write-seekable` | `tcp_protocol` | RFC 959 (FTP), plaintext. |
| `gopher` | gopher.c | – | `tcp_protocol` | RFC 1436 (Gopher). |
| `gophers` | gopher.c | – | `tls_protocol` | Gopher over TLS. |
| `http` | http.c | see §4.1 | `tcp_protocol` | RFC 9110/9112 (HTTP/1.1) client, with reconnect/ICY extensions. |
| `httpproxy` | httpauth.c / http.c | – | `tcp_protocol` | HTTP CONNECT proxy tunnel. |
| `https` | http.c | (shares `http` options) | `tls_protocol` | HTTP over TLS. |
| `icecast` | icecast.c | `ice_genre`, `ice_name`, `ice_description`, `ice_url`, `ice_public`, `user_agent`, `content_type`, `legacy_icecast` | `http_protocol` | Icecast/SHOUTcast source-client (mount-point publishing) protocol. |
| `mmsh` | mmsh.c | – | `http_protocol` | Reverse-engineered MMS-over-HTTP (Microsoft Media Server). |
| `mmst` | mmst.c | – | `network` | Reverse-engineered MMS-over-TCP. |
| `md5` | md5proto.c | – | none | Write-only: hashes output instead of writing a file (paired with `-f md5`/hash-style output patterns at the protocol level). |
| `pipe` | file.c | `blocksize` | POSIX/Win32 | `pipe:<fd>` — read/write a numbered standard stream. |
| `prompeg` | prompeg.c | `fec` | `network` | Pro-MPEG Code of Practice #3 FEC wrapper (SMPTE 2022-1-style) around `udp`. |
| `rtmp` | rtmpproto.c | see §4.2 | `tcp_protocol` | Reverse-engineered native RTMP (Adobe). |
| `rtmpe` | rtmpproto.c | (shares `rtmp_*`) | `ffrtmpcrypt_protocol` | Encrypted RTMP. |
| `rtmps` | rtmpproto.c | (shares `rtmp_*`) | `tls_protocol` | RTMP over TLS. |
| `rtmpt` | rtmpproto.c | (shares `rtmp_*`) | `ffrtmphttp_protocol` | RTMP tunneled over HTTP. |
| `rtmpte` | rtmpproto.c | (shares `rtmp_*`) | `ffrtmpcrypt_protocol` + `ffrtmphttp_protocol` | Encrypted RTMP tunneled over HTTP. |
| `rtmpts` | rtmpproto.c | (shares `rtmp_*`) | `ffrtmphttp_protocol` + `https_protocol` | RTMP tunneled over HTTPS. |
| `rtp` | rtpproto.c | see §4.3 | `udp_protocol` | RFC 3550 (RTP) unicast/multicast UDP transport (payload framing handled by the RTSP/RTP demuxer layer, §5.1). |
| `shared` | udp.c (shared ring-buffer variant) | – | mmap + stdatomic + unistd | Shared-memory ring buffer for multi-process fan-out of a UDP source. |
| `sctp` | sctp.c | `listen`, `max_streams` | OS SCTP socket API | RFC 4960 (SCTP) transport. |
| `srtp` | srtpproto.c | `srtp_in_suite`, `srtp_in_params`, `srtp_out_suite`, `srtp_out_params` | none (internal `libavutil` crypto) | RFC 3711 (SRTP) wrapper, keyed manually (not via SDES/DTLS key exchange in this protocol object). |
| `subfile` | subfile.c | `start`, `end` | none | Restrict a nested URL to a byte-range window. |
| `tee` | tee.c (protocol) | – | none | Duplicate writes across multiple nested output URLs. |
| `tcp` | tcp.c | see §4.4 | `network` | Raw TCP socket. |
| `tls` | tls.c + `tls_<backend>.c` | see §4.5 | one of `gnutls`/`openssl`/`schannel`/`securetransport`/`libtls`/`mbedtls` (mutually exclusive, `tls_protocol_deps_any`) | TLS 1.x client/server. |
| `dtls` | dtls.c + backend | – | `openssl`/`schannel`/`gnutls`/`mbedtls` | DTLS transport (used by `whip`/WebRTC). |
| `udp` | udp.c | see §4.6 | `network` | RFC 768 (UDP) unicast/multicast socket. |
| `udplite` | udp.c | (shares `udp` options) + `udplite_coverage` | `network` | RFC 3828 (UDP-Lite). |
| `unix` | unix.c | `listen`, `timeout` | `sys/un.h` | UNIX domain socket. |
| `libamqp` | libamqp.c | – | `librabbitmq` | AMQP 0-9-1 publish (control-message sink, e.g. for `-listen` event notification). |
| `libcurl` | libcurl.c | – | `libcurl` + threads | Alternate HTTP(S) backend routed through libcurl (`prefer_libcurl`/`ffio` opt-in). |
| `librist` | librist.c | `rist_profile` (unit: `simple`, `main`, `advanced`), `secret`, `encryption`, `pkt_size`, `log_level`, `fifo_size`, `overrun_nonfatal`, `buffer_size` | `librist` | RIST (Reliable Internet Stream Transport) via VideoLAN's librist. |
| `librtmp` | librtmp.c | `rtmp_*` (forwarded to librtmp) | `librtmp` (LGPL build of rtmpdump) | Alternate RTMP backend via librtmp (fuller feature set than native `rtmp`). |
| `librtmpe` | librtmp.c | (shares `librtmp`) | `librtmp` | Encrypted RTMP via librtmp. |
| `librtmps` | librtmp.c | (shares `librtmp`) | `librtmp` | RTMP over TLS via librtmp. |
| `librtmpt` | librtmp.c | (shares `librtmp`) | `librtmp` | RTMPT via librtmp. |
| `librtmpte` | librtmp.c | (shares `librtmp`) | `librtmp` | Encrypted RTMPT via librtmp. |
| `libsrt` | libsrt.c | see §4.7 | `libsrt` (Secure Reliable Transport, MPL-2.0) | SRT protocol (Haivision). |
| `libssh` | libssh.c | `priv_key`, `timeout` | `libssh` | SFTP over SSH. |
| `libsmbclient` | libsmbclient.c | `timeout`, `truncate`, `workgroup` | `libsmbclient` **+ requires `--enable-gplv3`** | SMB/CIFS access via Samba's libsmbclient. |
| `libzmq` | libzmq.c | `pkt_size` | `libzmq` | ZeroMQ PUB/SUB socket transport. |
| `ipfs_gateway` | ipfsgateway.c | `ipfs_gateway`, `ipfs_local` | `https_protocol` | Resolves `ipfs://<CID>` via a configured IPFS HTTP gateway. |
| `ipns_gateway` | ipfsgateway.c | (shares `ipfs_gateway`) | `https_protocol` | Resolves `ipns://<name>` similarly. |

### 4.1 `http`/`https` option surface
`auth_type` (unit: `none`, `basic`), `chunked_post`, `content_type`, `cookies`, `end_offset`, `headers`, `http_proxy`, `http_version`, `icy`, `icy_metadata_headers`, `icy_metadata_packet`, `initial_request_size`, `listen`, `location`, `max_redirects`, `method`, `mime_type`, `multiple_requests`, `offset`, `post_data`, `reconnect`, `reconnect_at_eof`, `reconnect_delay_max`, `reconnect_delay_total_max`, `reconnect_max_retries`, `reconnect_on_http_error`, `reconnect_on_network_error`, `reconnect_streamed`, `referer`, `reply_code`, `request_size`, `resource`, `respect_retry_after`, `seekable`, `send_expect_100`, `short_seek_size`, `user_agent`.

### 4.2 `rtmp` family option surface
`rtmp_app`, `rtmp_buffer`, `rtmp_conn`, `rtmp_enhanced_codecs`, `rtmp_flashver`, `rtmp_flush_interval`, `rtmp_live` (unit: `any`, `live`, `recorded`), `rtmp_listen`/`listen`, `rtmp_pageurl`, `rtmp_playpath`, `rtmp_subscribe`, `rtmp_swfhash`, `rtmp_swfsize`, `rtmp_swfurl`, `rtmp_swfverify`, `rtmp_tcurl`, `tcp_nodelay`, `timeout`.

### 4.3 `rtp` protocol option surface
`block`, `buffer_size`, `connect`, `dscp`, `fec` (Pro-MPEG-style, mirrors `prompeg`), `local_rtcpport`, `local_rtpport`, `localaddr`, `localport`, `localrtcpport`, `localrtpport`, `pkt_size`, `rtcp_port`/`rtcpport`, `sources`, `timeout`, `ttl`, `write_to_source`.

### 4.4 `tcp` option surface
`listen`, `listen_timeout`, `local_addr`, `local_port`, `recv_buffer_size`, `send_buffer_size`, `tcp_keepalive`, `tcp_mss`, `tcp_nodelay`, `timeout`.

### 4.5 TLS backend matrix
| Backend file | Library | Notes |
|---|---|---|
| `tls_openssl.c` | OpenSSL (or compatible: BoringSSL/LibreSSL) | Most complete backend; supports client certs, ALPN. |
| `tls_gnutls.c` | GnuTLS | LGPLv2.1+. |
| `tls_mbedtls.c` | mbedTLS | Adds `key_password` option. Apache-2.0. |
| `tls_schannel.c` | Windows SChannel | Adds `cert_store_name`, `cert_store_subject` options; Windows-native, no external dep. |
| `tls_securetransport.c` | Apple Secure Transport / Network.framework | macOS/iOS-native, no external dep. |
| `tls_libtls.c` | LibreSSL's `libtls` | BSD-oriented minimal API. |

Exactly one backend is compiled in per build (`tls_protocol_deps_any`); DTLS (`dtls.c`) reuses whichever of openssl/schannel/gnutls/mbedtls is available.

### 4.6 `udp` option surface
`bitrate`, `block`, `broadcast`, `buffer_size`, `burst_bits`, `connect`, `dscp`, `fifo_size`, `local_port`, `localaddr`, `localport`, `overrun_nonfatal`, `pkt_size`, `reuse`, `reuse_socket`, `sources`, `timeout`, `ttl`, `udplite_coverage` (udplite only).

### 4.7 `libsrt` option surface
`caller`, `connect_timeout`, `enforced_encryption`, `ffs`, `inputbw`, `iptos`, `ipttl`, `ipv6only`, `kmpreannounce`, `kmrefreshrate`, `latency`, `linger`, `listen_timeout`, `listener`, `live` (unit `transtype` alt), `lossmaxttl`, `maxbw`, `messageapi`, `minversion`, `mode` (unit: `caller`, `listener`, `rendezvous`), `mss`, `nakreport`, `oheadbw`, `passphrase`, `payload_size`, `pbkeylen`, `peerlatency`, `pkt_size`, `rcvbuf`, `rcvlatency`, `recv_buffer_size`, `rendezvous`, `send_buffer_size`, `smoother`, `sndbuf`, `snddropdelay`, `srt_streamid`/`streamid`, `timeout`, `tlpktdrop`, `transtype` (unit: `live`, `file`), `ts_size`, `tsbpd`, `tsbpddelay`.


---

## 5. CROSS-CUTTING CONCERNS

### 5.1 RTSP/RTP payload handlers

RTP depacketizers (`RTPDynamicProtocolHandler`, `rtpdec_formats.h`, dispatched by dynamic payload type / `a=rtpmap` name from SDP) — **28 registered**:
`ac3`, `amr_nb`, `amr_wb`, `av1`, `dv`, `g726_16`/`g726_24`/`g726_32`/`g726_40` (big-endian nibble order) and `g726le_16`/`g726le_24`/`g726le_32`/`g726le_40` (little-endian), `h261`, `h263_1998`, `h263_2000`, `h263_rfc2190`, `h264`, `hevc`, `ilbc`, `jpeg`, `mp4a_latm`, `mp4v_es`, `mpeg_audio`, `mpeg_audio_robust`, `mpeg_video`, `mpeg4_generic`, `mpegts`, `opus`, `qcelp`, `qdm2`, `svq3`, `theora`, `vc2hq`, `vorbis`, `vp8`, `vp9`.

RTP packetizers (muxer side, one `.c` per payload family in `rtpenc_*.c`): AAC (`rtpenc_aac.c`, RFC 3640), AMR (`rtpenc_amr.c`, RFC 4867), AV1 (`rtpenc_av1.c`), H.261 (`rtpenc_h261.c`, RFC 4587), H.263 (`rtpenc_h263.c`/`rtpenc_h263_rfc2190.c`), H.264/HEVC (`rtpenc_h264_hevc.c`, RFC 6184/7798), JPEG (`rtpenc_jpeg.c`, RFC 2435), MPEG-4 LATM (`rtpenc_latm.c`), MPEG-TS (`rtpenc_mpegts.c`, RFC 2250), MPEG audio/video generic (`rtpenc_mpv.c`), uncompressed video (`rtpenc_rfc4175.c`, RFC 4175/SMPTE 2110-style raw payload), VC-2/Dirac HQ (`rtpenc_vc2hq.c`), VP8/VP9 (`rtpenc_vp8.c`/`rtpenc_vp9.c`), Xiph (Vorbis/Theora, `rtpenc_xiph.c`). Generic chaining/fallback logic lives in `rtpenc_chain.c`; RDT (RealNetworks Data Transport, used by legacy `rm`/`rdt` streaming) is handled separately via `rdt.c`/`rdt.h`, not the standard RTP dynamic-handler table.

### 5.2 HLS / DASH manifest handling

- **HLS** (`hls.c` demux, `hlsplaylist.c`/`hlsenc.c` mux): parses/writes RFC 8216 M3U8 master + media playlists; supports variant streams (`#EXT-X-STREAM-INF`), alternate renditions (`#EXT-X-MEDIA`), byte-range segments (`#EXT-X-BYTERANGE`), discontinuities, program-date-time, and both MPEG-TS and fMP4 (CMAF-style) segment types. Key-handling supports AES-128 (`#EXT-X-KEY` with `METHOD=AES-128`) and Apple Sample-AES (`METHOD=SAMPLE-AES`, `hls_sample_encryption.h` — ID3-timed metadata + per-sample AES-CBC).
- **DASH** (`dashdec.c`/`dashenc.c`, `dash.h`): parses/writes ISO/IEC 23009-1 MPD XML — `AdaptationSet`/`Representation`/`SegmentTemplate`/`SegmentTimeline`/`SegmentList`, static and dynamic (live) MPD profiles (`urn:mpeg:dash:profile:isoff-live:2011`, DVB-DASH extensions via `mpd_profile=dvb_dash`), `SegmentBase` with `sidx`-driven byte-range indexing for on-demand profiles, CENC `ContentProtection` element parsing for encrypted representations.
- Both share the generic segmenting infrastructure in `segment.c` (file-based fallback / local testing) and both can target either MPEG-TS or fragmented-MP4/WebM segment payloads.

### 5.3 ID3 / metadata conventions

- **ID3v2** (`id3v2.h`/`id3v2.c`, shared by MP3, AIFF, WAV, and others as a leading/trailing tag block): versions 2.2/2.3/2.4 frame-ID tables (`ff_id3v2_3_tags`, `ff_id3v2_4_tags`), MIME/picture-type table (`ff_id3v2_mime_tags`), and per-version `AVMetadataConv` tables (`ff_id3v2_34_metadata_conv`, `ff_id3v2_4_metadata_conv`) mapping frame IDs (`TIT2`, `TPE1`, `TALB`, …) to FFmpeg's generic metadata keys (`title`, `artist`, `album`, …). `ID3v2_FLAG_*` header flags: `DATALEN`, `UNSYNCH`, `COMPRESSION`, `ENCRYPTION`. Private/unmapped frames are exposed under the `id3v2_priv.` metadata-key prefix. `FF_INFMT_FLAG_ID3V2_AUTO` lets a demuxer opt into automatic leading-tag stripping/parsing.
- **Generic metadata conversion**: every container with a native tag scheme (RIFF `INFO` chunk, ASF extended content description, Matroska `SimpleTag`, NUT, Ogg Vorbis comments, MOV `udta`/`meta`/`ilst`, LRC) ships its own `AVMetadataConv[]` table translating native key spellings to FFmpeg's canonical metadata keys (`title`, `author`→`artist`, `album`, `date`/`year`, `genre`, `track`, `comment`, etc.) via the shared `ff_metadata_conv()`/`ff_metadata_conv_ctx()` helpers (`metadata.c`).
- **Chapters**: ID3v2 `CHAP`/`CTOC` frames are mapped to `AVChapter` entries the same way as native per-format chapter atoms (MOV `chpl`/`chap`, Matroska `Chapters`, WAV `LIST/INFO`, etc.).

### 5.4 Encryption schemes supported

| Scheme | Where implemented | Standard |
|---|---|---|
| MP4 CENC (`cenc-aes-ctr`) | `movenc.c` (write, via `movenccenc.h`), `mov.c` (read) | ISO/IEC 23001-7 Common Encryption, `cenc`/`cbcs` scheme families; AES-CTR sample encryption, `pssh`/`tenc`/`senc` boxes, key/KID supplied via `encryption_key`/`encryption_kid` (mux) or `decryption_key`/CENC boxes (demux/DASH `cenc_decryption_key(s)`). |
| HLS AES-128 | `hlsenc.c` (write: `hls_key_info_file`/`hls_enc_key`/`hls_enc_iv`), `hls.c` (read) | Whole-segment AES-128-CBC per RFC 8216 §4.3.2.4 (`#EXT-X-KEY:METHOD=AES-128`). |
| HLS Sample-AES | `hls_sample_encryption.h`/`.c`, `hls.c`/`hlsenc.c` | Apple's per-sample AES-CBC scheme (`METHOD=SAMPLE-AES`), ID3-timed-metadata-carried IVs for audio, NAL-structure-aware encryption for video. |
| SRTP | `srtpproto.c`, `libavutil` AES/crypto primitives | RFC 3711; keys/suite passed explicitly via `srtp_in_params`/`srtp_out_params` (no in-band DTLS-SRTP key exchange in this protocol object — that's handled at a higher layer, e.g. `whip.c`, which does perform DTLS-SRTP). |
| Generic stream decryption | `crypto.c` protocol | AES-128/256-CTR wrapper around any nested URL, keyed via `key`/`iv` options. |
| RTMPE / legacy RTMP encryption | `rtmpcrypt.c`/`rtmpdh.c` | Adobe's proprietary Diffie-Hellman-based RTMPE handshake (reverse-engineered). |

### 5.5 `avpriv` shared helper / cross-format dependency map

| Helper module | Provides | Consumed by (representative) |
|---|---|---|
| `riff.c`/`riff.h`/`riffdec.c`/`riffenc.c` | RIFF chunk I/O helpers; `ff_codec_bmp_tags`/`ff_codec_bmp_tags_unofficial` (Video-for-Windows FOURCC↔`AVCodecID` table), `ff_codec_wav_tags` (WAVE format-tag↔`AVCodecID` table) | `avi`, `wav`, `asf` (borrows BMP/WAV tag tables), `matroska` (codec-tag fallback), `swf`, any RIFF-shaped format |
| `isom.c`/`isom.h`/`isom_tags.c` | MOV/MP4 atom-tree walking helpers, `ff_mov_obj_type`/codec-tag tables (`ff_codec_movaudio_tags`, `ff_codec_movvideo_tags`, `ff_codec_movsubtitle_tags`), object-type/`esds` constants | `mov`/`movenc`, `mxf` (partial reuse for wrapped MPEG-4 codec IDs) |
| `mpegts.h` | Stream-type constants, PSI table structure definitions (PAT/PMT/SDT/NIT ID space) | `mpegts.c`/`mpegtsenc.c`, `mpegts`-in-RTP (`rtpenc_mpegts.c`), `hls`/`dash` when segmenting to TS |
| `mpeg.h` | Shared MPEG-PS pack/system-header constants | `mpeg.c` (mpegps demux), `mpegenc.c` (PS/VCD/SVCD/DVD mux family) |
| `avlanguage.c`/`avlanguage.h` | ISO 639-1/2 language-code normalization | Matroska, MP4, MXF, ASF language tags |
| `replaygain.c`/`replaygain.h` | ReplayGain side-data parsing/emission | `wav`, `mp3`, `ape`, `flac` |
| `dovi_isom.c`/`dovi_isom.h` | Dolby Vision configuration-box (`dvcC`/`dvvC`) parsing/emission | `mov`/`movenc`, `matroska` |
| `vpcc.c`/`vpcc.h` | VP8/VP9 `vpcC` configuration-box helpers | `mov`/`movenc`, `webm` |
| `nal.c`/`nal.h`, `hevc.c`/`hevc.h`, `av1.c`/`av1.h` | Length-prefixed/Annex-B NAL and OBU extraction helpers shared between container writers and the corresponding raw elementary-stream demuxers | `movenc` (`avcC`/`hvcC`/`av1C` boxes), `matroskaenc`, raw `h264`/`hevc`/`obu` demuxers |
| `id3v2.c`/`id3v2.h`, `id3v2enc.c` | See §5.3 | `mp3`, `aiff`, `wav`, `asf`(read), any format with `FF_INFMT_FLAG_ID3V2_AUTO` |
| `apetag.c`/`apetag.h` | APEv1/v2 tag read/write | `ape`, `wv`, `mpc`, `tta` |
| `flac_picture.c`/`flac_picture.h`, `vorbiscomment.c`/`vorbiscomment.h` | FLAC `METADATA_BLOCK_PICTURE` and Vorbis-comment codecs shared by both native and Ogg-wrapped variants | `flac`, `ogg` (Vorbis/Opus/FLAC-in-Ogg) |
| `spdif.h` + `spdifenc.c`/`spdifdec.c` | IEC 61937 non-PCM-over-S/PDIF burst-preamble framing | `spdif` mux/demux, `s337m` demux (shares the burst-preamble concept) |
| `rawutils.c`/`rawutils.h` | Raw-pixel-format tag tables (`v210`-style FOURCC ↔ `AVPixelFormat`) | `movenc` (`colr`/raw-video FOURCCs), `mov`, `rawvideo`/`v210`/`v210x` demuxers |


---

## 6. SIZE OF LARGEST FORMAT IMPLEMENTATIONS

Approximate line counts (`wc -l`) of the 25 largest genuine (de)muxer implementation files (core infra — `demux.c`, `mux.c`, `avio.c`, `aviobuf.c`, `format.c`, `options_table.h`, `protocols.c` — and pure-protocol files are excluded; RTSP/HLS/DASH/MPEG-TS are included since they are simultaneously format parsers):

| Rank | File | Lines | Format(s) |
|---|---|---|---|
| 1 | `mov.c` | ~12,528 | MOV/MP4/QuickTime demuxer (by far the largest single format parser — ISOBMFF box tree is extremely deep) |
| 2 | `movenc.c` | ~9,570 | MOV/MP4/ISMV/F4V/PSP/3GP/3G2/iPod muxer family |
| 3 | `matroskadec.c` | ~5,050 | Matroska/WebM demuxer |
| 4 | `mxfdec.c` | ~4,377 | MXF demuxer |
| 5 | `mpegts.c` | ~3,902 | MPEG-TS demuxer (+ PSI table parsing) |
| 6 | `matroskaenc.c` | ~3,800 | Matroska/WebM muxer |
| 7 | `mxfenc.c` | ~3,759 | MXF muxer (+ D-10/OP-Atom variants) |
| 8 | `hlsenc.c` | ~3,217 | HLS muxer |
| 9 | `hls.c` | ~3,190 | HLS demuxer |
| 10 | `rtsp.c` | ~2,875 | RTSP/RTP/SDP session + transport layer |
| 11 | `dashdec.c` | ~2,570 | DASH demuxer |
| 12 | `mpegtsenc.c` | ~2,471 | MPEG-TS muxer |
| 13 | `dashenc.c` | ~2,320 | DASH muxer |
| 14 | `avidec.c` | ~2,050 | AVI demuxer |
| 15 | `flvdec.c` | ~2,029 | FLV/`live_flv`/`kux` demuxer |
| 16 | `dvdvideodec.c` | ~1,868 | DVD-Video (libdvdnav/libdvdread-backed) demuxer |
| 17 | `asfdec_o.c` | ~1,666 | ASF demuxer (object-based reader) |
| 18 | `asfdec_f.c` | ~1,638 | ASF demuxer (frame-based reader) |
| 19 | `flvenc.c` | ~1,560 | FLV muxer |
| 20 | `sbgdec.c` | ~1,547 | SBaGen binaural-beat script demuxer (large due to embedded expression/synthesis logic) |
| 21 | `rmdec.c` | ~1,432 | RealMedia/`ivr`/`rdt` demuxer |
| 22 | `hevc.c` | ~1,413 | Shared HEVC bitstream helpers (NAL/VPS/SPS/PPS parsing used by MOV/MKV/raw-HEVC) |
| 23 | `mpegenc.c` | ~1,403 | MPEG-1/2 Program Stream muxer family (`mpeg`/`vcd`/`dvd`/`svcd`/`vob`) |
| 24 | `iamf_parse.c` | ~1,322 | IAMF (Immersive Audio Model and Formats) parsing helpers |
| 25 | `nutdec.c` | ~1,320 | NUT demuxer |

For reference, the core (non-format) infrastructure is comparable in scale: `demux.c` (~3,213), `mux.c` (~1,436), `aviobuf.c` (~1,484). `rtmpproto.c` (~3,264) and `http.c` (~2,401) are protocol implementations, not container parsers, and are excluded from the ranking above despite being larger than most.

