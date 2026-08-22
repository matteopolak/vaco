I have gathered comprehensive data. Given my read-only restriction, I cannot write the deliverable file myself — I'll report the complete inventory content here for the orchestrator to save to `/Users/matthew/projects/vaco/planning/research/04-libavfilter.md`.

## FRAMEWORK

**Core object model** (`avfilter.h`, `filters.h`, `avfilter_internal.h`):
- `AVFilter`/`FFFilter` (public/private split — `FFFilter.p` embeds the public `AVFilter`): name, description, static input/output `AVFilterPad` arrays (or `AVFILTER_FLAG_DYNAMIC_INPUTS/OUTPUTS`), `priv_size`, `priv_class` (AVOption/AVClass), `flags`, `preinit`/`init`/`uninit`, `process_command`, `activate`.
- `AVFilterContext`: instantiated filter node; holds `priv` data, `inputs[]`/`outputs[]` link arrays, `AVFilterGraph` back-pointer, `enable_str`/timeline `enable` expression state, `nb_threads`, hw device ref.
- `AVFilterLink` (public) / `FilterLink` (private, in `filters.h`): connects one output pad to one input pad; carries format/timing negotiation state (`AVFilterFormatsConfig incfg/outcfg` — formats, color_spaces, color_ranges, alpha_modes for video; samplerates, channel_layouts for audio), `w/h`, `sample_aspect_ratio`, `time_base`, `frame_rate`, `hw_frames_ctx`, and (internal) the `FFFrameQueue` + `frame_wanted_out`/`status_in`/`status_out` (EOF/error propagation) fields used by the activate scheduler.
- `AVFilterGraph`/`FFFilterGraph`: owns filter instances, `nb_threads`, `thread_type`, `scale_sws_opts`/`aresample_swr_opts` (auto-insert conversion options), `execute` callback for slice threading.

**Pad model**: `AVFilterPad` has `name`, `type` (`AVMediaType`), `AVFILTERPAD_FLAG_NEEDS_WRITABLE`, `get_buffer` (video/audio union), legacy `filter_frame`/`request_frame`/`config_props` callbacks (mostly superseded by `activate`).

**Format negotiation**: two generations coexist.
- Legacy: `query_formats()` callback sets `AVFilterFormats`/`AVFilterChannelLayouts` lists on each link, then `formats.c` merges pairwise (`merge_formats`, `merge_channel_layouts`, `merge_generic` for color space/range/alpha).
- Newer declarative API (`FFFilter.formats` union + `formats_state` enum `FilterFormatsState`): `FILTER_QUERY_FUNC`/`FILTER_QUERY_FUNC2` (per-link arrays via `AVFilterFormatsConfig **cfg_in/cfg_out`), `FILTER_PIXFMTS_ARRAY`/`FILTER_SAMPLEFMTS_ARRAY` (static list, all I/O must share one format), `FILTER_SINGLE_PIXFMT`/`FILTER_SINGLE_SAMPLEFMT`, and `FF_FILTER_FORMATS_PASSTHROUGH` (default: any common format across same-type links).
- `AVFilterFormatsMerger` table (`formats.c`) drives **auto-inserted conversion filters**: pixel format / color space / color range mismatches insert a `scale` filter (options from `scale_sws_opts`, i.e. the graph-level `sws_flags=...;` prefix); alpha-mode (premultiplied vs straight) mismatches insert `premultiply_dynamic`; audio sample format / rate / channel-layout mismatches insert `aresample` (options from `aresample_swr_opts`). `avfilter_graph_set/disable_auto_convert` toggles this.

**Activate scheduling model** (`avfilter.c`, `filters.h`): single `activate(AVFilterContext*)` callback replaces old push/pull `filter_frame`/`request_frame`. A filter examines its inlinks/outlinks, does one bounded step of work (dequeue a frame from `FFFrameQueue`, call `ff_filter_frame`-equivalent internal push, or request upstream via `ff_request_frame_to_filter` style helpers), and must call `ff_filter_set_ready()` if more work remains; returning `FFERROR_NOT_READY` signals "nothing done this pass". The graph driver (`ff_filter_graph_run_once`) repeatedly picks the readiest filter (a min-heap/priority scheduler keyed by readiness) until quiescent. `FFERROR_BUFFERSRC_EMPTY` is a companion sentinel for buffersrc.

**Framequeue** (`framequeue.h/.c`, `FFFrameQueue`, `FFFrameBucket`): a simple, non-thread-safe FIFO of `AVFrame*` per link with a global `FFFrameQueueGlobal` byte/frame cap for backpressure; used internally by every `AVFilterLink`.

**Framesync** (`framesync.h/.c`, ~424 lines): helper for N-input filters needing time-aligned frames (overlay-style). Concepts: `FFFrameSyncExtMode` (`EXT_STOP`/`EXT_NULL`/`EXT_INFINITY` — behavior before first frame / after EOF of a stream), `EOFAction` (`EOF_ACTION_REPEAT`/`ENDALL`/`PASS`), timestamp sync mode, `on_event` callback fired once a synchronized "frame event" across inputs is ready. Exposed to users as the common `eof_action`/`shortest`/`repeatlast`/`ts_sync_mode` options on multi-input filters (documented in the "framesync" chapter of filters.texi). 68 filters link `framesync.o` (blend, overlay family incl. cuda/qsv/vaapi/vulkan, psnr, ssim/ssim360, libvmaf(_cuda), lut2/lut3d, maskedclamp/max/merge/min/threshold, mergeplanes, mix, convolve/deconvolve, corr, xcorrelate, displace, guided, hysteresis, identity, midequalizer, colormap, program_opencl, remap(_opencl), scale/scale2ref, haldclut, stack family incl. hw variants, streamselect/astreamselect, tblend/tmix/tmedian/tlut2, varblur, vif, vmafmotion, xmedian/xpsnr/xstack, alphamerge, bm3d, msad, palletteuse, premultiply/unpremultiply/premultiply_dynamic, threshold, limitdiff…).

**EOF / timestamp propagation**: link status fields (`status_in`/`status_out` + `status_in_pts`) carry `AVERROR_EOF` (or arbitrary error codes) downstream through the internal push path once a filter's activate observes an input EOF; `frame_wanted_out` and the scheduler's readiness bits drive re-activation. `ff_outlink_set_status`, `ff_avfilter_link_set_in_status`-style helpers live in `filters.h`.

**Timeline support (`enable=`)**: `AVFILTER_FLAG_SUPPORT_TIMELINE_GENERIC` (framework evaluates the expression and skips the filter call automatically) vs `_INTERNAL` (filter evaluates `enable` itself, e.g. per-slice) — combined as `AVFILTER_FLAG_SUPPORT_TIMELINE`. Expression vars: `t`, `n`, `pos` (deprecated), `w`, `h`. Also settable/updatable at runtime via the generic `enable` command. 206 filter source files reference this flag.

**Slice threading**: `AVFILTER_FLAG_SLICE_THREADS` capability flag + `AVFilterContext.nb_threads`/`AVFilterGraph.thread_type`; filters call `ff_filter_execute()`/graph's `execute` callback to fan a per-plane/per-slice work function across threads (backed by `pthread.o` when `HAVE_THREADS`). 157 filter source files declare this flag (mostly per-pixel/per-sample video and audio DSP filters — denoisers, color/LUT ops, blur/sharpen, deinterlacers, scopes, etc.).

