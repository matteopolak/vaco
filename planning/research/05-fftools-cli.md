I could not write the target file directly — my tool access in this task is strictly read-only (no file-creation/editing tools). Below is the complete compatibility-contract content for `planning/research/05-fftools-cli.md`; please have the orchestrating session write this to that path.

> **CORRECTIONS — see `planning/14-cli.md` §6.** This document was compiled by reading FFmpeg 8.0.git
> documentation and option tables. Subsequent black-box runs against a reference `ffprobe`/`ffmpeg`
> **8.1** binary found several statements here to be wrong. Where they conflict, the observed
> behaviour in `14-cli.md` wins:
> - The root child order given in §3.5 is incorrect; the observed order matches the XSD.
> - `-show_entries 'format='` emits nothing at all in the default writer, not a bare section header.
> - `-byte_binary_prefix` is a no-op in 8.1.
> - `packets_and_frames` does not appear in `-sections` output.
> - `-stats` has gained an `elapsed=` field.
>
> Also note the **version skew**: the source tree is 8.0.git, the available binary is 8.1. The
> differential harness must pin one reference version explicitly and record it, or these differences
> will surface as spurious failures. See D6.


Source snapshot: FFmpeg git HEAD `564f92cce23ae95399476617b8a1dc357f002a47` (2026-08-19), at `~/repos/FFmpeg`.

---

# FFmpeg fftools CLI Compatibility Contract (ffmpeg / ffprobe / ffplay)

Source: `~/repos/FFmpeg`, commit `564f92cce23ae95399476617b8a1dc357f002a47`. Derived from `doc/ffmpeg.texi`, `doc/ffprobe.texi`, `doc/ffplay.texi`, `doc/fftools-common-opts.texi`, `doc/filters.texi` (syntax chapters only), `doc/utils.texi`, `doc/codecs.texi`, `doc/ffprobe.xsd`, and the `fftools/*.c/h` source (option tables and section-schema tables only — no source logic reproduced).

## 0. File inventory and structural complexity (item 6)

| File | Lines | Role |
|---|---:|---|
| `fftools/ffplay.c` | 3982 | ffplay: full player (demux/decode/AV sync/SDL render/input) |
| `fftools/ffprobe.c` | 3544 | ffprobe: probing + all output writers' section emission |
| `fftools/ffmpeg_mux_init.c` | 3660 | ffmpeg: output file/stream setup, per-stream option resolution (huge AVOption/codec-option wiring surface) |
| `fftools/ffmpeg_filter.c` | 3605 | ffmpeg: simple + complex filtergraph construction/management |
| `fftools/ffmpeg_sched.c` | 2834 | ffmpeg: inter-thread scheduler (demux→decode→filter→encode→mux orchestration) |
| `fftools/ffmpeg_demux.c` | 2584 | ffmpeg: demuxer thread, input option resolution |
| `fftools/ffmpeg_opt.c` | 2110 | ffmpeg: CLI option table, per-file/per-stream option groups |
| `fftools/ffmpeg_dec.c` | 1795 | ffmpeg: decoder thread(s), loopback decoders |
| `fftools/cmdutils.c` | 1639 | shared: option parsing engine, `-h` help system |
| `fftools/opt_common.c` | 1516 | shared: `-formats/-codecs/-filters/...` listing implementations |
| `fftools/ffmpeg.h` | 975 | ffmpeg: core data-structure declarations |
| `fftools/ffmpeg_enc.c` | 1103 | ffmpeg: encoder thread |
| `fftools/ffplay_renderer.c` | 890 | ffplay: SDL/vulkan(libplacebo) rendering backend |
| `fftools/ffmpeg_mux.c` | 887 | ffmpeg: muxer thread |
| `fftools/sync_queue.c` | 684 | ffmpeg: `-shortest`/interleaving sync-queue support |
| `fftools/ffmpeg.c` | 1072 | ffmpeg: `main()`, top-level transcode loop, exit codes |
| `fftools/ffmpeg_hw.c` | 319 | ffmpeg: `-init_hw_device`/`-hwaccel` device management |
| `fftools/thread_queue.c` | 268 | shared: generic MPMC queue backing the scheduler |
| `fftools/graph/graphprint.c` | — | ffmpeg: `-print_graphs*` execution-graph dump (default/compact/csv/flat/ini/json/xml/**mermaid**/mermaidhtml) |
| `fftools/textformat/*` | ~4337 total | shared: `AVTextFormatContext` writer framework backing both ffprobe's `-output_format` writers (default/compact-csv/flat/ini/json/xml) and ffmpeg's `-print_graphs_format` |

**Structurally hardest parts** (most implementation risk for a reimplementation):
1. **`ffmpeg_sched.c`** — the scheduler: coordinates arbitrarily many demuxer/decoder/filtergraph/encoder/muxer threads via `thread_queue`, handles backpressure, `-shortest`, sync-queues, loopback decoders, and trailing/flush ordering. This is the "hardest" component — it's the concurrency core with no single linear control flow.
2. **`ffmpeg_filter.c`** — simple vs. complex filtergraph management: reconciling `-vf/-af` (per-output-stream, auto-inserted) against `-filter_complex`/`-lavfi` (standalone graphs with link-label resolution, unlabeled-pad auto-connection rules, loopback-decoder `[dec:N]` inputs, view-specifier handling for multiview, auto format-conversion filter insertion, `-reinit_filter`/`-drop_changed` reinit semantics).
3. **Stream selection** (`ffmpeg_opt.c` + `ffmpeg_demux.c` + `ffmpeg_mux_init.c`) — the automatic-vs-manual `-map` resolution rules, default-stream picking heuristics, disposition defaulting, and interaction with unlabeled filtergraph outputs (see §3.9). The rules are simple to state but have many interacting edge cases (see the four worked examples in §3.9.5).
4. **`ffmpeg_mux_init.c`** — the single largest ffmpeg file; it's where nearly every per-stream option (codec, tag, bitstream filters, disposition, metadata, timebases, encoder AVOptions) gets resolved and validated against the chosen muxer/encoder, so it acts as the de facto option-semantics ground truth.

---

## 1. Common options (all three tools) — `doc/fftools-common-opts.texi`

Shared parsing conventions:
- Numeric options accept SI suffixes: `K`, `M`, `G`; append `i` for binary multiples (1024-based) instead of decimal (1000-based); append `B` to multiply by 8. E.g. `KB`, `MiB`, `G`, `B`.
- Boolean/flag options take no argument and set true; prefix with `no` to set false (e.g. `-nostats`).
- **File-value indirection**: prefixing an option name with `/` (right after the leading dash) makes the CLI argument a *path* whose file contents are the actual option value. E.g. `-/filter:v filter.script`.

### 1.1 Generic options (`-L`, `-h`, listing options, etc.)

| Option | Meaning |
|---|---|
| `-L`, `-license` | Show license. |
| `-h`, `-?`, `-help`, `--help [arg]` | Show help. No `arg` → basic (non-expert) options only. |
| `-version` | Show version. |
| `-buildconf` | Show build configuration, one option per line. |
| `-formats` | Show demuxers+muxers (formats) including devices. |
| `-demuxers` / `-muxers` / `-devices` | List demuxers / muxers / devices. |
| `-codecs` | Show all codecs known to libavcodec ("codec" ≈ bitstream format). |
| `-decoders` / `-encoders` | List decoders / encoders. |
| `-bsfs` | List bitstream filters. |
| `-protocols` | List protocols. |
| `-filters` | List libavfilter filters. |
| `-pix_fmts` / `-sample_fmts` / `-layouts` / `-dispositions` / `-colors` | List pixel formats / sample formats / channel layouts & names / disposition flags / recognized color names. |
| `-sources device[,opt=val...]` / `-sinks device[,opt=val...]` | List autodetected sources/sinks of an input/output device. |
| `-hwaccels` | List hardware acceleration methods built into this ffmpeg. |
| `-loglevel [flags+]level` / `-v ...` | See §1.2. |
| `-report` | Dump full commandline + log to `PROGRAM-YYYYMMDD-HHMMSS.log`; implies `-loglevel debug`. Also triggerable via env var `FFREPORT` (`:`-separated `key=value`; keys `file`, `level`). |
| `-hide_banner` | Suppress the copyright/build/library banner. |
| `-cpuflags flags` (global) | Set/clear CPU feature flags for testing (`-cpuflags -sse+mmx`, `-cpuflags mmx`, `-cpuflags 0`). Flag sets: x86 (`mmx, mmxext, sse, sse2, sse2slow, sse3, sse3slow, ssse3, atom, sse4.1, sse4.2, avx, avx2, xop, fma3, fma4, 3dnow, 3dnowext, bmi1, bmi2, cmov`), ARM (`armv5te, armv6, armv6t2, vfp, vfpv3, neon, setend`), AArch64 (`armv8, vfp, neon`), PowerPC (`altivec`), specific processors (`pentium2/3/4, k6, k62, athlon, athlonxp, k8`). |
| `-cpucount count` (global) | Override detected CPU count (testing). |
| `-max_alloc bytes` | Max single heap allocation size for ffmpeg's malloc family. Default `INT_MAX`. |

