# FFmpeg Feature Inventory — libavdevice / configure / tests / tools

Source: `~/repos/FFmpeg`, commit `564f92cce23ae95399476617b8a1dc357f002a47` (2026-08-18), version `8.0.git`.
Catalogue-level only (names/options/flags/deps) per clean-room rules — no verbatim source reproduced.

---

## 1. libavdevice — Input and Output Devices

Registration lists (`alldevices.c`, `Makefile`) confirm the **complete** device set in this tree.
Devices NOT present (do not plan for them as existing features): `sdl2` (output, removed),
`opengl` (output, removed), `bktr` (BSD TV capture, removed), `vidsrc`/`vidcapture` (never existed),
`libv4l2` as a device (it is a v4l2 *option*, not a separate device).

| Device | Platform | Dir | External dep | Lib license | gpl/nonfree? | Enumeration |
|---|---|---|---|---|---|---|
| alsa | Linux | in+out | alsa-lib | LGPL-2.1+ | no | Yes (`get_device_list`) |
| android_camera | Android | in | camera2ndk, mediandk, pthreads | Apache-2.0 (NDK) | no | No |
| audiotoolbox | macOS/iOS | out | Apple AudioToolbox | Apple proprietary | no | `list_devices` (log only) |
| avfoundation | macOS | in | AVFoundation/CoreVideo/CoreMedia | Apple proprietary | no | `list_devices` (log only) |
| caca | cross | out | libcaca | WTFPL | no | `list_drivers`/`list_dither` |
| decklink | cross (vendor SDK) | in+out | Blackmagic DeckLink SDK | Proprietary | **nonfree** | Yes (`get_device_list`) |
| dshow | Windows | in | DirectShow (Windows SDK) | Windows system API | no | Yes (`get_device_list`) |
| fbdev | Linux | in+out | linux/fb.h | kernel UAPI | no | Yes (`get_device_list`) |
| gdigrab | Windows | in | GDI; selects bmp_decoder | Windows system API | no | No |
| iec61883 | Linux FireWire | in | libiec61883, libraw1394 | LGPL-2.1+ | no | No |
| jack | cross | in | libjack | LGPL-2.1+ | no | No |
| kmsgrab | Linux DRM/KMS | in | libdrm | MIT | no | No |
| lavfi | cross | in | internal avfilter | n/a | no | No |
| libcdio | cross | in | libcdio(+paranoia) | **GPL-2+** | **gpl** | No |
| libdc1394 | cross IIDC | in | libdc1394, libraw1394 | LGPL-2.1+ | no | No |
| openal | cross | in | OpenAL 1.1 | LGPL-2+ (OpenAL Soft) | no | `list_devices` (log only) |
| oss | *BSD/Unix | in+out | sys/soundcard.h | OS header | no | No |
| pulse | cross | in+out | libpulse | LGPL-2.1+ | no | Yes (both directions) |
| sndio | OpenBSD/cross | in+out | sndio | ISC | no | No |
| v4l2 | Linux | in+out | videodev2.h; optional libv4l2 | kernel UAPI / LGPL-2.1 | no | Yes (input only) |
| vfwcap | Windows legacy | in | vfw32 | Windows system API | no | No |
| xcbgrab (x11grab) | Linux/X11 | in | libxcb (+shm/xfixes/shape) | MIT/X11 | no | No |
| xv | Linux/X11 | out | Xv/Xlib/Xext | MIT/X11 | no | No |

### Per-device option identifiers
- **alsa** (dec): `sample_rate`, `ch_layout`
- **oss** (dec): `sample_rate`, `channels`
- **jack**: `channels`
- **pulse** (dec): `server`, `name`, `stream_name`, `sample_rate`, `channels`, `frame_size`, `fragment_size`, `wallclock`
- **pulse** (enc): `server`, `name`, `stream_name`, `device`, `buffer_size`, `buffer_duration`, `prebuf`, `minreq`
- **avfoundation**: `list_devices`, `video_device_index`, `audio_device_index`, `video_device_id`, `audio_device_id`, `pixel_format`, `framerate`, `video_size`, `capture_cursor`, `capture_mouse_clicks`, `capture_raw_data`, `drop_late_frames`
- **xcbgrab/x11grab**: `window_id`, `x`, `y`, `grab_x`, `grab_y`, `video_size`, `framerate`, `draw_mouse`, `follow_mouse`, `centered`, `show_region`, `region_border`, `select_region`
- **gdigrab**: `draw_mouse`, `show_region`, `framerate`, `video_size`, `offset_x`, `offset_y`
- **dshow**: `video_size`, `pixel_format`, `framerate`, `sample_rate`, `sample_size`, `channels`, `audio_buffer_size`, `list_devices`, `list_options`, `video_device_number`, `audio_device_number`, `video_pin_name`, `audio_pin_name`, `crossbar_video_input_pin_number`, `crossbar_audio_input_pin_number`, 6x `show_*_dialog`, `audio_device_load/save`, `video_device_load/save`, `use_video_device_timestamps`
- **vfwcap**: `video_size`, `framerate`
- **v4l2**: `standard`, `channel`, `video_size`, `pixel_format`, `input_format`, `framerate`, `list_formats` (all/raw/compressed), `list_standards`, `timestamps`/`ts` (default/abs/mono2abs), `use_libv4l2`
- **decklink** (in): `list_devices`(dep), `list_formats`, `format_code`, `raw_format`, `enable_klv`, `teletext_lines`, `channels`, `duplex_mode`, `timecode_format`, `video_input`, `audio_input`, `audio_pts`, `video_pts`, `draw_bars`(dep), `queue_size`, `audio_depth`, `decklink_copyts`, `timestamp_align`, `wait_for_tc`, `signal_loss_action`
- **decklink** (out): `list_devices`(dep), `list_formats`, `vanc_queue_size`, `timing_offset`
- **kmsgrab**: `device`, `format`, `format_modifier`, `crtc_id`, `plane_id`, `framerate`
- **lavfi**: `graph`, `graph_file`, `dumpgraph`
- **libcdio**: `speed`, `paranoia_mode`
- **libdc1394**: `video_size`, `pixel_format`, `framerate`
- **openal**: `channels`, `sample_rate`, `sample_size`, `list_devices`
- **fbdev** (dec): `framerate`; (enc): `xoffset`, `yoffset`
- **iec61883**: `dvtype`, `dvbuffer`, `dvguid`
- **android_camera**: `video_size`, `framerate`, `camera_index`, `input_queue_size`
- **caca**: `window_size`, `window_title`, `driver`, `algorithm`, `antialias`, `charset`, `color`, `list_drivers`, `list_dither`
- **xv**: `display_name`, `window_id`, `window_size`, `window_title`, `window_x`, `window_y`
- **audiotoolbox**: `list_devices`, `audio_device_index`
- **sndio**: no option table