**Command interface**: `avfilter_process_command()` (public) / `ff_filter_process_command()` (internal helper matching against the filter's AVOption table) dispatch runtime option changes to `FFFilter.process_command`. Options marked `AV_OPT_FLAG_RUNTIME_PARAM` (shown as `T` in `-h filter=`) are settable live. Two graph-level command sources: `sendcmd`/`asendcmd` (`f_sendcmd.c`) parse a scripted command file (`time cmd_list` syntax, per-filter target by name/id) and inject `AVFilterCommand` events consumed via `avfilter_graph_send_command` / `avfilter_graph_queue_command`; `zmq`/`azmq` (`f_zmq.c`, needs `libzmq`) receive the same command grammar over a ZeroMQ socket for remote/live control.

**Hardware frame propagation**: `AVFILTER_FLAG_HWDEVICE` marks filters that accept an explicit `hw_device_ctx` (mostly hwupload/openclsrc/vulkan-src/AMF/ddagrab-style sources). `FF_FILTER_FLAG_HWFRAME_AWARE` (internal, `filters.h`) marks filters whose `AVFilterLink.hw_frames_ctx` should NOT be auto-propagated by the generic layer (i.e., the filter manages hw frame context itself — hwupload/hwdownload/hwmap and the GPU-backed filters). Otherwise the core propagates `hw_frames_ctx` link-to-link automatically so CPU-side filters interposed between two hw filters still see the device context. `hwmap`/`hwdownload`/`hwupload` are the explicit transition filters between address spaces (derive/map/copy semantics governed by their `mode`/`derive_device` options).

**Filtergraph textual syntax** (`doc/filters.texi`, "Filtergraph description"): a graph is `;`-separated **filterchains**; each filterchain is a `,`-separated sequence of filter instances. A filter instance: optional input link labels `[label]...`, `name[@id]`, optional `=arguments`, optional output link labels. Arguments are either `key=value` pairs joined by `:`, positional `value:value:...` matched to declared option order, or a mix (positional values must precede any `key=value`). List-valued options are typically `|`-separated. Unlabeled first/last pads default to `in`/`out`. Two labels of the same name join an output pad to an input pad. `sws_flags=flags;` may prefix the whole graph to configure auto-inserted scale filters. Per-file loading of an option value via `ffmpeg` CLI: prefix the option name with `/` (e.g. `drawtext=/text=/path/to/file`). BNF is given directly in the docs (`FILTER_NAME ::= NAME["@"NAME]`, `LINKLABEL ::= "[" NAME "]"`, etc.). **Escaping** is 3-tier: (1) within one option value, `:` and `'`/`\` need escaping; (2) the whole filter description needs `\'`/`[],;` escaped when nested inside a further composition step; (3) shell-level escaping on top. Filters commonly expose alternatives to avoid deep escaping (e.g. drawtext's `textfile` option).

## AVFILTER_FLAG_* / capability flags

| Flag | Meaning |
|---|---|
| `AVFILTER_FLAG_DYNAMIC_INPUTS` | filter's input pad count can exceed the static `AVFilterPad` array (grows at init/option-parse time) |
| `AVFILTER_FLAG_DYNAMIC_OUTPUTS` | same, for outputs |
| `AVFILTER_FLAG_SLICE_THREADS` | filter can be slice-threaded via the graph's `execute` callback |
| `AVFILTER_FLAG_METADATA_ONLY` | filter only reads/writes frame metadata/side data, doesn't touch pixel/sample data (used for graph optimization/hw passthrough decisions) |
| `AVFILTER_FLAG_HWDEVICE` | filter can accept an explicit hardware device context |
| `AVFILTER_FLAG_SUPPORT_TIMELINE_GENERIC` | supports `enable=`, evaluated/gated by the generic framework |
| `AVFILTER_FLAG_SUPPORT_TIMELINE_INTERNAL` | supports `enable=`, but the filter evaluates it itself |
| `AVFILTER_FLAG_SUPPORT_TIMELINE` | OR of the two above |
| `AVFILTER_THREAD_SLICE` | value for `AVFilterContext.thread_type` / graph `thread_type` |
| `AVFILTERPAD_FLAG_NEEDS_WRITABLE` (pad-level) | input pad requires a writable (non-shared) frame buffer |
| `AVFILTERPAD_FLAG_FREE_NAME` (pad-level) | pad name was dynamically allocated |
| `FF_FILTER_FLAG_HWFRAME_AWARE` (internal, `flags_internal`) | disables automatic `hw_frames_ctx` propagation for this filter |

## Shared internal helper modules → dependents

| Module | Files | Purpose | Representative dependents |
|---|---|---|---|
| framequeue | `framequeue.c/h` | per-link AVFrame FIFO + global queue byte cap | every `AVFilterLink` (core, always linked) |
| framesync | `framesync.c/h` | multi-input timestamp alignment | 68 filters (blend, overlay*, psnr/ssim/ssim360/vmaf*, lut2/lut3d, masked*, mix, stack*, xstack*, program_opencl, remap(_opencl), scale/scale2ref, streamselect, t-prefixed temporal filters, vif, xpsnr, alphamerge, bm3d, msad, paletteuse…) |
| drawutils | `drawutils.c/h` | pixel-format-aware primitive fill/blend/box drawing | drawbox/drawgrid/drawtext/drawvg, subtitles/ass overlay compositing, boxblur, and other on-screen-drawing filters (built unconditionally into `avfilter` core) |
| colorspace / colorspacedsp | `colorspace.c/h`, `colorspacedsp.c` | colorimetry matrix math, YUV↔RGB conversion primitives | `vf_colorspace` (only filter linking `colorspacedsp.o`); `colorspace.o` is core-linked and used by multiple filters doing colorimetry (zscale, tonemap family, eq, etc. via headers) |
| scene_sad | `scene_sad.c` | scene-change/SAD frame-difference metric (optionally SIMD) | framerate, freezedetect, identity, minterpolate, msad, scdet, select (via `select_filter_select="scene_sad"`) |
| dnn common | `dnn/dnn_interface.c`, `dnn_io_proc.c`, `queue.c`, `safe_queue.c`, `dnn_backend_common.c` + pluggable backends `dnn_backend_{onnx,openvino,tf}.c`, `dnn_backend_torch.cpp` | generic DNN inference abstraction w/ swappable backend (ONNX Runtime / OpenVINO / TensorFlow / libtorch) | `dnn_classify`, `dnn_detect`, `dnn_processing`, `derain` (select), `sr` (select) |
| vulkan / vulkan_filter | `vulkan.c/h`, `vulkan_filter.c/h`, per-filter `vulkan/*.comp` → `.spv.o` | Vulkan compute-shader filter infra (device setup, descriptor/pipeline mgmt, SPIR-V shader loading) | ~20+ `*_vulkan` filters: avgblur, blackdetect, blend, bwdif, chromaber, color, gblur, scale (+debayer), scdet, overlay, flip/hflip/vflip, transpose, v360, interlace, xfade, nlmeans(_horizontal/vertical), libplacebo (also uses vulkan.o) |
| opencl | `opencl.c/h`, `opencl_source.h`, per-filter `opencl/*.cl` kernels | OpenCL context/kernel-source management | ~20 `*_opencl` filters: avgblur/boxblur, colorkey, convolution, deshake, dilation/erosion/neighbor, nlmeans, overlay, pad, prewitt, program, remap, roberts, sobel, tonemap, transpose, unsharp, xfade, openclsrc |
| cuda helpers | `cuda/load_helper.*`, per-filter `.cu`→`.ptx.o` | CUDA/PTX kernel loading via ffnvcodec | bilateral_cuda, bwdif_cuda, chromakey_cuda, colorspace_cuda, hwupload_cuda, overlay_cuda, pad_cuda, scale_cuda, thumbnail_cuda, transpose_cuda, yadif_cuda, libvmaf_cuda |
| qsvvpp | `qsvvpp.c/h` | Intel Quick Sync VPP pipeline wrapper (needs `libmfx`) | deinterlace_qsv, overlay_qsv, scale_qsv, vpp_qsv, hstack/vstack/xstack_qsv |
| vaapi_vpp | `vaapi_vpp.c/h` | VA-API VPP pipeline wrapper | deinterlace_vaapi, denoise_vaapi (misc_vaapi), overlay_vaapi, procamp_vaapi, scale_vaapi, sharpness_vaapi (misc_vaapi), tonemap_vaapi, transpose_vaapi, hstack/vstack/xstack_vaapi (stack_vaapi), pad_vaapi, drawbox_vaapi |
| stack_internal | `stack_internal.h` | shared layout-parsing logic for hstack/vstack/xstack across CPU/vaapi/qsv variants | vf_stack.c, vf_stack_vaapi.c, vf_stack_qsv.c |
| amf common | `vf_amf_common.c/h` | AMD AMF device/session bootstrap | sr_amf, vpp_amf, frc_amf, vqe_amf, vsrc_amf(_capture) |
| lut/CLUT helpers | shared code in `vf_lut3d.c` (also backs `lut1d`, `haldclut`) | Hald/3D/1D LUT sampling & interpolation | lut1d, lut3d, haldclut |
| bufferqueue | `bufferqueue.h` | generic ring-buffer used by framesync | framesync |
| motion_estimation | `motion_estimation.c/h` | block motion search primitives | mestimate, minterpolate |
| edge_common | `edge_common.c/h` | shared edge-detection kernels | edgedetect, cropdetect, blurdetect |
| boxblur | `boxblur.c/h` | separable box-blur core | boxblur, boxblur_opencl/avgblur_opencl |
| generate_wave_table | `generate_wave_table.c/h` | LFO waveform generation | chorus, flanger, vibrato, aphaser |
| textutils | `textutils.c/h` | text layout shared by drawing filters | drawtext, drawvg |
| qp_table | `qp_table.c/h` | codec QP-map extraction | codecview, fspp |
| transform | `transform.c/h` | affine transform math | deshake, deshake_opencl |
| lavfutils | `lavfutils.c/h` | image-file loading via avcodec/avformat | cover_rect, find_rect |
| fflcms2 | `fflcms2.c/h` | Little-CMS2 wrapper | iccdetect, iccgen (needs `lcms2`) |
| ccfifo | `ccfifo.c/h` | closed-caption side-data FIFO | ccrepack and CC-aware encoders/decoders (shared w/ avcodec) |
| bbox | `bbox.c/h` | bounding-box scan helper | vf_bbox |
| ebur128 core | `ebur128.c/h` | EBU R128 loudness core algorithm | loudnorm (f_ebur128.c is the separate standalone `ebur128` filter) |
| pixelutils (avutil) | external to libavfilter | SAD/hadamard primitives | deshake, mpdecimate |