### 1.2 `-h` help topics

`-h [arg]` where `arg` ∈:
- `long` — advanced options too.
- `full` — every option including shared/private options of encoders/decoders/demuxers/muxers/filters etc.
- `decoder=NAME`, `encoder=NAME`, `demuxer=NAME`, `muxer=NAME`, `filter=NAME`, `bsf=NAME`, `protocol=NAME` — detailed info about that one component (`NAME` from the corresponding list option).

### 1.3 `-loglevel` / `-v`

Syntax: `[flags+]loglevel`. Flags may be given standalone with `+`/`-` prefix to toggle without changing level (`-loglevel +repeat`). When combining, a `+` separator is required between the last flag and the level.

Flags: `repeat` (don't compress repeated lines), `level` (prefix each line with `[level]`), `time` (prefix with time), `datetime` (prefix with date+time).

Levels (`name, numeric`): `quiet -8`, `panic 0`, `fatal 8`, `error 16`, `warning 24`, `info 32` (default), `verbose 40`, `debug 48`, `trace 56`.

Color: auto-detected terminal support; force off via env `AV_LOG_FORCE_NOCOLOR`, force on via `AV_LOG_FORCE_COLOR`.

### 1.4 AVOption-driven generic options & the `-opt`/`-opt:stream_specifier` mechanism

- AVOptions come from libavformat/libavdevice/libavcodec and are exposed directly as CLI flags (`-option value`). Two categories:
  - **generic** — settable on any container/codec/device (e.g. `AVFormatContext`/`AVCodecContext` fields).
  - **private** — specific to one given container/device/codec (e.g. muxer's `id3v2_version`).
- Example: `ffmpeg -i input.flac -id3v2_version 3 out.mp3` (private muxer option).
- All codec AVOptions are inherently per-stream and should carry a stream specifier: `-c:a:0 ac3 -b:a:0 640k -ac:a:1 2 -c:a:1 aac -b:2 128k`.
- Boolean AVOptions do **not** support the `-nooption` shorthand (that's only for the fftools' own boolean CLI switches) — use `-option 0`/`-option 1`.
- The historical undocumented `v/a/s`-prefixed per-stream AVOption naming (e.g. `-vb`) is obsolete/going away; use `-b:v` etc.
- Selected generic `AVCodecContext` options relevant to item 3 (`doc/codecs.texi`, table `Codec Options`): `b` (bitrate, default 200K), `ab` (audio bitrate, deprecated in favor of `b:a`), `bt` (bitrate tolerance), `g` (GOP size, default 12), `qmin`/`qmax`/`qdiff`/`qcomp`/`qblur` (VBR quantizer controls), `bf` (max B-frames, -1..16, 0=disabled), `maxrate`/`minrate` (require `bufsize` to be set for `maxrate`), `bufsize` (ratecontrol buffer bits), `i_qfactor`/`i_qoffset`/`b_qfactor`/`b_qoffset`, `profile` (default `unknown`), `level` (default `unknown`), `strict` (`very/strict/normal/unofficial/experimental`), `flags` (`mv4, qpel, loop, qscale, pass1, pass2, gray, psnr, truncated, drop_changed, ildct, low_delay, global_header, bitexact, aic, ilme, cgop, output_corrupt`), `err_detect`, `bug`. `top` is **not** a generic codec option — it's a private boolean option of the `rawvideo` decoder ("top field first" override), used e.g. for raw DV import.

### 1.5 Stream specifiers (full grammar) — item 2

A stream specifier is generally a `:`-suffixed string appended to an option name, e.g. `-codec:a:1 ac3` (`a:1` = second audio stream). An empty specifier (`-codec: copy` / bare `-codec copy`) matches all streams. A specifier can match multiple streams (option applies to all matches).

Grammar (all forms; may be combined as shown):

1. **`stream_index`** — matches the stream with this absolute index (index basis: libavformat's ordering, *unless* a group/program specifier also constrains it, in which case indexing is relative to that group/program). If used as an *additional* specifier appended after another form, it selects the Nth stream *among the already-matched streams* rather than by absolute index.
2. **`stream_type[:additional_specifier]`** — `stream_type` ∈ `v` (video), `V` (video excluding attached pictures/thumbnails/cover art), `a` (audio), `s` (subtitle), `d` (data), `t` (attachments). Optional trailing `:additional_specifier` further restricts to streams matching both.
3. **`g:group_specifier[:additional_specifier]`** — matches streams in the given stream group. `group_specifier` is one of:
   - `group_index` — group by index.
   - `#group_id` or `i:group_id` — group by id.
4. **`p:program_id[:additional_specifier]`** — matches streams in the given program (by id).
5. **`#stream_id` or `i:stream_id`** — match by container-level stream id (e.g. MPEG-TS PID).
6. **`m:key[:value]`** — match streams carrying metadata tag `key` (optionally with exact `value`). `:` inside `key`/`value` must be backslash-escaped. In `ffmpeg`, metadata matching is reliable only for **input** files.
7. **`disp:dispositions[:additional_specifier]`** — match streams having ALL of the given disposition flag(s), `+`-joined (flag names as printed by `-dispositions`, see §3.7 list).
8. **`u`** — matches streams with a "usable" configuration (codec defined, and essential info such as video dimensions or audio sample rate present).

Combination/chaining rule: several of the above forms can be concatenated with `:` (e.g. `g:0:a:m:language:eng`); each additional segment further restricts the match set of the preceding segment. Numeric `stream_index` used as a trailing segment re-indexes *within* the already-narrowed match set rather than globally.

---

## 2. ffmpeg CLI — exhaustive option catalogue (item 3)

Synopsis: `ffmpeg [global_options] {[input_file_options] -i input_url} ... {[output_file_options] output_url} ...`

General placement rule: options apply to the **next** specified file (input or output) and are reset between files; exceptions are options tagged `(global)`. Input files must all precede output files.

### 2.1 Execution model (conceptual)

Pipeline stages, each an explicit component instance:
- **Demuxers** — one per `-i`; read global metadata/chapters + N elementary streams; emit packets per stream to decoders and/or muxers.
- **Decoders** — packets → raw frames (video: pixel arrays; audio: PCM). Normally bound to a demuxer stream; **loopback decoders** (`-dec input_index`, see §2.12) instead decode an *encoder's* output for feedback into a complex filtergraph, indexed 0,1,2,... in declaration order; decoding AVOptions for them go before `-dec`.
- **Filtergraphs** — frames → frames. *Simple* graphs (`-filter`/`-vf`/`-af`) are 1-in/1-out and bound to one output stream (multiple audio outputs ⇒ one filtergraph per `-af` instance). *Complex* graphs (`-filter_complex`/`-lavfi`, global) are standalone, N-in/M-out, of any media type per port.
- **Encoders** — frames → packets, bound to one muxer output stream; video/audio encoders read from a filtergraph output, subtitle encoders read straight from a decoder (no subtitle filtering support).
- **Muxers** — packets (from encoders = transcode path, or straight from demuxers = streamcopy path) → interleaved container bytes.

**Streamcopy**: a stream mapped with `-c copy` skips decode/filter/encode entirely — packets pass demuxer→muxer unchanged. Fast, lossless, but incompatible with filtering and may fail if the target container needs info the source lacks.

**Transcoding**: decode→(filter)→encode. Used when filtering is needed or the target requires a different codec. ffmpeg transcodes any stream not given `-c copy`.

Orchestration is handled at runtime by the **scheduler** (`ffmpeg_sched.c`) which runs each component on its own thread and manages backpressure/queueing between them (see §0).

### 2.2 Stream selection (automatic vs. manual) — full rules (item 3)

- **No `-map` for an output** → automatic selection: for each stream type the output format supports (video/audio/subtitle), ffmpeg picks **one** stream across all inputs:
  - video: highest resolution: ties broken by lowest input/stream index.
  - audio: most channels; ties → lowest index.
  - subtitle: first subtitle stream found, but **only** of the type (text vs image) matching the output format's default subtitle encoder's type.
  - data/attachment streams are **never** auto-selected; only `-map` can include them.
- **`-map` present for an output** → only explicitly mapped streams are included (plus unlabeled complex-filtergraph outputs, see below); automatic selection is fully disabled for that output.
- **Complex filtergraph outputs**: unlabeled output pads are auto-added to the **first** output file (fatal error if the type is unsupported there); their presence *skips* automatic selection of that stream type for that output (but does not skip it if `-map` is also given — then they're additive). Labeled output pads must be mapped **exactly once** (unmapped label = error; mapped twice = error).
- **`-vn`/`-an`/`-sn`/`-dn`** suppress automatic/manual mapping of that type but never suppress filtergraph-produced streams reaching the output via unlabeled pads.
- **Codec/stream-handling independence**: `-codec`/`-c` selection happens *after* stream selection and doesn't influence it, **except** subtitles: if any subtitle encoder is explicitly set for an output, the first subtitle stream of *any* type (text or image) is selected regardless of encoder compatibility (ffmpeg does not pre-validate compatibility; failure aborts *all* outputs).
- If no `-codec` given for a selected stream type, the output muxer's registered default encoder for that type is used.

### 2.3 Main options

| Option | Applies to | Semantics |
|---|---|---|
| `-f fmt` | input/output | Force container format (usually auto-detected). |
| `-i url` | input | Add an input. |
| `-y` / `-n` | global | Overwrite outputs without asking / never overwrite (exit if exists). |
| `-stream_loop number` | input | Loop input N times; `0`=no loop, `-1`=infinite. |
| `-recast_media` | global | Allow forcing a decoder of a different media type than detected (e.g. decode data streams as audio/video). |
| `-c[:spec]` / `-codec[:spec] codec` | input/output, per-stream | Decoder (before `-i`) or encoder (before output) selection; `copy` = streamcopy (output only). Last matching `-c` wins per stream. |
| `-t duration` | input/output | Input: limit read duration. Output: stop once written duration reached. Mutually exclusive with `-to` (`-t` wins if both given). |
| `-to position` | input/output | Stop reading/writing at absolute position; loses to `-t` if both given. |
| `-fs limit_size` | output | File size limit in bytes (soft; output may slightly exceed it). |
| `-ss position` | input/output | Input: seek (inexact; nearest seek point ≤ position; with `-accurate_seek`, default on, the gap is decoded+discarded when transcoding, preserved on streamcopy or `-noaccurate_seek`). Output: decode-and-discard until reaching position. |
| `-sseof position` | input | Like `-ss` but relative to EOF (negative = earlier; 0 = EOF). |
| `-isync input_index` | input | Sync this input's timestamps to another input's start time delta. `-1` (default) = no sync; can't chain (a sync source can't itself be synced); requires `-start_at_zero` if `-copyts` set. |
| `-itsoffset offset` | input | Add a fixed offset to input timestamps (delay). |
| `-itsscale scale` | input, per-stream | Rescale input timestamps by float `scale`. |
| `-timestamp date` | output | Set container recording timestamp (date syntax, §4). |
| `-metadata[:metadata_specifier] key=value` | output, per-metadata | Set metadata; empty value deletes; overrides `-map_metadata`. Specifier forms: none/`g` (global), `s[:stream_spec]`, `c:chapter_index`, `p:program_index`. |
| `-keep_metadata[:metadata_specifier] key` | output, per-metadata | Prevent auto-discard of normally-stripped stale metadata keys (e.g. `iTunSMPB`, `encoded_by`, `NUMBER_OF_BYTES`) on re-encode; `g`/`s[:spec]` restrict scope; suffixed keys (`KEY-eng`) matched by family unless the full key is given; keys like `major_brand`/`minor_version`/`compatible_brands` are always stripped regardless. Applies only to format/stream metadata (not chapters/programs). |
| `-disposition[:spec] value` | output, per-stream | Set disposition flags. Default: copied from input unless fed by a complex filtergraph. `value` = `+`/`-`-joined flag list; leading `+`/`-` = update default, otherwise = absolute set; `0` clears. If no `-disposition` given at all, ffmpeg auto-sets `default` on the first stream of each type when there are ≥2 streams of that type and none already marked default. See §3.7 for the flag list (`-dispositions`). |
| `-program [title=T:][program_num=N:]st=S[:st=S...]` | output | Create a program with given streams. |
| `-stream_group [map=in_id=grp][type=T:]st=S[:st=S][:stg=S][:id=ID...]` | output | Create/derive a stream group. `type` ∈ `iamf_audio_element`, `iamf_mix_presentation` (each with a rich sub-option grammar — audio_element_type, demixing, recon_gain, layer/ch_layout/flags/output_gain, submix/element/layout/annotations, etc., all `,`/`:`/`\|`-delimited — see `doc/ffmpeg.texi` lines 1030–1250 for the full IAMF grammar). |
| `-reinit_opts[:spec] pts=P\|key=val[:force_reinit=1][:key=val,pts=P...]` | output, per-stream | Reconfigure/reinitialize an encoder at a given output PTS (AV_TIME_BASE units); `force_reinit` forces full reinit instead of soft reconfigure. |
| `-target type` | output | `vcd`/`svcd`/`dvd`/`dv`/`dv50`, optionally prefixed `pal-`/`ntsc-`/`film-`; auto-applies a documented bundle of format/codec/bitrate/size/rate options per standard (full per-target values in `doc/ffmpeg.texi` §"Main options"/`-target`); user-supplied options override target defaults. |
| `-dn` | input/output | Input: block all data streams from filtering/auto-select/map. Output: disable data auto-selection/recording. |
| `-dframes number` | output | Obsolete alias for `-frames:d`. |
| `-frames[:spec] framecount` | output, per-stream | Stop stream after N frames. |
| `-q[:spec]` / `-qscale[:spec] q` | output, per-stream | Fixed quality scale (VBR), codec-dependent meaning. Bare `-qscale` (no specifier) applies to video only (compat). |
| `-filter[:spec] filtergraph` | output, per-stream | Simple filtergraph (1-in/1-out, labels `in`/`out` implicit). See §2.11. |
| `-reinit_filter[:spec] bool` | input, per-stream | (default on) reinit the filtergraph on mid-stream frame-parameter changes (resolution/pix_fmt for video; sample_fmt/rate/channels/layout for audio); loses filter state (e.g. `n`) and buffered frames on reinit. |
| `-drop_changed[:spec] bool` | input, per-stream | (default off) drop frames with changed parameters instead of reinitializing. |
| `-filter_threads nb` | global | Thread-pool size per filter pipeline; default = CPU count. |
| `-filter_buffered_frames nb` | global | Max buffered frames across a filtergraph before aborting; `0` (default) = unlimited. |
| `-pre[:spec] preset_name` | output, per-stream | Apply an avpreset. |
| `-stats` / `-nostats` | global | Progress/statistics logging at info level; on by default. |
| `-stats_period time` | global | Update period for `-stats`; default 0.5s. |
| `-print_graphs` / `-print_graphs_file file` / `-print_graphs_format fmt` | global | Dump execution-graph details; formats: `default, compact, csv, flat, ini, json, xml, mermaid, mermaidhtml` (default `json`). |
| `-progress url` | global | Write machine-readable `key=value` progress lines periodically (period = `-stats_period`) and at the end; final key of each block is `progress=continue`/`progress=end`. |
| `-stdin` / `-nostdin` | global | Enable/disable stdin interactivity; on by default unless stdin is itself an input. |
| `-debug_ts` | global | Print timestamp/latency debug info (unstable format; not for scripts). |
| `-attach filename` | output | Add an attachment stream (e.g. font for subtitle rendering); appended after all other streams; needs `-metadata:s:N mimetype=...` for Matroska. |
| `-dump_attachment[:spec] filename` | input, per-stream | Extract a matching attachment (or any stream's codec extradata) to file; empty filename → use the stream's `filename` tag. |

### 2.4 Video options

| Option | Notes |
|---|---|
| `-vframes number` (output) | Obsolete alias for `-frames:v`. |
| `-r[:spec] fps` (input/output, per-stream) | Input: override/ignore container timestamps, generate at constant `fps` (distinct from `-framerate`, used by grabbers/image2). Output+encode: duplicate/drop frames to hit constant `fps`. Output+streamcopy: just tags the muxer with `fps` (no dropping); mismatch vs. real packet timestamps can produce invalid files (see `setts` bsf). |
| `-fpsmax[:spec] fps` (output, per-stream) | Clamp auto-set output framerate; cannot combine with `-r`; ignored on streamcopy. |
| `-s[:spec] size` (input/output, per-stream) | Input: shortcut for demuxer `video_size` private option. Output: appends a `scale` filter at the **end** of the filtergraph (use `scale` filter directly for other placement). Format `WxH`. |
| `-aspect[:spec] aspect` (output, per-stream) | DAR as float or `num:den`; with `-vcodec copy` affects only container-level AR, not encoded-frame AR. |
| `-display_rotation[:spec] degrees` (input, per-stream) | Overrides rotation metadata; applied at filtering stage if transcoding+`-autorotate`, else written to output container if supported. |
| `-display_hflip[:spec]` / `-display_vflip[:spec]` (input, per-stream) | Display flip flags, applied after `-display_rotation`. |
| `-mastering_display[:spec] G(x,y)B(x,y)R(x,y)WP(x,y)L(x,y)` (input, per-stream) | HDR mastering-display metadata override (units: 1/50000 for primaries/WP, 1/10000 for luminance). |
| `-content_light[:spec] MaxCLL,MaxFALL` (input, per-stream) | HDR content-light-level metadata override. |
| `-vn` (input/output) | Block/disable video streams (see §2.2). |
| `-vcodec codec` (output) | Alias for `-codec:v`. |
| `-pass[:spec] n` (output, per-stream) | 2-pass encode; pass 1 typically `-an -f rawvideo -y /dev/null` (or `NUL`). |
| `-passlogfile[:spec] prefix` (output, per-stream) | 2-pass log prefix; default `ffmpeg2pass`; actual file `PREFIX-N.log`. |
| `-vf filtergraph` (output) | Alias for `-filter:v`. |
| `-autorotate` / `-noautorotate` | Auto-rotate per metadata; on by default. |
| `-autoscale` / `-noautoscale` | Auto-scale filtergraph output to first frame's resolution; on by default; disabling can yield mixed-resolution frames unsuitable for some encoders/muxers. |

Advanced video:

| Option | Notes |
|---|---|
| `-pix_fmt[:spec] format` (input/output, per-stream) | `+prefix` = error (not warn) + disable automatic filtergraph conversions if unselectable; bare `+` = force same pix_fmt as input/graph-output with conversions disabled. |
| `-sws_flags flags` (input/output) | Default flags for auto-inserted `scale` filters. |
| `-rc_override[:spec] "int,int,int/..."` (output, per-stream) | Rate-control override intervals: start-frame,end-frame,quantizer(+)/quality-factor(−). |
| `-vstats` / `-vstats_file file` / `-vstats_version {1,2}` | Video coding stats dump; default file `vstats_HHMMSS.log`; format v2 (default) adds `out=`/`st=` fields and `q=`...`f` suffix vs v1. Fields: `out, st, frame, q, PSNR, f_size, s_size(kB), time, br(kbits/s), avg_br(kbits/s)`. |
| `-vtag fourcc` (output) | Alias for `-tag:v`. |
| `-force_key_frames[:spec] ...` (output, per-stream) | See §2.13. |
| `-apply_cropping[:spec] source` (input, per-stream) | `none(0)` / `all(1, default)` / `codec(2)` / `container(3)` — which cropping metadata to auto-apply after decode. |
| `-copyinkf[:spec]` (output, per-stream) | On streamcopy, also copy leading non-key frames. |

### 2.5 Hardware acceleration

| Option | Notes |
|---|---|
| `-init_hw_device type[=name][:device[,key=val...]]` (global) | Create a named hw device. `type` ∈ `cuda` (device idx; `primary_ctx=1`), `dxva2` (D3D9 adapter idx), `d3d11va` (D3D11 adapter idx or `vendor_id=`), `vaapi` (X11 display / DRM render node / DirectX idx; or `kernel_driver=`/`vendor_id=` filters), `vdpau` (X11 display), `qsv` (`auto/sw/hw/auto_any/hw_any/hw2/hw3/hw4`, default `auto_any`; sub-options `child_device`, `child_device_type`), `opencl` (`platform_idx.device_idx`; filters `platform_profile/version/name/vendor/extensions`, `device_name/vendor/driver_version/version/profile/extensions/type`), `vulkan` (index or name-substring; options `debug`, `linear_images`, `instance_extensions`, `device_extensions`, `+`-joined). |
| `-init_hw_device type[=name]@source` | Derive a device from an existing named device. |
| `-init_hw_device list` | List supported hw device types in this build. |
| `-filter_hw_device name` (global) | Pass a named hw device to all filters in any graph (e.g. for `hwupload`/`hwmap`); global, all filters share it. |
| `-hwaccel[:spec] value` (input, per-stream) | `none` (default) / `auto` / `vdpau` / `dxva2` / `d3d11va` / `vaapi` / `qsv` (accelerated *transcode* without CPU copy — decoder+encoder must both support QSV, no filters allowed) / `videotoolbox`. No effect if unsupported by chosen decoder. |
| `-hwaccel_device[:spec] device` (input, per-stream) | Device to use for `-hwaccel`; refers to an `-init_hw_device`-created name, or implicitly runs `-init_hw_device type:device` first. |
| `-hwaccels` | List hwaccel methods built in (see §1.1). |
| `-qsv_device device` (expert, undocumented in texi) | Shortcut for `-init_hw_device qsv=__qsv_device:hw_any,child_device=device`. |
| `-vaapi_device device` (expert, undocumented in texi) | Shortcut for `-init_hw_device vaapi:device`. |
| `-fix_sub_duration_heartbeat[:spec]` | Mark an output video stream as the "heartbeat" driving subtitle push-through timing; requires `-fix_sub_duration` on the mapped input subtitle stream and that it feed the same output. |

### 2.6 Audio options

Main: `-aframes number` (output, obsolete alias `-frames:a`) · `-ar[:spec] freq` (input/output, per-stream; input use limited to grabbers/raw demuxers) · `-aq q` (output, alias `-q:a`) · `-ac[:spec] channels` (input/output, per-stream) · `-an` (input/output) · `-acodec codec` (alias `-codec:a`) · `-sample_fmt[:spec] fmt` (output, per-stream) · `-af filtergraph` (alias `-filter:a`).

Advanced: `-atag fourcc` (alias `-tag:a`) · `-ch_layout[:spec]` (alias of) `-channel_layout[:spec] layout` (input override / output default-from-input; not all decoders honor override) · `-guess_layout_max channels` (input, per-stream; cap channel-count for which unlabeled layouts are guessed; `0` disables guessing; explicit `-channel_layout` also disables it).

### 2.7 Subtitle options

Main: `-scodec codec` (alias `-codec:s`) · `-sn` (input/output).

Advanced: `-fix_sub_duration` (wait for next packet to compute accurate duration; adds latency/memory) · `-canvas_size size` (render canvas size for subtitle decode).

### 2.8 `-map` — full syntax (item 3)

```
-map [-]input_file_id[:stream_specifier][:view_specifier][:?] | [linklabel]
```

- **Source form 1** (from an input file): creates one output stream per matching input stream (all of `input_file_id` if no `stream_specifier`). Leading `-` = **negative mapping**: excludes matching streams from already-created mappings (doesn't add new ones).
- **View specifier** (multiview video only), appended after the stream specifier:
  - `view:view_id` (id, or `all` = interleave all views into one stream)
  - `vidx:view_idx` (0 = base view, 1 = first non-base, ...)
  - `vpos:position` (`left`/`right`)
  - Default for transcoding = `vidx:0` (base view only); view specifiers are unsupported for streamcopy (all views always copied).
- **Optional map** — trailing `?`: if the map matches zero streams, it's silently skipped (but an invalid *input index* is still a hard error).
- **Source form 2** (`[linklabel]`): map a complex-filtergraph output link label to the output file.
- Repeatable; each `-map` adds streams in commandline order; a given input stream may be mapped multiple times (e.g. to encode it two ways). Using `-map` at all disables automatic/default mapping for that output.
- Examples covered: map-all (`-map 0`), map specific index (`0:1`), multi-file selection, type+index (`0:v -map 0:a:2`), negative (`-map 0 -map -0:a:1`), optional (`0:a?`), by-metadata (`0:m:language:eng`).

Related mapping options:
- `-ignore_unknown` — skip unknown-type input streams instead of failing when copy is attempted on them.
- `-copy_unknown` — allow copying unknown-type input streams instead of failing.
- `-map_metadata[:metadata_spec_out] infile[:metadata_spec_in]` (output, per-metadata) — file indices, not names. Specifier forms: `g` (global), `s[:stream_spec]` (input: first match copied from; output: all matches copied to), `c:chapter_index`, `p:program_index`; omitted = global. Default: global metadata copied from first input, per-stream/per-chapter copied along with their streams/chapters — any explicit mapping of a type disables its default; negative file index = dummy mapping that just disables the default copy.
- `-map_chapters input_file_index` (output) — copy chapters from a given input; default = first input with ≥1 chapter; negative index disables.
- `-map_channel` — **removed from current FFmpeg** (grep of `fftools/*.c` for `map_channel` found no live option; it existed in older FFmpeg/avconv for per-channel remapping and no longer appears in `doc/ffmpeg.texi` or the option tables of this revision — treat as legacy/unsupported for the reimplementation unless targeting old compatibility).

### 2.9 Timestamps, sync, and rate control of timing (item 3)

| Option | Notes |
|---|---|
| `-copyts` | Keep input timestamps as-is (no sanitizing / no start-offset removal). Muxer processing (e.g. `avoid_negative_ts`) or `-fps_mode` may still alter output timestamps. |
| `-start_at_zero` | With `-copyts`, shift timestamps so the (first) output starts at 0; e.g. `-ss 50` then yields output starting at 50s regardless of source's own start offset. |
| `-copytb mode` | Encoder timebase source on streamcopy: `1` = demuxer timebase, `0` = decoder timebase, `-1` (default) = automatic/sane choice. |
| `-enc_time_base[:spec] timebase` (output, per-stream) | `0` (default) = media-type default (`1/framerate` video, `1/samplerate` audio); `demux` = demuxer timebase; `filter` = filtergraph timebase; or an explicit rational/decimal. |
| `-fps_mode[:spec] mode` (output, per-stream) | **Current name for the historical `-vsync`** (removed from current CLI; internal enum is still `VideoSyncMethod`). Modes: `passthrough` (pass timestamps as-is), `cfr` (dup/drop to exact constant fps), `vfr` (pass through / drop to avoid duplicate timestamps), `auto` (default; cfr or vfr per muxer capability). Muxer post-processing (e.g. `avoid_negative_ts`) may further adjust. Sync source per stream is controlled via which streams are mapped. |
| `-frame_drop_threshold value` | How far behind (in frame-rate units, default `-1.1`) a video frame can be before being dropped. |
| `-async` | **Removed from current CLI** — audio sync-to-video resampling is now done via the `aresample` filter's `async`/`min_hard_comp`/`first_pts` options in an explicit `-af aresample=...` chain, not a top-level flag. |
| `-apad params` (output, per-stream) | Equivalent to `-af apad`; requires `-shortest` on that output to matter. |
| `-dts_delta_threshold threshold` | Seconds; discontinuity-correction threshold for formats accepting discontinuity (`AVFMT_TS_DISCONT`, e.g. MPEG-TS/HLS); auto-disabled with `-copyts` unless wraparound detected. Default `10`. |
| `-dts_error_threshold threshold` | Seconds; for formats *not* accepting discontinuity — drops the PTS/DTS if the jump exceeds this. Default `3600*30` (30h). |
| `-shortest` (output) | End encode when the shortest output stream ends; may buffer frames (extra latency), bounded by `-shortest_buf_duration` (default 10s). |
| `-bitexact` (input/output) | Bit-exact (de)muxer and (de/en)coder mode. |
| `-muxdelay seconds` / `-muxpreload seconds` (output) | Max demux-decode delay / initial demux-decode delay. |

### 2.10 `-force_key_frames` full syntax (item 3)

```
-force_key_frames[:spec] time[,time...]
-force_key_frames[:spec] expr:EXPR
-force_key_frames[:spec] source
-force_key_frames[:spec] scd_metadata
```
- **`time[,time...]`**: times rounded to nearest output timestamp per encoder timebase (`-enc_time_base`); a coarse timebase may force the keyframe earlier than requested. Special token `chapters[delta]` expands to every chapter start ± `delta` seconds (e.g. `0:05:00,chapters-0.1`).
- **`expr:EXPR`**: evaluated per frame; non-zero ⇒ force keyframe. Constants: `n` (processed frame count), `n_forced`, `prev_forced_n` (NaN if none yet), `prev_forced_t` (NaN if none yet), `t` (current frame time). E.g. `expr:gte(t,n_forced*5)`.
- **`source`**: force keyframe when the *source* frame is itself flagged as a keyframe (if that source frame is dropped, the next available frame is forced instead).
- **`scd_metadata`**: force keyframe on frames carrying the `lavfi.scd.time` metadata key (produced by `scdet`/`scdet_vulkan` filters); avoid frame-duplicating filters downstream of `scdet` (duplicate metadata risk).
- Caution: excessive forced keyframes hurts encoder lookahead; prefer fixed-GOP options where possible.

### 2.11 Filtering options

- `-filter[:spec] filtergraph` (output, per-stream; anchor `filter_option`) — see §4 syntax; single in/out, labels `in`/`out` implicit; `-vf`/`-af` are aliases for `-filter:v`/`-filter:a`.
- `-filter_complex filtergraph` (global; anchor `filter_complex_option`) — arbitrary N-in/M-out graph; repeatable (each use = a new graph). Input link sources: `[file_index:stream_specifier]` (map syntax; first match if ambiguous; may carry a view specifier), `[dec:dec_idx]` (loopback decoder), or another complex graph's output label. Unlabeled input ⇒ connects to first unused stream of matching type. Output labels are consumed by `-map`; unlabeled outputs auto-add to the first output file. Special exception: a bitmap subtitle stream can be used directly as a complex-filter video input (converted to video sized to largest video stream, or 720x576 if none — experimental/temporary). Two complex graphs cannot be merged if doing so would create a transcoding cycle (output→encode→loopback-decode→same graph is fine across two separate `-filter_complex` invocations, not within one).
- `-filter_complex_threads nb` (global) — thread pool for `-filter_complex` graphs (mirrors `-filter_threads`); default = CPU count.
- `-lavfi filtergraph` (global) — alias for `-filter_complex`.
- `-auto_conversion_filters` / `-noauto_conversion_filters` (global) — auto-insert format-conversion filters (`scale`/`aresample`) wherever negotiation requires it, across `-vf/-af/-filter_complex/-lavfi`; on by default; disabling makes negotiation failures fatal unless the user inserts conversions manually.

### 2.12 Loopback decoders

`-dec output_stream_index` (a *directive*, not a keyed option) creates a new loopback decoder that decodes the *encoded output* of the given output stream index, indexed 0,1,2,... in declaration order for use as `[dec:N]` in a later `-filter_complex`. Decoding AVOptions for a loopback decoder are placed before its `-dec` directive, exactly like input-file options precede `-i`.

### 2.13 Stream mapping / muxing detail options

`-streamid output_stream_index:new_value` (output; e.g. reassign MPEG-TS PID) · `-bsf[:spec] filter[=opt=val:...][,filter2...]` (input/output, per-stream; comma-separated bitstream-filter chain; `,=:` in an option value must be backslash-escaped; applied on receipt from demuxer for input-side use, or right before muxer for output-side use) · `-tag[:spec] codec_tag` (input/output, per-stream) · `-timecode hh:mm:ssSEPff` (`SEP` = `:` non-drop, `;`/`.` drop) · `-thread_queue_size size` (input/output; input: max queued demux packets, forces a separate reader thread when set or when multiple inputs are given; output: max queued packets per muxing thread) · `-sdp_file file` (global; dump SDP info, needs ≥1 rtp output) · `-discard value` (input; `none`/`default` = discard nothing, `noref` = non-reference frames, `bidir` = B frames, `nokey` = all but keyframes, `all` = everything; whole-stream discard uses `all` on that stream) · `-max_muxing_queue_size packets` (output, per-stream; buffer size while waiting for the first packet of every transcoded stream) · `-muxing_queue_data_threshold bytes` (output, per-stream; default 50MB; threshold below which the queue-size limit isn't enforced) · `-bits_per_raw_sample[:spec] value` (output, per-stream; informational hint to encoder/muxer only) · `-stats_enc_pre[:spec]`/`-stats_enc_post[:spec]`/`-stats_mux_pre[:spec] path` (output, per-stream; per-frame/packet stats dump; `_fmt` variants take a `{directive}`-based format string; directives: `fidx, sidx, n, ni, tb, tbi, pts, ptsi, t, ti, dts(packet), dt(packet), sn(frame,audio), samp(frame,audio), size(packet), br(packet), abr(packet), key(packet)`; default format `{fidx} {sidx} {n} {t}`).

### 2.14 Error handling / robustness / reading rate

`-abort_on flags` (global; `empty_output` = no packets ever reached the muxer; `empty_output_stream` = no packets for some output stream) · `-max_error_rate rate` (global; float 0–1, default `2/3`; exceeding it does **not** stop processing but sets exit code **69**) · `-xerror` (global; stop+exit on error) · `-readrate speed` (input; float ≥0, `0`=unlimited, `1`≈`-re`; max media-seconds ingested per wallclock second) · `-re` (input; shorthand for `-readrate 1`) · `-readrate_initial_burst seconds` · `-readrate_catchup speed` (input; must be ≥ primary readrate; used to catch up after a stall) · `-debug_ts`, `-dump`, `-hex` (global; packet dump + optional payload hexdump) · `-benchmark` / `-benchmark_all` (global; end-of-run vs. per-step timing) · `-timelimit duration` (global; CPU user-time seconds).

### 2.15 Presets

Two systems: **ffpreset** (`.ffpreset` files, options `vpre/apre/spre/fpre`; search order `$FFMPEG_DATADIR`, `$HOME/.ffmpeg`, configured datadir, or `ffpresets/` beside the exe on Windows; lookup tries `ARG.ffpreset` then `CODEC-ARG.ffpreset`) and **avpreset** (`.avpreset` files, option `pre`; encoder-specific options only; search `$AVCONV_DATADIR`, `$HOME/.avconv`, datadir; lookup tries `CODEC-ARG.avpreset` then `ARG.avpreset`).

### 2.16 Exit codes (item 3)

From `fftools/ffmpeg.c: main()`:
- `0` — success (an internal `AVERROR_EXIT` is normalized to 0).
- `1` — usage errors: no input/output files given, or no output file given.
- `69` — `-max_error_rate` threshold exceeded (`FFMPEG_ERROR_RATE_EXCEEDED`); processing still completed to the extent possible.
- `255` — the process received ≥1 termination signal (`received_nb_signals` set by the SIGINT/SIGTERM handler; >3 repeated signals force an immediate abort).
- otherwise — the raw (possibly negative, OS-truncated) return value from `transcode()`/option parsing, e.g. AVERROR codes bubbled up as process exit status.

`ffprobe` (`fftools/ffprobe.c: main()`) returns `1` if the internal `ret` is negative (any failure, including "no input file specified"), else `0`. Per the manual: "a positive exit code is returned" if the URL can't be opened/probed.

---

## 3. ffprobe CLI — exhaustive (item 4)

Synopsis: `ffprobe [options] input_url`. Writes to stdout unless `-o output_url` given.

### 3.1 Main options

`-f format` (force input format) · `-unit` (show units) · `-prefix` (SI prefixes) · `-byte_binary_prefix` (force binary/1024 prefixes for byte values) · `-sexagesimal` (`HH:MM:SS.MICROSECONDS` time format) · `-pretty` (= `-unit -prefix -byte_binary_prefix -sexagesimal`) · `-output_format`/`-of`/`-print_format writer_name[=writer_options]` (select writer; §3.3) · `-sections` (print section-structure tree and exit) · `-select_streams stream_specifier` (restricts stream-related outputs: `-show_streams`, `-show_packets`, etc.) · `-show_data` (hexdump payload; with `-show_packets` dumps packet data, with `-show_streams` dumps codec extradata) · `-show_data_hash algorithm` (hash payload for packets/extradata) · `-data_dump_format {xxd(default), base64}` · `-show_error` (section `ERROR`) · `-show_format` (section `FORMAT`) · `-show_entries section_entries` (§3.4) · `-show_packets` (section `PACKET`) · `-show_frames` (sections `FRAME`/`SUBTITLE`) · `-show_log loglevel` (section `LOG`; requires `-show_frames`) · `-show_streams` (section `STREAM`) · `-show_programs` (section `PROGRAM_STREAM`) · `-show_stream_groups` (section `STREAM_GROUP_STREAM`) · `-show_chapters` (section `CHAPTER`) · `-count_frames` / `-count_packets` (per-stream frame/packet counts) · `-read_intervals read_intervals` (§3.5) · `-show_private_data`/`-private` (on by default; per-format/codec private data) · `-show_program_version` (section `PROGRAM_VERSION`) · `-show_library_versions` (section `LIBRARY_VERSION`) · `-show_versions` (= both of the above) · `-show_pixel_formats` (section `PIXEL_FORMAT`) · `-show_optional_fields value` (`always`/`1`, `never`/`0`, `auto`/`-1` (default); controls whether JSON/XML omit invalid/N-A fields) · `-analyze_frames` (populate `closed_captions`/`film_grain` stream fields by scanning frames up to the read interval; needs `-show_streams`) · `-bitexact` (build-independent output) · `-i input_url` · `-o output_url` · `-c:media_specifier codec_name` / `-codec:media_specifier codec_name` (force decoder; `media_specifier` ∈ `a,v,s,d`).

### 3.2 `-read_intervals` full syntax

```
INTERVAL  ::= [START|+START_OFFSET][%[END|+END_OFFSET]]
INTERVALS ::= INTERVAL[,INTERVALS]
```
- `START`/`END` absolute, or `+OFFSET` relative to current position; if `START` omitted, no seek is performed for that interval.
- `END` may be `#N` = read N packets (excluding flush packets) from the interval start instead of a time/position.
- No `START` given at all → read until end of input.
- Seeking is inexact — actual interval start may differ from requested; when an interval has a duration, the absolute end is computed from the *actual found* seek position, not the requested one.
- Examples: `10%+20,01:30%01:45`; `01:23%+#42`; `%+20` (first 20s); `%02:30` (start to 02:30).

### 3.3 Writers

All writers accept `string_validation`/`sv` (`fail`/`ignore`/`replace`, default `replace`) and `string_validation_replacement`/`svr` (default empty string).

| Writer | Aliases | Key options | Notes |
|---|---|---|---|
| `default` | — | `nokey`/`nk` (default 0), `noprint_wrappers`/`nw` (default 0) | `[SECTION]\nkey=val\n...\n[/SECTION]`; metadata lines prefixed `TAG:`. |
| `compact` | `csv` (different defaults) | `item_sep`/`s` (default `\|`, `,` for csv), `nokey`/`nk` (default 0, `1` for csv), `escape`/`e` (`c`/`csv`/`none`, default `c`, `csv` for csv writer), `print_section`/`p` (default 1) | One line per section: `section|key1=val1|...`; metadata key prefixed `tag:`. |
| `flat` | — | `sep_char`/`s` (default `.`), `hierarchical`/`h` (default 1) | Free-form `key=value` lines, e.g. `streams.stream.3.tags.foo=bar`; shell-escaped. |
| `ini` | — | `hierarchical`/`h` (default 1) | INI format; conventions: UTF-8, `.` subgroup sep, `\`-escaping, `#` comments, `=` kv separator. |
| `json` | — | `compact`/`c` (default 0) | Standard JSON sections. |
| `xml` | — | `fully_qualified`/`q` (default 0), `xsd_strict`/`x` (default 0, implies `fully_qualified`) | Schema: `doc/ffprobe.xsd` (also at `http://www.ffmpeg.org/schema/ffprobe.xsd`); XSD-compliance requires *no* `-unit/-prefix/-byte_binary_prefix/-sexagesimal`. |

### 3.4 `-show_entries` syntax

```
SECTION_ENTRY_NAME   (a field name local to a section)
LOCAL_SECTION_ENTRIES ::= SECTION_ENTRY_NAME[,LOCAL_SECTION_ENTRIES]
SECTION_ENTRY         ::= SECTION_NAME[=[LOCAL_SECTION_ENTRIES]]
SECTION_ENTRIES        ::= SECTION_ENTRY[:SECTION_ENTRIES]
```
- Section name with no `=` → all its entries + all nested sections printed.
- `=` with a non-empty entry list → only those entries.
- `=` with an **empty** list → no entries printed for that section (but the section header itself still emitted, per the container semantics).
- Section-entry order in the spec is not honored on output (usual display order is retained).
- Examples: `packet=pts_time,duration_time,stream_index : stream=index,codec_type`; `format : stream=codec_type`; `stream_tags : format_tags`; `stream_tags=title`.

### 3.5 Section hierarchy (full schema — exact names)

Root wraps (in this order): `chapters`, `format`, `frames`, `programs`, `stream_groups`, `streams`, `packets`, `error`, `program_version`, `library_versions`, `pixel_formats`.

```
root (wrapper, not printed as a real section)
├── chapters[] → chapter → chapter_tags(*)
├── format → format_tags(*)
├── frames[] → frame | subtitle
│    frame → frame_tags(*), frame_side_data_list[] → frame_side_data
│              → frame_side_data_timecode_list[] → frame_side_data_timecode
│              → frame_side_data_component_list[] → frame_side_data_component
│                  → frame_side_data_piece_list[] → frame_side_data_piece
│           → frame_logs[] → frame_log
├── packets_and_frames[]  (interleaved packet | frame | subtitle; numbered by type)
├── programs[] → program → program_tags(*), program_streams[] → program_stream
│                            → program_stream_disposition, program_stream_tags(*)
├── stream_groups[] → stream_group → stream_group_tags(*), stream_group_disposition,
│      stream_group_components[] → stream_group_component
│         → stream_group_side_data_list[] → stream_group_side_data
│         → stream_group_subcomponents[] → stream_group_subcomponent
│            → stream_group_pieces[] → stream_group_piece
│               → stream_group_subpieces[] → stream_group_subpiece
│                  → stream_group_blocks[] → stream_group_block
│    → stream_group_streams[] → stream_group_stream
│         → stream_group_stream_disposition, stream_group_stream_tags(*)
├── streams[] → stream → stream_disposition, stream_tags(*),
│                          stream_side_data_list[] → stream_side_data
├── packets[] → packet → packet_tags(*), packet_side_data_list[] → packet_side_data
├── error
├── program_version
├── library_versions[] → library_version
└── pixel_formats[] → pixel_format → pixel_format_flags,
                                       pixel_format_components[] → pixel_format_component
```
(`(*)` = variable-fields "tags" section, element name `tag`.) Note: `packets_and_frames` is a separate root child used only when `-show_frames -show_packets` together request interleaved output.

### 3.6 Field names per major section (exact, from `fftools/ffprobe.c`)

- **PACKET**: `codec_type, stream_index, pts, pts_time, dts, dts_time, duration, duration_time, size, pos, flags` (3-char `K`/`_`,`D`/`_`,`C`/`_` = keyframe/discard/corrupt), `data` (if `-show_data`), `data_hash` (if `-show_data_hash`).
- **FRAME**: `media_type, stream_index, key_frame, pts, pts_time, pkt_dts, pkt_dts_time, best_effort_timestamp, best_effort_timestamp_time, duration, duration_time, pkt_pos, pkt_size`; video adds `width, height, crop_top, crop_bottom, crop_left, crop_right, pix_fmt, sample_aspect_ratio, pict_type, interlaced_frame, top_field_first, lossless, repeat_pict`; audio adds `sample_fmt, nb_samples, channels, channel_layout`.
- **STREAM**: `index, codec_name, codec_long_name, profile, codec_type, codec_tag_string, codec_tag, mime_codec_string`; video: `width, height, coded_width, coded_height, closed_captions, film_grain, has_b_frames, sample_aspect_ratio, display_aspect_ratio, pix_fmt, level, color_range, color_space, color_transfer, color_primaries, chroma_location, field_order (progressive/tt/bb/tb/bt/unknown), refs`; audio: `sample_fmt, sample_rate, channels, channel_layout, bits_per_sample, initial_padding`; subtitle: `width, height` (as N/A if absent); common tail: `id, r_frame_rate, avg_frame_rate, time_base, start_pts, start_time, duration_ts, duration, bit_rate, max_bit_rate, bits_per_raw_sample, nb_frames, nb_read_frames, nb_read_packets, extradata (if -show_data), extradata_size, extradata_hash`; plus nested `disposition` and `tags` sections.
- **FORMAT**: `filename, nb_streams, nb_programs, nb_stream_groups, format_name, format_long_name (unless -bitexact), start_time, duration, size, bit_rate, probe_score`, plus `tags`.
- **CHAPTER**: `id, time_base, start, start_time, end, end_time`, plus `tags`.

### 3.7 Disposition flag names (`-dispositions`, `disp:` specifier, `-disposition` values)

`default, dub, original, comment, lyrics, karaoke, forced, hearing_impaired, visual_impaired, clean_effects, attached_pic, timed_thumbnails, non_diegetic, captions, descriptions, metadata, dependent, still_image, multilayer`.

### 3.8 XSD schema (`doc/ffprobe.xsd`)

Root element `ffprobe` (type `ffprobeType`), namespace `http://www.ffmpeg.org/schema/ffprobe`, containing (in order, all optional, 0..1): `program_version, library_versions, pixel_formats, packets, frames, packets_and_frames, programs, stream_groups, streams, chapters, format, error`. Each maps to a `*Type` complexType whose fields are XML **attributes** (e.g. `packetType` has attributes `codec_type, stream_index, pts, pts_time, dts, dts_time, duration, duration_time, size, pos, flags, data, data_hash`, with `codec_type`/`size`/`flags` `use="required"`), while nested sections are child **elements** (e.g. `packetType` has child elements `tags`, `side_data_list`). This attribute-for-scalar / element-for-nested-section convention holds throughout the schema (531 lines total) and is the compatibility target for the `xml` writer's `fully_qualified`/`xsd_strict` modes.

---

## 4. Stream-specifier grammar

Fully specified in §1.5 above (single canonical grammar shared by all three tools via `doc/fftools-common-opts.texi`).

---

## 5. Filtergraph syntax (item 3 filtering, and supporting `doc/filters.texi`/`doc/utils.texi` syntax chapters)

### 5.1 Filtergraph description grammar

```
NAME             ::= sequence of alphanumeric characters and '_'
FILTER_NAME      ::= NAME["@"NAME]                     (optional @id instance tag)
LINKLABEL        ::= "[" NAME "]"
LINKLABELS       ::= LINKLABEL [LINKLABELS]
FILTER_ARGUMENTS ::= sequence of chars (possibly quoted)
FILTER           ::= [LINKLABELS] FILTER_NAME ["=" FILTER_ARGUMENTS] [LINKLABELS]
FILTERCHAIN      ::= FILTER [,FILTERCHAIN]
FILTERGRAPH      ::= [sws_flags=flags;] FILTERCHAIN [;FILTERGRAPH]
```
- Filters in one linear chain separated by `,`; distinct chains separated by `;`.
- `FILTER_ARGUMENTS` may be: (a) `:`-separated `key=value` pairs; (b) `:`-separated positional values (bound to option-declaration order); (c) positional values followed by `key=value` pairs (positional must come first). List-valued options are usually `|`-separated internally.
- Quoting: `'...'` for literal content, `\` to escape within/outside quotes; unquoted arguments terminate at the next special char in `[]=;,`.
- **ffmpeg-only extension**: prefixing an option name with `/` loads its value from a file path (e.g. `drawtext=/text=/tmp/some_text`).
- Link-label matching: identical labels on an output pad and an input pad elsewhere in the graph create that link. Unlabeled output pad ⇒ auto-links to the next filter's first unlabeled input pad in the same chain. First filter's unlabeled input defaults to label `in`; last filter's unlabeled output defaults to label `out`. All unlabeled pads in a chain must end up connected; the whole graph is valid only if every pad (all chains) is connected.
- Whitespace (space/tab/newline) around tokens is ignored, enabling multi-line formatting.
- `libavfilter` auto-inserts `scale`/format-conversion filters where required; `sws_flags=FLAGS;` prefix on the whole graph controls those auto-inserted scalers.

### 5.2 Escaping levels (three, per `doc/utils.texi` §Quoting and escaping + filters.texi §Notes on filtergraph escaping)

1. Filter **option value** level — escape `:` and `\'` special to the arg-list parser.
2. Filter **description**/graph level — escape `\'` and `[],;` special to the filtergraph parser (plus `,` if present in a value at this level).
3. **Shell** level — escaping per the invoking shell's own rules.
Recommendation: prefer file-based option input (e.g. `drawtext`'s `textfile` vs `text`) to avoid multi-level escaping.

### 5.3 Timeline editing (`enable` option)

Filters supporting timeline editing accept `enable=EXPR`, evaluated per-frame; non-zero ⇒ filter applied, else frame passed through unchanged. Expression constants: `t` (timestamp seconds, NaN if unknown), `n` (0-based frame number), `pos` (file position, NaN if unknown; deprecated), `w`/`h` (frame width/height, video only). Also exposed at runtime as a settable **command** (see §5.4). `ffmpeg -filters` marks timeline-capable filters.

### 5.4 Runtime commands

Options marked `T` in `ffmpeg -h filter=NAME` output are changeable at runtime via a command whose name = option name, argument = new value.

### 5.5 Framesync options (multi-input filters)

Common option set, settable **by name only** (no positional/short form): `eof_action` (`repeat` default / `endall` / `pass`), `shortest` (bool, default 0), `repeatlast` (bool, default 1), `ts_sync_mode` (`default` = nearest-lower-or-equal secondary timestamp / `nearest` = absolute nearest).

### 5.6 Supporting syntaxes referenced throughout the CLI (`doc/utils.texi`)

- **Quoting/escaping** (general FFmpeg convention, not just filtergraphs): `'` and `\` are special; `'...'` = literal; `\` escapes a following special char; unescaped/unquoted leading/trailing whitespace stripped.
- **Date**: `[(YYYY-MM-DD|YYYYMMDD)[T|t| ]]((HH:MM:SS[.m...])|(HHMMSS[.m...]))[Z]` or `now`; local time unless `Z` suffix (UTC); missing date part = today.
- **Time duration**: `[-][HH:]MM:SS[.m...]` or `[-]S+[.m...][s|ms|us]` (bare/`s`/`ms`/`us` suffix); leading `-` = negative duration.
- **Video size**: `WxH` or a named abbreviation (`ntsc, pal, qntsc, qpal, sntsc, spal, film, ntsc-film, sqcif, qcif, cif, 4cif, 16cif, qqvga, qvga, vga, svga, xga, uxga, qxga, sxga, qsxga, hsxga, wvga, wxga, wsxga, wuxga, woxga, wqsxga, wquxga, whsxga, whuxga, cga, ega, hd480, hd720, hd1080, 2k, 2kflat, 2kscope, 4k, 4kflat, 4kscope, nhd, hqvga, wqvga, fwqvga, hvga, qhd, 2kdci, 4kdci, uhd2160, uhd4320`).
- **Video rate**: `num/den`, integer, float, or abbreviation `ntsc`(30000/1001)/`pal`(25/1)/`qntsc`/`qpal`/`sntsc`/`spal`/`film`(24/1)/`ntsc-film`(24000/1001).
- **Ratio**: expression or `numerator:denominator`; `1/0` (infinite) and negative values parse as "valid" — caller must filter them out if unwanted.
- **Channel layout**: individual-channel ids (`FL, FR, FC, LFE, BL, BR, FLC, FRC, BC, SL, SR, TC, TFL, TFC, TFR, TBL, TBC, TBR, DL, DR, WL, WR, SDL, SDR, LFE2`) combinable with `+`, or named standard layouts (`mono, stereo, 2.1, 3.0, 3.0(back), 4.0, quad, quad(side), 3.1, 5.0, 5.0(side), 4.1, 5.1, ...` — full table in `doc/utils.texi` lines 663+).

---

## 6. ffplay CLI (item 5)

Synopsis: `ffplay [options] [input_url]`.

### 6.1 Main options

`-x width` / `-y height` (force display size) · `-fs` (start fullscreen) · `-an`/`-vn`/`-sn` (disable audio/video/subtitles) · `-ss pos` (seek; inexact, nearest point) · `-t duration` (play duration) · `-bytes` (seek by byte offset) · `-seek_interval seconds` (left/right-key seek step; default 10) · `-nodisp` (no graphical display) · `-noborder` (borderless window) · `-alwaysontop` (X11 SDL≥2.0.5 / Windows SDL≥2.0.6 only) · `-volume N` (0–100, out-of-range clamped) · `-f fmt` (force format) · `-window_title title` (default = input filename) · `-left x` / `-top y` (window position; default centered) · `-loop N` (0 = forever) · `-showmode mode` (`0`/`video`, `1`/`waves`, `2`/`rdft`; default `video`, falls back to `rdft` if no video; cycle at runtime with `w`) · `-vf filtergraph` (single video in/out; cycle with `w` if given multiple times) · `-af filtergraph` (audio) · `-i input_url`.

### 6.2 Advanced options

`-stats`/`-nostats` (playback stats: duration, codec params, position, AV sync drift; on by default unless loglevel < info) · `-fast` (non-spec-compliant speed optimizations) · `-genpts` (generate PTS) · `-sync type` (`audio` default / `video` / `ext`; selects master clock) · `-ast audio_stream_specifier` / `-vst video_stream_specifier` / `-sst subtitle_stream_specifier` (explicit stream selection via the shared specifier grammar; default = "best" stream heuristics scoped to the already-chosen program) · `-autoexit` (quit at EOF) · `-exitonkeydown` / `-exitonmousedown` · `-codec:media_specifier codec_name` (force decoder; media ∈ `a,v,s`) · `-acodec`/`-vcodec`/`-scodec name` (force decoder per type) · `-autorotate`/`-noautorotate` (default on) · `-framedrop`/`-noframedrop` (drop late video frames; default on unless master clock = video) · `-infbuf`/`-noinfbuf` (unlimited input buffering; default on for realtime streams) · `-filter_threads nb` (default 0 = auto/CPU count) · `-enable_vulkan` (use libplacebo/Vulkan renderer instead of SDL builtin) · `-vulkan_params key=val:...` · `-hwaccel` (HW-accelerated decode; auto-enables Vulkan renderer) · `-video_bg pattern` (color name/code, or `tiles` (default, checkerboard) or `none` (no alpha blend) for transparent-video background).

### 6.3 Display modes

`SHOW_MODE_VIDEO` (0, default when video present), `SHOW_MODE_WAVES` (1, audio waveform), `SHOW_MODE_RDFT` (2, audio frequency-band via inverse RDFT — fallback default when no video or video can't be played). Cycled at runtime with the `w` key (also cycles through any multiple `-vf` filtergraphs supplied).

### 6.4 Sync model (conceptual, from `AV_SYNC_*` enum in `fftools/ffplay.c`)

Three master-clock modes: `AV_SYNC_AUDIO_MASTER` (default), `AV_SYNC_VIDEO_MASTER`, `AV_SYNC_EXTERNAL_CLOCK` (wall clock). Effective master is resolved per available streams (e.g. requesting `AV_SYNC_VIDEO_MASTER` with no video stream falls back toward audio, then external). Selected via `-sync`; frame dropping (`-framedrop`) and buffering (`-infbuf`) both interact with which clock is master.

### 6.5 SDL dependency surface

Core windowing/input/audio-output via SDL2. Optional Vulkan/libplacebo rendering path (`-enable_vulkan`, `fftools/ffplay_renderer.c`, 890 lines) as an alternative to the SDL builtin renderer, required automatically when `-hwaccel` is used.

### 6.6 Seeking behaviour

`-ss`/`-t` control initial seek/play-duration exactly as ffmpeg's input-side semantics (inexact, nearest seek point). At runtime: left/right = ±10s (or `-seek_interval` seconds); down/up = ±1 minute; page-down/page-up = previous/next chapter, or ±10 minutes if the file has no chapters; right mouse click = seek to the position corresponding to the click's horizontal fraction of window width; `-bytes` switches seek units to byte offsets. `s` = single-step to next video frame (pauses first if not already paused).

### 6.7 Keyboard/mouse bindings (full table)

| Key/action | Effect |
|---|---|
| `q`, `Esc` | Quit |
| `f` | Toggle fullscreen |
| `p`, `Space` | Pause |
| `m` | Toggle mute |
| `9`, `0` / `/`, `*` | Decrease/increase volume |
| `a` | Cycle audio stream (within current program) |
| `v` | Cycle video stream |
| `t` | Cycle subtitle stream (within current program) |
| `c` | Cycle program |
| `w` | Cycle video filters / show modes |
| `s` | Step to next frame (pauses) |
| Left / Right | Seek −/+ 10s (or `-seek_interval`) |
| Down / Up | Seek −/+ 1 minute |
| Page Down / Page Up | Prev/next chapter, or −/+10 minutes if no chapters |
| Right mouse click | Seek to width-fraction position |
| Left mouse double-click | Toggle fullscreen |

---

## 7. Notable clean-room-relevant gaps / legacy-name notes

- `-vsync` and `-async` (pre-scheduler-rewrite CLI names) are **not present** in the current option tables (`fftools/ffmpeg_opt.c`) — current equivalents are `-fps_mode` and the `aresample` filter's `async` option respectively. A reimplementation targeting current FFmpeg should treat these as removed, not as aliases to support.
- `-map_channel` likewise does not appear in current source/docs — legacy/avconv-era, not part of the current contract.
- `-qsv_device`/`-vaapi_device` are real, functioning CLI options (`OPT_EXPERT`) but are **not documented** in `doc/ffmpeg.texi`; they're pure shortcuts for specific `-init_hw_device` invocations (see §2.5).
- The `mermaid`/`mermaidhtml` output formats exist only for `-print_graphs_format` (execution-graph dumps via `fftools/graph/graphprint.c`), not for ffprobe's `-output_format` writer set.