---

## 2. `configure` — Feature-Flag Architecture

### 2.1 Dependency-resolution model (described, not quoted)

Every component (codec, muxer/demuxer, filter, device, protocol, bsf, hwaccel, external-library
switch, arch-extension, feature) is a shell variable holding yes/no. Relationships are declared via
suffix-named variables on each component's base name:

- `<name>_deps` — hard requirements; ALL must hold or the component is force-disabled.
- `<name>_deps_any` — ANY one satisfies.
- `<name>_conflict` — must NOT be simultaneously enabled.
- `<name>_select` — force-enables (deep, non-weak) when this component is enabled; if a selected
  dependency cannot be satisfied, this component is disabled instead.
- `<name>_suggest` — weakly enables (best-effort, never fails hard).
- `<name>_if` / `<name>_if_any` — conditional weak-enable when ALL/ANY listed conditions hold.
- `<name>_extralibs` — extra linker flags contributed when enabled.

The resolver walks this graph per-component depth-first, memoized via a checking state machine
(`done`/`inprogress`/unset) that also detects cycles; applies `_if`/`_if_any` weak-enables; hard-disables
on failed `_deps`/`_deps_any`/`_conflict`/unsatisfied `_select`; then aggregates `_extralibs` from every
satisfied dependency.

**Rust implication:** this is a declarative dependency DAG with weak *and* strong edges. Cargo's
`[features]` model expresses strong edges (`feature = ["dep:x"]`) but has no native "suggest"
(best-effort) edge and no "conflict" edge. A faithful Vaco equivalent needs either a build-script
resolver or a convention where suggest-edges become explicit opt-in features and conflicts become
compile_error! guards.

### 2.2 Top-level switches by category

**Licensing:** `--enable-gpl`, `--enable-version3`, `--enable-nonfree`

**Configuration/global:** `--disable-static`, `--enable-shared`, `--enable-small`,
`--disable-runtime-cpudetect`, `--enable-gray`, `--disable-swscale-alpha`, `--disable-unstable`,
`--disable-all`, `--disable-autodetect`, `--disable-checkasm`

**Program selection:** `--disable-programs`, `--disable-ffmpeg`, `--disable-ffplay`, `--disable-ffprobe`

**Documentation:** `--disable-doc`, `--disable-htmlpages`, `--disable-manpages`, `--disable-podpages`,
`--disable-txtpages`

**Library/component:** `--disable-avdevice`, `--disable-avcodec`, `--disable-avformat`,
`--disable-swresample`, `--disable-swscale`, `--disable-avfilter`, `--disable-pthreads`,
`--disable-w32threads`, `--disable-os2threads`, `--disable-network`, `--disable-dwt`,
`--disable-error-resilience`, `--disable-lsp`, `--disable-faan`, `--disable-iamf`, `--disable-pixelutils`

**Fine-grained by name:** `--disable-everything`; per-category enable/disable-by-name and
disable-all-of-category for: encoder(s), decoder(s), hwaccel(s), muxer(s), demuxer(s), parser(s),
bsf(s), protocol(s), indev(s), outdev(s) (+ `--disable-devices`), filter(s)

**Optimization/toolchain:** `--enable-lto[=arg]`, `--enable-pic`, `--enable-thumb`, plus full toolchain
overrides (`--arch`, `--cpu`, `--cross-prefix`, `--cc`/`--cxx`/`--objcc`/`--ld`/`--nm`/`--ar`/`--as`/
`--strip`/`--windres`/`--x86asmexe`/`--nvcc`/`--glslc`/`--metalcc`/`--metallib`/`--pkg-config`,
`--target-os`, `--sysroot`, `--extra-cflags`)

**Hardware accelerators:** `--disable-amf`, `--disable-audiotoolbox`, `--enable-cuda-nvcc`,
`--disable-cuda-llvm`, `--disable-cuvid`, `--disable-d3d11va`, `--disable-d3d12va`, `--disable-dxva2`,
`--disable-ffnvcodec`, `--disable-libdrm`, `--enable-libmfx`, `--enable-libvpl`, `--enable-mmal`,
`--disable-nvdec`, `--disable-nvenc`, `--enable-rkmpp`, `--disable-v4l2-m2m`, `--disable-vaapi`,
`--disable-vdpau`, `--disable-videotoolbox`, `--disable-vulkan`, `--enable-vulkan-static`