## FILTER INVENTORY

Legend: **GPL** = requires `--enable-gpl` (`configure ..._filter_deps="gpl"`); **Lib** = mandatory external library; **Thr** = declares `AVFILTER_FLAG_SLICE_THREADS`; **TL** = timeline (`enable=`) support; descriptions paraphrased from the one-line filter descriptions, not quoted.

### VIDEO — scaling / pixel-format / colorspace

| name | purpose | GPL | Lib | Thr |
|---|---|---|---|---|
| scale | generic swscale-based resize + pixfmt convert | | swscale | |
| scale2ref | scale relative to a second reference stream | | swscale | |
| zscale | resize + colorspace + bit-depth via zimg | | libzimg | |
| format | restrict/convert to a pixel-format list (auto-inserted target) | | | |
| noformat | exclude pixel formats from negotiation | | | |
| colorspace | explicit colorspace conversion (matrix/primaries/trc) | | | Thr |
| colormatrix | legacy YUV colormatrix conversion | GPL | | |
| setrange / setparams / setfield | force color_range / multiple props / field order metadata (no pixel work) | | | |
| pixdesctest | round-trips pixel format descriptors (self-test) | | | |
| pixfmts_super2xsai (test dep only) | — | | | |
| colordetect | detect actual color range/matrix in use | | | Thr |
| icc detect/gen | detect/attach ICC profiles | | lcms2 | |
| ocio | apply OpenColorIO display/view transform | | libopencolorio | |
| scale_cuda / scale_d3d11 / scale_d3d12 / scale_qsv / scale_vaapi / scale_vt / scale_vulkan | HW-accelerated resize | | ffnvcodec / d3d11va / d3d12va / libmfx / vaapi / videotoolbox / vulkan | |
| hwdownload / hwupload / hwupload_cuda / hwmap | HW⟷SW frame transitions | | (device-specific) | |

### VIDEO — cropping / padding / geometry / transform

| name | purpose | GPL | Lib | Thr |
|---|---|---|---|---|
| crop | crop to w:h:x:y (expr-based) | | | TL |
| cropdetect | auto-detect crop rectangle | GPL | | |
| pad / pad_cuda / pad_opencl / pad_vaapi | pad to larger canvas | | (hw libs) | Thr(cpu)/TL |
| addroi | attach region-of-interest side data | | | |
| rotate | arbitrary-angle rotation | | | Thr, TL |
| transpose / transpose_cuda / transpose_opencl / transpose_vaapi / transpose_vt / transpose_vulkan | 90°-family transpose | | (hw libs) | TL |
| hflip / hflip_vulkan / vflip / vflip_vulkan / flip_vulkan | mirror flip | | (vulkan) | Thr, TL |
| shear | shear transform | | | TL |
| perspective | perspective correction | GPL | | TL |
| lenscorrection | radial lens-distortion correction | | | Thr, TL |
| lensfun | lens correction from lensfun DB (vignetting/distortion/TCA) | | liblensfun (v3) | |
| scroll | scroll content over time | | | Thr |
| il | (de)interleave field lines | | | TL |
| field | extract one field | | | |
| shuffleframes / shufflepixels / shuffleplanes | reorder frames/pixels/planes | | | Thr(pixels), TL |
| swaprect / swapuv | swap regions / swap U-V planes | | | TL |
| extractplanes / alphaextract | split planes / alpha to grayscale outputs | | | |
| alphamerge | merge a grayscale stream as alpha | | | (framesync) |
| mergeplanes | assemble planes from separate inputs | | | (framesync) |
| framepack | build stereoscopic frame-packed output | | | |
| stereo3d | convert between 3D stereo layouts | GPL | | Thr |
| v360 / v360_vulkan | 360°/VR projection conversion | | (vulkan) | Thr |
| tile / untile | pack N frames into a grid image / reverse | | | |
| stack family: hstack/vstack/xstack (+ _vaapi/_qsv variants) | side-by-side/grid compositing | | (hw libs) | (framesync) |
| ccrepack | repack CEA-708 caption side data | | | |
| ddagrab | Windows Desktop Duplication screen-capture source | | d3d11va | (HWDEVICE) |
| gfxcapture | Windows graphics-capture screen source | | d3d11va, cxx17 | (HWDEVICE) |
| vsrc_amf / amf_capture | AMD AMF screen capture source | | amf | (HWDEVICE) |

### VIDEO — temporal (fps/framerate/interpolation) & deinterlace

