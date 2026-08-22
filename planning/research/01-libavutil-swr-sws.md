# FFmpeg libavutil / libswresample / libswscale — Feature Inventory

*(Clean-room catalogue: identifiers, structure, and public API shape only — no algorithm internals reproduced.)*

Source tree: `~/repos/FFmpeg` (current checkout). Scope: `libavutil`, `libswresample`, `libswscale`.

---

## 1. libavutil — Header Inventory (capability area, one line each)

| Header | Capability area |
|---|---|
| `avutil.h` | Top-level umbrella include; library version/media-type/picture-type constants |
| `common.h` | Core macros (clamp, min/max, align, byte order helpers) shared across FFmpeg |
| `macros.h` | Generic preprocessor helper macros |
| `attributes.h` / `attributes_internal.h` | Compiler attribute macros (visibility, alignment, cold/hot, printf-format checking) |
| `version.h` / `version_major.h` | Library version macros |
| `error.h` | `AVERROR_*` error code space, `av_strerror` |
| `mem.h` / `mem_internal.h` | Aligned/reallocating allocators, fast-malloc patterns |
| `avassert.h` | Assertion macros (`av_assert0/1/2`) |
| `avstring.h` | String utilities (case-insensitive compare, escaping, tokenizing, UTF-8 helpers) |
| `bprint.h` | Growable `AVBPrint` string-building buffer |
| `dict.h` | `AVDictionary` key/value store |
| `opt.h` | `AVOption`/`AVClass` generic object-introspection and option system |
| `log.h` | `av_log` logging with levels and per-object `AVClass` context |
| `rational.h` | `AVRational` fraction type and arithmetic |
| `mathematics.h` | Rescaling, GCD, rounding-mode helpers built atop `AVRational` |
| `intmath.h` / `libm.h` / `ffmath.h` | Integer math helpers, libm polyfills, internal float math |
| `fixed_dsp.h` / `float_dsp.h` / `float_scalarproduct.h` | Fixed/float point vector DSP primitive tables |
| `softfloat.h` / `softfloat_ieee754.h` / `softfloat_tables.h` | Software floating point emulation |
| `int128.h` / `integer.h` | 128-bit and arbitrary-precision integer types |
| `intfloat.h` / `intreadwrite.h` / `bswap.h` | Bit-reinterpretation and endian-safe memory access |
| `rational64.h` (swscale-local) | 64-bit rational used internally by swscale |
| `pixfmt.h` | `AVPixelFormat` enum and color/chroma metadata enums |
| `pixdesc.h` | `AVPixFmtDescriptor`/`AVComponentDescriptor` introspection of pixel formats |
| `pixelutils.h` | SAD/pixel-comparison primitives |
| `samplefmt.h` | `AVSampleFormat` enum and audio buffer-layout helpers |
| `channel_layout.h` | `AVChannelLayout`/`AVChannel` channel model (new-style) |
| `csp.h` / `colorspace.h` / `raw_color_params.h` | Colorspace coefficient/primitive helper utilities |
| `frame.h` | `AVFrame`, frame side-data enum, cropping/refcounting |
| `buffer.h` / `buffer_internal.h` | `AVBufferRef`/`AVBufferPool` refcounted buffer model |
| `refstruct.h` | Refcounted generic struct helper (opaque refcount wrapper) |
| `imgutils.h` / `imgutils_internal.h` | Image plane size/stride/fill/copy helpers |
| `motion_vector.h` | `AVMotionVector` struct for motion-vector side data |
| `mastering_display_metadata.h` | HDR mastering display / content light level metadata |
| `hdr_dynamic_metadata.h` / `hdr_dynamic_vivid_metadata.h` | HDR10+ and HDR Vivid dynamic metadata structs |
| `dovi_meta.h` | Dolby Vision RPU/level metadata structs |
| `film_grain_params.h` | AV1/H.274 film grain synthesis parameter structs |
| `video_enc_params.h` | Per-block video encoder parameter export (QP maps etc.) |
| `video_hint.h` | Motion/encoder hinting side data |
| `detection_bbox.h` | Object-detection bounding box side data |
| `spherical.h` | 360°/spherical video projection metadata |
| `stereo3d.h` | Stereoscopic 3D layout metadata |
| `tdrdi.h` | 3D reference displays info metadata |
| `display.h` | Display transformation matrix helpers |
| `downmix_info.h` | Audio downmix metadata |
| `replaygain.h` | ReplayGain metadata struct |
| `ambient_viewing_environment.h` | Ambient viewing environment (HDR) metadata |
| `encryption_info.h` | Common encryption (CENC) info side data |
| `iamf.h` | Immersive Audio Model and Formats (IAMF) parameter structs |
| `side_data.h` | Generic side-data descriptor/props helper shared by frame & packet side data |
| `hwcontext.h` + `hwcontext_*.h` | Hardware device/frame context abstraction and per-backend API headers |
| `cpu.h` / `cpu_internal.h` | Runtime CPU feature detection/dispatch flags |
| `thread.h` / `slicethread.h` / `threadmessage.h` / `executor.h` | Threading primitives: mutex wrappers, slice thread pool, message queue, task executor |
| `timer.h` | High-resolution profiling timer macros |
| `time.h` / `time_internal.h` | Wall-clock helpers |
| `timecode.h` / `timecode_internal.h` | SMPTE timecode parse/format |
| `timestamp.h` | PTS/DTS human-readable formatting helpers |
| `random_seed.h` | Seed acquisition for RNGs |
| `lfg.h` / `sfc64.h` | Lagged-Fibonacci and SFC64 PRNGs |
| `crc.h` / `crc_internal.h` | CRC computation with selectable polynomials |
| `adler32.h` | Adler-32 checksum |
| `md5.h` / `sha.h` / `sha512.h` / `ripemd.h` / `murmur3.h` / `hash.h` | Cryptographic/non-crypto hash implementations and generic `AVHashContext` dispatcher |
| `hmac.h` | HMAC construction over the hash primitives |
| `aes.h` / `aes_ctr.h` / `aes_internal.h` | AES block cipher + CTR mode wrapper |
| `des.h` | DES/3DES cipher |
| `rc4.h` | RC4 stream cipher |
| `blowfish.h` | Blowfish cipher |
| `cast5.h` | CAST5 cipher |
| `tea.h` / `xtea.h` | TEA / XTEA ciphers |
| `twofish.h` | Twofish cipher |
| `camellia.h` | Camellia cipher |
| `lzo.h` | LZO1x decompression |
| `zlib_utils.h` | zlib helper wrappers |
| `base64.h` | Base64 encode/decode |
| `uuid.h` | UUID parse/format |
| `eval.h` / `eval.c` | `AVExpr` arithmetic-expression mini-language evaluator |
| `parseutils.h` | Date/time/ratio/color-name string parsing helpers |
| `lls.h` | Linear least squares solver |
| `pca.h` | Principal component analysis |
| `fifo.h` / `audio_fifo.h` / `container_fifo.h` | Generic, audio-sample-aware, and container-of-frames FIFOs |
| `dynarray.h` | Dynamic array growth macros |
| `tree.h` | AVL-style tree container |
| `qsort.h` | Reentrant sort helper |
| `reverse.h` | Bit-reversal lookup table accessor |
| `file.h` / `file_open.h` / `getenv_utf8.h` / `wchar_filename.h` | File I/O and Windows-safe path/environment helpers |
| `tablegen.h` | Host-side table-generation compatibility shims |
| `xga_font_data.h` | Built-in bitmap font glyph data (for burned-in text/debug overlays) |
| `objc.h` | Objective-C ARC helper macros (Apple platforms) |
| `macos_kperf.h` | macOS kperf profiling counter access |
| `vulkan.h` / `vulkan_functions.h` / `vulkan_loader.h` | Shared Vulkan instance/device/function-table plumbing used by hwcontext_vulkan and swscale's Vulkan backend |
| `emms.h` | x86 MMX state-clearing helper macro |
| `float2half.h` / `half2float.h` | IEEE 754 half-precision float conversion |
| `cuda_check.h` | CUDA driver API error-check helper macros |