### 2.3 License classification lists

configure sorts external libraries into license-tagged lists:

- **`EXTERNAL_LIBRARY_GPL_LIST`** (needs `--enable-gpl`): `avisynth`, `frei0r`, `libcdio`, `libdavs2`,
  `libdvdnav`, `libdvdread`, `librubberband`, `libvidstab`, `libx264`, `libx265`, `libxavs`,
  `libxavs2`, `libxvid`
- **`EXTERNAL_LIBRARY_NONFREE_LIST`** (needs `--enable-nonfree`): `decklink`, `libfdk_aac`, `libmpeghdec`
- **`EXTERNAL_LIBRARY_VERSION3_LIST`** (needs `--enable-version3`): `gmp`, `libaribb24`, `liblensfun`,
  `libopencore_amrnb`, `libopencore_amrwb`, `libvo_amrwbenc`, `mbedtls`, `rkmpp`
- **`EXTERNAL_LIBRARY_GPLV3_LIST`** (needs gpl AND version3): `libsmbclient`
- **`HWACCEL_LIBRARY_NONFREE_LIST`**: `cuda_nvcc`, `cuda_sdk`
- Everything else in `EXTERNAL_LIBRARY_LIST` / `HWACCEL_LIBRARY_LIST` / the AUTODETECT lists is unrestricted.

### 2.4 External-library table (provides / upstream / license / gate)