| name | purpose | GPL | Lib | Thr |
|---|---|---|---|---|
| fps | force constant output frame rate (drop/dup) | | | |
| framerate | motion-compensated up/down frame-rate conversion | | | (scene_sad select), Thr |
| framestep | keep every Nth frame | | | |
| minterpolate | motion-interpolation frame-rate conversion | | | (scene_sad select) |
| tpad | pad in time (repeat/black frames at start/end) | | | |
| tmix / tblend | blend across N successive frames / blend two temporal frames | | | Thr, TL |
| tmedian | temporal median of N frames | | | Thr |
| tmidequalizer | temporal midway histogram equalization | | | |
| tlut2 | 2-frame temporal LUT expression | | | Thr |
| decimate | drop near-duplicate frames (post-fieldmatch) | | | |
| mpdecimate | drop near-duplicate frames (simpler heuristic) | GPL | | (pixelutils select) |
| deflicker | remove luminance flicker across frames | | | Thr |
| lagfun | slow rise of luminance (afterglow) | | | Thr |
| freezedetect / freezeframes | detect / force frozen frames | | | (scene_sad select) |
| **Deinterlacers**: yadif / yadif_cuda / yadif_videotoolbox | motion-adaptive deinterlace | | (cuda/videotoolbox) | Thr |
| bwdif / bwdif_cuda / bwdif_vulkan | "bob weaver" deinterlace (yadif-derived, better edges) | | (cuda/vulkan) | Thr |
| w3fdif | Martin Weston 3-field deinterlace | | | Thr |
| estdif | edge-slope-tracing deinterlace | | | Thr |
| kerndeint | kernel deinterlace | GPL | | |
| nnedi | neural-net edge-directed intra-field deinterlace | GPL | | |
| mcdeint | motion-compensating deinterlace (uses avcodec ME) | GPL | avcodec | |
| deinterlace_qsv / deinterlace_vaapi / deinterlace_d3d12 | HW deinterlace | | libmfx/vaapi/d3d12va | |
| separatefields / doubleweave / weave | split fields to frames / recombine | | | |
| interlace / interlace_vulkan / tinterlace | progressive→interlaced conversion | GPL(interlace,tinterlace) | (vulkan) | |
| fieldorder | force field order metadata | | | |
| fieldmatch | IVTC field matching | | | |
| fieldhint | field matching via hint file | | | |
| detelecine / telecine | inverse/apply telecine pattern | | | |
| repeatfields | hard-repeat fields per MPEG flag | GPL | | |
| pullup | pullup-style IVTC (field→frame) | GPL | | |
| dejudder | remove judder from bad pullup | | | |
| idet | interlace-type detection (metadata only) | | | |
| vfrdet | variable-frame-rate detection (metadata only) | | | |
| fsync | resync frames against external timestamp file | | | |
| tiltandshift | rolling-shutter "tilt-and-shift" temporal effect | | | |

### VIDEO — denoise

| name | purpose | GPL | Lib | Thr |
|---|---|---|---|---|
| hqdn3d | high-quality 3D (spatial+temporal) denoiser | GPL | | Thr |
| owdenoise | overcomplete-wavelet denoiser | GPL | | |
| atadenoise | adaptive temporal averaging denoiser | | | Thr, TL |
| nlmeans / nlmeans_opencl / nlmeans_vulkan | non-local-means denoiser | | (opencl/vulkan) | Thr, TL |
| bm3d | block-matching 3D denoiser | | | (framesync) |
| dctdnoiz | 2D DCT denoiser | | | |
| fftdnoiz | 3D FFT denoiser | | | Thr |
| vaguedenoiser | wavelet-based denoiser | GPL | | Thr |
| bilateral / bilateral_cuda | bilateral (edge-preserving) filter | | (cuda) | |
| deband | remove banding via dithered thresholding | | | Thr, TL |
| deblock | deblocking filter | | | |
| removegrain | RemoveGrain-style spatial denoise (variants 0-24) | | | |
| chromanr | chrominance-only noise reduction | | | Thr |
| dedot | reduce cross-luma/cross-color (rainbow) artifacts | | | |
| fspp / spp / uspp | (fast/simple/ultra) post-processing deblock filters (DCT-domain) | GPL | avcodec (idct/fdct/pixblock dsp) | |
| pp7 | Postprocessing7 deblock filter | GPL | | |
| gradfun | debanding via gradient smoothing | | | Thr |
| derain | DNN-based rain/noise removal | | (dnn select) | |

### VIDEO — sharpen / blur

| name | purpose | GPL | Lib | Thr |
|---|---|---|---|---|
| unsharp / unsharp_opencl | unsharp mask sharpen/blur | | (opencl) | Thr |
| cas | Contrast-Adaptive Sharpening | | | Thr |
| avgblur / avgblur_opencl / avgblur_vulkan | box/average blur | | (opencl/vulkan) | Thr |
| boxblur / boxblur_opencl | separable box blur | GPL(cpu) | (opencl) | |
| gblur / gblur_vulkan | Gaussian blur | | (vulkan) | Thr |
| dblur | directional blur | | | Thr |
| sab | shape-adaptive blur | GPL | swscale | |
| smartblur | edge-preserving blur | GPL | swscale | |
| varblur | spatially-varying blur (radius map input) | | | (framesync) |
| yaepblur | "yet another" edge-preserving blur | | | Thr |
| guided | guided filter (edge-preserving smoothing/denoise/sharpen) | | | (framesync) |

### VIDEO — color correction / grading / LUT

| name | purpose | GPL | Lib | Thr |
|---|---|---|---|---|
| eq | brightness/contrast/gamma/saturation | GPL | | TL |
| curves | per-channel tone curves (presets + custom points) | | | Thr, TL |
| colorbalance | lift/gamma/gain color balance | | | Thr, TL |
| colorchannelmixer | arbitrary channel-mix matrix | | | Thr, TL |
| colorcontrast | RGB pairwise contrast adjust | | | Thr, TL |
| colorcorrect | selective white balance for shadows/highlights | | | Thr, TL |
| colorize | tint video with a solid hue | | | Thr, TL |
| colorlevels | per-channel input/output level remap | | | Thr, TL |
| colortemperature | adjust white-balance temperature | | | Thr, TL |
| huesaturation | combined hue/sat/intensity adjust | | | Thr, TL |
| hue | hue/saturation rotate (legacy) | | | TL |
| vibrance | boost saturation selectively | | | Thr, TL |
| exposure | exposure/black-point adjust | | | Thr, TL |
| selectivecolor | CMYK-style selective color grading | | | Thr, TL |
| grayworld | gray-world auto white balance | | | Thr |
| greyedge | gray-edge color-constancy white balance | | | |
| normalize | histogram stretch/normalize RGB | | | |
| monochrome | tinted grayscale conversion | | | Thr |
| midequalizer | midway histogram equalization between two streams | | | (framesync) |
| tmidequalizer | temporal midway equalization | | | |
| histeq | global histogram equalization | GPL | | |
| lut / lutrgb / lutyuv | per-plane expression LUT | | | Thr, TL |
| lut1d / lut3d / haldclut | 1D / 3D / Hald-image LUT application | | | (lut3d/haldclut: framesync) |
| lut2 / tlut2 | 2-input (or 2-frame) expression LUT | | | (framesync) |
| geq | generic per-pixel equation | | | |
| colormap | apply custom color-map gradient LUT | | | (framesync) |
| pseudocolor | pseudocolor mapping by luma/plane value | | | Thr |
| limitdiff | limit difference vs reference stream(s) | | | (framesync) |
| tonemap / tonemap_opencl / tonemap_vaapi | HDR↔SDR dynamic-range tonemap | | (opencl/vaapi) | Thr |
| libplacebo | GPU (Vulkan) shader-based scale/tonemap/color pipeline | | libplacebo, vulkan | (HWDEVICE) |
| iccgen / iccdetect | attach/detect ICC color profile | | lcms2 | |

### VIDEO — keying / matting / alpha