---

## 2. Pixel Formats (`AVPixelFormat`, `pixfmt.h`)

268 concrete enumerators (plus `AV_PIX_FMT_NONE`). Descriptive metadata for each format is exposed at runtime via `av_pix_fmt_desc_get()` returning `AVPixFmtDescriptor` (name, `nb_components`, `log2_chroma_w/h`, per-component `AVComponentDescriptor{plane, step, offset, shift, depth}`, and a flags bitmask).

### `AVPixFmtDescriptor` flags (`AV_PIX_FMT_FLAG_*`)
`BE`, `PAL`, `BITSTREAM`, `HWACCEL`, `PLANAR`, `RGB`, `ALPHA`, `BAYER`, `FLOAT`, `XYZ` (bit-packed formats implied by absence of `PLANAR`).

### Families (grouped by name pattern / descriptor properties)

**Planar YUV, 8-bit** (`PLANAR`, no `BE`/`FLOAT`): `YUV410P, YUV411P, YUV420P, YUV422P, YUV440P, YUV444P, YUVJ411P, YUVJ420P, YUVJ422P, YUVJ440P, YUVJ444P` (the `YUVJ*` variants are full-range aliases, deprecated in favor of `color_range`), plus alpha variants `YUVA420P, YUVA422P, YUVA444P`.

**Planar YUV, high-bit-depth (9/10/12/14/16-bit, LE+BE pairs)**: `YUV420P9/10/12/14/16, YUV422P9/10/12/14/16, YUV444P9/10/12/14/16, YUV440P10/12`, each with `BE`/`LE` suffix; MSB-packed variants `YUV444P10MSBBE/LE`, `YUV444P12MSBBE/LE`. Alpha-plane counterparts: `YUVA420P9/10/16, YUVA422P9/10/12/16, YUVA444P9/10/12/16` (BE/LE).

**Planar GBR (RGB stored as separate planes)**: `GBRP` (8-bit, alias `GBR24P`) plus `GBRP9/10/12/14/16` (BE/LE), MSB variants `GBRP10MSBBE/LE`, `GBRP12MSBBE/LE`; alpha variants `GBRAP, GBRAP10/12/14/16` (BE/LE), `GBRAP32BE/LE`; float variants `GBRPF16/F32` and `GBRAPF16/F32` (BE/LE, IEEE-754 half/single precision).

**Packed YUV 4:2:2 / 4:1:1**: `YUYV422, UYVY422, YVYU422` (8-bit 4:2:2, differing byte order), `UYYVYY411` (4:1:1). High-bit-depth packed 4:2:2: `Y210, Y212, Y216` (BE/LE); `Y412`-style is exposed via `XV36`; interleaved-chroma "P2xx" biplanar families below.

**Packed 4:4:4 (Y+chroma+alpha variants)**: `AYUV, UYVA, VUYA, VUYX, VYU444`, high bit-depth packed `AYUV64` (BE/LE), `XV30/XV36/XV48` (BE/LE, alpha-undefined variants of Y410/Y412/Y416-style layouts), `V30XBE/LE`.

**Biplanar YUV (NV-style, one interleaved chroma plane)**: 8-bit `NV12, NV21` (4:2:0), `NV16` (4:2:2), `NV24, NV42` (4:4:4); high-bit-depth interleaved-chroma biplanar: `P010, P012, P016` (4:2:0, BE/LE), `P210, P212, P216` (4:2:2, BE/LE), `P410, P412, P416` (4:4:4, BE/LE); legacy `NV20BE/LE` (4:2:2, 10-bit).

**Grayscale / luma-only**: `GRAY8, GRAY9, GRAY10, GRAY12, GRAY14, GRAY16, GRAY32` (BE/LE where bit depth > 8), float `GRAYF16, GRAYF32` (BE/LE); with-alpha `YA8` (alias `YA16`... actually `YA8`/`Y400A`/`GRAY8A` alias group), `YA16` (BE/LE), float+alpha `YAF16, YAF32` (BE/LE). `MONOWHITE`, `MONOBLACK` (1bpp bitstream).

**Packed RGB (classic)**: `RGB24, BGR24` (8:8:8); low-bit packed `RGB4, RGB4_BYTE, RGB8, BGR4, BGR4_BYTE, BGR8` (3:3:2 / 1:2:1 packings); 15/16-bit `RGB444, RGB555, RGB565, BGR444, BGR555, BGR565` (BE/LE); high depth `RGB48, BGR48` (16-bit/channel, BE/LE); 10-bit packed `X2RGB10, X2BGR10` (BE/LE); 32-bit with padding `0RGB, RGB0, 0BGR, BGR0`; with alpha `ARGB, RGBA, ABGR, BGRA`; 16-bit/channel with alpha `RGBA64, BGRA64` (BE/LE); float `RGBF16/F32` and `RGBAF16/F32` (BE/LE), `RGB96, RGBA128` (32-bit int/channel, BE/LE).

**Palettized / indexed**: `PAL8` (8-bit index into an RGB32 palette carried in a side plane).

**Bayer (single-sensor mosaic RGB)**: `BAYER_BGGR8/RGGB8/GBRG8/GRBG8` (8-bit) and 16-bit variants `BAYER_BGGR16/RGGB16/GBRG16/GRBG16` (BE/LE) — 8 base patterns × bit depths.

**XYZ (CIE colorimetric)**: `XYZ12` (BE/LE).

**Hardware surface handles** (`HWACCEL` flag, opaque backend-specific payload): `VAAPI, DXVA2_VLD, D3D11VA_VLD, D3D11, D3D12, VDPAU, VIDEOTOOLBOX, CUDA, QSV, MMAL, MEDIACODEC, OPENCL, DRM_PRIME, VULKAN, AMF_SURFACE, OHCODEC, CUARRAY`.

Aliases worth noting for a Rust port's format table: `Y400A`/`GRAY8A` = `YA8`; `GBR24P` = `GBRP`; `SMPTEST428_1` naming pattern also appears in the color-primaries/transfer enums (see §3).

---

## 3. Color Science Enumerations (`pixfmt.h`)

### `AVColorPrimaries` (matches ISO/IEC 23091-2 / ITU-T H.273 §8.1 code points)
`RESERVED0, BT709(1), UNSPECIFIED(2), RESERVED(3), BT470M(4), BT470BG(5), SMPTE170M(6), SMPTE240M(7), FILM(8), BT2020(9), SMPTE428(10, alias SMPTEST428_1), SMPTE431(11), SMPTE432(12), EBU3213(22, alias JEDEC_P22), NB` + FFmpeg-custom extension block starting at `EXT_BASE=256`: `V_GAMUT`.

### `AVColorTransferCharacteristic` (H.273 §8.2)
`RESERVED0, BT709(1), UNSPECIFIED(2), RESERVED(3), GAMMA22(4), GAMMA28(5), SMPTE170M(6), SMPTE240M(7), LINEAR(8), LOG(9), LOG_SQRT(10), IEC61966_2_4(11), BT1361_ECG(12), IEC61966_2_1(13), BT2020_10(14), BT2020_12(15), SMPTE2084(16, alias SMPTEST2084), SMPTE428(17, alias SMPTEST428_1), ARIB_STD_B67(18), NB` + custom extension `V_LOG`.

### `AVColorSpace` (H.273 §8.3, YUV matrix coefficients)
`RGB(0), BT709(1), UNSPECIFIED(2), RESERVED(3), FCC(4), BT470BG(5), SMPTE170M(6), SMPTE240M(7), YCGCO(8, alias YCOCG), BT2020_NCL(9), BT2020_CL(10), SMPTE2085(11), CHROMA_DERIVED_NCL(12), CHROMA_DERIVED_CL(13), ICTCP(14), IPT_C2(15), YCGCO_RE(16), YCGCO_RO(17), NB`.