| Flag | Provides | Upstream | License | Gate |
|---|---|---|---|---|
| libx264 | H.264 encoder | x264 | GPL-2+ | gpl |
| libx265 | HEVC encoder | x265 | GPL-2+ | gpl |
| libxvid | MPEG-4/Xvid encoder | Xvid | GPL-2+ | gpl |
| libxavs / libxavs2 | AVS/AVS2 encoders | xavs/xavs2 | GPL-2+ | gpl |
| libdavs2 | AVS2 decoder | davs2 | GPL-2+ | gpl |
| libvidstab | vidstab* filters | vid.stab | GPL-2+ | gpl |
| librubberband | rubberband filter | rubberband | GPL-2+ (dual commercial) | gpl |
| frei0r | frei0r filter wrapper | frei0r | GPL-2+/MIT mixed | gpl |
| libcdio | CD demuxer/indev | libcdio+paranoia | LGPL-2+ / GPL-2+ | gpl |
| libdvdnav / libdvdread | DVD demuxing | libdvd* | GPL-2+ | gpl |
| avisynth | AviSynth demuxer | AviSynth+ | GPL-2+ | gpl |
| decklink | decklink in/outdev | Blackmagic SDK | proprietary | nonfree |
| libfdk-aac | AAC enc/dec | fdk-aac (Fraunhofer) | custom nonfree | nonfree |
| libmpeghdec | MPEG-H 3D Audio dec | mpeghdec | custom nonfree | nonfree |
| cuda-nvcc / cuda_sdk | CUDA kernel compilation | Nvidia CUDA Toolkit | proprietary | nonfree |
| gmp | rtmp(t)e crypto | GMP | LGPL-3+/GPL-2+ | version3 |
| libaribb24 | ARIB B24 captions | aribb24 | LGPL-3 | version3 |
| liblensfun | lens correction | lensfun | LGPL-3 | version3 |
| libopencore-amrnb/wb | AMR-NB/WB | opencore-amr | Apache-2.0 upstream, FFmpeg gates version3 | version3 |
| libvo-amrwbenc | AMR-WB encoder | vo-amrwbenc | Apache-2.0, gated | version3 |
| mbedtls | TLS backend | mbedTLS | Apache-2.0/GPL-2 dual | version3 |
| rkmpp | Rockchip hwaccel | Rockchip MPP | Apache-2.0 | version3 |
| libsmbclient | SMB protocol | Samba | GPL-3 | gpl+version3 |
| libaom | AV1 enc/dec | AOMedia | BSD-2-Clause (+AOM Patent Licence 1.0) | none |
| libdav1d | AV1 decode | VideoLAN | BSD-2-Clause | none |
| libaribcaption | ARIB captions | libaribcaption | MIT | none |
| libass | subtitle rendering | libass | ISC | none |
| libbluray | Blu-ray demux | VideoLAN | LGPL-2.1+ | none |
| libbs2b | bs2b filter | libbs2b | MIT | none |
| libcaca | caca outdev / vf_caca | libcaca | WTFPL | none |
| libcodec2 | codec2 | codec2 | LGPL-2.1 | none |
| libdc1394 | IIDC indev | libdc1394 | LGPL-2.1 | none |
| libflite | flite TTS source | CMU flite | BSD-like | none |
| libfontconfig | drawtext font lookup | fontconfig | MIT-style (HPND) | none |
| libfreetype | drawtext rendering | FreeType | FTL / GPL-2 dual | none |
| libfribidi | bidi text | fribidi | LGPL-2.1+ | none |
| libharfbuzz | text shaping | HarfBuzz | "Old MIT" | none |
| libgme | Game Music Emu demux | libgme | LGPL-2.1+/MIT mixed | none |
| libgsm | GSM 06.10 | libgsm | permissive as-is | none |
| libiec61883 | iec61883 indev | libiec61883 | LGPL-2.1+ | none |
| libilbc | iLBC | libilbc | BSD-3-Clause | none |
| libjack | jack indev | JACK | LGPL-2.1+ | none |
| libjxl | JPEG XL enc/dec | libjxl | BSD-3-Clause | none |
| libklvanc | Kernel Labs VANC | libklvanc | LGPL-3 (not in VERSION3_LIST — verify) | none |
| libkvazaar | HEVC encoder | kvazaar | BSD-3-Clause | none |
| liblc3 | LC3 codec | liblc3 | Apache-2.0 | none |
| liblcevc-dec | LCEVC enhancement | V-Nova | BSD-3-Clause | none |
| libmodplug | tracker demux | libmodplug | public-domain-like | none |
| libmp3lame | MP3 encoder | LAME | LGPL-2.1+ | none |
| liboapv | APV encoder | oapv | BSD-3-Clause | none |
| libonnxruntime | DNN backend | ONNX Runtime | MIT | none |
| libopencv | OpenCV filters | OpenCV | Apache-2.0 | none |
| libopencolorio | OCIO color mgmt | OpenColorIO | BSD-3-Clause | none |
| libopenh264 | H.264 enc/dec | Cisco OpenH264 | BSD-2-Clause (+Cisco binary patent cover) | none |
| libopenjpeg | JPEG2000 encode | OpenJPEG | BSD-2-Clause | none |
| libopenmpt | tracker demux | libopenmpt | BSD-3-Clause | none |
| libopenvino | DNN backend | OpenVINO | Apache-2.0 | none |
| libopus | Opus | libopus | BSD-3-Clause | none |
| libplacebo | placebo filter | libplacebo | LGPL-2.1+/MIT dual | none |
| libpulse | pulse in/outdev | PulseAudio | LGPL-2.1+ | none |
| libqrencode | QR generation | libqrencode | LGPL-2.1+ | none |
| libquirc | QR decode | quirc | MIT-ish | none |
| librabbitmq | RabbitMQ protocol | rabbitmq-c | MIT | none |
| librav1e | AV1 encoder | rav1e | BSD-2-Clause | none |
| librist | RIST protocol | librist | BSD-2-Clause | none |
| librsvg | SVG decode | librsvg | LGPL-2.1+ | none |
| librtmp | RTMP protocol | librtmp | LGPL-2.1+ | none |
| libshine | fixed-point MP3 enc | libshine | LGPL-2.1+ | none |
| libsnappy | Snappy (hap) | Google snappy | BSD-3-Clause | none |
| libsoxr | resampling | libsoxr | LGPL-2.1+ | none |
| libspeex | Speex | speex | BSD-3-Clause | none |
| libsrt | SRT protocol | Haivision libsrt | MPL-2.0 | none |
| libssh | SFTP protocol | libssh | LGPL-2.1+ | none |
| libsvtav1 | AV1 encoder | SVT-AV1 | BSD-3-Clause-Clear (+AOM patent lic) | none |
| libsvtjpegxs | JPEG XS | SVT-JPEG-XS | BSD-2-Clause-Patent | none |
| libtensorflow | DNN backend | TensorFlow | Apache-2.0 | none |
| libtesseract | OCR filter | Tesseract | Apache-2.0 | none |
| libtheora | Theora encoder | libtheora | BSD-3-Clause | none |
| libtls | TLS via LibreSSL | libtls | ISC | none (conflicts with gpl) |
| libtorch | DNN backend | PyTorch | BSD-3-Clause | none |
| libtwolame | MP2 encoder | TwoLAME | LGPL-2.1+ | none |
| libuavs3d | AVS3 decoder | uavs3d | BSD-3-Clause | none |
| libv4l2 | v4l2 conversion | v4l-utils | LGPL-2.1+ | none |
| libvmaf | VMAF filter | Netflix libvmaf | BSD-2-Clause-Patent | none |
| libvorbis | Vorbis | libvorbis | BSD-3-Clause | none |
| libvpx | VP8/VP9 | Google libvpx | BSD-3-Clause | none |
| libvvenc | VVC encoder | Fraunhofer HHI vvenc | BSD-3-Clause-Clear | none |
| libwebp | WebP encoder | libwebp | BSD-3-Clause | none |
| libxeve / libxeveb | EVC encode | xeve | BSD-3-Clause | none |
| libxevd / libxevdb | EVC decode | xevd | BSD-3-Clause | none |
| libxml2 | XML (dash/imf) | libxml2 | MIT | none |
| libzimg | zscale filter | z.lib | WTFPL | none |
| libzmq | zmq filter control | ZeroMQ | LGPL-3+ (newer MPL-2.0) | none |
| libzvbi | teletext decode | zvbi | GPL-2+ pre-0.2.28 | conditional gpl (direct die() check) |
| lv2 | LV2 plugin filtering | lilv/LV2 | ISC/permissive | none |
| cairo | cairo rendering | cairo | LGPL-2.1/MPL-1.1 dual | none |
| chromaprint | fingerprinting muxer | chromaprint | LGPL-2.1+/MIT dual | none |
| gcrypt | RTMPE crypto | libgcrypt | LGPL-2.1+ | none |
| gnutls | TLS backend | GnuTLS | LGPL-2.1+ | none |
| openssl | TLS backend | OpenSSL | Apache-2.0 (>=3.0) | conditional |
| jni | Android JNI | Android NDK | Apache-2.0 | none |
| ladspa | LADSPA filtering | LADSPA SDK | LGPL-2.1 / varies | none |
| lcms2 | ICC profile filter | Little CMS 2 | MIT | none |
| mediacodec | Android hwaccel | Android NDK | Apache-2.0 | none |
| ohcodec | OpenHarmony codec | OpenHarmony SDK | Apache-2.0 | none |
| openal | openal indev | OpenAL Soft | LGPL-2+ | none |
| opencl | OpenCL filters | Khronos headers/ICD | Apache-2.0 | none |
| pocketsphinx | asr filter | PocketSphinx | BSD-2-Clause | none |
| vapoursynth | VapourSynth demuxer | VapourSynth | LGPL-2.1+ | none |
| whisper | whisper filter | whisper.cpp | MIT | none |
| **Autodetect group** | alsa, appkit, avfoundation, bzlib, coreimage, iconv, libcurl, libxcb(+shm/shape/xfixes), lzma, mediafoundation, metal, schannel, sdl2, securetransport, sndio, xlib, zlib | system | varies (mostly permissive) | none |
| **HW SDKs** | amf (MIT headers), cuda/cuvid/ffnvcodec/nvdec/nvenc (nv-codec-headers, MIT), d3d11va/d3d12va/dxva2 (Windows SDK), libdrm (MIT), libmfx/libvpl (MIT / proprietary redistribution), mmal (BSD-3), vaapi (libva, MIT), vdpau (libvdpau, MIT), videotoolbox/audiotoolbox (Apple), vulkan (Apache-2.0) | | | none |