| name | purpose | GPL | Lib | Thr |
|---|---|---|---|---|
| chromakey / chromakey_cuda | YUV color-range keying to alpha | | (cuda) | Thr |
| chromahold | gray-out outside a chroma range | | | Thr |
| colorkey / colorkey_opencl | RGB color keying to alpha | | (opencl) | Thr |
| colorhold | gray-out outside an RGB range | | | Thr |
| hsvkey / hsvhold | HSV-range keying / hold | | | Thr |
| lumakey | luma-based keying | | | Thr |
| backgroundkey | key out a static background | | | Thr |
| despill | remove color spill from keyed footage | | | Thr |
| premultiply / premultiply_dynamic / unpremultiply | alpha premultiply conversions | | | (framesync) |
| maskedmerge | merge two streams via a third as mask | | | (framesync) |
| maskedclamp / maskedmax / maskedmin | clamp/min/max relative to two streams | | | (framesync) |
| maskedthreshold | threshold by abs-diff of two streams | | | (framesync) |
| maskfun | build a mask from thresholds | | | |
| threshold | 4-input per-pixel threshold select | | | (framesync) |
| hysteresis | grow a mask via connected components guided by a second stream | | | (framesync) |

### VIDEO — compositing / overlay / stack / blend / transition

| name | purpose | GPL | Lib | Thr |
|---|---|---|---|---|
| overlay / overlay_cuda / overlay_opencl / overlay_qsv / overlay_vaapi / overlay_vulkan | composite one video atop another | | (hw libs) | Thr(cpu), TL, (framesync) |
| blend / blend_vulkan / tblend | per-pixel blend-mode compositing (33+ blend modes) | | (vulkan) | Thr, TL, (framesync) |
| xfade / xfade_opencl / xfade_vulkan | transition/crossfade effects library between two clips | | (opencl/vulkan) | Thr |
| mix | weighted mix of N video inputs | | | (framesync) |
| multiply | multiply two streams | | | (framesync) |
| xmedian / tmedian | median across N streams / N frames | | | (framesync)/Thr |
| convolve / deconvolve | frequency-domain convolution of two streams | | | (framesync) |
| corr / xcorrelate | correlation / cross-correlation of two streams | | | (framesync) |
| displace | displace pixels using a vector-map stream | | | (framesync) |
| feedback | route filtered output back as a feedback input | | | |

### VIDEO — drawing / text / subtitles

| name | purpose | GPL | Lib | Thr |
|---|---|---|---|---|
| drawbox / drawbox_vaapi | draw a filled/outline box | | (vaapi) | TL |
| drawgrid | draw a grid overlay | | | TL |
| drawtext | render text (expressions, timecodes, files) | | libfreetype+libharfbuzz (suggest fontconfig, fribidi) | TL |
| drawvg | render simple vector-graphics scripts | | cairo | |
| subtitles / ass | render SSA/ASS or generic subtitle files onto video | | avformat, avcodec, libass | |
| floodfill | flood-fill a region by color similarity | | | |
| qrencodesrc / qrencode(vf) | generate / overlay a QR code | | libqrencode | |

### VIDEO — analysis / metrics

| name | purpose | GPL | Lib | Thr |
|---|---|---|---|---|
| psnr | Peak Signal-to-Noise Ratio vs reference stream | | | (framesync) |
| ssim / ssim360 | Structural Similarity (+360-aware variant) | | | (framesync) |
| libvmaf / libvmaf_cuda | Netflix VMAF perceptual quality score | | libvmaf(+cuda) | (framesync) |
| xpsnr | extended perceptually-weighted PSNR | | | (framesync) |
| vif | Visual Information Fidelity metric | | | (framesync) |
| vmafmotion | VMAF motion component only | | | (framesync) |
| msad | mean sum-of-absolute-differences between streams | | | (scene_sad select), (framesync) |
| apsnr / asdr / asisdr | audio PSNR / SDR / scale-invariant SDR (see audio table) | | | |
| identity | identity/self-similarity metric between two streams | | | (scene_sad select), (framesync) |
| blackdetect / blackdetect_vulkan | detect near-black intervals | | (vulkan) | |
| blackframe | detect near-black single frames | GPL | | |
| freezedetect | detect frozen video | | | (scene_sad select) |
| blockdetect | detect blocking artifacts | | | |
| blurdetect | detect blur level | | edge_common | |
| bitplanenoise | measure bit-plane noise | | | |
| cropdetect | auto crop detection (also geometry) | GPL | | |
| entropy | measure per-plane entropy | | | |
| siti | Spatial/Temporal Information metrics (ITU-T P.910-ish) | | | |
| signalstats | broad per-frame video signal statistics | | | Thr |
| signature | MPEG-7 video signature/fingerprint | GPL | avcodec, avformat | |
| bbox | bounding box of non-black content | | bbox.o | |
| codecview | visualize motion vectors/QP from codec side data | | qp_table | |
| readeia608 / readvitc | extract EIA-608 CC / VITC timecode to metadata | | | |
| showinfo | dump per-frame metadata to log | | | |
| photosensitivity | flag/mitigate seizure-inducing flash content | | | |
| corr | frame correlation metric (also listed under compositing) | | | (framesync) |
| scdet / scdet_vulkan | scene-change detection | | (vulkan) | (scene_sad select) |
| ciescope | CIE chromaticity diagram scope | | | |
| datascope | numeric pixel-value overlay | | | |
| pixscope | pixel-value probe overlay | | | |
| histogram / thistogram | (temporal) color histogram display | | | |
| waveform | waveform monitor | | | Thr |
| vectorscope | vectorscope display | | | Thr |
| oscilloscope | 2D signal oscilloscope | | | |
| colordetect | color-range/matrix detection (metadata only) | | | Thr |

### VIDEO — detection / AI / DNN

| name | purpose | GPL | Lib | Thr |
|---|---|---|---|---|
| dnn_processing | generic DNN frame-to-frame processing (super-res, filters) | | dnn (backend-selectable) | |
| dnn_detect | DNN object detection, writes bounding boxes to side data | | dnn | |
| dnn_classify | DNN classification, writes labels to side data | | dnn | |
| sr | DNN super-resolution (older, single-purpose wrapper) | | dnn, avformat, swscale | |
| sr_amf | AMD AMF hardware super-resolution/upscaling | | amf | (HWDEVICE) |
| derain | DNN de-rain filter | | dnn | |
| ocr | Tesseract OCR text extraction to metadata | | libtesseract | |
| ocv | OpenCV transform passthrough (smooth/erode/dilate/etc. via libopencv) | | libopencv | |
| find_rect | locate a template object via avcodec ME | GPL | avcodec, avformat | |
| cover_rect | find and cover an object (uses find_rect model) | GPL | avcodec, avformat | |
| quirc | decode & render QR code contents | | libquirc | |
| removelogo | remove a station logo using a mask image | | avcodec, avformat, swscale | |
| delogo | remove logo via blur-fill box | GPL | | |

### VIDEO — misc / artistic