### `AVColorRange`
`UNSPECIFIED(0), MPEG(1, narrow/limited range), JPEG(2, full range), NB`.

### `AVChromaLocation`
`UNSPECIFIED(0), LEFT(1), CENTER(2), TOPLEFT(3), TOP(4), BOTTOMLEFT(5), BOTTOM(6), NB`.

### `AVAlphaMode` (new)
`UNSPECIFIED(0), PREMULTIPLIED(1), STRAIGHT(2), NB`.

Supporting subsystem: **`csp.h`** exposes helpers to derive RGB↔YUV conversion coefficients and primaries/whitepoint chromaticity from the enums above (`av_csp_primaries_desc_from_id`, luma coefficient lookup, whitepoint/primaries structs); **`colorspace.h`** carries the low-level YUV↔RGB coefficient defines consumed by swscale.

---

## 4. Sample Formats & Channel Layouts (audio)

### `AVSampleFormat` (`samplefmt.h`)
Interleaved: `U8, S16, S32, FLT, DBL, S64`. Planar counterparts: `U8P, S16P, S32P, FLTP, DBLP, S64P`. Terminator `NB`. Helper API: `av_get_sample_fmt_name`, `av_get_bytes_per_sample`, `av_sample_fmt_is_planar`, buffer-size/fill/copy helpers (`av_samples_*`).

### Channel model — `AVChannelLayout` / `AVChannel` (`channel_layout.h`, replaces the old bitmask-only API)