### 2.5 Licence-flag semantics

- `--enable-gpl` unlocks `EXTERNAL_LIBRARY_GPL_LIST` + `EXTERNAL_LIBRARY_GPLV3_LIST` plus internal
  components tagged `lgpl_gpl`. Output licence becomes GPL-2+ (GPL-3 with version3).
- `--enable-version3` unlocks `EXTERNAL_LIBRARY_VERSION3_LIST` + `EXTERNAL_LIBRARY_GPLV3_LIST`;
  upgrades effective licence (with gpl -> gplv3, alone -> lgplv3).
- `--enable-nonfree` unlocks the NONFREE lists; the build is marked "nonfree and unredistributable",
  overriding all other licence strings. Combined with gpl it additionally must pass a GPL-incompatibility check.
- Enforcement: helper functions walk the four lists and hard-fail configure when a listed library is
  enabled without its required flag.

### 2.6 Optimization switches

`--enable-lto[=arg]`, `--enable-optimizations`/`--disable-optimizations`, `--disable-asm` (master
kill-switch), `--disable-x86asm` (standalone NASM/YASM), `--disable-inline-asm`, `--enable-small`,
`--disable-runtime-cpudetect`, `--enable-hardcoded-tables`, `--disable-fast-unaligned`, plus per-ISA
`--disable-<ext>`: mmx, mmxext, sse..avx512icl, aesni, clmul, armv5te, armv6, armv6t2, vfp, neon,
arm-crc, dotprod, i8mm, pmull, eor3, sve, sve2, sme, sme-i16i64, sme2, altivec, vsx, power8, mipsdsp,
mipsdspr2, msa, mipsfpu, mmi, lsx, lasx, rvv, simd128.

### 2.7 Assembly file counts by architecture

| Library | x86 | aarch64 | arm | ppc | riscv | loongarch |
|---|---|---|---|---|---|---|
| libavcodec | 113 | 46 | 63 | 1 | 60 | 9 |
| libavfilter | 42 | 3 | 0 | 0 | 2 | 0 |
| libavutil | 12 | 7 | 4 | 0 | 6 | 0 |
| libswscale | 13 | 8 | 6 | 0 | 4 | 3 |
| libswresample | 3 | 2 | 2 | 0 | 0 | 0 |
| **Total** | **183** | **66** | **75** | **1** | **72** | **12** |

MIPS (msa/mmi) and PPC (altivec/vsx) optimizations are written as C with compiler intrinsics, not
standalone asm — hence near-zero counts despite 111 `.c` files in `libavcodec/mips`. WASM `simd128`
also goes through C intrinsics. x86 asm is NASM/YASM syntax; ARM/AArch64/RISC-V/LoongArch use GNU `.S`.

---

## 3. `tests/` — FATE

**Declaration model:** each `tests/fate/*.mak` (118 files) declares `fate-<name>` targets with a
`CMD = <comparator-tool> <args>` assignment. ~2991 distinct `fate-*` targets exist.

**Reference model:** `tests/ref/{fate,acodec,lavf,lavf-fate,pixfmt,seek,vsynth}/` holds **4,936**
reference files checked into the repo — small CRC/MD5/text digests, not media. Actual sample media
lives in the external **fate-suite** corpus fetched via `make fate-rsync` into `$FATE_SAMPLES`.
`tests/fate-run.sh` executes each `CMD`, compares against the ref with a configurable comparison mode,
fuzz tolerance, thread count, cpuflags override and hwaccel selection.

**Comparator usage:** `framecrc` appears **1,130** times (per-frame CRC-32, deterministic),
`framemd5` **40** times. Also `md5`, plain diff, and numeric-quality tools `tests/tiny_psnr.c`,
`tests/tiny_ssim.c` (both GPL).

**Categories:** per-codec conformance, per-container demux/mux, filters (audio/video), ffmpeg/ffprobe
CLI behaviour suites, checkasm, api (C API tests), build sanity, demux, enc_external, hw.

**checkasm** (`tests/checkasm/`): standalone SIMD correctness + benchmark binary. ~260 registered test
entries across 102 test source files, covering idct, motion comp, deblocking, DCT/FFT, pixel ops,
hashing, LPC, float DSP. No per-arch source dirs — each test calls the target function and its C
reference directly; runtime CPU-flag detection iterates every available ISA variant for the host,
verifying bit-exactness and benchmarking cycles. **checkasm is GPL.**