| name | purpose | GPL | Lib | Thr |
|---|---|---|---|---|
| noise | add synthetic noise | | | Thr |
| random | shuffle frames randomly within a buffer window | | | |
| vignette | apply/reverse vignette | | | Thr, TL |
| edgedetect | edge detection (Sobel/Canny-ish) | | edge_common | |
| sobel / sobel_opencl, prewitt / prewitt_opencl, roberts / roberts_opencl, scharr, kirsch | directional gradient/edge operators | | (opencl variants) | Thr |
| convolution / convolution_opencl | generic NxN convolution kernel | | (opencl) | Thr, TL |
| morpho | generic morphological operator (erode/dilate/open/close/etc.) | | | Thr, TL |
| erosion / dilation / inflate / deflate (+_opencl for erosion/dilation) | classic morphology ops (shared `vf_neighbor.c`) | | (opencl) | Thr, TL |
| median | median filter (spatial) | | | Thr |
| deshake / deshake_opencl | video stabilization (motion estimate + compensate) | | (opencl), transform.o, pixelutils select | |
| vidstabdetect / vidstabtransform | 2-pass stabilization via vid.stab | | libvidstab (GPL-licensed lib) | |
| mestimate / mestimate_d3d12 | block motion-vector estimation | | motion_estimation.o / d3d12va | |
| epx / xbr / hqx / super2xsai | pixel-art integer upscalers | GPL(super2xsai) | | |
| latticepal | RGB→PAL8 via per-frame FCC-lattice palette | | | |
| palettegen / paletteuse | optimal-palette GIF encode helpers | | | (paletteuse: framesync) |
| elbg | posterize via ELBG vector-quantization | | avcodec | |
| zoompan | Ken-Burns zoom/pan effect | | swscale | |
| perlin | Perlin-noise generator (video) | | | |
| fillborders | fill/extend frame borders | | | Thr |
| displace | (listed above) | | | |
| amplify | amplify inter-frame changes | | | Thr |
| lagfun | (listed under temporal) | | | |
| exposure/vibrance/etc. | (listed under color) | | | |
| frei0r / frei0r_src | load external frei0r plugin effects (filter/source) | | frei0r (dlopen) | |
| coreimage / coreimagesrc | Apple CoreImage GPU filter/generator | | CoreImage, AppKit, OpenGL | |
| lcevc | MPEG-5 LCEVC enhancement-layer decode/apply | | liblcevc_dec | |
| ccrepack | (listed under geometry) | | | |
| pad_cuda | (listed under geometry) | | | |
| pixelize | pixelation/mosaic effect | | | Thr, TL |
| removegrain | (listed under denoise) | | | |
| framepack | (listed under geometry) | | | |
| vpp_amf / frc_amf / vqe_amf | AMD AMF VPP scale+convert / frame-rate-convert / video-quality-enhance | | amf | (HWDEVICE) |

### VIDEO — source filters

| name | purpose | GPL | Lib |
|---|---|---|---|
| nullsrc / vsrc buffer | no-op / API frame-injection source | | |
| color / color_vulkan | solid-color generator | | (vulkan) |
| colorchart / colorspectrum | test color-chart / spectrum generator | | |
| testsrc / testsrc2 / rgbtestsrc / yuvtestsrc | synthetic test patterns | | |
| testsrc_vulkan | Vulkan test source | | vulkan |
| smptebars / smptehdbars / pal75bars / pal100bars | broadcast bar patterns | | |
| allrgb / allyuv | exhaustive-color test frame | | |
| haldclutsrc | generate an identity Hald CLUT image | | |
| gradients | gradient generator | | |
| cellauto | elementary cellular-automaton pattern | | |
| life | Conway's Life pattern generator | | |
| mandelbrot | Mandelbrot fractal renderer | | |
| sierpinski | Sierpinski fractal renderer | | |
| mptestsrc | MPEG-test-pattern generator | GPL | |
| zoneplate | zone-plate test pattern | | |
| perlin | Perlin-noise video source | | |
| openclsrc | run an OpenCL program as a video source | | opencl |
| qrencodesrc | generate a QR-code video source | | libqrencode |
| frei0r_src | frei0r source-plugin generator | | frei0r |
| coreimagesrc | CoreImage generator source | | CoreImage/AppKit |
| ddagrab / gfxcapture / vsrc_amf/amf_capture | Windows/AMF screen-capture sources | | d3d11va / amf |
| movie | demux+decode a file as a video source | | avcodec, avformat |

### VIDEO — sink filters

| name | purpose |
|---|---|
| nullsink | discard video |
| vsink buffer | API frame-extraction sink |

## AUDIO — format / resample / channel manipulation

| name | purpose | GPL | Lib | Thr |
|---|---|---|---|---|
| aformat | restrict/convert sample format, rate, layout | | | |
| aresample | resample / reformat / remix via libswresample | | swresample | |
| asetrate | change sample rate metadata (pitch-shift side-effect) | | | (metadata only) |
| channelmap | remap channels by explicit mapping | | | |
| channelsplit | split into per-channel mono streams | | | |
| join | join multiple inputs into one multichannel stream | | | |
| pan | remix via per-output-channel coefficient mixing | | swresample | |
| aformat/acopy/anull | (acopy/anull are pass-through/no-op, metadata-only) | | | |
| asetnsamples | force fixed output frame sizes | | | |
| asettb | set output link timebase | | | |

## AUDIO — mixing

| name | purpose | GPL | Lib |
|---|---|---|---|
| amix | mix N audio streams with per-input weighting | | |
| amerge | merge N mono/multichannel streams into one multichannel stream | | |
| amultiply | multiply two audio streams sample-wise | | |

## AUDIO — filtering / EQ

| name | purpose | GPL | Lib | Thr |
|---|---|---|---|---|
| equalizer / bandpass / bandreject / allpass / bass(lowshelf) / treble(highshelf) / highpass / lowpass / tiltshelf / biquad | biquad IIR filter family (shared `af_biquads.c`, one AVFilter per response type + generic `biquad`) | | | |
| firequalizer | FIR equalizer via frequency-domain gain curve | | | |
| superequalizer | 18-band graphic EQ | | | |
| anequalizer | high-order parametric multiband EQ (arbitrary bands) | | | Thr |
| aiir | arbitrary IIR filter from supplied coefficients | | | Thr |
| afir | arbitrary FIR filter from supplied coefficients (via extra input streams) | | | Thr |
| afreqshift / aphaseshift | frequency shift / phase shift | | | Thr |
| atilt | spectral tilt EQ | | | Thr |
| asubboost / asubcut / asupercut / asuperpass / asuperstop | subwoofer boost / high-order Butterworth cut/pass filters | | | |
| crossfeed | headphone stereo crossfeed | | | |
| stereotools / stereowiden | stereo image manipulation | | | |
| extrastereo | widen stereo difference | | | |
| earwax | stereo-widening (headphone) effect | | | |
| haas | Haas-effect stereo enhancer | | | |
| bs2b | Bauer stereo-to-binaural | | libbs2b | |
| headphone | HRTF-based binaural spatialization | | | |
| sofalizer | SOFA-format HRTF binaural spatialization | | libmysofa | |
| surround | surround up-mix from stereo | | | |
| dcshift | apply DC offset | | | |
| bandreject/bandpass | (see biquad family) | | | |
| crystalizer | noise/transient sharpening | | | Thr |
| virtualbass | psychoacoustic virtual bass enhancement | | | |
| deesser | reduce sibilance | | | |
| dialoguenhance | boost dialogue clarity (center-channel-style) | | | |
| tremolo / vibrato | amplitude / pitch LFO modulation | | generate_wave_table | |
| chorus / flanger / aphaser | classic modulation effects (delay-line based) | | generate_wave_table | |
| aecho | echo/delay effect | | | |
| adelay | per-channel delay | | | |
| compensationdelay | speaker-distance compensation delay | | | |
| aexciter | harmonic exciter (high-frequency enhancement) | | | Thr |

## AUDIO — dynamics