**`AVChannelOrder`** (how a layout's channel positions are expressed):
- `AV_CHANNEL_ORDER_UNSPEC` — only channel count known, no position info
- `AV_CHANNEL_ORDER_NATIVE` — bitmask over `AVChannel` enum (native/legacy order, ≤63 channels)
- `AV_CHANNEL_ORDER_CUSTOM` — explicit per-index channel map (arbitrary count, supports `AV_CHAN_UNUSED`/gaps)
- `AV_CHANNEL_ORDER_AMBISONIC` — implicit ACN-ordered ambisonic channels, optionally with extra non-diegetic channels appended

**`AVChannel`** identifiers: `FRONT_LEFT, FRONT_RIGHT, FRONT_CENTER, LOW_FREQUENCY, BACK_LEFT, BACK_RIGHT, FRONT_LEFT_OF_CENTER, FRONT_RIGHT_OF_CENTER, BACK_CENTER, SIDE_LEFT, SIDE_RIGHT, TOP_CENTER, TOP_FRONT_LEFT, TOP_FRONT_CENTER, TOP_FRONT_RIGHT, TOP_BACK_LEFT, TOP_BACK_CENTER, TOP_BACK_RIGHT, STEREO_LEFT, STEREO_RIGHT, WIDE_LEFT, WIDE_RIGHT, SURROUND_DIRECT_LEFT, SURROUND_DIRECT_RIGHT, LOW_FREQUENCY_2, TOP_SIDE_LEFT, TOP_SIDE_RIGHT, BOTTOM_FRONT_CENTER, BOTTOM_FRONT_LEFT, BOTTOM_FRONT_RIGHT, SIDE_SURROUND_LEFT, SIDE_SURROUND_RIGHT, TOP_SURROUND_LEFT, TOP_SURROUND_RIGHT, BINAURAL_LEFT, BINAURAL_RIGHT`, plus sentinel ranges `UNUSED (0x200)`, `UNKNOWN (0x300)`, and an Ambisonic ACN index range `AMBISONIC_BASE (0x400)…AMBISONIC_END (0x7ff)`.

**Predefined native layouts** (`AV_CH_LAYOUT_*` bitmask macros, wrapped as `AV_CHANNEL_LAYOUT_*` constants of type `AVChannelLayout`): `MONO, STEREO, 2POINT1, 2_1, SURROUND, 3POINT1, 4POINT0, 4POINT1, 2_2, QUAD, 5POINT0, 5POINT1, 5POINT0_BACK, 5POINT1_BACK, 6POINT0, 6POINT0_FRONT, HEXAGONAL, 3POINT1POINT2, 6POINT1, 6POINT1_BACK, 6POINT1_FRONT, 7POINT0, 7POINT0_FRONT, 7POINT1, 7POINT1_WIDE, 7POINT1_WIDE_BACK, 5POINT1POINT2, 5POINT1POINT2_BACK, OCTAGONAL, CUBE, 5POINT1POINT4_BACK, 7POINT1POINT2, 7POINT1POINT4_BACK, 7POINT2POINT3, 9POINT1POINT4_BACK, 9POINT1POINT6, HEXADECAGONAL, BINAURAL, STEREO_DOWNMIX, 22POINT2` plus alias `7POINT1_TOP_BACK`.

**API surface**: construction (`av_channel_layout_default`, `_from_mask`, `_from_string`, `_custom_init`, ambisonic init), copy/compare/uninit, per-channel name lookup (`av_channel_name`, `av_channel_description`, `av_channel_from_string`), index↔channel translation (`av_channel_layout_channel_from_index`, `_index_from_channel`, `_index_from_string`, `_channel_from_string`), subset queries, retype (`av_channel_layout_retype`).

**`AVMatrixEncoding`** (used by downmix/rematrix metadata): `NONE, DOLBY, DPLII, DPLIIX, DPLIIZ, DOLBYEX, DOLBYHEADPHONE, NB`.

---

## 5. AVOption System (`opt.h`)

### `AVOptionType`
`FLAGS, INT, INT64, DOUBLE, FLOAT, STRING, RATIONAL, BINARY, DICT, UINT64, CONST, IMAGE_SIZE, PIXEL_FMT, SAMPLE_FMT, VIDEO_RATE, DURATION, COLOR, BOOL, CHLAYOUT, UINT, FLAG_ARRAY`.

Each `AVOption` entry carries: `name`, `help` string, struct-member `offset`, `type`, default value union, `min`/`max`, a `flags` bitmask, and an optional `unit` string used to group related `CONST` entries (e.g., named flag/enum choices) under a parent option — this `unit` mechanism is how FFmpeg exposes "named constant" choices for flags/enum-typed options generically.

### `AV_OPT_FLAG_*`
`ENCODING_PARAM, DECODING_PARAM, AUDIO_PARAM, VIDEO_PARAM, SUBTITLE_PARAM, EXPORT, READONLY, BSF_PARAM, RUNTIME_PARAM, FILTERING_PARAM, DEPRECATED, CHILD_CONSTS` — bit flags classifying which contexts/tools an option applies to and whether it's read-only/exported/deprecated.

### Object model
Any struct wanting AVOption support embeds an `AVClass *` as its first member; the `AVClass` supplies `class_name`, `item_name()`, a `static const AVOption[]` table, `version`, and offsets for log-level/parent-context fields. `av_opt_next`/`av_opt_find`/`av_opt_find2` walk the table (optionally recursing into "child" objects via `AV_OPT_FLAG_CHILD_CONSTS` / child-class iteration).

### Get/Set API family
Per-type setters/getters: `av_opt_set{,_int,_double,_q,_bin,_image_size,_pixel_fmt,_sample_fmt,_video_rate,_chlayout,_dict_val}` and matching `av_opt_get_*`; array-valued options via `av_opt_set_array`/`av_opt_get_array`/`av_opt_get_array_size` (backing `AV_OPT_TYPE_FLAG_ARRAY`, etc.); generic string-based `av_opt_set_from_string`, `av_opt_set_dict[2]`; string-value evaluation helpers `av_opt_eval_*`; introspection `av_opt_is_set_to_default[_by_name]`, `av_opt_flag_is_set`; serialization to/from a single string via `av_opt_serialize`; range introspection via `av_opt_query_ranges[_default]` returning `AVOptionRanges`/`AVOptionRange` (component ranges, useful for e.g. multi-component options).

### AVDictionary (`dict.h`)
Simple ordered key/value string store used for demuxer/muxer/codec option maps and metadata. Flags: `AV_DICT_MATCH_CASE, AV_DICT_IGNORE_SUFFIX, AV_DICT_DONT_STRDUP_KEY, AV_DICT_DONT_STRDUP_VAL, AV_DICT_DONT_OVERWRITE, AV_DICT_APPEND, AV_DICT_MULTIKEY, AV_DICT_DEDUP`. Core ops: `av_dict_get`, `av_dict_set[_int]`, `av_dict_get_string`, `av_dict_parse_string`, `av_dict_copy`, `av_dict_free`, iteration via `av_dict_iterate`.

---

## 6. AVFrame / AVBuffer / Refcounting

### Refcounting primitives
- **`AVBufferRef`/`AVBuffer`** (`buffer.h`): atomic-refcounted heap allocation; `av_buffer_alloc/allocz/create`, `av_buffer_ref/unref`, `av_buffer_is_writable`, `av_buffer_make_writable` (copy-on-write), `av_buffer_realloc`, `av_buffer_replace`; **`AVBufferPool`** provides a reusable-allocation pool (`av_buffer_pool_init[2]`, `av_buffer_pool_get`).
- **`AVRefStructRef`**-style generic refcounted struct helper (`refstruct.h`) used internally for non-buffer refcounted objects.

### `AVFrame` (`frame.h`)
Holds up to `AV_NUM_DATA_POINTERS` plane pointers/linesizes (video) or per-channel `extended_data` (audio), an `AVBufferRef *buf[]`/`extended_buf` array backing the data, format (`AVPixelFormat` or `AVSampleFormat`), width/height or `nb_samples` + `AVChannelLayout ch_layout`, PTS/duration, `AVDictionary *metadata`, a linked list of `AVFrameSideData *side_data[]`, HW frame context reference (`hw_frames_ctx`), color metadata (primaries/trc/space/range/chroma_location), and cropping fields. API: `av_frame_alloc/free`, `av_frame_ref/unref/clone`, `av_frame_get_buffer`, `av_frame_make_writable`, `av_frame_copy[_props]`, `av_frame_apply_cropping`, plus per-side-data accessors (`av_frame_get_side_data`, `av_frame_new_side_data[_from_buf]`, `av_frame_remove_side_data`, `av_frame_side_data_*` iterator helpers shared with packets via `side_data.h`).

### `AVFrameSideDataType` (full enumeration)
| Enumerator | Meaning |
|---|---|
| `PANSCAN` | Pan/scan cropping rectangle |
| `A53_CC` | CEA-708/A53 closed captions |
| `STEREO3D` | Stereoscopic 3D layout |
| `MATRIXENCODING` | Matrixed-surround encoding mode |
| `DOWNMIX_INFO` | Audio downmix metadata |
| `REPLAYGAIN` | ReplayGain values |
| `DISPLAYMATRIX` | 3x3 display transform matrix |
| `AFD` | Active Format Description |
| `MOTION_VECTORS` | Per-block motion vectors |
| `SKIP_SAMPLES` | Encoder padding/skip sample counts |
| `AUDIO_SERVICE_TYPE` | Audio service type (main, comментary, etc.) |
| `MASTERING_DISPLAY_METADATA` | HDR mastering display luminance/primaries |
| `GOP_TIMECODE` | GOP-level timecode |
| `SPHERICAL` | 360° projection metadata |
| `CONTENT_LIGHT_LEVEL` | HDR MaxCLL/MaxFALL |
| `ICC_PROFILE` | Embedded ICC color profile |
| `S12M_TIMECODE` | SMPTE 12M timecode |
| `DYNAMIC_HDR_PLUS` | HDR10+ dynamic metadata |
| `REGIONS_OF_INTEREST` | Encoder region-of-interest hints |
| `VIDEO_ENC_PARAMS` | Per-block encode parameter export |
| `SEI_UNREGISTERED` | Raw unregistered SEI payload |
| `FILM_GRAIN_PARAMS` | AV1/H.274 film grain synthesis params |
| `DETECTION_BBOXES` | Object detection bounding boxes |
| `DOVI_RPU_BUFFER` | Raw Dolby Vision RPU buffer |
| `DOVI_METADATA` | Parsed Dolby Vision metadata |
| `DYNAMIC_HDR_VIVID` | HDR Vivid dynamic metadata |
| `AMBIENT_VIEWING_ENVIRONMENT` | Ambient viewing environment metadata |
| `VIDEO_HINT` | Motion/encoder hinting |
| `LCEVC` | LCEVC enhancement layer data |
| `VIEW_ID` | Multiview stream view identifier |
| `3D_REFERENCE_DISPLAYS` | 3D reference display info |
| `EXIF` | Embedded EXIF metadata |
| `DYNAMIC_HDR_SMPTE_2094_APP5` | SMPTE ST 2094-40 App5 dynamic HDR |
| `IAMF_MIX_GAIN_PARAM` | IAMF mix gain parameter |
| `IAMF_DEMIXING_INFO_PARAM` | IAMF demixing info parameter |
| `IAMF_RECON_GAIN_INFO_PARAM` | IAMF reconstruction gain info |

### `AVPacketSideDataType` (defined in `libavcodec/packet.h`, listed here since it's the packet-level analogue of the same `side_data.h` descriptor mechanism)
`PALETTE, NEW_EXTRADATA, PARAM_CHANGE, H263_MB_INFO, REPLAYGAIN, DISPLAYMATRIX, STEREO3D, AUDIO_SERVICE_TYPE, QUALITY_STATS, FALLBACK_TRACK, CPB_PROPERTIES, SKIP_SAMPLES, JP_DUALMONO, STRINGS_METADATA, SUBTITLE_POSITION, MATROSKA_BLOCKADDITIONAL, WEBVTT_IDENTIFIER, WEBVTT_SETTINGS, METADATA_UPDATE, MPEGTS_STREAM_ID, MASTERING_DISPLAY_METADATA, SPHERICAL, CONTENT_LIGHT_LEVEL, A53_CC, ENCRYPTION_INIT_INFO, ENCRYPTION_INFO, AFD, PRFT, ICC_PROFILE, DOVI_CONF, S12M_TIMECODE, DYNAMIC_HDR10_PLUS, IAMF_MIX_GAIN_PARAM, IAMF_DEMIXING_INFO_PARAM, IAMF_RECON_GAIN_INFO_PARAM, AMBIENT_VIEWING_ENVIRONMENT, FRAME_CROPPING, LCEVC, RTCP_SR, DOVI_RPU_BUFFER (implied), DYNAMIC_HDR_SMPTE_2094_APP5, 3D_REFERENCE_DISPLAYS, HEVC_CONF, EXIF`. (Both side-data enums share a common descriptor mechanism in `libavutil/side_data.h` exposing per-type name/property lookup.)

---

## 7. Hardware Device / Frame Context Model (`hwcontext*.h`)

### `AVHWDeviceType`
| Type | Primary OS / API |
|---|---|
| `VDPAU` | Linux (NVIDIA VDPAU) |
| `CUDA` | Cross-platform (NVIDIA CUDA) |
| `VAAPI` | Linux/BSD (Intel/AMD VA-API) |
| `DXVA2` | Windows (legacy DirectX Video Acceleration 2) |
| `QSV` | Cross-platform (Intel Quick Sync via oneVPL/MFX) |
| `VIDEOTOOLBOX` | macOS/iOS |
| `D3D11VA` | Windows (Direct3D 11) |
| `DRM` | Linux (DRM/KMS PRIME buffer sharing) |
| `OPENCL` | Cross-platform |
| `MEDIACODEC` | Android |
| `VULKAN` | Cross-platform |
| `D3D12VA` | Windows (Direct3D 12) |
| `AMF` | Windows/Linux (AMD AMF) |
| `OHCODEC` | OpenHarmony |

### Structural model
`AVHWDeviceContext` (opaque backend `hwctx`, device-level state, refcounted via `AVBufferRef`) is allocated with `av_hwdevice_ctx_alloc(type)` then finalized with `av_hwdevice_ctx_init`. `AVHWFramesContext` (`av_hwframe_ctx_alloc`) describes a pool of hardware surfaces of a given `AVPixelFormat`/`sw_format`/size tied to a device context, allocated frames obtained via `av_hwframe_get_buffer`; transfer to/from system memory via `av_hwframe_transfer_data`; cross-device mapping via `av_hwframe_map`/`av_hwframe_ctx_create_derived`. Each backend header (`hwcontext_cuda.h`, `hwcontext_vaapi.h`, etc.) defines the backend-specific struct embedded as `hwctx`/`AVHWDeviceContext.hwctx` payload (e.g., CUDA context handle, VADisplay, ID3D11Device, etc.) and the corresponding `AV_PIX_FMT_*` HW surface handle from §2.

---

## 8. Math / Util Subsystems

### Rational & rounding (`rational.h`, `mathematics.h`)
`AVRational{num,den}` with `av_add_q/sub_q/mul_q/div_q/cmp_q/inv_q`, `av_d2q`/`av_q2d`, `av_reduce`. Rescaling helpers `av_rescale`, `av_rescale_rnd`, `av_rescale_q[_rnd]`, `av_compare_ts`, `av_gcd`. `AVRounding`: `ZERO, INF, DOWN, UP, NEAR_INF`, plus the bitmask modifier `AV_ROUND_PASS_MINMAX` for passthrough of `INT64_MIN/MAX`.

### Fixed point / software float
`fixed_dsp.h` — fixed-point vector DSP function table (butterfly/scalarproduct primitives used by fixed-point audio transforms). `softfloat.h`/`softfloat_ieee754.h` — deterministic bit-exact software float type for cross-platform-reproducible codecs.

### Transform module — `tx.h` (FFT/MDCT/RDFT/DCT unification, replaces legacy separate FFT/MDCT/RDFT/DCT APIs)
**`AVTXType`** (data type × transform kind), each available in float/double/int32 precision:
- `FLOAT_FFT / DOUBLE_FFT / INT32_FFT` — complex-to-complex FFT
- `FLOAT_MDCT / DOUBLE_MDCT / INT32_MDCT` — Modified DCT (forward + inverse, half- or full-length inverse)
- `FLOAT_RDFT / DOUBLE_RDFT / INT32_RDFT` — real↔complex DFT
- `FLOAT_DCT / DOUBLE_DCT / INT32_DCT` — DCT-II/III family
- `FLOAT_DCT_I / DOUBLE_DCT_I / INT32_DCT_I` — DCT-I
- `FLOAT_DST_I / DOUBLE_DST_I / INT32_DST_I` — DST-I
- `AV_TX_NB` terminator.

**Init flags** (`av_tx_init`): `AV_TX_INPLACE`, `AV_TX_UNALIGNED`, `AV_TX_FULL_IMDCT`, `AV_TX_REAL_TO_REAL`, `AV_TX_REAL_TO_IMAGINARY`. Public entry points: `av_tx_init(AVTXContext**, av_tx_fn*, enum AVTXType, int inv, int len, const void *scale, uint64_t flags)`, invoked per-call via the returned function pointer. Backing implementation files present in-tree: `tx_template.c`, `tx_float.c`, `tx_double.c`, `tx_int32.c`.

### LLS / PCA
`lls.h` — incremental linear least squares solver (`LLSModel`, `av_init_lls`, `av_update_lls`, `av_solve_lls`) used by codecs needing online regression. `pca.h` — principal component analysis (`av_pca_init`, `av_pca_add_data`, `av_pca_get_eigenvectors`).

### Hash algorithms (`hash.h` generic dispatcher + dedicated headers)
Generic `AVHashContext` supports algorithm selection by name string via `av_hash_alloc(name)`; algorithm name table: `MD5, murmur3, RIPEMD128, RIPEMD160, RIPEMD256, RIPEMD320, SHA160, SHA224, SHA256, SHA512/224, SHA512/256, SHA384, SHA512, CRC32, adler32`. Each also has a dedicated lower-level header/API: `md5.h`, `sha.h` (SHA-1/224/256), `sha512.h` (SHA-384/512 family incl. truncated 512/224, 512/256), `ripemd.h` (128/160/256/320), `murmur3.h`, `crc.h` (parameterized polynomial CRC via `av_crc_get_table`/`av_crc`), `adler32.h`. `hmac.h` layers HMAC construction over any of the above digest primitives (algorithm selection enum mirrors the hash types).

### Symmetric ciphers
`aes.h` (+ `aes_ctr.h` for CTR-mode streaming), `des.h`, `rc4.h`, `blowfish.h`, `cast5.h`, `tea.h`/`xtea.h`, `twofish.h`, `camellia.h` — each exposes an opaque context struct, an init/key-schedule function, and ECB/CBC-style encrypt/decrypt entry points (exact signatures per-cipher; CTR only generically wrapped for AES).

### Compression / encoding utilities
`lzo.h` — `av_lzo1x_decode` (decompress-only). `zlib_utils.h` — thin zlib inflate/deflate helper wrappers. `base64.h` — `av_base64_encode`/`av_base64_decode`.

### RNG
`random_seed.h` — `av_get_random_seed()` gathers OS entropy. `lfg.h` — Lagged Fibonacci generator (`AVLFG`, `av_lfg_init`, `av_lfg_get`). `sfc64.h` — SFC64 PRNG state/step API.

### Timecode / display / HDR / DoVi / film grain / video-enc-params / detection-bbox metadata structs
Covered in §6 as frame side-data payload types; the corresponding headers (`timecode.h`, `display.h`, `spherical.h`, `stereo3d.h`, `mastering_display_metadata.h`, `hdr_dynamic_metadata.h`, `hdr_dynamic_vivid_metadata.h`, `dovi_meta.h`, `film_grain_params.h`, `video_enc_params.h`, `detection_bbox.h`, `tdrdi.h`, `ambient_viewing_environment.h`) define the plain-data struct layouts and small alloc helpers (e.g. `av_mastering_display_metadata_alloc`, `av_film_grain_params_create_side_data`) used to attach them via the `AVFrameSideData`/`AVPacketSideData` mechanism.

### Dictionaries, logging, threading, bprint, opt, parseutils — see §5, plus:
- **`log.h`**: level constants (`AV_LOG_QUIET` … `AV_LOG_TRACE`), `av_log(avcl, level, fmt, …)`, per-object class-driven prefixing, `av_log_set_callback`/`av_log_set_level`.
- **Threading** (`thread.h`, `slicethread.h`, `threadmessage.h`, `executor.h`): pthread/Win32-abstracted mutex/cond/once wrappers; `AVSliceThread` fixed worker-pool for slice-parallel filters/codecs; `AVThreadMessageQueue` bounded producer/consumer queue; `AVExecutor` generic task-submission executor with a user-supplied task callback.
- **`bprint.h`**: `AVBPrint` auto-growing string builder with `av_bprintf`, `av_bprint_chars`, size-limited variants.
- **`parseutils.h`**: `av_parse_video_rate`, `av_parse_color` (named/hex color parsing), `av_parse_time` (date/duration parsing), image-size string parsing.

### Timestamp handling (`timestamp.h`)
`av_ts2str`/`av_ts2timestr` macros producing human-readable PTS/time strings for logging, using a thread-local scratch buffer.

### Error codes (`error.h`)
POSIX-errno-based `AVERROR(e)` wrapping plus FFmpeg-specific codes: `AVERROR_BSF_NOT_FOUND, AVERROR_BUG, AVERROR_BUFFER_TOO_SMALL, AVERROR_DECODER_NOT_FOUND, AVERROR_DEMUXER_NOT_FOUND, AVERROR_ENCODER_NOT_FOUND, AVERROR_EOF, AVERROR_EXIT, AVERROR_EXTERNAL, AVERROR_FILTER_NOT_FOUND, AVERROR_INVALIDDATA, AVERROR_MUXER_NOT_FOUND, AVERROR_OPTION_NOT_FOUND, AVERROR_PATCHWELCOME, AVERROR_PROTOCOL_NOT_FOUND, AVERROR_STREAM_NOT_FOUND, AVERROR_BUG2, AVERROR_UNKNOWN, AVERROR_EXPERIMENTAL, AVERROR_INPUT_CHANGED, AVERROR_OUTPUT_CHANGED, AVERROR_HTTP_*` (400/401/403/404/other-4xx/5xx). `av_strerror(errnum, buf, size)` renders a message.

---

## 9. `eval.c` Expression Language (`AVExpr`, user-facing DSL used by filters)

Public API (`eval.h`): `av_expr_parse(&expr, s, const_names, func1_names, funcs1, func2_names, funcs2, log_offset, log_ctx)`, `av_expr_eval(expr, const_values, opaque)`, `av_expr_parse_and_eval(...)`, `av_expr_free`. Callers supply arrays of named constants and 1-/2-argument callback functions that extend the grammar per-context (e.g., filters expose frame-specific variables this way).

### Built-in constants
`PI`, `E`.

### Built-in unary math functions (libm passthrough)
`sinh, cosh, tanh, sin, cos, tan, atan, asin, acos, exp, log, abs`.

### Built-in special forms / operators (parsed as function-call syntax)
`squish, gauss, mod, max, min, eq, gte, gt, lte, lt, ld, isnan, isinf, st, while, taylor, root, floor, ceil, trunc, sqrt, not, pow, print, random, hypot, if, ifnot, bitand, bitor, between, clip`.

### Infix operators (grammar-level, standard precedence)
`+ - * / ^` (power) plus comparison/logical forms surfaced as the named functions above (`eq/gt/lt/gte/lte`), and `,`/`;`-style sequencing (`e_last`) for chained `st(...)`-based scripting. Values are `double`; there is no string type. `ld(idx)`/`st(idx, val)` provide 10-slot mutable variable storage across an evaluation for iterative expressions (`while`, `taylor`).

---

## 10. libswresample — Feature Inventory

### Files
`swresample.c/.h` (public API + `SwrContext`), `options.c` (AVOption table), `resample.c/.h` + `resample_template.c` + `resample_dsp.c` (native FFmpeg polyphase resampler), `soxr_resample.c` (optional libsoxr backend), `rematrix.c` + `rematrix_template.c` (channel mixing matrix build/apply), `audioconvert.c/.h` (sample-format conversion), `dither.c` + `dither_template.c` + `noise_shaping_data.c` (dither/noise-shaping tables), `log2_tab.c`. SIMD: `x86/` (audio_convert, rematrix, resample asm), `aarch64/` and `arm/` (audio_convert_neon, resample NEON asm).

### Resampler engines (`SwrEngine`)
`SWR_ENGINE_SWR` (native FFmpeg polyphase filter resampler), `SWR_ENGINE_SOXR` (delegates to the external libsoxr library when configured in), `SWR_ENGINE_NB`.

### Resampling filter types (`SwrFilterType`, native engine only)
`SWR_FILTER_TYPE_CUBIC`, `SWR_FILTER_TYPE_BLACKMAN_NUTTALL` (windowed sinc), `SWR_FILTER_TYPE_KAISER` (windowed sinc, tunable beta).

### Dither methods (`SwrDitherType`)
`SWR_DITHER_NONE, SWR_DITHER_RECTANGULAR, SWR_DITHER_TRIANGULAR, SWR_DITHER_TRIANGULAR_HIGHPASS`, and a noise-shaping bank (`SWR_DITHER_NS` base): `SWR_DITHER_NS_LIPSHITZ, SWR_DITHER_NS_F_WEIGHTED, SWR_DITHER_NS_MODIFIED_E_WEIGHTED, SWR_DITHER_NS_IMPROVED_E_WEIGHTED, SWR_DITHER_NS_SHIBATA, SWR_DITHER_NS_LOW_SHIBATA, SWR_DITHER_NS_HIGH_SHIBATA`.

### Rematrixing / channel mixing
Builds a per-(input channel, output channel) mix matrix from `AVChannelLayout` pairs (`swr_build_matrix2`), configurable per-channel mix levels (center/surround/LFE), overall rematrix volume and clipping ceiling, an explicit user-supplied matrix (`swr_set_matrix`), a raw channel index remap bypassing matrixing (`swr_set_channel_mapping`), and `AVMatrixEncoding` (Dolby/DPLII/etc.) selection for surround-encoded stereo downmix (§4).

### Sample format conversion
`audioconvert.c` performs conversion across the full `AVSampleFormat` matrix (interleaved ↔ planar, and among U8/S16/S32/S64/FLT/DBL) as an independent stage from resampling/rematrixing, with format-pair-specific SIMD kernels on x86/ARM.

### Every `AVOption` exposed by `SwrContext` (`options.c`)
| Name(s) | Type | Meaning |
|---|---|---|
| `isr`/`in_sample_rate` | int | input sample rate |
| `osr`/`out_sample_rate` | int | output sample rate |
| `isf`/`in_sample_fmt` | sample_fmt | input sample format |
| `osf`/`out_sample_fmt` | sample_fmt | output sample format |
| `tsf`/`internal_sample_fmt` | sample_fmt | internal working sample format |
| `ichl`/`in_chlayout` | chlayout | input channel layout |
| `ochl`/`out_chlayout` | chlayout | output channel layout |
| `uchl`/`used_chlayout` | chlayout | channel layout actually used |
| `clev`/`center_mix_level` | float | center channel mix level (dB-derived linear, range ±32) |
| `slev`/`surround_mix_level` | float | surround channel mix level |
| `lfe_mix_level` | float | LFE channel mix level |
| `rmvol`/`rematrix_volume` | float | overall rematrix volume scale |
| `rematrix_maxval` | float | clipping ceiling for rematrixed samples |
| `flags`/`swr_flags` | flags (unit `flags`) | engine flags; named const `res` = force resampling |
| `dither_scale` | float | dither amplitude scale |
| `dither_method` | int (unit `dither_method`) | selects one of the `SwrDitherType` values above by name |
| `filter_size` | int | native-resampler filter length |
| `phase_shift` | int (0–24) | native-resampler polyphase subdivision |
| `linear_interp` | bool | enable linear interpolation between filter phases |
| `exact_rational` | bool | prefer exact rational resampling ratio |
| `cutoff`/`resample_cutoff` | double (0–1) | filter cutoff frequency ratio |
| `resampler` (unit `resampler`) | int | selects `SwrEngine`; named consts `swr`, `soxr` |
| `precision` | double (15–33) | soxr resampling precision in bits |
| `cheby` | bool | soxr Chebyshev passband mode |
| `min_comp` | float | min timestamp/audio-data discrepancy (s) before compensation kicks in |
| `min_hard_comp` | float | min discrepancy (s) triggering hard pad/trim |
| `comp_duration` | float | duration (s) over which soft compensation stretches/squeezes data |
| `max_soft_comp` | float | max stretch/squeeze factor for soft compensation |
| `async` | float | simplified single-parameter timestamp-matching control |
| `first_pts` | int64 | assumed first PTS (in samples) |
| `matrix_encoding` (unit `matrix_encoding`) | int | selects `AVMatrixEncoding`; named consts `none, dolby, dplii` |
| `filter_type` (unit `filter_type`) | int | selects `SwrFilterType`; named consts `cubic, blackman_nuttall, kaiser` |
| `kaiser_beta` | double (2–16) | Kaiser window beta parameter |
| `output_sample_bits` | int (0–64) | output sample bit-depth override (dither target depth) |

### Public API surface (`swresample.h`)
`swr_alloc`, `swr_alloc_set_opts2`, `swr_init`, `swr_is_initialized`, `swr_close`, `swr_free`, `swr_convert`, `swr_convert_frame`, `swr_config_frame`, `swr_next_pts`, `swr_set_compensation`, `swr_set_channel_mapping`, `swr_build_matrix2`, `swr_set_matrix`, `swr_drop_output`, `swr_inject_silence`, `swr_get_delay`, `swr_get_out_samples`, `swr_get_class`.

### SIMD / dispatch
Architectures with hand-written asm: **x86** (audio format conversion, rematrix, resample — NASM/YASM `.asm`), **AArch64** and **ARM32** (NEON `.S` for audio_convert and resample). Dispatch follows the standard FFmpeg pattern: each `*_init.c` (`audio_convert_init.c`, `resample_init.c`, `rematrix_init.c`) queries `av_get_cpu_flags()` once at context-init time and overwrites C function-pointer table entries with the best available SIMD implementation for the detected flag set — no per-call branching.

---

## 11. libswscale — Feature Inventory

### Files
Legacy scaling core: `swscale.c`, `swscale_unscaled.c`, `utils.c`, `slice.c`, `hscale.c`, `hscale_fast_bilinear.c`, `vscale.c`, `output.c`, `input.c`, `yuv2rgb.c`, `rgb2rgb.c` (+ `_template.c`), `format.c`, `filters.c`, `gamma.c`, `alphablend.c`, `half2float.c`, `csputils.c`, `framepool.c`. New graph/ops backend (see below): `graph.c`, `ops.c`, `ops_chain.c`, `ops_dispatch.c`, `ops_memcpy.c`, `ops_optimizer.c`, `uops.c` (+ `_backend.c`, `_tmpl.c`, `uops_list.h`), `op_list_gen_template.c`. Color management: `cms.c` (ICC-style color-management/tone-mapping), `lut3d.c` (3D LUT support). `options.c` (AVOption table), `version.c`. Vulkan backend under `vulkan/` (`vulkan.c`, `ops.c`, `spvasm.h` — SPIR-V assembly-based compute pipeline).

### Legacy scaler algorithm flags (`SwsFlags` enum, `sws_flags`/`SWS_*` bitmask)
`SWS_FAST_BILINEAR, SWS_BILINEAR, SWS_BICUBIC, SWS_X (experimental), SWS_POINT (nearest neighbor), SWS_AREA (area averaging), SWS_BICUBLIN (bicubic luma + bilinear chroma), SWS_GAUSS, SWS_SINC (unwindowed), SWS_LANCZOS, SWS_SPLINE (natural cubic)` — exactly one scaler flag active at a time; plus modifier flags `SWS_PRINT_INFO, SWS_ACCURATE_RND, SWS_FULL_CHR_H_INT, SWS_FULL_CHR_H_INP, SWS_BITEXACT, SWS_ERROR_DIFFUSION (deprecated alias, use SwsDither), SWS_DIRECT_BGR (no-op, retained for ABI), SWS_UNSTABLE, SWS_STRICT`, and colorspace-selection macros `SWS_CS_ITU709, SWS_CS_FCC, SWS_CS_ITU601/ITU624/SMPTE170M, SWS_CS_SMPTE240M, SWS_CS_DEFAULT, SWS_CS_BT2020`.

### New-style scaler selection (`SwsScaler` via `scaler`/`scaler_sub` options, preferred over `sws_flags`)
`SWS_SCALE_AUTO, SWS_SCALE_BILINEAR, SWS_SCALE_BICUBIC (2-tap cubic B-spline), SWS_SCALE_POINT, SWS_SCALE_AREA, SWS_SCALE_GAUSSIAN, SWS_SCALE_SINC, SWS_SCALE_LANCZOS, SWS_SCALE_SPLINE, SWS_SCALE_NB`.

### Dithering (`SwsDither`)
`SWS_DITHER_NONE, SWS_DITHER_AUTO, SWS_DITHER_BAYER (ordered matrix), SWS_DITHER_ED (error diffusion), SWS_DITHER_A_DITHER (arithmetic addition), SWS_DITHER_X_DITHER (arithmetic XOR), SWS_DITHER_NB`.

### Alpha handling (`SwsAlphaBlend`)
`SWS_ALPHA_BLEND_NONE (ignore alpha), SWS_ALPHA_BLEND_UNIFORM (blend onto solid color), SWS_ALPHA_BLEND_CHECKERBOARD, SWS_ALPHA_BLEND_NB`.

### Color-mapping intent (`SwsIntent`, for gamut/tone mapping between differing primaries/transfer/gamut)
`SWS_INTENT_PERCEPTUAL, SWS_INTENT_RELATIVE_COLORIMETRIC (default), SWS_INTENT_SATURATION, SWS_INTENT_ABSOLUTE_COLORIMETRIC, SWS_INTENT_NB`.

### Backend selection (`SwsBackend`, bitmask, `sws_backends` option — selects which internal implementation pool may service a conversion)
`SWS_BACKEND_LEGACY` (= `SWS_BACKEND_STABLE`, the original per-format C/asm code path), `SWS_BACKEND_C` (new template-based generic C reference implementation for the ops graph), `SWS_BACKEND_MEMCPY` (fast-path when no conversion is actually needed), `SWS_BACKEND_X86`, `SWS_BACKEND_AARCH64` (chained SIMD kernels for the ops graph), `SWS_BACKEND_SPIRV` (Vulkan compute backend); `SWS_BACKEND_UNSTABLE` = C|MEMCPY|X86|AARCH64|SPIRV; `SWS_BACKEND_ALL` = STABLE|UNSTABLE.

### Every `AVOption` exposed by `SwsContext` (`options.c`)
| Name | Type | Meaning |
|---|---|---|
| `sws_flags` (unit `sws_flags`) | flags | legacy scaler-algorithm bitmask, named consts = every `SWS_*` flag above |
| `scaler` / `scaler_sub` (unit `sws_scaler`) | int | new-style luma / chroma-subsampling scaler algorithm selection |
| `param0` / `param1` | double | scaler-specific tuning parameters (e.g. Lanczos taps, cubic B/C) |
| `srcw` / `srch` / `dstw` / `dsth` | int | source/destination dimensions |
| `src_format` / `dst_format` | pixel_fmt | source/destination `AVPixelFormat` |
| `src_range` / `dst_range` | bool | full-range flag for source/destination |
| `gamma` | bool | enable gamma-correct scaling |
| `src_v_chr_pos` / `src_h_chr_pos` / `dst_v_chr_pos` / `dst_h_chr_pos` | int | explicit chroma sample position (1/256 luma-grid units) |
| `sws_dither` (unit `sws_dither`) | int | selects `SwsDither` |
| `alphablend` (unit `alphablend`) | int | selects `SwsAlphaBlend` |
| `threads` (unit `threads`) | int | worker thread count (`auto` = 0) |
| `intent` (unit `intent`) | int | selects `SwsIntent` |
| `sws_backends` (unit `sws_backend`) | flags | restricts allowed `SwsBackend` set |

### Format conversion & colorspace scope
The legacy core (`swscale_unscaled.c`, `rgb2rgb.c`, `yuv2rgb.c`, `input.c`/`output.c`) implements pairwise conversion + scaling across essentially the full `AVPixelFormat` set from §2 (planar/packed/semi-planar YUV of all subsamplings and bit depths, all RGB packings, Bayer, gray, palette), including primaries/transfer/matrix/range-aware colorspace conversion (`sws_setColorspaceDetails`/`sws_getColorspaceDetails`, `sws_getCoefficients`), gamma-correct scaling, and alpha compositing. `sws_isSupportedInput`/`sws_isSupportedOutput`/`sws_isSupportedEndiannessConversion` let callers query per-format support at runtime; the newer `sws_test_format`/`sws_test_colorspace`/`sws_test_primaries`/`sws_test_transfer`/`sws_test_frame` extend that capability query to the ops/graph backend's coverage (which is being built out incrementally and may support a narrower matrix than the legacy path for some formats).

### Slice-based API (legacy)
`sws_getContext`/`sws_getCachedContext` build a stateful `SwsContext`; `sws_scale` processes a horizontal slice given per-plane source pointers/linesizes and a starting row; `sws_init_context` (with optional `SwsFilter` src/dst vertical/horizontal filter overrides via `SwsVector`/`sws_getGaussianVec`-style filter construction, `sws_scaleVec`, `sws_normalizeVec`) initializes without allocating. `sws_convertPalette8ToPacked32/24` handle direct PAL8 conversion.

### Newer frame-based / slice-threaded API
`sws_alloc_context` + `sws_frame_setup`/`sws_scale_frame` (single-call whole-`AVFrame` conversion, format/size taken from the frames themselves) and an explicit slice-streaming variant `sws_frame_start`/`sws_send_slice`/`sws_receive_slice`/`sws_frame_end` for incremental/threaded production and consumption of output rows. `sws_is_noop` detects when no conversion is needed.

### Graph / ops architecture (new backend, files: `graph.h/.c`, `ops.h/.c`, `ops_chain.*`, `ops_dispatch.*`, `ops_optimizer.c`, `uops.*`)
Internal (non-`sws_`-prefixed, `ff_`-prefixed) architecture that represents a conversion as a directed graph of composable primitive operations (`SwsOpType`) rather than one monolithic per-format-pair function:
`SWS_OP_READ, SWS_OP_WRITE` (gather/scatter raw pixels from/to planes), `SWS_OP_SWAP_BYTES` (endianness), `SWS_OP_SWIZZLE` (channel reorder/duplicate), `SWS_OP_UNPACK, SWS_OP_PACK` (bit-packed component split/merge), `SWS_OP_LSHIFT, SWS_OP_RSHIFT` (bit shifts), `SWS_OP_CLEAR` (zero/const-fill channels), `SWS_OP_CONVERT` (type cast), `SWS_OP_MIN, SWS_OP_MAX` (clamping), `SWS_OP_SCALE` (scalar multiply), `SWS_OP_LINEAR` (generalized affine transform — used for colorspace matrices), `SWS_OP_DITHER`, `SWS_OP_FILTER_H, SWS_OP_FILTER_V` (horizontal/vertical resampling filter application), `SWS_OP_LUT_3D` (3D LUT application, `lut3d.h`), terminated by `SWS_OP_TYPE_NB`. A `SwsGraph` (`graph.h`) chains passes (`SwsPass`) built from these ops; `ops_optimizer.c` fuses/simplifies chains; `ops_dispatch.c` picks per-op backend kernels (C template, x86, AArch64, SPIR-V) at graph-build time — this is the mechanism behind the `SWS_BACKEND_*` selection above. `cms.c`/`cms.h` layer ICC-style color management (gamut mapping per `SwsIntent`, tone mapping) on top of this graph for the intent-driven conversions.

### SIMD / architecture coverage
Directories with hand-written kernels: **x86** (`hscale_fast_bilinear_simd.c`, `input.asm`, `output.asm`, `scale.asm`/`scale_avx2.asm`, `rgb2rgb.c`/`rgb_2_rgb.asm`, `yuv2rgb.c`/`yuv_2_rgb.asm`, `yuv2yuvX.asm`, `range_convert.asm`, plus ops-graph kernels `ops_common.asm`/`ops_float.asm`/`ops_int.asm`); **AArch64** (NEON `.S` for hscale/output/input/rgb2rgb/xyz2rgb/yuv2rgb plus a full ops-graph NEON kernel set: `ops.c`, `ops_asmgen.*`, `ops_entries.c`, `ops_impl*.c`, `rasm.*`); **ARM32** (hscale/output NEON, rgb2yuv NEON, yuv2rgb NEON — legacy path only, no ops-graph kernels yet); **PPC** (Altivec/VSX `swscale_altivec.c`, `swscale_vsx.c`, `yuv2rgb_altivec.c`, `yuv2yuv_altivec.c`); **RISC-V** (`input_rvv.S`, `range_rvv.S`, `rgb2rgb_rvv.S`/`rgb2rgb_rvb.S`); **LoongArch** (LSX/LASX `input_l{s,a}x.c`, `output_l{s,a}x.c`, `rgb2rgb_lasx.c`, `swscale_l{s,a}x.c`, `yuv2rgb_l{s,a}x.c`); **Vulkan** (`vulkan/vulkan.c` + SPIR-V assembly `spvasm.h`, backend-selected via `SWS_BACKEND_SPIRV`, operates as a compute-shader realization of the same ops graph rather than CPU SIMD).

### CPU dispatch mechanism (conceptual, shared pattern with libavutil/libswresample)
`av_get_cpu_flags()` (libavutil `cpu.h`) is queried once during context/graph initialization; the legacy path's `*_init.c`-equivalent logic (in `utils.c`/`swscale.c`) rewrites C function pointers with the best available SIMD implementation, while the ops-graph path's `ops_dispatch.c` additionally chooses among architecture-specific op-kernel tables (declared per-arch, e.g. AArch64's `ops_entries.c`) versus the generic C template (`uops_tmpl.c`/`op_list_gen_template.c`) versus SPIR-V. `AV_CPU_FLAG_*` constants cover x86 (MMX…AVX-512ICL), PPC (Altivec/VSX/POWER8), ARM/AArch64 (VFP/NEON/dotprod/i8mm/SVE/SVE2/SME/SME2/CRC/PMULL/EOR3), MIPS (MMI/MSA), LoongArch (LSX/LASX), and RISC-V (vector extension variants I32/F32/I64/F64, bit-manipulation, misaligned-access fast path).

---

## 12. Notes for a Rust Architect

- **Format tables are the highest-leverage porting target**: `AVPixelFormat` (268 variants) and its `AVPixFmtDescriptor` metadata, plus `AVSampleFormat`/`AVChannelLayout`, should become data-driven Rust enums/const tables generated once rather than hand-transcribed, to avoid silent metadata drift.
- **`AVOption`/`AVClass` is a generic reflection system** underpinning every configurable object in this scope (`SwrContext`, `SwsContext`) — a Rust port likely wants a derive-macro or builder-pattern equivalent rather than a literal reflection port; the option tables in §10/§11 are the authoritative contract to preserve (names, types, ranges, defaults, unit groupings) for CLI/API compatibility.
- **`libswresample`** cleanly separates three independently portable stages: format conversion (`audioconvert.c`), channel rematrixing (`rematrix.c`), and resampling (`resample.c` or an external soxr-equivalent) — a Rust design can mirror that seam.
- **`libswscale`** is mid-migration from a monolithic per-format-pair C/asm implementation (`swscale_unscaled.c` etc., "legacy backend") to a composable ops-graph (`ops.h`, `SwsOpType`, `SwsBackend`) — for a clean-room reimplementation, the ops-graph's primitive vocabulary (§11) is arguably a better target architecture to emulate than the legacy per-format kernels, since it already factors scaling into orthogonal, individually testable primitives (unpack/pack, swizzle, shift, linear transform, filter-H/V, dither, 3D LUT).
- **HDR/DoVi/film-grain/detection-bbox/IAMF side-data structs** (§6) are plain-data payloads with a common attach/iterate mechanism (`side_data.h`) — these are good candidates for a shared Rust "frame metadata" enum mirroring `AVFrameSideDataType`.
- **Crypto/hash/compression primitives** in libavutil (§8) are self-contained and have mature, well-audited Rust crate equivalents (`aes`, `sha2`, `md-5`, `crc`, `base64`, `minilzo`-style crates) — likely not worth hand-porting; the porting effort should focus on the media-specific surface (formats, options, side data, tx/resample/scale engines).