**GPL test infrastructure:** `tests/checkasm/*` and `tests/tiny_ssim.c` are GPL. The rest of the
framework is LGPL/BSD-style. Individual tests inherit encumbrance from whichever external codec their
`CMD` exercises.

---

## 4. `tools/` inventory

| Tool | Purpose |
|---|---|
| aviocat.c | Cat-like copy between two URLs via AVIOContext |
| bisect-create | Scaffolds a git bisect session |
| bookmarklets.html | Browser bookmarklets for Trac/patchwork |
| check_arm_indent.sh | Validates ARM asm indentation |
| check_commit_msg.sh | Validates commit message format |
| clean-diff | Normalizes diff output |
| compare-cvelists.sh | Compares CVE lists against fixed versions |
| coverity.c | Coverity static-analysis model stubs |
| crypto_bench.c | Benchmarks libavutil hash/crypto primitives |
| cws2fws.c | Converts compressed SWF to uncompressed |
| decode_simple.c/.h | Shared helper for decode example programs |
| dvd2concat | Generates concat script from DVD VOB structure |
| enc_recon_frame_test.c | Tests encoder reconstructed-frame API |
| enum_options.c | Exercises AVOption enum handling |
| ffescape.c | Escapes strings for filtergraph/option syntax |
| ffeval.c | CLI evaluator for the libavutil eval engine |
| ffhash.c | Hash digests of files via libavutil hashes |
| fourcc2pixfmt.c | Maps FourCC to AVPixelFormat |
| gen-rc | Generates Windows .rc version-info resources |
| general_assembly.pl | Assembly source normalization |
| graph2dot.c | Filtergraph description to Graphviz dot |
| indent_arm_assembly.pl | Reindents ARM assembly |
| ismindex.c | Generates IIS Smooth Streaming manifests |
| loudnorm.rb | Drives two-pass EBU R128 normalization |
| make_chlayout_test | Generates channel-layout test cases |
| merge-all-source-plugins | Manages external source-plugin patch sets |
| missing_codec_desc | Finds codecs missing long-name metadata |
| murge | Git branch-merge helper |
| normalize.py | Normalizes source formatting |
| patcheck | Checks a patch against coding style |
| pktdumper.c | Dumps demuxed packets |
| plotframes | Plots per-frame metadata |
| probetest.c | Fuzz/test harness for format probing |
| python/tf_sess_config.py | Generates TF session-config protobuf |
| qt-faststart.c | Relocates MOV/MP4 moov atom to front |
| scale_slice_test.c | Tests swscale slice-based scaling |
| seek_print.c | Demonstrates/tests seeking |
| sidxindex.c | Generates/parses fragmented-MP4 sidx boxes |
| sofa2wavs.c | SOFA to WAV impulse responses (sofalizer) |
| source2c.c | Converts a file into a C byte array |
| source-plugins.txt | Registry of external source-plugin patch sets |
| target_bsf_fuzzer.c | OSS-Fuzz entry point for bitstream filters |
| target_dec_fate.list/.sh | Corpus list + driver for OSS-Fuzz decode regressions |
| target_dec_fuzzer.c | OSS-Fuzz entry point for decoders |
| target_dem_fuzzer.c | OSS-Fuzz entry point for demuxers |
| target_enc_fuzzer.c | OSS-Fuzz entry point for encoders |
| target_swr_fuzzer.c | OSS-Fuzz entry point for swresample |
| target_sws_fuzzer.c | OSS-Fuzz entry point for swscale |
| trasher.c | Randomly corrupts files for robustness testing |
| uncoded_frame.c | Tests muxer raw/uncoded frame passthrough |
| unwrap-diff | Unwraps word-diff output |
| venc_data_dump.c | Dumps raw encoder side-data |
| yuvcmp.c | Compares two raw YUV files |
| zmqsend.c | Sends commands to a running zmq filter |
| zmqshell.py | Interactive shell for zmqsend |

**Note for Vaco:** FFmpeg already ships OSS-Fuzz entry points for decoders, demuxers, encoders, BSFs,
swscale and swresample. That target decomposition (one fuzz entry per component category, driven by a
corpus list) is a proven shape worth mirroring directly in our own `cargo-fuzz` layout.

---

## 5. Licence Risk Table for a permissive (MIT/Apache-2.0) redistributable build

**BLOCKS** = cannot ship in the default build. **OK** = permissive. **OK-SYSTEM** = proprietary OS
framework, link-only, platform-gated.