| name | purpose | GPL | Lib |
|---|---|---|---|
| acompressor (sidechaincompress) | dynamic-range compressor (self or sidechain) | | |
| sidechaingate / agate | noise/sidechain gate | | |
| alimiter | lookahead limiter | | |
| acrusher | bit-depth/sample-rate crush distortion | | |
| adrc | spectral dynamic-range controller | | Thr |
| dynaudnorm | dynamic audio normalizer | | |
| mcompand | multiband compand | | |
| compand | classic compand (level mapping curve) | | |
| loudnorm | EBU R128 two-pass loudness normalization | | uses ebur128.o | |
| speechnorm | speech-oriented dynamic normalizer | | |
| apsyclip | psychoacoustic clipper | | Thr |
| asoftclip | soft clipping / saturation | | |
| adynamicequalizer | dynamic (level-dependent) EQ band | | Thr |
| adynamicsmooth | dynamic smoothing filter | | Thr |
| volume | apply gain (fixed/expr, precision options) | | |

## AUDIO — effects / reverb / delay

| name | purpose | GPL | Lib |
|---|---|---|---|
| aecho / chorus / flanger / aphaser / tremolo / vibrato | (listed above — classic modulation/echo effects) | | |
| rubberband | high-quality time-stretch/pitch-shift | | librubberband |
| atempo | tempo change (WSOLA-style) preserving pitch | | |
| hdcd | HDCD decode (peak extend / dynamic range) | | |
| ladspa | host arbitrary LADSPA plugin effects | | ladspa headers, libdl |
| lv2 | host LV2 plugin effects | | lv2 |
| adecorrelate | stereo decorrelation | | |
| aecho | (dup) | | |

## AUDIO — analysis / metering

| name | purpose | GPL | Lib |
|---|---|---|---|
| astats | broad time-domain statistics per channel | | |
| aspectralstats | frequency-domain statistics | | |
| ebur128 | EBU R128 loudness scanner (+ optional video meter output) | | |
| volumedetect | measure peak/mean volume histogram | | |
| drmeter | dynamic-range (DR) measurement | | |
| silencedetect | detect silence intervals | | |
| replaygain | ReplayGain gain-scan | | |
| apsnr / asdr / asisdr | audio PSNR / signal-to-distortion / scale-invariant SDR vs reference | | |
| axcorrelate | cross-correlate two audio streams | | |
| aderivative / aintegral | derivative/integral of the waveform | | |
| adenorm | inject sub-audible noise to avoid float denormals | | |
| ashowinfo | dump per-frame audio metadata to log | | |
| asr | automatic speech recognition → text metadata | | pocketsphinx |
| whisper | speech-to-text via whisper.cpp | | whisper |

## AUDIO — synthesis / source filters

| name | purpose | GPL | Lib |
|---|---|---|---|
| anullsrc / abuffer | silent / API-injected audio source | | |
| sine | sine-wave generator | | |
| anoisesrc | noise generator | | |
| aevalsrc | expression-defined signal generator | | |
| afirsrc / afireqsrc / afdelaysrc | generate FIR / FIR-EQ / fractional-delay coefficient streams | | |
| sinc | windowed-sinc FIR coefficient generator (lp/hp/bp/br) | | |
| hilbert | Hilbert-transform FIR coefficient generator | | |
| flite | text-to-speech synthesis | | libflite |

## AUDIO — sink filters

| name | purpose |
|---|---|
| anullsink | discard audio |
| asink abuffer | API sample-extraction sink |

## MULTIMEDIA (av) filters — audio↔video / mixed I-O

| name | purpose | GPL | Lib | Thr |
|---|---|---|---|---|
| concat | concatenate N audio+video segment streams into one | | | |
| interleave / ainterleave | temporally interleave multiple video/audio inputs by timestamp | | | |
| amovie / movie | demux+decode a file as an audio(/video) source inline in a graph | | avcodec, avformat | |
| avsynctest | generate a synthetic A/V-sync test pattern (audio+video) | | | |
| showspectrum / showspectrumpic | audio→spectrogram video (live / single still) | | | Thr |
| showcqt | audio→Constant-Q-Transform spectrum video | | avformat, swscale | |
| showcwt | audio→Continuous-Wavelet-Transform spectrum video | | | Thr |
| showfreqs | audio→per-frequency-bin bar video | | | |
| showspatial | audio→stereo-spatial video | | | Thr |
| showvolume | audio→per-channel volume-meter video | | | |
| showwaves / showwavespic | audio→waveform video (live / single still) | | | |
| avectorscope | audio→vectorscope video | | | Thr |
| a3dscope | audio→3D scope video | | | Thr |
| abitscope | audio→bit-scope video | | | |
| ahistogram | audio→histogram video | | | |
| aphasemeter | audio→stereo phase-correlation meter video | | | |
| spectrumsynth | inverse of showspectrum: spectrum video → audio | | | |
| adrawgraph / drawgraph | plot arbitrary metadata keys as a line-graph video (audio/video variants) | | | |
| agraphmonitor / graphmonitor | visualize live filtergraph internal stats as video | | | |
| ametadata / metadata | read/print/modify frame metadata (audio/video variants) | | avformat | |
| asendcmd / sendcmd | inject scripted runtime commands into the graph | | | |
| azmq / zmq | receive runtime commands over ZeroMQ | | libzmq | |
| aselect / select | conditionally route/drop frames by expression (audio/video variants) | | (scene_sad select for `select`) | |
| asegment / segment | split a stream into segments at given timestamps | | | |
| astreamselect / streamselect | dynamically choose among N input streams | | | |
| asidedata / sidedata | manipulate frame side-data (audio/video variants) | | | |
| aperms / perms | mark frame buffer read-only/writable for testing | | | |
| alatency / latency | report end-to-end filter latency | | | |
| arealtime / realtime | throttle filtering speed to wall-clock realtime | | | |
| areverse / reverse | reverse a buffered clip | | | |
| aloop / loop | loop a buffered range of frames/samples | | | |
| atrim / trim | cut to a time/frame range | | | |
| acue / cue | delay start until a cue point | | | |
| abench / bench | measure processing time of a graph section | | | |
| aeval | apply arbitrary per-sample expressions | | | |

## HARDWARE FILTERS — by backend

**VAAPI** (all need `vaapi`, wrapper `vaapi_vpp.o`): deinterlace_vaapi, denoise_vaapi (vf_misc_vaapi), sharpness_vaapi (vf_misc_vaapi), overlay_vaapi, procamp_vaapi, scale_vaapi, tonemap_vaapi, transpose_vaapi, hstack_vaapi/vstack_vaapi/xstack_vaapi (vf_stack_vaapi), pad_vaapi, drawbox_vaapi.

**QSV** (need `libmfx`, wrapper `qsvvpp.o`): deinterlace_qsv (vf_vpp_qsv), vpp_qsv, overlay_qsv, scale_qsv, hstack_qsv/vstack_qsv/xstack_qsv (vf_stack_qsv).

**CUDA / NPP** (need `ffnvcodec` + `cuda_nvcc`/`cuda_llvm`): bilateral_cuda, bwdif_cuda, chromakey_cuda, colorspace_cuda, hwupload_cuda, overlay_cuda, pad_cuda, scale_cuda (NPP-adjacent), thumbnail_cuda, transpose_cuda, yadif_cuda, libvmaf_cuda.

**OpenCL** (need `opencl`): avgblur_opencl, boxblur_opencl, colorkey_opencl, convolution_opencl, deshake_opencl, dilation_opencl, erosion_opencl, neighbor_opencl (shared kernel for dilation/erosion), nlmeans_opencl, overlay_opencl, pad_opencl, prewitt_opencl, program_opencl, remap_opencl, roberts_opencl, sobel_opencl, tonemap_opencl, transpose_opencl, unsharp_opencl, xfade_opencl, openclsrc (source).

**Vulkan** (need `vulkan spirv_compiler`): avgblur_vulkan, blackdetect_vulkan, blend_vulkan, bwdif_vulkan, chromaber_vulkan, color_vulkan (source), flip_vulkan/hflip_vulkan/vflip_vulkan, gblur_vulkan, interlace_vulkan, nlmeans_vulkan, overlay_vulkan, scale_vulkan, scdet_vulkan, transpose_vulkan, v360_vulkan, xfade_vulkan, libplacebo (vulkan-backed), testsrc_vulkan (source).

**VideoToolbox** (macOS, need `videotoolbox`): scale_vt, transpose_vt, yadif_videotoolbox.

**D3D11/D3D12** (Windows): scale_d3d11, scale_d3d12, deinterlace_d3d12, mestimate_d3d12, ddagrab (source), gfxcapture (source).

**AMD AMF**: vpp_amf, sr_amf, frc_amf, vqe_amf, vsrc_amf/amf_capture (source).

**rkrga (Rockchip)**: not present in this FFmpeg checkout's `libavfilter` (no `rkrga`-named filter files exist here — Rockchip RGA support in this tree lives only as an avcodec/hwcontext concept if enabled, not a libavfilter filter).

## 25 largest filter implementations (approx. lines of C/ObjC/CUDA, this file only, excludes shared headers)

| rank | file | filter(s) | LOC |
|---|---|---|---|
| 1 | vf_v360.c | v360 | 5107 |
| 2 | vf_waveform.c | waveform | 3531 |
| 3 | vf_drawvg.c | drawvg | 2842 |
| 4 | vf_xfade.c | xfade | 2404 |
| 5 | vsrc_testsrc.c | testsrc/testsrc2/rgbtestsrc/yuvtestsrc/etc. | 2289 |
| 6 | vf_lut3d.c | lut1d/lut3d/haldclut | 2250 |
| 7 | vf_deshake_opencl.c | deshake_opencl | 2167 |
| 8 | vf_drawtext.c | drawtext | 1968 |
| 9 | avf_showspectrum.c | showspectrum/showspectrumpic | 1879 |
| 10 | vf_libplacebo.c | libplacebo | 1813 |
| 11 | af_hdcd.c | hdcd | 1771 |
| 12 | vf_ssim360.c | ssim360 | 1766 |
| 13 | af_biquads.c | equalizer/highpass/lowpass/bandpass/bandreject/allpass/bass/treble/highshelf/lowshelf/tiltshelf/biquad | 1697 |
| 14 | af_arnndn.c | arnndn | 1615 |
| 15 | avf_showcqt.c | showcqt | 1613 |
| 16 | vf_vectorscope.c | vectorscope | 1601 |
| 17 | af_aiir.c | aiir | 1574 |
| 18 | vf_ciescope.c | ciescope | 1565 |
| 19 | af_surround.c | surround | 1526 |
| 20 | vf_latticepal.c | latticepal | 1393 |
| 21 | af_afftdn.c | afftdn | 1381 |
| 22 | avf_showcwt.c | showcwt | 1333 |
| 23 | af_afwtdn.c | afwtdn | 1317 |
| 24 | vf_scale.c | scale | 1270 |
| 25 | vsrc_ddagrab.c | ddagrab | 1269 |

**Additional complexity flags** (not all in the top-25 above but structurally heavy / multi-module, relevant to crate-decomposition planning):
- **subtitles/ass** (`vf_subtitles.c`, 565 LOC) — small wrapper but pulls in the full external `libass` rendering/typesetting engine (its own large codebase, not counted here) plus `avformat`/`avcodec` for subtitle-track demux/decode.
- **dnn family** — `vf_dnn_detect.c` (835), `vf_dnn_processing.c` (397), `vf_dnn_classify.c` (308) sit atop `dnn/dnn_backend_openvino.c` (1625), `dnn/dnn_backend_onnx.c` (1105), `dnn/dnn_backend_tf.c` (894), `dnn/dnn_backend_torch.cpp` (787) — four independent, mutually-exclusive-buildable inference backends behind one abstraction (`dnn_interface.c`); this is the single biggest "external dependency surface" in libavfilter for a Rust rewrite (4 separate FFI/runtime integrations).
- **libvmaf** (`vf_libvmaf.c`, 828) wraps the external `libvmaf` project (itself large; not counted here) plus framesync multi-input handling.
- **deshake** (602) / **deshake_opencl** (2167) — full feature-point motion tracking + affine transform stabilization; the OpenCL variant re-implements the whole pipeline as GPU kernels, effectively doubling the algorithm surface.
- **nlmeans** (486) is deceptively small in LOC but implements an integral-image-accelerated non-local-means search; **nlmeans_opencl**/**nlmeans_vulkan** re-implement it again per-backend.
- **mestimate/minterpolate** — `vf_mestimate.c` (361) + shared `motion_estimation.c` (433) implement block motion search reused by frame interpolation; **mestimate_d3d12** duplicates this on GPU.
- **vidstabdetect/vidstabtransform** — thin FFmpeg-side wrappers, but delegate essentially all algorithm complexity to the external GPL `libvidstab` project.
- **colorspace/zscale/tonemap family** — small individual files but implement dense colorimetry math (matrix derivation, PQ/HLG/BT.2020 transfer functions) that a Rust port would want as one shared, carefully-tested crate rather than reimplemented per filter.

---
**Note on delivery**: I am a read-only search subagent and cannot write files. Please save the content above to `/Users/matthew/projects/vaco/planning/research/04-libavfilter.md`.

**10-line summary**: libavfilter registers ~560 `FFFilter` instances (per `allfilters.c`) spanning video, audio, multimedia, and hardware (VAAPI/QSV/CUDA/OpenCL/Vulkan/D3D11-12/VideoToolbox/AMF) backends, built on an `activate()`-based cooperative scheduler with per-link `FFFrameQueue`, declarative/legacy format negotiation (`FF_FILTER_FORMATS_*` states) that auto-inserts `scale`/`aresample`/`premultiply_dynamic` for mismatches, plus `framesync` for N-input time alignment. Cross-cutting capability flags are `AVFILTER_FLAG_{DYNAMIC_INPUTS,DYNAMIC_OUTPUTS,SLICE_THREADS,METADATA_ONLY,HWDEVICE,SUPPORT_TIMELINE_{GENERIC,INTERNAL}}`, plus internal `FF_FILTER_FLAG_HWFRAME_AWARE`. Shared helper crates worth extracting first: framequeue, framesync, drawutils, colorspace, scene_sad, dnn (4 swappable backends), vulkan/opencl/cuda/qsvvpp/vaapi_vpp GPU infra, and stack_internal. GPL-gated filters (boxblur, eq, cropdetect, delogo, nnedi, pullup, mpdecimate, stereo3d, tinterlace/interlace, kerndeint, mcdeint, super2xsai, smartblur, sab, histeq, hqdn3d, owdenoise, perspective, phase, pp7, spp/fspp/uspp, repeatfields, signature, cover_rect/find_rect, colormatrix, blackframe, mptestsrc, vaguedenoiser) need reimplementation-from-spec, not porting. External-library-gated filters (libass, libvmaf, libplacebo+vulkan, libmysofa, librubberband, ladspa/lv2, frei0r, libopencv, libtesseract, whisper.cpp, pocketsphinx, liblensfun, lcms2, libbs2b, libzmq, libzimg, liblcevc_dec) define the FFI-boundary surface for a Rust reimplementation. The 25 largest single-file filters (v360 5107 LOC down to ddagrab 1269 LOC) plus the DNN backend cluster, libvmaf, deshake(+opencl), nlmeans(+opencl/vulkan), and subtitles/libass wrapper are the highest-complexity/highest-risk items for porting effort estimation.