| Component | Licence | Blocks? | Permissive alternative |
|---|---|---|---|
| libx264 | GPL-2+ | **BLOCKS** | openh264 (BSD-2) is the practical permissive H.264 encoder |
| libx265 | GPL-2+ | **BLOCKS** | none permissive for HEVC encode; would need from-scratch |
| libxvid | GPL-2+ | **BLOCKS** | native MPEG-4 encoder |
| libxavs / libxavs2 | GPL-2+ | **BLOCKS** | none |
| libdavs2 | GPL-2+ | **BLOCKS** | none (uavs3d is AVS3, not AVS2) |
| libvidstab | GPL-2+ | **BLOCKS** | none |
| librubberband | GPL-2+ | **BLOCKS** | none permissive for high-quality time-stretch |
| frei0r | GPL-2+ | **BLOCKS** | none |
| libcdio (+paranoia) | LGPL-2+ / GPL-2+ | **BLOCKS** | core libcdio alone is LGPL; paranoia forces the gate |
| libdvdnav / libdvdread | GPL-2+ | **BLOCKS** | none |
| avisynth | GPL-2+ | **BLOCKS** | n/a |
| libzvbi | GPL-2+ (<0.2.28) | **BLOCKS** unless >=0.2.28 | use >=0.2.28 |
| decklink | proprietary | **BLOCKS** | none — vendor-locked, optional feature only |
| libfdk-aac | custom nonfree (explicit no-patent-grant) | **BLOCKS** | native AAC encoder |
| libmpeghdec | custom nonfree | **BLOCKS** | none |
| cuda_nvcc / cuda_sdk | proprietary | **BLOCKS** | nv-codec-headers (MIT) alone is fine |
| gmp | LGPL-3+/GPL-2+ | BLOCKS MIT-only | use rustls/ring-family for crypto |
| libaribb24, liblensfun | LGPL-3 | BLOCKS MIT-only | none needed |
| libopencore-amr*, libvo-amrwbenc | Apache-2.0 upstream (FFmpeg self-gates version3) | OK on merits | reverify — FFmpeg's gate is its own policy, not a licence obligation |
| mbedtls | Apache-2.0/GPL-2 dual | OK via Apache track | rustls |
| rkmpp | Apache-2.0 | OK | n/a |
| libsmbclient | GPL-3 | **BLOCKS** | permissive SMB crate |
| alsa-lib, libpulse, libjack, libssh, libbluray, librtmp, libshine, libsoxr, libtwolame, librsvg, libmp3lame, libqrencode, cairo, libzmq | LGPL-2.1+/LGPL-3 | **BLOCKS** MIT-only static; OK if dynamically linked under an LGPL-tolerant policy | prefer native Rust or BSD/MIT equivalents |
| libsrt | MPL-2.0 | file-level copyleft — denied by Vaco policy D3 | implement SRT natively, or make opt-in |
| libaom, libdav1d, libvpx, libopenh264, libwebp, libtheora, libspeex, libvorbis, libgsm, kvazaar, SVT-AV1, SVT-JPEG-XS, rav1e, vvenc, oapv, xeve/xevd, libjxl, liblc3, uavs3d, libvmaf, libass (ISC) | BSD-2/3, MIT, Apache-2.0, ISC | **OK** | n/a |
| Apple frameworks (AVFoundation, AudioToolbox, VideoToolbox, CoreVideo, CoreMedia, Metal) | proprietary OS SDK | OK-SYSTEM | platform-gated |
| Windows SDK (DirectShow, GDI, VfW, MediaFoundation, D3D11/12VA, DXVA2) | proprietary OS SDK | OK-SYSTEM | platform-gated |
| libdrm, libxcb, Xv/Xlib, sndio, OSS headers | MIT/ISC | OK | n/a |
| FFmpeg checkasm, tiny_ssim | **GPL** | **BLOCKS** as code | reimplement the *concept* (per-kernel variant verification + cycle bench) clean-room |
| FFmpeg FATE core (fate-run.sh, tiny_psnr) | LGPL/BSD-style | Not reusable under clean-room anyway | design our own harness |

**Bottom line for Vaco:** the permissive default build must exclude equivalents of x264, x265, xvid,
xavs/xavs2, davs2, vidstab, rubberband, frei0r, dvdnav/dvdread, avisynth, decklink, fdk-aac,
mpeghdec, cuda_nvcc and libsmbclient — mirroring FFmpeg's own opt-in model with clearly labeled
non-default `gpl-*`/`nonfree-*` Cargo feature families. Default-enable only BSD/MIT/Apache/ISC
material plus per-OS system hwaccel paths.

## 5. License Risk Table (permissive/MIT redistributable build)

Legend: **BLOCKS** = cannot be included in a default MIT-redistributable Rust build; **OK** = permissive/compatible, safe to depend on; **OK-SYSTEM** = proprietary but a system/OS-provided framework (no redistribution of the lib itself, only linking — still not a "clean" dependency for a portable crate, treat as platform-conditional).

| Component | License | Blocks MIT build? | Permissive alternative |
|---|---|---|---|
| libx264 | GPL-2+ | **BLOCKS** | rav1e/SVT-AV1 don't cover H.264; **openh264** (BSD-2, Cisco patent-covered binaries) is the practical permissive H.264 encoder alternative |
| libx265 | GPL-2+ | **BLOCKS** | none permissive for HEVC encode in FFmpeg's own set (vvenc is BSD but targets VVC, not HEVC) — would need a from-scratch or third-party permissive HEVC encoder |
| libxvid | GPL-2+ | **BLOCKS** | none in FFmpeg's list (native FFmpeg MPEG-4 encoder is LGPL-in-FFmpeg-core, not external) |
| libxavs / libxavs2 | GPL-2+ | **BLOCKS** | none |
| libdavs2 | GPL-2+ | **BLOCKS** | none (uavs3d covers AVS3, not AVS2) |
| libvidstab | GPL-2+ | **BLOCKS** | none for `vidstabdetect`/`vidstabtransform` filters |
| librubberband | GPL-2+ (dual commercial) | **BLOCKS** | none permissive for high-quality time-stretch in FFmpeg's set |
| frei0r | GPL-2+ | **BLOCKS** | none (frei0r wrapper itself is GPL regardless of per-plugin license) |
| libcdio(+paranoia) | LGPL-2+ / GPL-2+ (paranoia) | **BLOCKS** (paranoia portion) | libcdio core alone is LGPL and usable, but FFmpeg gates the whole indev behind gpl since it links paranoia |
| libdvdnav / libdvdread | GPL-2+ | **BLOCKS** | none for DVD demuxing |
| avisynth | GPL-2+ | **BLOCKS** | n/a (Windows-only scripting demuxer) |
| libzvbi | GPL-2+ (pre-0.2.28) | **BLOCKS unless** zvbi ≥0.2.28 relicensed portions used | use zvbi ≥0.2.28 build to avoid gpl gate |
| decklink | proprietary vendor SDK | **BLOCKS** (nonfree + unredistributable) | none — vendor-locked; must be an optional platform feature never in default build |
| libfdk-aac | custom nonfree | **BLOCKS** | native FFmpeg AAC encoder (in-tree, LGPL) as functional substitute; **liblc3**/**libopus** for other codecs |
| libmpeghdec | custom nonfree | **BLOCKS** | none |
| cuda_nvcc / cuda_sdk | proprietary Nvidia SDK | **BLOCKS** (nonfree) | nv-codec-headers (MIT) alone is fine; avoid requiring the CUDA compiler toolchain itself |
| gmp | LGPL-3+/GPL-2+ | BLOCKS a *permissive* (non-LGPL-tolerant) build; fine under general LGPL policy | use gnutls/openssl/mbedtls TLS path instead for RTMPE crypto needs |
| libaribb24 | LGPL-3 | version3-gated, not gpl; blocks MIT-only, fine under LGPL policy | none needed unless ARIB captions required |
| liblensfun | LGPL-3 | same as above | none |
| libopencore-amrnb/-amrwb, libvo-amrwbenc | Apache-2.0 upstream but FFmpeg gates as version3 | Note: license is actually permissive upstream; FFmpeg's own gating choice, not a strict copyleft obligation — re-verify at implementation time whether a clean-room reimplementation needs the same gate | Opus/AAC as royalty-free voice-codec alternative |
| mbedtls | Apache-2.0/GPL-2 dual | version3-gated by FFmpeg but Apache-2.0 track is permissive | prefer using mbedTLS under its Apache-2.0 track without the version3 restriction FFmpeg self-imposes |
| rkmpp | Apache-2.0 | version3-gated by FFmpeg but Apache-2.0 is permissive | none needed — platform-specific hwaccel |
| libsmbclient | GPL-3 | **BLOCKS** | none for native SMB; use a permissive SMB client crate in Rust instead |
| DVB/Kernel headers (v4l2, fbdev) | Linux kernel UAPI (GPL-2 kernel, but headers carry syscall exception) | OK (syscall/header exception applies) | n/a |
| alsa-lib, libpulse, libjack, libssh, libbluray, librtmp, libshine, libsoxr, libtwolame, librsvg, libmp3lame, libqrencode, cairo, libzmq(LGPL-3 track) | LGPL-2.1+/LGPL-3 | OK for LGPL-tolerant builds; **BLOCKS** if project mandates strict MIT-only (no LGPL dynamic-link exception accepted) | prefer BSD/MIT equivalents where they exist (e.g., libopus over patent-bearing codecs, native Rust crates over LGPL C libs) |
| libaom, libdav1d, libvpx, libopenh264, libwebp, libtheora, libspeex, libvorbis, libgme(mixed), libgsm, kvazaar, SVT-AV1, SVT-JPEG-XS, rav1e, vvenc, oapv, xeve/xevd, libjxl, liblc3, uavs3d, libcodec2(LGPL — exception), libvmaf | BSD-2/3, MIT, Apache-2.0 | OK | n/a — already permissive |
| Apple frameworks (AVFoundation, AudioToolbox, VideoToolbox, CoreVideo, CoreMedia, Metal) | proprietary OS SDK | OK-SYSTEM (no redistribution issue, but not portable/open) | n/a — platform-gated feature only |
| Windows SDK (DirectShow, GDI, VfW, MediaFoundation, D3D11/12VA, DXVA2) | proprietary OS SDK | OK-SYSTEM | n/a — platform-gated feature only |
| libdrm, libxcb, Xv/Xlib, sndio, OSS headers | MIT/ISC/permissive | OK | n/a |
| FATE core framework, checkasm, tiny_psnr/tiny_ssim | FFmpeg project (LGPL/BSD-style, non-GPL) | OK as *test infrastructure design reference* (clean-room: reimplement logic, don't copy code) | n/a |
| FATE tests exercising GPL codecs (e.g. any `fate-h264-*` conformance test run through a GPL x264/x265 encode path, or `libcdio` tests) | inherits GPL from the external tool under test | Only the *test*, not the framework, is encumbered — irrelevant to a clean-room Rust reimplementation since no FFmpeg test code would be reused anyway | n/a |

**Overall implication for the Rust project:** a strictly-MIT-redistributable default build must exclude x264/x265/xvid/xavs/xavs2/davs2/vidstab/rubberband/frei0r/dvdnav/dvdread/avisynth/decklink/fdk-aac/mpeghdec/cuda_nvcc/libsmbclient equivalents from the default feature set (treat as opt-in, clearly labeled non-default "gpl"/"nonfree" Cargo feature families, mirroring FFmpeg's own `--enable-gpl`/`--enable-nonfree` opt-in model), and should default-enable only BSD/MIT/Apache/ISC-licensed codec/protocol bindings (dav1d, aom, svt-av1, rav1e, vpx, opus, webp, theora, vorbis, speex, vmaf, etc.) plus system-framework hwaccel paths gated per-OS.
