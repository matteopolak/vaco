# 18 — Containers, Protocols and I/O (`vaco-format-core`, `vaco-io`, `vaco-protocol-*`, `vaco-demux-*`, `vaco-mux-*`)

> **CORRECTED BY D15.** This document classifies a large part of the inventory as impossible to
> clean-room. **That was a legal error.** 17 U.S.C. §102(b) excludes procedures, processes and methods
> from copyright "regardless of the form in which it is described", and format-dictated tables are the
> paradigm case for the merger doctrine — a Huffman table a decoder *must* contain is a fact about the
> format, not authorial expression. Reverse-engineered formats are therefore **legally implementable**
> from a functional specification.
>
> What actually constrains them is **cost and demand**: a specification-extraction pass (0.5–3 pw per
> format) must precede implementation, and most of these formats have very few users. Re-read every
> "cannot be done cleanly" below as **"requires spec extraction first, prioritise on demand"**.
>
> The genuinely blocked set is small: trained model weights (`nnedi`) and hand-tuned perceptual tables
> that the format does not dictate. See D15 for the full analysis and the process.


Constraints: `planning/00-decisions.md` (D1–D12). Layering: `planning/10-architecture.md` §3.
Primary source: `planning/research/03-libavformat.md`. Dependency candidates:
`planning/research/09-dependency-licence-register.md`. Interfaces consumed: `planning/14-cli.md` §5
(`vaco-probe`), §6.4 (the CLI timestamp pipeline), and `planning/13-correctness.md` §1 (the
differential harness). Work-breakdown format matches `planning/15-codecs.md` §7 and
`planning/16-filters.md` §8 so the three merge into one roadmap.

**Clean-room (D7).** Nothing in this plan was written from FFmpeg source. Every structural claim is
attributed to a public specification, cited inline. Where research 03 marks a format
"reverse-engineered", this plan treats it exactly as `15-codecs.md` §3.5 treats Tier 5: we do not
implement it spec-first, because there is no spec, and we say so rather than pretending otherwise.
Where FFmpeg's *observable behaviour* is underspecified by the standards, the plan says so, marks
the item **VERIFY**, and names the black-box experiment that settles it. Those experiments query the
reference binary as an oracle (D6, correctness §1.6) — never its source.

---

## 0. Position summary

1. **This subsystem is the first milestone.** D5 makes v0.1 "ffprobe on MP4, Matroska and MPEG-TS,
   byte-identical". Every field `vaco-probe` prints comes from here. Plan 14 builds the writer;
   we build everything it writes about. If this plan is wrong, v0.1 does not ship.

2. **Containers are where "byte-identical" is genuinely reachable.** Muxing is a pure function of
   (packet sequence, muxer options, bitexact flag). There is no floating point in the decision path,
   no rate control, no psychovisual model. Correctness §1.2's C0 mode already names deterministic
   remuxes as exact-byte cases. That makes this the subsystem where D6's hardest requirement is both
   most achievable and most testable — and it means a divergence here is always a bug, never a
   tolerance.

3. **The build/buy answer inverts relative to codecs.** Plan 15 concluded "wrap where a good crate
   exists". For containers the conclusion is **build**, for a specific reason: a codec crate that is
   bit-exact against the spec is automatically bit-exact against FFmpeg, because the spec pins the
   output. A container crate has no such anchor — the spec permits a large space of valid files and
   FFmpeg picks one point in it. An external demuxer will parse the file correctly and still report
   `start_time` differently. Section 5 works this through crate by crate; the short version is that
   only the *peripheral* crates (`quick-xml`, RustCrypto, `flate2`, `ureq`, `rustls`) survive, and
   every actual demuxer and muxer is ours.

4. **Just over half of FFmpeg's demuxer list cannot be clean-roomed.** 192 of 368 demuxers (52%) are
   reverse-engineered game, FMV and legacy formats with no public specification. Section 4 gives the
   full tiering. This mirrors plan 15's finding for codecs almost exactly, and for the same reason —
   the two lists describe the same 25 years of accumulated scratch-itching.

5. **The timestamp model is the single highest-risk area in the whole project after `vaco-sched`.**
   §1.7 gets more space than anything else in this document, and every rule in it is numbered so
   that plan 14 §6.4 can compose with it by reference.

6. **Two decisions need escalation before protocol work starts.** (a) D10 Gate 1 excludes *both* of
   `rustls`'s production crypto providers, because `ring` and `aws-lc-rs` each compile C and assembly
   in their build scripts — §2.6.3. (b) SRT: libsrt is MPL and excluded, but the protocol has a
   public IETF specification, so native is possible — §2.7 recommends native, T3, and explains why
   "absent" is the wrong answer.

---

# 1. `vaco-format-core` — the framework

Layer 3. `#![forbid(unsafe_code)]`. Knows nothing about any specific container (architecture §1.5).
Depends on `vaco-core`, `vaco-packet`, `vaco-opts`, `vaco-io`, and — see §1.0 — `vaco-codec-core`.

## 1.0 A layering correction that has to come first

Architecture §3 places `vaco-format-core` in layer 3 and `vaco-codec-core` in layer 4. That is
unsatisfiable: `Stream` holds `CodecParameters`, which plan 15 §1.1 defines in `vaco-codec-core`.
FFmpeg has the same edge (libavformat → libavcodec) for the same reason.

**Proposed amendment to architecture §3.** Split the codec layer in two:

| Crate | Layer | Contents |
|---|---|---|
| `vaco-codec-core` | **3a** | `CodecId`, `CodecTag`, `CodecParameters`, `Profile`/`Level`, `Caps`, the `Decoder`/`Encoder`/`Parser`/`BitstreamFilter` **traits and descriptors**. Data model and seams only — zero codec implementations. |
| `vaco-format-core` | **3b** | Depends on 3a. |
| `vaco-codec-<name>` | **4** | Implementations. Format crates never depend on these. |

The layer-check script (correctness §5.5, `layers.toml`) gets `3a`/`3b` as distinct layers and the
rule "no `vaco-format-*` crate may depend on any `vaco-codec-<name>` crate" as an explicit ban. This
is a one-line change to `layers.toml` and it makes the real constraint checkable, which the current
numbering does not.

**The parser problem, and how the seam stays clean.** Demuxers need parsers for two jobs: splitting
a byte stream into frames (`AVSTREAM_PARSE_*`), and refining `CodecParameters` from in-band headers
during stream discovery. Both would drag layer 4 into layer 3. Instead `vaco-format-core` declares
what it needs and someone above supplies it:

```rust
/// Supplied by `vaco-registry` (layer 6). `vaco-format-core` never links a parser.
pub trait ParserProvider: Send + Sync {
    /// A frame splitter for this codec, or `None` if we have no parser for it.
    fn parser(&self, codec: CodecId) -> Option<Box<dyn StreamParser>>;
    /// Refine parameters from a coded payload without decoding. Used by stream discovery to
    /// fill profile/level/pix_fmt/sample_rate from SPS/OBU/ASC/OpusHead. Returns `true` if
    /// anything changed.
    fn refine(&self, par: &mut CodecParameters, payload: &[u8], flavour: ExtraFlavour) -> bool;
    /// Content-sniff a payload to a codec id, for containers that identify streams only by
    /// payload (raw ES, `data` streams in TS, `mpegtsraw`).
    fn probe_codec(&self, media: MediaType, payload: &[u8]) -> Option<(CodecId, u8 /*score 0..=100*/)>;
}

pub struct NoParsers;      // the default; stream discovery degrades gracefully
impl ParserProvider for NoParsers { /* … */ }
```

`DemuxContext::open` takes `Arc<dyn ParserProvider>`. In `vaco-probe` it is the real registry; in a
`vaco-demux-mp4` unit test it is `NoParsers`; in a fuzz target it is `NoParsers`, which is also what
keeps demuxer fuzzing fast and independent of codec code.

## 1.1 Object model

```rust
// ---- identifiers -------------------------------------------------------------------------
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct StreamIndex(pub u32);
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ProgramIndex(pub u32);
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct StreamGroupIndex(pub u32);

// ---- timestamps --------------------------------------------------------------------------
/// A timestamp in some stream's time base. Deliberately *not* `i64`, so that a rescale that
/// forgets its time base cannot compile. `None` at the option level is FFmpeg's AV_NOPTS_VALUE.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Ts(pub i64);

/// Rational time base, always stored reduced with a positive denominator (`vaco-core`).
pub type TimeBase = Rational;

/// The fixed rescale target for every API that speaks in absolute time. 1/1_000_000.
pub const TIME_BASE_Q: TimeBase = Rational::new_raw(1, 1_000_000);

// ---- streams -----------------------------------------------------------------------------
#[derive(Clone, Debug)]
pub struct Stream {
    pub index: StreamIndex,
    /// Container-native identifier: MPEG-TS elementary PID, MP4 track_ID, Matroska TrackNumber,
    /// ASF stream number. Printed by `vaco-probe` as `id` when the format sets `SHOW_IDS`.
    pub id: i64,
    pub params: CodecParameters,
    pub time_base: TimeBase,
    pub start_time: Option<Ts>,
    pub duration: Option<Ts>,
    pub nb_frames: Option<u64>,
    pub disposition: Disposition,
    pub discard: Discard,
    /// Container-declared display aspect override. Distinct from `params.sample_aspect_ratio`,
    /// which is the codec's own signalling; `vaco-probe` prints the container value when both exist.
    pub sample_aspect_ratio: Option<Rational>,
    pub avg_frame_rate: Option<Rational>,
    pub r_frame_rate: Option<Rational>,
    /// Width of the container's native timestamp field. 33 for MPEG-2 PES/PCR, 32 for
    /// several RIFF-derived formats, 64 (= no wrapping possible) otherwise.
    pub pts_wrap_bits: u32,
    pub metadata: Metadata,
    /// Present iff `disposition.attached_pic`: the single coded picture (cover art, MP4 `covr`,
    /// ID3 `APIC`, Matroska attachment promoted to a stream).
    pub attached_pic: Option<Packet>,
    pub side_data: StreamSideData,
    pub event_flags: StreamEventFlags,
}

bitflags! {
    /// Container-declared role. Names are interface facts (D9) and are reproduced exactly,
    /// because `vaco-probe`'s DISPOSITION section prints one field per flag.
    pub struct Disposition: u32 {
        const DEFAULT = 1 << 0;          const DUB = 1 << 1;
        const ORIGINAL = 1 << 2;         const COMMENT = 1 << 3;
        const LYRICS = 1 << 4;           const KARAOKE = 1 << 5;
        const FORCED = 1 << 6;           const HEARING_IMPAIRED = 1 << 7;
        const VISUAL_IMPAIRED = 1 << 8;  const CLEAN_EFFECTS = 1 << 9;
        const ATTACHED_PIC = 1 << 10;    const TIMED_THUMBNAILS = 1 << 11;
        const NON_DIEGETIC = 1 << 12;    const CAPTIONS = 1 << 16;
        const DESCRIPTIONS = 1 << 17;    const METADATA = 1 << 18;
        const DEPENDENT = 1 << 19;       const STILL_IMAGE = 1 << 20;
        const MULTILAYER = 1 << 21;
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Discard { None, Default, #[default] NonRef, Bidir, NonIntra, NonKey, All }

/// Stream-scoped side data that is a property of the *stream*, not of any packet:
/// display matrix, stereo3d, spherical/projection, mastering-display and content-light
/// metadata, ICC profile, ambient viewing environment, CPB properties, ReplayGain,
/// audio service type, IAMF descriptors, Dolby Vision configuration.
#[derive(Clone, Debug, Default)]
pub struct StreamSideData(Vec<SideDataEntry>);
```

`Program`, `Chapter`, `StreamGroup`:

```rust
#[derive(Clone, Debug)]
pub struct Program {
    pub index: ProgramIndex,
    pub id: i64,                       // MPEG-TS program_number
    pub streams: Vec<StreamIndex>,     // container order, never sorted
    pub discard: Discard,
    pub metadata: Metadata,
    // MPEG-TS specifics; `vaco-probe` prints all four in the PROGRAM section.
    pub program_num: Option<u32>,
    pub pmt_pid: Option<u16>,
    pub pcr_pid: Option<u16>,
    pub pmt_version: Option<u8>,
    /// Not public in FFmpeg but load-bearing: wraparound state is per-program because a
    /// multiplex shares one 33-bit clock across its streams (§1.7 R7).
    pub(crate) wrap: WrapState,
    pub(crate) start_time: Option<Ts>,   // in TIME_BASE_Q
    pub(crate) end_time: Option<Ts>,
}

#[derive(Clone, Debug)]
pub struct Chapter {
    pub id: i64,
    pub time_base: TimeBase,
    pub start: Ts,
    pub end: Ts,
    pub metadata: Metadata,
}

/// FFmpeg 7.1+ `AVStreamGroup`. `vaco-probe -show_stream_groups` prints these, so v0.1 needs
/// at least the TILE_GRID variant (AVIF/HEIC land in the MOV demuxer's extension list).
#[derive(Clone, Debug)]
pub struct StreamGroup {
    pub index: StreamGroupIndex,
    pub id: i64,
    pub streams: Vec<StreamIndex>,
    pub disposition: Disposition,
    pub metadata: Metadata,
    pub kind: StreamGroupKind,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum StreamGroupKind {
    /// AOM Immersive Audio Model and Formats, `iamf` spec.
    IamfAudioElement(Box<IamfAudioElement>),
    IamfMixPresentation(Box<IamfMixPresentation>),
    /// ISO/IEC 23008-12 `grid` derived image item: a tiled still image.
    TileGrid(TileGrid),
    /// ISO/IEC 23094-2 LCEVC enhancement paired with a base stream.
    LcevcVideo(LcevcVideo),
}

#[derive(Clone, Debug)]
pub struct TileGrid {
    pub tile_rows: u32, pub tile_columns: u32,
    pub output_width: u32, pub output_height: u32,
    pub horizontal_offset: i32, pub vertical_offset: i32,
    pub background_color: [u8; 4],
    /// One entry per member stream, in `streams` order.
    pub offsets: Vec<(i32, i32)>,
}
```

**Metadata** is an insertion-ordered, case-insensitive-lookup, duplicate-preserving multimap. Not a
`HashMap` — iteration order is output order and D6 requires it to be deterministic:

```rust
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metadata { entries: Vec<(MetaKey, String)> }   // MetaKey compares ASCII-case-insensitively

impl Metadata {
    pub fn get(&self, key: &str) -> Option<&str>;              // first match, case-insensitive
    pub fn get_all<'a>(&'a self, key: &str) -> impl Iterator<Item = &'a str>;
    pub fn set(&mut self, key: &str, value: impl Into<String>); // replaces first, keeps position
    pub fn append(&mut self, key: &str, value: impl Into<String>);
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)>;   // insertion order — this is output order
}
```

## 1.2 The `Demuxer` trait

```rust
pub trait Demuxer: Send {
    /// Called once after `open`. The context is fully constructed; `read_header` populates
    /// streams, programs, chapters, groups and container metadata.
    fn read_header(&mut self, ctx: &mut DemuxCtx<'_>) -> Result<()>;

    /// Produce the next packet. `Ok(None)` is clean EOF. A recoverable parse error should be
    /// reported by emitting a packet flagged `CORRUPT` and continuing, or by returning
    /// `Err(Error::InvalidData)` — never by panicking (D6: every demuxer is fuzzed from day one).
    fn read_packet(&mut self, ctx: &mut DemuxCtx<'_>) -> Result<Option<Packet>>;

    /// Format-native seek. Return `Err(Error::NotSupported)` to fall through to the generic
    /// paths in §1.8. Implementing this is what `AVFMT_*SEARCH` opt-outs express upstream.
    fn seek(&mut self, ctx: &mut DemuxCtx<'_>, req: &SeekRequest) -> Result<()> {
        let _ = (ctx, req); Err(Error::NotSupported)
    }

    /// Probe a timestamp near byte position `*pos`, used by the binary-search seek path.
    /// Scans forward from `*pos` for a sync point at or before `pos_limit`, sets `*pos` to the
    /// position actually found, and returns its DTS in `stream`'s time base.
    fn read_timestamp(
        &mut self, ctx: &mut DemuxCtx<'_>, stream: StreamIndex,
        pos: &mut u64, pos_limit: u64,
    ) -> Result<Option<Ts>> {
        let _ = (ctx, stream, pos, pos_limit); Ok(None)
    }

    /// Discard buffered state after an external seek or a discontinuity.
    fn flush(&mut self, ctx: &mut DemuxCtx<'_>) { let _ = ctx; }

    /// Live sources only (RTSP): pause/resume without tearing down the session.
    fn play(&mut self, _ctx: &mut DemuxCtx<'_>) -> Result<()> { Err(Error::NotSupported) }
    fn pause(&mut self, _ctx: &mut DemuxCtx<'_>) -> Result<()> { Err(Error::NotSupported) }
}
```

`DemuxCtx` is the mutable half of the context, handed to the demuxer per call. Splitting it from the
demuxer's own state is what lets both be `&mut` at once without interior mutability:

```rust
pub struct DemuxCtx<'a> {
    pub io: &'a mut IoContext,
    pub streams: &'a mut StreamSet,        // add_stream / get_mut, index-stable
    pub programs: &'a mut Vec<Program>,
    pub chapters: &'a mut Vec<Chapter>,
    pub groups: &'a mut Vec<StreamGroup>,
    pub metadata: &'a mut Metadata,
    pub opts: &'a FormatOptions,           // the 38 generic options, §1.11
    pub parsers: &'a dyn ParserProvider,
    pub log: &'a Logger,
    pub cancel: &'a CancelToken,
    /// Set by the demuxer to report container-level facts it knows and the core does not.
    pub reported: &'a mut ContainerFacts,  // duration, bit_rate, start_time, packet_size, …
}
```

The descriptor, held by the registry, constructed without instantiating anything (architecture §5):

```rust
pub struct DemuxerDesc {
    pub name: &'static str,                  // "mov,mp4,m4a,3gp,3g2,mj2" — CLI-stable, D9
    pub long_name: &'static str,
    pub extensions: &'static [&'static str],
    pub mime_types: &'static [&'static str],
    pub flags: FormatFlags,
    pub priority: i16,                       // tie-break key, §1.5 R6
    pub options: &'static OptionSchema,
    pub probe: Option<fn(&ProbeData<'_>) -> ProbeScore>,
    pub open: fn(&Options) -> Result<Box<dyn Demuxer>>,
}
```

## 1.3 The `Muxer` trait

```rust
pub trait Muxer: Send {
    /// Called after all streams are added and their parameters frozen. May rewrite
    /// `Stream::time_base` — the muxer, not the caller, decides what the container can express.
    fn init(&mut self, ctx: &mut MuxCtx<'_>) -> Result<()> { let _ = ctx; Ok(()) }
    fn write_header(&mut self, ctx: &mut MuxCtx<'_>) -> Result<()>;

    /// Write one packet, already interleaved and already timestamp-normalised by the core.
    /// `None` means "flush whatever you have buffered internally" and is only ever passed to
    /// muxers declaring `OutputFlags::ALLOW_FLUSH`.
    fn write_packet(&mut self, ctx: &mut MuxCtx<'_>, pkt: Option<&Packet>) -> Result<()>;

    fn write_trailer(&mut self, ctx: &mut MuxCtx<'_>) -> Result<()>;

    /// Override the interleaving policy. Default = per-DTS (§1.9). MOV overrides to emit whole
    /// chunks; the fragmenting muxers override to align on fragment boundaries.
    fn interleave(&mut self, q: &mut InterleaveQueue, pkt: Option<Packet>, flush: bool)
        -> Result<Option<Packet>>
    { interleave_per_dts(q, pkt, flush) }

    /// Bitstream-filter-in-muxer (§1.10). Called on the first packet of each stream, then cached.
    fn check_bitstream(&mut self, st: &Stream, pkt: &Packet) -> Result<BitstreamAction> {
        let _ = (st, pkt); Ok(BitstreamAction::Keep)
    }

    /// Segmenting muxers (`segment`, `hls`, `dash`, `smoothstreaming`) implement this to be told
    /// where the byte stream may be cut.
    fn query_codec(codec: CodecId, strict: Compliance) -> CodecSupport where Self: Sized;
}

pub struct MuxerDesc {
    pub name: &'static str,
    pub long_name: &'static str,
    pub extensions: &'static [&'static str],
    pub mime_type: Option<&'static str>,
    pub default_video: Option<CodecId>,
    pub default_audio: Option<CodecId>,
    pub default_subtitle: Option<CodecId>,
    pub flags: FormatFlags,
    pub internal: OutputFlags,   // ALLOW_FLUSH | MAX_ONE_OF_EACH | ONLY_DEFAULT_CODECS
    pub priority: i16,
    pub options: &'static OptionSchema,
    pub open: fn(&Options) -> Result<Box<dyn Muxer>>,
}
```

**`FormatFlags`** reproduces the `AVFMT_*` set (research §1.9). The names are interface facts and
several are user-visible through `vaco -formats`; the *values* are ours, since nothing external
depends on them:

```rust
bitflags! {
    pub struct FormatFlags: u32 {
        const NOFILE          = 1 << 0;   // no IoContext (devices)
        const NEEDNUMBER      = 1 << 1;   // filename needs %d (image sequences)
        const EXPERIMENTAL    = 1 << 2;
        const SHOW_IDS        = 1 << 3;   // print numeric stream ids
        const GLOBALHEADER    = 1 << 4;   // wants extradata out-of-band
        const NOTIMESTAMPS    = 1 << 5;
        const GENERIC_INDEX   = 1 << 6;   // core may build and use an index
        const TS_DISCONT      = 1 << 7;   // timestamps may jump legitimately
        const VARIABLE_FPS    = 1 << 8;
        const NODIMENSIONS    = 1 << 9;
        const NOSTREAMS       = 1 << 10;
        const NOBINSEARCH     = 1 << 11;
        const NOGENSEARCH     = 1 << 12;
        const NO_BYTE_SEEK    = 1 << 13;
        const TS_NONSTRICT    = 1 << 14;  // non-decreasing DTS, not strictly increasing
        const TS_NEGATIVE     = 1 << 15;  // negative timestamps are representable
        const FIXED_FRAMESIZE = 1 << 16;
        const SEEK_TO_PTS     = 1 << 17;
    }
}
```

## 1.4 Registry seam

Registration is explicit generated code (architecture §6 — no `inventory`, no link sections). The
generated module is committed and reviewable, and it is also where **tie-break priority** is
assigned (§1.5 R6), which makes probe ordering a reviewed artefact rather than an emergent property
of link order — a strict improvement over upstream, where ties fall to registration order.

```rust
// crates/registry/src/generated/formats.rs — generated from formats.toml, committed
pub static DEMUXERS: &[&DemuxerDesc] = &[
    #[cfg(feature = "format-mp4")]      &vaco_demux_mp4::DEMUXER,
    #[cfg(feature = "format-matroska")] &vaco_demux_matroska::DEMUXER,
    #[cfg(feature = "format-mpegts")]   &vaco_demux_mpegts::DEMUXER_TS,
    #[cfg(feature = "format-mpegts")]   &vaco_demux_mpegts::DEMUXER_TSRAW,
    // …
];
```

## 1.5 Probing — the scoring model

`vaco-probe` prints `probe_score` in its FORMAT section. **The scoring model is therefore directly
byte-verified by D5's acceptance matrix**, which is unusually lucky: an area that would normally be
untestable folklore is a hard equality assertion from week one.

### 1.5.1 Score space and inputs

```rust
pub struct ProbeScore(pub u8);   // 0..=100

impl ProbeScore {
    pub const NONE: Self       = ProbeScore(0);
    pub const RETRY: Self      = ProbeScore(25);   // MAX/4
    pub const STREAM_RETRY: Self = ProbeScore(24); // MAX/4 - 1
    pub const EXTENSION: Self  = ProbeScore(50);
    pub const MIME_BONUS: u8   = 30;
    pub const MAX: Self        = ProbeScore(100);
}

pub struct ProbeData<'a> {
    filename: &'a str,
    mime_type: Option<&'a str>,
    buf: &'a [u8],
}
```

**Zero-padding is reproduced, deliberately.** Upstream appends 32 zero bytes past the probe buffer so
that probe functions can read a fixed-size header without bounds checks. We do not need that for
safety, but we need it for *fidelity*: on a 6-byte file, a probe that reads a 16-byte header sees ten
zeros upstream and would see an error for us — a different score, a different chosen format, a
different `probe_score` line. So `ProbeData` exposes a cursor that yields zeros for
`buf.len() .. buf.len() + 32` and errors beyond:

```rust
impl<'a> ProbeData<'a> {
    pub const PADDING: usize = 32;
    pub fn len(&self) -> usize { self.buf.len() }
    /// Byte at `i`, or 0 for `len() <= i < len()+PADDING`, or `None` past that.
    pub fn get(&self, i: usize) -> Option<u8>;
    pub fn rb32(&self, i: usize) -> Option<u32>;   // and rl16/rb16/rl32/rb64/rl64
    pub fn tag(&self, i: usize) -> Option<[u8; 4]>;
    pub fn starts_with(&self, magic: &[u8]) -> bool;
    pub fn filename(&self) -> &str;
    pub fn extension(&self) -> Option<&str>;       // lowercased, no dot
    pub fn mime_type(&self) -> Option<&str>;
}
```

### 1.5.2 The rules

Numbered so per-format plans and tests can cite them.

- **R1 — content score.** For each candidate demuxer with a `probe` function, `s = probe(data)`.
- **R2 — extension fallback.** A demuxer with no `probe` function but a matching extension scores
  `EXTENSION` (50). A demuxer *with* a probe function does **not** get an extension bonus; the probe
  function is expected to consult `data.extension()` itself if the format needs it (several do —
  `image2`, the PCM family, `mp3` vs `mp2`).
- **R3 — MIME bonus.** If `data.mime_type()` matches one of the descriptor's `mime_types`, the score
  becomes `max(s, EXTENSION)` then `min(100, that + MIME_BONUS)`. The mime type only ever arrives from
  a protocol that supplies one (HTTP `Content-Type`), never from a local file. **VERIFY-P1**: whether
  the bonus applies to a zero-scoring demuxer. Test: serve a WebM file over HTTP with
  `Content-Type: video/webm` and a `.bin` extension, mangling the EBML magic so the content probe
  returns 0; read `probe_score`.
- **R4 — winner.** The candidate with the highest score wins.
- **R5 — floor.** Score 0 is never a winner. If every candidate scores 0, the open fails with
  `Error::UnknownFormat` and `vaco-probe` exits 1.
- **R6 — ties.** Order by `(score desc, priority asc, name asc)`. `priority` is an explicit `i16` in
  `formats.toml`, defaulting to 0. This replaces upstream's registration-order tie-break with a
  reviewed one. **Calibration is a task, not an assumption**: `just calibrate-probe` builds a corpus
  of deliberately ambiguous files (a `.ts` that is also valid `mpegtsraw`; a WAV with an ID3 prefix;
  an MP4 whose `ftyp` brand is `3gp4`; an EBML file with `DocType=webm` versus `matroska`; a raw
  H.264 stream that also sniffs as `mpegvideo`), runs the reference binary on each, and emits the
  priority assignments that reproduce its choices. The generated table is committed with the corpus.
- **R7 — retry with a larger buffer.** If the winning score is `<= RETRY` (25) and more input is
  available, double the probe buffer and repeat from R1. Start at `PROBE_BUF_MIN`, double to at most
  `min(PROBE_BUF_MAX, opts.format_probesize)`. **VERIFY-P2**: the two constants. Test: generate files
  whose only identifying magic sits at offset N, bisect N for the smallest N at which detection
  fails; that gives `PROBE_BUF_MAX`. Repeat with a format whose probe returns exactly `RETRY` at
  small buffer sizes to find `PROBE_BUF_MIN`.
- **R8 — forced format.** `-f <name>` (or a format-specific URL prefix) **bypasses R1–R7 entirely**:
  the named demuxer is instantiated without its probe being called, and `probe_score` is reported as
  `MAX` (100). **VERIFY-P3**: `ffprobe -f matroska x.mkv` and read `probe_score`. If it prints 100
  the model is right; if it prints the probe's own score, R8 becomes "probe is still run, for the
  score only".
- **R9 — whitelist.** `format_whitelist` filters the candidate set *before* R1. A whitelisted-out
  format never runs its probe. This matters for security (a hostile playlist cannot pivot into a
  weird demuxer) and it is cheap.
- **R10 — the `skip_initial_bytes` interaction.** `skip_initial_bytes` is applied by the I/O layer
  before probing, so probe offsets are relative to the skipped stream. Likewise `FF_INFMT_FLAG_ID3V2_AUTO`
  equivalents: a leading ID3v2 tag is *stripped by the core* before probing for demuxers that opt in
  via `InputFlags::ID3V2_AUTO`, and the parsed tag is stashed for the demuxer to merge into metadata.
- **R11 — determinism.** Probing must depend only on `(bytes, filename, mime_type, options)`. No
  file metadata, no clock, no locale. `filename` is used only for extension matching and for the
  `%d` test in `NEEDNUMBER` formats.

### 1.5.3 Probe strength conventions

To keep 368 independently written probe functions from drifting, `vaco-format-core` publishes a
convention table and a lint (a test that every registered probe returns a score drawn from it):

| Evidence | Score |
|---|---|
| Unambiguous magic at a fixed offset, plus a self-consistency check (length field, CRC, version) | 100 |
| Unambiguous magic at a fixed offset, no further check | 90 |
| Magic at a variable offset, found and consistent | 75 |
| Repeating structural evidence (N consecutive valid frames/packets) | `min(100, 25 + 8·N)` |
| Extension match only | 50 |
| Weak heuristic (plausible header, no magic) | 5–25 (retry band) |
| No evidence | 0 |

## 1.6 Stream discovery — the `find_stream_info` equivalent

```rust
pub struct DiscoveryLimits {
    pub probesize: u64,              // bytes read from the start of the file
    pub max_analyze_duration: Option<i64>,  // µs; None -> the per-format default (§1.6.4)
    pub fps_probe_size: Option<u32>, // frames used to establish the frame rate
    pub max_ts_probe: u32,           // packets read waiting for a first timestamp
    pub max_probe_packets: u32,      // packets fed to a codec parser per stream
    pub duration_probesize: u64,     // bytes read from the tail for FromPts duration
    pub skip_estimate_duration_from_pts: bool,
}

pub fn find_stream_info(ctx: &mut DemuxContext) -> Result<()>;
```

### 1.6.1 The loop, and why it terminates deterministically

Read packets into a **parse queue** (packets are not discarded — they are replayed by `read_packet`
afterwards, so discovery is transparent to the caller). After each packet, update per-stream state,
then evaluate termination:

```
loop {
    if every stream is Complete                            -> stop (reason: Complete)
    if bytes_read_since_open >= probesize                  -> stop (reason: ProbeSize)
    if analyzed_duration()  >= max_analyze_duration        -> stop (reason: AnalyzeDuration)
    if packets_read         >= max_probe_packets * streams -> stop (reason: PacketCap)
    if streams.is_empty() && packets_read >= max_ts_probe  -> stop (reason: NoStreams)
    match read_packet() { Some(p) => absorb(p), None => stop (reason: Eof) }
}
```

Four determinism rules, all of which exist because D6 requires bit-identical output across runs,
machines and thread counts:

- **DD1 — no wall clock.** `analyzed_duration()` is media time, computed from packet timestamps.
  Nothing in this loop may read a clock. (`use_wallclock_as_timestamps` is the one exception, and it
  is off by default and explicitly excluded from the conformance corpus.)
- **DD2 — no unordered iteration.** Every per-stream map is a `Vec` indexed by `StreamIndex`, never a
  `HashMap`. Every metadata map is insertion-ordered (§1.1).
- **DD3 — no float accumulation.** Duration and rate estimation use `i64`/`i128` and `Rational`
  throughout. Where a container stores a float (Matroska `Duration`, Ogg granulepos derivations),
  the float is converted to a rational once, at a defined precision, and never accumulated.
- **DD4 — no threading.** Discovery is single-threaded by construction. Parallel discovery would be
  a legitimate optimisation and it is forbidden, because packet-arrival order feeds the frame-rate
  estimator.

`stop_reason` is retained on the context and exposed by `-loglevel debug`; it is the single most
useful diagnostic when a field comes out wrong.

### 1.6.2 What gets inferred

| Field | Source, in priority order |
|---|---|
| `params.codec` | container's codec tag → `vaco-format-riff`/`-isom` table; else `ParserProvider::probe_codec` on the first payload; else `CodecId::None` |
| `params.extra` | container extradata; else synthesised from the first packets (ADTS→AudioSpecificConfig, Annex-B SPS/PPS→`avcC`) |
| `profile`/`level` | `ParserProvider::refine` on extradata, then on the first keyframe payload |
| video `width`/`height`/`pix_fmt`/`color`/`field_order`/`delay` | container; refined from the sequence header |
| audio `sample_rate`/`ch_layout`/`sample_fmt`/`frame_size` | container; refined from the first frame header |
| `bits_per_raw_sample` | container; else codec-specific header |
| `sample_aspect_ratio` | container track box; codec signalling only if the container has none |
| `start_time` | §1.7 R11 |
| `duration` | §1.7 R14–R18 |
| `nb_frames` | container count (MP4 sample count, Matroska cue count is *not* usable); else unset |
| `avg_frame_rate` | §1.6.3 |
| `r_frame_rate` | §1.6.3 |
| `bit_rate` (stream) | container field; else `stream_bytes·8·time_base_den / duration_ticks` |
| `bit_rate` (container) | container field; else sum of stream bit rates; else `file_size·8/duration` |

### 1.6.3 Frame rates — the two fields, precisely

These are two different quantities that people constantly conflate, and `vaco-probe` prints both.

- **`avg_frame_rate`** — the average. Defined as `nb_frames / duration`, reduced. If either is
  unknown, and the stream has a container-declared constant rate (MP4 `stts` with a single entry,
  Matroska `DefaultDuration`, AVI `dwRate/dwScale`), use that. Otherwise unset ("0/0" in output).
- **`r_frame_rate`** — documented upstream as *"the lowest frame rate with which all timestamps can
  be represented accurately (i.e. the least common multiple of all frame rates in the stream)"*.
  That sentence is a public interface fact and it is implementable directly:

  1. Collect the multiset of consecutive-PTS deltas `d_i` observed over the first
     `fps_probe_size` frames (default: until the stream is otherwise complete, capped at 20 frames
     — **VERIFY-P4**, the default is `-1` meaning "internal default", whose value must be measured
     by feeding a stream whose rate changes after frame N and bisecting N).
  2. Discard deltas that are `<= 0` or that exceed the median by more than a factor of 8 (dropped
     frames and pauses).
  3. Let `g = gcd` of the surviving deltas, in stream time base ticks. Candidate rate
     `= time_base.den / (g · time_base.num)`.
  4. Snap to a standard rate if the candidate is within 1/1001 relative error of one of
     {24, 24000/1001, 25, 30, 30000/1001, 50, 60, 60000/1001, 120, 120000/1001, 15, 12, 10, 5}, and
     the container time base is compatible with the snapped value. This snapping step is why a
     29.97 fps MP4 with a 1/30000 time base reports `30000/1001` and not `2997/100`.
  5. Cap the numerator at 1<<30 and the denominator at 1<<16; if the cap is hit, fall back to
     `avg_frame_rate`.
  6. **VERIFY-P5** — the snapping tolerance and the standard-rate list. Test: synthesise Matroska
     files (millisecond time base, so no rate is exactly representable) at 23.976, 29.97, 59.94 and
     119.88 fps and compare `r_frame_rate`. Matroska is the right vehicle precisely because its
     coarse time base forces the estimator to do real work.

For a stream with exactly one frame, `r_frame_rate` is unset and `avg_frame_rate` is unset. For an
attached picture, both are unset and `nb_frames` is 1. **VERIFY-P6**: upstream reports
`r_frame_rate=90000/1` for some single-frame MPEG-TS streams; confirm and record.

### 1.6.4 Per-format analyse-duration defaults

`analyzeduration` defaults to 0, meaning "the format's own default". The defaults differ, and the
difference is user-visible (an MPEG-TS with a late-appearing audio stream will or will not show that
stream). Our table, to be calibrated by **VERIFY-P7** (construct a TS where the second program's
audio first appears at t seconds; bisect t):

| Format class | Default |
|---|---|
| MPEG-TS, MPEG-PS, RTP/RTSP/SDP, HLS, DASH | 5 s |
| Everything else | 1 s |

`fps_probe_size`, `max_ts_probe`, `max_probe_packets` and `duration_probesize` are per-context
options, not per-format.

### 1.6.5 The `-show_frames` scoping problem — owed to plan 14

Plan 14 §5.1 puts `-show_frames` in the v0.1 Tier A option set, and §5.6's axis 2 runs
`-show_packets -show_frames` across the acceptance matrix. But `-show_frames` **decodes**: the
reference binary will emit FRAME elements for H.264/AAC in the MP4 corpus and we, under D5's
"parse only, no decode", will emit none. That is a guaranteed byte difference on a large slice of
the matrix.

Three resolutions, one of which must be chosen before the v0.1 corpus is frozen:

1. Drop `-show_frames` from the v0.1 axis, moving it to v0.2 with the decoders. Cleanest.
2. Restrict the v0.1 corpus's `-show_frames` cases to streams whose codec neither side decodes.
   Fragile — it constrains corpus authoring forever.
3. Ship a *frame-header-only* frame emitter that reports the fields derivable from parsing
   (`pkt_pts`, `pkt_dts`, `key_frame`, `pict_type`, `coded_picture_number`) and leave the
   decode-derived fields absent. This diverges from the reference by construction.

**Recommendation: (1).** It is the only one that keeps "byte-identical" unqualified. Raised here
because it is a formats/CLI boundary issue and neither plan owns it alone.

## 1.7 The timestamp model

The most error-prone area in the subsystem. Everything below is a numbered rule so that plan 14
§6.4's pipeline can compose with it by citation rather than by restatement. The boundary between
the two documents is stated once, precisely:

> **`vaco-format-core` owns: field decoding, wraparound, NOPTS normalisation, PTS generation from
> DTS, per-stream monotonic-DTS repair, packet duration fill-in, `start_time` derivation, duration
> estimation, and the muxer-side normalisation chain.**
> **Plan 14 §6.4 owns: `-itsoffset`, `-itsscale`, `-isync`, discontinuity *policy* against
> `dts_delta_threshold`/`dts_error_threshold`, `-ss`/`-t`/`-to` trimming, output-base
> normalisation, `-fps_mode`, and encoder time bases.**

The format layer never applies a user-specified offset. The CLI layer never touches wraparound.

### 1.7.1 Rescaling — the primitive everything rests on

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rounding { Zero, Inf, Down, Up, NearInf }

/// `a * b / c`, computed without intermediate overflow, with explicit rounding.
/// `NearInf` = round to nearest, ties away from zero. This is the default everywhere.
pub fn rescale_rnd(a: i64, b: i64, c: i64, rnd: Rounding) -> i64;

/// Rescale a timestamp between time bases. Returns `None` for `None` — NOPTS never becomes a number.
pub fn rescale_ts(ts: Option<Ts>, from: TimeBase, to: TimeBase) -> Option<Ts>;

/// The "pass min/max through unchanged" variant, needed by seek ranges where `i64::MIN`/`i64::MAX`
/// are sentinels for "unbounded" and must not be rescaled into garbage.
pub fn rescale_ts_passthrough(ts: i64, from: TimeBase, to: TimeBase) -> i64;
```

- **R1.** Rescaling reduces `(from/to)` to lowest terms first, then computes `a·num/den` in `i128`
  and narrows. The `i128` path is not a hot path — one multiply per packet per rescale — and it
  removes an entire class of bug for free.
- **R2.** `NearInf` is the default rounding for timestamps and durations. `Down` is used for
  seek-target lower bounds and `Up` for upper bounds, so that a rescaled seek range never shrinks.
- **R3.** Saturation, not wrapping, on `i64` overflow, with a `TimestampOverflow` diagnostic. A
  container that produces overflowing timestamps is corrupt and must not silently produce nonsense.

### 1.7.2 NOPTS and the `i64::MIN` alias

- **R4.** In our model an unknown timestamp is `None`, not a sentinel. This eliminates the class of
  bug where `AV_NOPTS_VALUE` is used in arithmetic.
- **R5.** **But the alias is reproduced at the container boundary.** A container field that decodes
  to exactly `i64::MIN` becomes `None`. This matters only for formats with a full 64-bit signed
  timestamp field (NUT, and Matroska if a pathological TimestampScale is used), but it is a
  one-line rule and it removes a whole family of "we disagree on one weird file" reports.
- **R6.** Packets carry `pts: Option<Ts>`, `dts: Option<Ts>`, `duration: Option<Ts>`. All three are
  independently optional. The invariant `dts <= pts` is **not** enforced at the format layer —
  several real files violate it, and upstream passes them through. It is checked and warned at
  `-loglevel warning`, and repaired only by the specific rules below.

### 1.7.3 Wraparound

Containers with an N-bit timestamp field wrap. MPEG-2 PES/PCR is 33 bits at 90 kHz — a period of
`2^33 / 90000 ≈ 26.5` hours, which real broadcast recordings routinely cross.

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WrapBehavior { #[default] Ignore, AddOffset, SubOffset }

#[derive(Clone, Copy, Debug, Default)]
pub struct WrapState {
    pub bits: u32,                 // Stream::pts_wrap_bits; 64 = no wrapping
    pub reference: Option<i64>,    // the pivot, in native units
    pub behavior: WrapBehavior,
}

impl WrapState {
    pub fn apply(&self, ts: i64) -> i64 {
        let (Some(r), true) = (self.reference, self.bits < 64) else { return ts };
        let period = 1i64 << self.bits;
        match self.behavior {
            WrapBehavior::Ignore    => ts,
            WrapBehavior::AddOffset => if ts < r { ts + period } else { ts },
            WrapBehavior::SubOffset => if ts >= r { ts - period } else { ts },
        }
    }
}
```

- **R7 — wrap state is per *program*, not per stream.** A multiplex shares one clock; correcting
  video and leaving audio uncorrected desynchronises them permanently. Streams not in any program
  share a synthetic program-0 wrap state.
- **R8 — establishing the reference.** During stream discovery, take the first observed timestamp
  `t0` of the program. If `t0 > 3·period/4`, set `reference = period/2` and
  `behavior = AddOffset` (we are near the top of the range; small values seen later are post-wrap).
  If `t0 < period/4`, set `reference = period/2` and `behavior = SubOffset` **only if** a timestamp
  `> 3·period/4` is subsequently observed within the discovery window (we started just after a wrap
  and are seeing stale pre-wrap values). Otherwise `Ignore`.
- **R9 — mid-stream wrap.** Independently of R8, and only when `correct_ts_overflow` is set
  (default on): if consecutive DTS on a stream differ by more than `period/2`, adjust by `±period`
  in the direction that minimises `|delta|`, and record a cumulative per-program wrap offset. The
  offset is cumulative, so a file crossing two wraps stays monotonic.
- **R10 — the interaction with seeking.** A seek invalidates the cumulative wrap offset, because
  the new position may be on the other side of a wrap. After a seek, the offset is recomputed from
  the seek target: `offset = round((target - first_observed_after_seek) / period) · period`. This
  is the rule that stops a seek into the second half of a 30-hour recording from producing
  timestamps 26.5 hours in the past. **VERIFY-T1**: construct a TS file that crosses a PTS wrap
  (write two segments with PCR/PTS bases straddling `2^33`), seek past the wrap, compare
  `-show_packets` output.

### 1.7.4 `start_time`

- **R11 — per-stream.** `Stream::start_time` is the smallest **PTS** (not DTS) observed among that
  stream's packets during discovery, in the stream's time base, after R7–R10. A demuxer may set it
  authoritatively during `read_header` (MP4 does, from the edit list; Matroska does not), in which
  case discovery does not overwrite it. Streams with `ATTACHED_PIC` are excluded from having one.
- **R12 — container-level.** `FormatContext::start_time` = the **minimum** over streams that have a
  `start_time`, rescaled to `TIME_BASE_Q`, excluding `ATTACHED_PIC` streams and excluding streams
  whose `params.codec` is `None`. Unset if no stream qualifies. **VERIFY-T2**: mux an MP4 whose
  audio track starts at 0.000000 and video at 0.041708 (one 24 fps frame of empty edit), read
  `format.start_time` and both `stream.start_time`s. If the reference reports the *maximum* the rule
  inverts; if it reports something else again, record what.
- **R13 — the ASF exception.** Research §1.3 records that ASF's header start-time field is
  unreliable and is deliberately not surfaced. Our ASF demuxer follows: it does not set
  `start_time` from the header, leaving discovery to derive it from packets. This is a
  format-specific opt-out expressed as a demuxer decision, not a core flag.

### 1.7.5 Duration estimation — the three strategies

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DurationSource { FromStream, FromPts, FromBitrate, Unknown }
```

Applied in this order; the first that yields a value wins.

- **R14 — `FromStream`.** The demuxer set `reported.duration` in `read_header` from an authoritative
  container field: MP4 `mvhd.duration`/`tkhd.duration`/`mdhd.duration`, Matroska `Info/Duration`,
  AVI `dwTotalFrames·dwMicroSecPerFrame`, WAV `data` chunk size ÷ byte rate, Ogg final granule
  position, FLAC `STREAMINFO.total_samples`. Container duration = `max` over streams of the
  rescaled per-stream duration, **not** the container-level field, when both exist and disagree —
  because a container-level field is often written by a first pass and never corrected.
  **VERIFY-T3**: build an MP4 whose `mvhd.duration` says 10 s and whose longest track says 12 s.
- **R15 — `FromPts`.** Requires byte-seekability and `!skip_estimate_duration_from_pts`. Seek to
  `max(0, size − duration_probesize)` (default `duration_probesize = 0` meaning "the internal
  default" — **VERIFY-T4**, measured by truncating a TS file's tail progressively until the
  duration goes wrong; upstream's value is believed to be around 250 kB but must be measured), demux
  to EOF without emitting packets, and take per-stream `max(pts + duration)`. Container duration =
  `max` over streams of that, minus `start_time`. If the tail read yields no timestamps, retry from
  a position twice as far back, up to three times, then give up. Retry counts and step sizes must be
  fixed constants, never adaptive on wall clock (DD1).
- **R16 — `FromBitrate`.** `duration = file_size · 8 / bit_rate` where `bit_rate` is the
  container's declared value, else the sum of declared stream bit rates. Only used when both are
  known and non-zero.
- **R17 — `Unknown`.** Printed as `N/A`.
- **R18 — the order is not negotiable and it is per-format-tunable in exactly one way.** A demuxer
  may set `reported.duration_is_authoritative`, which pins `FromStream` and suppresses R15 even when
  the file is seekable. WAV and FLAC set it (their container field is exact); MPEG-TS never does.

The chosen `DurationSource` is not printed by `vaco-probe`, but the resulting `duration` is, so the
choice is byte-testable through its consequences. It is exposed at `-loglevel verbose` for triage.

### 1.7.6 Generating what the container omitted

- **R19 — DTS from PTS.** If a packet has PTS and no DTS, and the stream's codec has
  `params.video_delay == 0` (no reordering), set `dts = pts`. If reordering is possible, leave DTS
  unset unless a parser supplies it.
- **R20 — PTS from DTS (`GENPTS`).** Only when `fflags +genpts`. Maintain a reorder buffer of
  `delay + 1` DTS values per stream, where `delay = params.video_delay` (or 1 if unknown and the
  codec has the `REORDER` property). The PTS of the packet emitted at position `i` is the
  `(i − delay)`-th smallest DTS observed so far. This is exact whenever the DTS sequence is a sorted
  permutation of the PTS sequence, which is the definition of a conforming reordering stream. When
  it is not, the result is wrong in the same way upstream's is, and both are wrong identically.
- **R21 — duration fill-in.** If a packet has no duration: use `next_packet.dts − this.dts` on the
  same stream if the next packet is already available (the parse queue makes this possible during
  discovery and for formats that read ahead); else `time_base.den / (avg_frame_rate · time_base.num)`
  for video; else `frame_size / sample_rate` rescaled for audio; else leave unset. **Order matters
  and is fixed** — the next-DTS delta wins over the frame-rate estimate even when they disagree.
- **R22 — monotonic DTS repair.** Per stream, maintain `cur_dts`. If an incoming DTS is `<= cur_dts`
  and the format does **not** declare `TS_DISCONT`, the packet's DTS is replaced by
  `cur_dts + max(1, last_duration)` and the packet is flagged `Repaired` (visible at
  `-loglevel debug`, and counted in the conformance report). If the format *does* declare
  `TS_DISCONT`, the value is passed through untouched — discontinuity policy is the CLI's job
  (plan 14 Stage I). **This split is the single most important boundary rule in the document.**
- **R23 — `IGNDTS`.** `fflags +igndts` drops DTS on packets that carry both, before R19–R22.
- **R24 — `NOFILLIN`.** `fflags +nofillin` disables R19, R21 and R22 entirely: only what the
  container stored is reported. `+noparse` additionally disables parser-supplied timestamps and
  requires `+nofillin` (upstream documents the dependency; we enforce it with an error rather than
  silently repairing the option set, and we say why).

### 1.7.7 The muxer-side chain

Applied by `vaco-format-core` before `Muxer::write_packet` sees anything. Fixed order:

```
pkt (input stream time base)
 M1  rescale to the muxer's Stream::time_base                        (NearInf)
 M2  + output_ts_offset, rescaled                                     (option, default 0)
 M3  + avoid_negative_ts offset                                       (R25)
 M4  monotonicity assertion                                           (R26)
 M5  interleave queue                                                 (§1.9)
 M6  bitstream filter chain                                           (§1.10)
 M7  Muxer::write_packet
```

- **R25 — `avoid_negative_ts`.** Values `auto` (−1, default), `disabled` (0), `make_non_negative`
  (1), `make_zero` (2). `auto` resolves to `make_non_negative` when the muxer does **not** declare
  `FormatFlags::TS_NEGATIVE`, and to `disabled` when it does. The offset is computed **once**, from
  the first packet written across *all* streams, and applied uniformly to every stream — a
  per-stream offset would desynchronise them. `make_zero` shifts so the first DTS is exactly 0;
  `make_non_negative` shifts only if the first DTS is negative. The offset is recorded so
  `-copyts` diagnostics can report it. **VERIFY-T5**: remux an MP4 with negative `ctts`-derived
  DTS into MPEG-TS with each of the four values and compare byte output.
- **R26 — monotonicity.** Muxers without `TS_NONSTRICT` require strictly increasing DTS per stream;
  with it, non-decreasing. A violation is `Error::NonMonotonicDts` naming the stream, the previous
  and the current value. It is an error, not a repair: silently repairing here is how files with
  subtly wrong durations get made.
- **R27 — packets with no DTS.** A muxer that needs DTS (`!NOTIMESTAMPS`) receiving a packet without
  one gets `dts = pts` if PTS exists, else an error. For `NOTIMESTAMPS` formats both are dropped.
- **R28 — `max_delay` / `muxdelay` / `muxpreload`.** These are CLI-side (plan 14 Stage VI) and reach
  us only as the already-computed `max_interleave_delta`.

### 1.7.8 The test that settles this section

A single differential matrix, run at `framecrc` and `exact-bytes` levels (correctness §1.2 C0/C3):

- **Inputs**: an MP4 with `start_time = 0.041708` and version-0 `ctts` producing negative DTS; an
  MP4 with an empty edit followed by a `media_time`-trimmed edit; an MPEG-TS crossing a 33-bit PTS
  wrap; an MPEG-TS with a mid-stream `discontinuity_indicator`; a Matroska with `TimestampScale`
  ≠ 1000000, non-zero `CodecDelay` and `DiscardPadding`; a Matroska written with unknown-size
  clusters; a WAV with an `ID3v2` prefix.
- **Axes**: `{-fflags +genpts, +igndts, +nofillin, +sortdts, none}` ×
  `{-avoid_negative_ts auto|disabled|make_non_negative|make_zero}` ×
  `{stream copy to mp4, to mkv, to ts, to nut}` × `{-copyts, none}`.
- ≈ 700 cases. Combined with plan 14 §6.4's ~600 CLI-side cases, this is the highest-value test set
  in the two plans and the two matrices must be authored together to avoid overlap.

## 1.8 The seek model

### 1.8.1 Surface

```rust
bitflags! {
    pub struct SeekFlags: u32 {
        const BACKWARD = 1 << 0;  // prefer a seek point <= target
        const BYTE     = 1 << 1;  // `ts` is a byte offset
        const ANY       = 1 << 2; // may land on a non-keyframe
        const FRAME    = 1 << 3;  // `ts` is a frame number
    }
}

pub struct SeekRequest {
    /// `None` = "any stream", meaning targets are in `TIME_BASE_Q` and the core picks a
    /// reference stream (§1.8.2 S2).
    pub stream: Option<StreamIndex>,
    pub min_ts: i64,
    pub ts: i64,
    pub max_ts: i64,
    pub flags: SeekFlags,
}

impl DemuxContext {
    /// The range-bounded form. Everything else is sugar over it.
    pub fn seek_file(&mut self, req: &SeekRequest) -> Result<()>;
    /// Legacy single-target form: `seek_file` with (MIN, ts, ts) or (ts, ts, MAX) by BACKWARD.
    pub fn seek_frame(&mut self, s: Option<StreamIndex>, ts: i64, f: SeekFlags) -> Result<()>;
}
```

### 1.8.2 Dispatch

- **S1 — order.** (a) If the demuxer implements `seek`, call it; it owns the whole operation. (b)
  Else if `BYTE` and `!NO_BYTE_SEEK`: byte seek (S6). (c) Else if the demuxer implements
  `read_timestamp`, `!NOBINSEARCH` and `!TS_DISCONT`: binary search (S5). (d) Else if
  `!NOGENSEARCH` and an index exists or `GENERIC_INDEX` is set: generic index seek (S4). (e) Else
  `Error::NotSupported`.
- **S2 — reference stream choice** when `req.stream` is `None`: the first stream, in index order,
  that (i) is not `ATTACHED_PIC`, (ii) is not `Discard::All`, and (iii) has media type Video if any
  video stream qualifies, else Audio, else any. Targets in `TIME_BASE_Q` are rescaled into that
  stream's time base with `Down` rounding for `min_ts`/`ts` and `Up` for `max_ts` (R2).
- **S3 — post-seek state reset, always.** After any successful seek: flush the I/O buffer, call
  `Demuxer::flush`, clear the parse queue, reset every stream's `cur_dts`/reorder buffer/duration
  history, and recompute the wrap offset per R10. Forgetting one of these is the classic
  "timestamps go strange after seeking" bug, so the core does it and the demuxer cannot forget.
- **S4 — generic index seek.** `index.search(stream, ts, flags)` → entry → `io.seek(entry.pos)` →
  S3 → read packets, discarding until the first packet on `stream` satisfying the target predicate
  (`dts >= ts` forward; already satisfied for `BACKWARD` since the entry was chosen at or before).
  If `!ANY`, discarded packets must also be pre-keyframe.
- **S5 — binary search.** Invariant-preserving bisection over byte positions:

  ```
  lo = 0 (or the first index entry <= ts), hi = file_size
  while hi - lo > MIN_STEP {
      mid = lo + (hi - lo)/2
      p = mid; t = read_timestamp(stream, &mut p, hi)?     // p moves forward to a real sync point
      match t { None => hi = mid, Some(t) if t < ts => lo = p, Some(_) => hi = mid }
      add_index_entry(stream, p, t)                        // bisection populates the index for free
  }
  ```

  `MIN_STEP` is a fixed constant (`64 KiB`) and the loop is bounded at `log2(size/MIN_STEP) + 4`
  iterations, so a pathological `read_timestamp` cannot hang — a real fuzzing concern (correctness
  §2.2's non-termination class).
- **S6 — byte seek.** `io.seek(ts)` then S3, then resync: the demuxer's next `read_packet` is
  responsible for finding the next sync point. For TS this means scanning for `0x47` at the packet
  stride; for MPEG-PS, a start code; for Matroska, a Cluster ID.
- **S7 — `FRAME`.** Converted to a timestamp via `avg_frame_rate` before dispatch. If the rate is
  unknown, `Error::NotSupported`.
- **S8 — `seek2any`.** The context option (default off) that permits landing on a non-keyframe even
  when the caller did not pass `ANY`. It is separate from `ANY` because some formats can honour one
  and not the other; a demuxer reads `ctx.opts.seek2any` directly.
- **S9 — `FAST_SEEK`.** `fflags +fastseek` permits the demuxer to take a cheaper, less accurate
  path (MP4: use `sidx` instead of walking `moof`s; HLS: seek to a segment boundary and not within
  it). Never changes correctness of the *reported* timestamps, only which packet you land on.

### 1.8.3 Index

```rust
#[derive(Clone, Copy, Debug)]
pub struct IndexEntry {
    pub pos: u64,
    pub timestamp: Ts,
    pub flags: IndexFlags,   // KEYFRAME | DISCARD_FRAME
    pub size: u32,
    pub min_distance: u32,   // bytes to the previous keyframe; 0 = unknown
}

pub struct StreamIndex_ { entries: Vec<IndexEntry> }   // strictly sorted by timestamp
```

- **I1 — insertion.** `add_entry` binary-searches by timestamp. An existing entry with the same
  timestamp is *updated* (position and flags refreshed) rather than duplicated. An entry whose
  position is within `min_distance` of an existing one is merged.
- **I2 — memory cap.** `indexmem` (default 1 MiB per stream). On overflow, **decimate**: drop every
  second non-keyframe entry, then every second keyframe entry if still over. Decimation is
  deterministic and preserves the endpoints. Upstream's exact eviction policy is unknown to us and
  is not observable through any output field, so this is a free choice — recorded as such rather
  than as a guess.
- **I3 — sources.** Built from a container-native index at `read_header` (MP4 `stss`+`stts`+`stco`,
  Matroska `Cues`, AVI `idx1`, ASF simple index, FLV `keyframes`), or incrementally as packets are
  read for `GENERIC_INDEX` formats, or as a by-product of S5's bisection.
- **I4 — `fflags +ignidx`** discards the container's index and forces incremental building. This is
  the escape hatch for files with lying indexes, and it changes seek results, so it is in the
  conformance matrix.

### 1.8.4 Composition with the CLI's `-ss` / `-to`

The contract plan 14 §6.4 Stage II relies on:

- **C1.** Input `-ss T` → `seek_file(stream=None, min_ts=i64::MIN, ts=T', max_ts=T', flags=∅)`,
  where `T' = T + start_time` unless `-seek_timestamp` is given, in which case `T' = T`. **The
  `start_time` addition is the rule naive implementations get wrong**, and it is why `-ss 0` on a
  file whose first PTS is 3.6 s does not seek to the beginning of the media timeline. `max_ts = ts`
  expresses "do not overshoot"; the range form then lands on the greatest seek point ≤ target
  without needing `BACKWARD`.
- **C2.** `-sseof T` (T negative) → resolve against `duration`; fails with a clear error if
  duration is unknown, rather than seeking to 0.
- **C3.** `-accurate_seek` (default on) is entirely above us: after C1 lands, the CLI decodes and
  discards to the exact target. With `-c copy` there is nothing to discard, so packets from the
  seek point onward are kept — which is why a stream copy with `-ss` starts early. We expose
  `DemuxContext::last_seek_landing` (the actual timestamp reached) so the CLI can report the
  difference and so `-read_intervals`' "+duration is measured from the *found* position" rule
  (plan 14 §5.3) has something exact to measure from.
- **C4.** `-to`/`-t` are not seeks. They are demux cutoffs applied by the CLI on `input_ts`.
- **C5.** `vaco-probe -read_intervals` uses exactly C1 and C3's `last_seek_landing`. No special
  path.
- **C6.** An unseekable input (`Seekability::none()`) makes C1 fail with `Error::NotSeekable` unless
  the target is forward of the current position, in which case the core degrades to
  **read-and-discard** — bounded by `probesize`-independent option `max_forward_seek` (default
  16 MiB) to stop a pipe input from silently reading a terabyte. **VERIFY-S1**: does the reference
  degrade to forward-discard on a pipe, and is it bounded? Test: `cat big.ts | ffprobe -ss 3600 -i -`
  and observe whether it reads to 3600 s or fails.

## 1.9 Muxer interleaving

```rust
pub struct InterleaveQueue {
    per_stream: Vec<VecDeque<QueuedPacket>>,   // indexed by StreamIndex
    live: usize,                                // streams not yet at EOF
    max_delta: i64,                             // max_interleave_delta, µs
    chunk: ChunkPolicy,
    seq: u64,                                   // arrival counter, the final tie-break
}

struct QueuedPacket { pkt: Packet, dts_us: i64, seq: u64 }

pub fn interleave_per_dts(q: &mut InterleaveQueue, pkt: Option<Packet>, flush: bool)
    -> Result<Option<Packet>>;
```

- **N1 — readiness.** Output one packet when *every* live stream has at least one queued packet, or
  `flush` is set, or the sparse-stream escape (N3) fires.
- **N2 — selection order.** Among the head packets of each stream, pick the smallest by
  `(dts_us, stream_index, seq)`. The second and third keys are what make the output deterministic
  when two streams share a DTS — and they are **VERIFY-N1**: remux a file with a subtitle packet and
  a video packet at identical DTS into Matroska and into MP4, and compare which is written first.
  If the reference's order differs from `(dts, index, seq)`, this is a one-line change and a
  recorded observation. Getting it wrong costs byte-identity on every multi-stream remux, so it is
  worth a dedicated fixture.
- **N3 — sparse-stream escape.** A stream that produces packets rarely (subtitles, timed metadata,
  a data track) would otherwise stall the whole queue. If
  `newest_dts_across_all − oldest_queued_dts > max_interleave_delta` (default 10 s), emit the
  oldest packet without waiting for the sparse stream. This is why the default is 10 s and not
  smaller: a subtitle gap of 8 s is normal.
- **N4 — EOF.** A stream is marked not-live when the caller signals its end. The queue then
  interleaves the remainder among the survivors. At final flush, drain in `(dts, index, seq)` order.
- **N5 — chunking.** `ChunkPolicy { max_duration_us, max_size_bytes, audio_preload_us }`. Formats
  that store data in per-track runs (MOV chunks, AVI `movi` chunks, MXF content packages) group
  consecutive same-stream packets rather than strictly alternating. The policy emits a whole chunk
  of stream A before switching to stream B, subject to N3. `audio_preload` biases audio packets
  earlier by a fixed µs offset **for interleaving purposes only** — it does not modify timestamps.
- **N6 — `write_frame` vs `write_interleaved_frame`.** The non-interleaving path exists and is what
  `-fflags +flush_packets` style low-latency muxing uses; the caller then owns DTS ordering, and R26
  still applies. Segmenting muxers use it internally.
- **N7 — custom policies.** MOV in fragmented mode interleaves within a fragment and flushes at
  fragment boundaries. MPEG-TS does not interleave at all in the queue sense — it multiplexes at the
  188-byte packet level against a PCR clock, so its `interleave` is pass-through and the real
  scheduling lives in the muxer. NUT interleaves by its own syncpoint policy. Each override is
  documented in that format's plan.

## 1.10 Bitstream-filter-in-muxer

Distinct from user `-bsf:v` chains, which the CLI applies before packets reach us.

```rust
pub enum BitstreamAction {
    Keep,
    /// Insert this filter and re-ask on the filtered packet, so chains compose.
    Insert { name: &'static str, opts: Options },
}
```

- **B1.** Enabled by `fflags +autobsf`, on by default. `-fflags -autobsf` disables it entirely.
- **B2.** `check_bitstream` is called on the first packet of a stream. If it returns `Insert`, the
  filter is attached and `check_bitstream` is called **again** on the filter's output, until it
  returns `Keep` or a depth limit of 4 is hit. Chaining matters: MP4 output of an Annex-B H.264
  stream needs `extract_extradata` *and* the length-prefix conversion.
- **B3.** The decision is cached per stream after the first packet. A stream whose bitstream form
  changes mid-file (in-band parameter set switch) is not re-examined — same as upstream, and it is
  why `avc3`/`hev1` sample entries exist.
- **B4.** The filters are supplied through the same injection pattern as parsers:
  `vaco-format-core` declares `trait BsfProvider { fn open(&self, name: &str, opts: &Options,
  par: &CodecParameters) -> Result<Box<dyn BitstreamFilter>>; }`, implemented by `vaco-registry`.
  No format crate depends on a `vaco-bsf-*` crate.
- **B5 — the known set**, per muxer (each container plan lists its own):

| Muxer | Condition | Filter |
|---|---|---|
| mp4/mov family | H.264/HEVC/VVC in Annex-B form | `h264_annexb2mp4` / `hevc_…` / `vvc_…` (length-prefixing) |
| mp4/mov family | AAC in ADTS | `aac_adtstoasc` |
| mp4/mov family | any codec needing global extradata not present | `extract_extradata` |
| mpegts | H.264/HEVC/VVC in length-prefixed form | `h264_mp4toannexb` / `hevc_mp4toannexb` / `vvc_mp4toannexb` |
| mpegts, hls(ts) | AAC in raw ASC form | `aac_asctoadts` (or LATM when `latm=1`) |
| matroska/webm | VP9 with superframes and `dash=1` | `vp9_superframe_split` |
| matroska/webm | AV1 in Annex-B/low-overhead mismatch | `av1_frame_merge` |
| any `GLOBALHEADER` muxer | extradata absent | `extract_extradata` |
| flv | AAC in ADTS | `aac_adtstoasc` |

## 1.11 The generic format-level option set

Research §1.12 lists 38 distinct option names plus two deprecated aliases (`f_err_detect`,
`f_strict`) — the "40 options". All are `vaco-opts` entries on the format context, introspectable
through `-h demuxer=…`/`-h muxer=…`, and settable per-input/per-output by the CLI's grouping model
(plan 14 §2.6). Names are interface facts and are reproduced exactly (D9).

| # | Option | Type | Default | D/E | Our implementation note |
|---:|---|---|---|---|---|
| 1 | `avioflags` | flags | 0 | D,E | Only const: `direct` → `IoFlags::DIRECT`, minimises buffering (§2.2). |
| 2 | `probesize` | i64 | 5 000 000 | D | Byte budget for §1.6's loop, counted from the I/O layer's `bytes_read`. |
| 3 | `formatprobesize` | i32 | `PROBE_BUF_MAX` | D | Ceiling for §1.5 R7. Value is VERIFY-P2. |
| 4 | `packetsize` | i32 | 0 | E | Fixed output packet size; MPEG-PS/TS only. |
| 5 | `fflags` | flags | `autobsf` | D,E | 14 consts: `flush_packets`, `ignidx`, `genpts`, `nofillin`, `noparse`, `igndts`, `discardcorrupt`, `sortdts`, `fastseek`, `nobuffer`, `bitexact`, `autobsf`, `nonblock`, `shortest`? — the exact const list is §1.10 of the research and is reproduced verbatim in `format_flags.toml`. |
| 6 | `seek2any` | bool | 0 | D | §1.8.2 S8. |
| 7 | `analyzeduration` | i64 µs | 0 → per-format | D | §1.6.4. |
| 8 | `cryptokey` | bytes | — | D | Raw decryption key; consumed by CENC (MP4) and by the `crypto:` protocol. |
| 9 | `indexmem` | i32 | 1 048 576 | D | §1.8.3 I2. |
| 10 | `rtbufsize` | i32 | 3 041 280 | D | Realtime capture buffer cap. Device-only; devices are out of scope for v1. |
| 11 | `fdebug` | flags | 0 | D,E | Consts `ts`, `id3v2`. Maps to `tracing` targets, not to a bespoke debug printer. |
| 12 | `max_delay` | i32 µs | −1 | D,E | Muxing/demuxing delay bound. |
| 13 | `start_time_realtime` | i64 | unset | E | Wall clock corresponding to PTS 0; written to containers that carry one (MKV `DateUTC`, MXF). **Suppressed by `bitexact`.** |
| 14 | `fpsprobesize` | i32 | −1 | D | §1.6.3. Default value is VERIFY-P4. |
| 15 | `audio_preload` | i32 µs | 0 | E | §1.9 N5. |
| 16 | `chunk_duration` | i32 µs | 0 | E | §1.9 N5. |
| 17 | `chunk_size` | i32 | 0 | E | §1.9 N5. |
| 18 | `err_detect` | flags | `crccheck` | D | Consts `crccheck`, `bitstream`, `buffer`, `explode`, `ignore_err`, `careful`, `compliant`, `aggressive`. Governs whether a CRC failure warns, marks `CORRUPT`, or errors. |
| 19 | `f_err_detect` | flags | — | D | Deprecated alias for 18. Accepted, warns once. |
| 20 | `use_wallclock_as_timestamps` | bool | 0 | D | Violates DD1 by design; excluded from the conformance corpus, and documented as such. |
| 21 | `skip_initial_bytes` | i64 | 0 | D | Applied at the I/O layer before probing (§1.5 R10). |
| 22 | `correct_ts_overflow` | bool | 1 | D | §1.7 R9. |
| 23 | `flush_packets` | i32 | −1 | E | −1 = format default. |
| 24 | `metadata_header_padding` | i32 | −1 | E | Reserved bytes in the written metadata header (MP4 `free` after `moov`, MKV `Void`). |
| 25 | `output_ts_offset` | duration | 0 | E | §1.7 M2. |
| 26 | `max_interleave_delta` | i64 µs | 10 000 000 | E | §1.9 N3. |
| 27 | `strict` | int | 0 | D,E | Consts `very`(2) `strict`(1) `normal`(0) `unofficial`(−1) `experimental`(−2). Gates non-conforming writes. |
| 28 | `f_strict` | int | — | D,E | Deprecated alias for 27. |
| 29 | `max_ts_probe` | i32 | 50 | D | §1.6.1. |
| 30 | `avoid_negative_ts` | int | −1 | E | §1.7 R25. |
| 31 | `dump_separator` | string | `", "` | D,E | Consumed by the CLI's info dump, not by us; carried here because it lives on the context. |
| 32 | `codec_whitelist` | csv | none | D | Enforced when a demuxer sets `params.codec`; a non-whitelisted codec becomes `None` and the stream is reported without a decoder. |
| 33 | `format_whitelist` | csv | none | D | §1.5 R9. |
| 34 | `protocol_whitelist` | csv | none | D | §2.4. |
| 35 | `protocol_blacklist` | csv | none | D | §2.4; blacklist wins over whitelist. |
| 36 | `max_streams` | i32 | 1000 | D | Hard cap; exceeding is an error, not a truncation. A fuzzing-critical bound. |
| 37 | `skip_estimate_duration_from_pts` | bool | 0 | D | §1.7 R15. |
| 38 | `max_probe_packets` | i32 | 2500 | D | §1.6.1. |
| 39 | `duration_probesize` | i64 | 0 → default | D | §1.7 R15. Default value is VERIFY-T4. |
| 40 | `recursion_limit` | i32 | 10 | D | Depth cap on nested demuxer opens (concat lists, HLS variant playlists, DASH periods, `tee`). Enforced in `vaco-format-core`, not per-format, so no nested demuxer can forget it. |

Three of these are security bounds rather than conveniences — `max_streams`, `recursion_limit` and
the whitelists — and each gets a dedicated fuzz target (correctness §2.1).

---

# 2. `vaco-io` and the protocol layer

Layer 2. The AVIO equivalent, plus URL dispatch. `#![forbid(unsafe_code)]` throughout
(`vaco-io-mmap`, if we ever adopt it, is on D2's allowlist and stays behind a non-default feature).

## 2.1 The core traits

Rust already has `std::io::{Read, Write, Seek}`, and we do **not** reimplement them. What we add is
the three things media I/O needs that `std::io` does not model: a *seekability class* that can be
queried without attempting a seek, a *size* query that works on protocols where `Seek::End` is
expensive or impossible, and a cooperative *cancellation* token.

```rust
/// What a source can do. Queried, never guessed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Seekability { pub byte: bool, pub time: bool }
impl Seekability {
    pub const NONE: Self = Seekability { byte: false, time: false };
    pub const BYTE: Self = Seekability { byte: true,  time: false };
}

pub trait MediaSource: Send {
    /// Short reads are permitted and normal. `Ok(0)` means EOF.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize>;
    /// `None` when the size is genuinely unknown (a pipe, a live HTTP stream with no
    /// Content-Length, a growing file being written). Never a guess.
    fn size(&mut self) -> Option<u64> { None }
    fn seek(&mut self, pos: u64) -> Result<u64> { let _ = pos; Err(Error::NotSeekable) }
    fn seekability(&self) -> Seekability { Seekability::NONE }
    /// Cost hint: a forward seek of at most this many bytes is cheaper as a read-and-discard.
    /// HTTP sets it high (a new request costs an RTT); a local file sets it to 0.
    fn short_seek_threshold(&self) -> u64 { 0 }
    /// Live sources: `Seekability::time`. RTSP implements these; everyone else does not.
    fn seek_time(&mut self, _ts: i64, _flags: SeekFlags) -> Result<i64> { Err(Error::NotSupported) }
    fn pause(&mut self) -> Result<()> { Err(Error::NotSupported) }
    fn play(&mut self)  -> Result<()> { Err(Error::NotSupported) }
}

pub trait MediaSink: Send {
    fn write(&mut self, buf: &[u8]) -> Result<()>;
    fn flush(&mut self) -> Result<()>;
    fn seek(&mut self, pos: u64) -> Result<u64> { let _ = pos; Err(Error::NotSeekable) }
    fn seekability(&self) -> Seekability { Seekability::NONE }
    /// Typed write, so segmenting muxers know where they may cut. Default forwards to `write`.
    fn write_marked(&mut self, buf: &[u8], _m: DataMarker) -> Result<()> { self.write(buf) }
    fn truncate(&mut self, _len: u64) -> Result<()> { Err(Error::NotSupported) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataMarker { Header, Trailer, SyncPoint, BoundaryPoint, Unknown, FlushPoint }
```

**Custom callback I/O disappears as a concept.** FFmpeg needs `avio_alloc_context` with three
function pointers because C has no other way to inject behaviour. We take
`Box<dyn MediaSource>` and the problem is gone — an in-memory buffer, a Rust `File`, a decrypting
wrapper and a network socket are all just implementations. `AVFMT_FLAG_CUSTOM_IO` has no analogue
because ownership is expressed by the type system: whoever passes the `Box` gave it away.

## 2.2 `IoContext` — the buffered reader/writer

```rust
pub struct IoContext { /* buffer, source/sink, position, sticky eof/error, checksum */ }

impl IoContext {
    pub fn new(src: Box<dyn MediaSource>, opts: &IoOptions) -> Self;
    pub fn new_write(dst: Box<dyn MediaSink>, opts: &IoOptions) -> Self;

    // --- byte-order readers; the workhorses of every demuxer ---------------------------------
    pub fn r8(&mut self)   -> Result<u8>;
    pub fn rb16(&mut self) -> Result<u16>;   pub fn rl16(&mut self) -> Result<u16>;
    pub fn rb24(&mut self) -> Result<u32>;   pub fn rl24(&mut self) -> Result<u32>;
    pub fn rb32(&mut self) -> Result<u32>;   pub fn rl32(&mut self) -> Result<u32>;
    pub fn rb64(&mut self) -> Result<u64>;   pub fn rl64(&mut self) -> Result<u64>;
    pub fn tag(&mut self)  -> Result<[u8; 4]>;
    pub fn read_exact(&mut self, buf: &mut [u8]) -> Result<()>;
    /// Short read permitted; returns how much was obtained. The only reader that tolerates EOF.
    pub fn read_partial(&mut self, buf: &mut [u8]) -> Result<usize>;
    /// Read `n` bytes into a packet payload without an intermediate copy.
    pub fn read_into_packet(&mut self, pkt: &mut Packet, n: usize) -> Result<usize>;
    pub fn skip(&mut self, n: u64) -> Result<()>;
    /// NUL-terminated or length-limited string readers, used by a dozen formats.
    pub fn get_str(&mut self, max: usize) -> Result<String>;
    pub fn get_str16be(&mut self, len: usize) -> Result<String>;   // ASF, MOV `keys`

    // --- position and shape ------------------------------------------------------------------
    pub fn pos(&self) -> u64;
    pub fn seek(&mut self, pos: u64) -> Result<u64>;
    pub fn seek_from_end(&mut self, back: u64) -> Result<u64>;
    pub fn size(&mut self) -> Option<u64>;
    pub fn seekability(&self) -> Seekability;
    pub fn at_eof(&self) -> bool;
    pub fn bytes_read(&self) -> u64;      // total, for `probesize` accounting
    pub fn error(&self) -> Option<&Error>; // sticky

    // --- integrity ---------------------------------------------------------------------------
    /// Start a running checksum at the current position. Matroska CRC-32 elements, MPEG-TS
    /// section CRC, PNG chunk CRC, and the `crc`/`md5` muxers all use this.
    pub fn start_checksum(&mut self, kind: ChecksumKind);
    pub fn take_checksum(&mut self) -> u64;

    // --- writing -----------------------------------------------------------------------------
    pub fn w8(&mut self, v: u8) -> Result<()>;    // …wb16/wl16/wb24/wb32/wl32/wb64/wl64
    pub fn write(&mut self, buf: &[u8]) -> Result<()>;
    pub fn write_marked(&mut self, buf: &[u8], m: DataMarker) -> Result<()>;
    pub fn flush(&mut self) -> Result<()>;
}
```

**Buffering rules.**

- Default buffer 32 KiB for reads, 32 KiB for writes; `IoOptions::block_size` overrides, and
  protocols may raise the minimum (`udp` sets it to the datagram size so a read never splits a
  datagram).
- `IoFlags::DIRECT` (`-avioflags direct`) bypasses the buffer for reads larger than it and forces a
  flush after every write. This is what makes `-avioflags direct` meaningful for pipes and devices.
- **Short seek.** `seek(p)` where `p` is ahead of the current position by at most
  `max(buffered_ahead, source.short_seek_threshold())` becomes a read-and-discard. For HTTP this is
  the difference between one connection and one connection per box header while walking an MP4's
  `moov`, and it is worth more than any other single optimisation in this crate.
- **Backward seek within the buffer** is free and common (every box/element parser overshoots by a
  few bytes and rewinds).
- **Sticky error and EOF.** Once set, every subsequent read returns the same error rather than
  retrying. Demuxers rely on this to unwind cleanly.

**Dynamic buffers**, the `avio_open_dyn_buf` role:

```rust
/// A growable in-memory sink. Used by every muxer that must know an element's size before
/// writing its header: MP4 `moov` and `moof`, Matroska master elements, NUT syncpoints,
/// and the two-pass "write, measure, patch" pattern.
pub struct DynBuf { buf: Vec<u8> }
impl MediaSink for DynBuf { /* … */ }
impl DynBuf {
    pub fn new() -> Self;
    pub fn with_capacity(n: usize) -> Self;
    pub fn as_slice(&self) -> &[u8];
    pub fn into_vec(self) -> Vec<u8>;
    /// Cap, so a hostile input cannot make a muxer allocate without bound.
    pub fn set_limit(&mut self, bytes: usize);
}
```

**Cancellation.** `AVIOInterruptCB` becomes `CancelToken` (`Arc<AtomicBool>` plus an optional
`Waker`), checked at every I/O boundary and at every `read_packet`. Blocking socket reads use a
poll-with-timeout loop so cancellation is bounded by the timeout, not by the read.

**Seekability probing.** Never "try a seek and see". `MediaSource::seekability()` is answered by the
protocol from what it knows: `file` is byte-seekable; `pipe` is not; `http` is byte-seekable iff the
server answered a range request or advertised `Accept-Ranges` *and* `seekable` is not 0;
`rtsp` is time-seekable and not byte-seekable; `udp` is neither. The context also carries the
`AVFMTCTX_UNSEEKABLE` equivalent, which HLS flips at runtime when it discovers a live playlist.

## 2.3 The `Protocol` trait and URL dispatch

```rust
pub trait Protocol: Send + Sync {
    fn open(&self, url: &Url, flags: IoFlags, opts: &Options, env: &ProtocolEnv)
        -> Result<Box<dyn MediaSource>>;
    fn create(&self, url: &Url, flags: IoFlags, opts: &Options, env: &ProtocolEnv)
        -> Result<Box<dyn MediaSink>> { let _ = (url, flags, opts, env); Err(Error::NotSupported) }
    /// Server mode: `-listen 1` on http/tcp/rtsp/rtmp/unix.
    fn accept(&self, _url: &Url, _opts: &Options, _env: &ProtocolEnv)
        -> Result<Box<dyn Connection>> { Err(Error::NotSupported) }
    fn check(&self, _url: &Url, _env: &ProtocolEnv) -> Result<Access> { Err(Error::NotSupported) }
    fn list_dir(&self, _url: &Url, _env: &ProtocolEnv)
        -> Result<Box<dyn Iterator<Item = Result<DirEntry>>>> { Err(Error::NotSupported) }
    fn delete(&self, _url: &Url, _env: &ProtocolEnv) -> Result<()> { Err(Error::NotSupported) }
    fn rename(&self, _from: &Url, _to: &Url, _env: &ProtocolEnv) -> Result<()> { Err(Error::NotSupported) }
}

pub struct ProtocolDesc {
    pub name: &'static str,
    pub flags: ProtocolFlags,          // NETWORK | NESTED_SCHEME | SERVER_CAPABLE
    /// The nested schemes this protocol implicitly grants. `hls` → http,https,tls,tcp,file,crypto.
    pub default_whitelist: &'static [&'static str],
    pub options: &'static OptionSchema,
    pub proto: &'static dyn Protocol,
}

/// Everything a nested open needs. Passed down through every level, never reconstructed.
pub struct ProtocolEnv<'a> {
    pub registry: &'a ProtocolRegistry,
    pub whitelist: Option<&'a [&'a str]>,
    pub blacklist: Option<&'a [&'a str]>,
    pub depth: u32,                // against `recursion_limit`
    pub cancel: &'a CancelToken,
    pub log: &'a Logger,
    pub rw_timeout: Option<Duration>,
}
```

### 2.3.1 The URL grammar is not RFC 3986, and pretending otherwise breaks things

FFmpeg's URL space is a superset of RFC 3986 with several format-specific escapes:

```
concat:file1.ts|file2.ts|file3.ts
subfile,,start,1024,end,4096,,:archive.bin
crypto+file:secret.bin                       (nested scheme via '+')
tee:out1.mkv|[f=mpegts]out2.ts
pipe:1
data:audio/wav;base64,UklGR…
async:http://host/path
cache:https://host/path
rtmp://host/app/stream live=1
```

We therefore split URLs ourselves and use the `url` crate only *inside* protocols that genuinely
speak RFC 3986 (http/https/ftp/rtsp):

```rust
pub struct Url { pub scheme: Option<String>, pub nested: Option<String>, pub rest: String,
                 pub inline_opts: Options }

/// Split rules, in order:
///   1. If the string has no ':' before the first '/', it is a bare path -> scheme = "file".
///   2. Scheme = chars up to the first ':' that are [A-Za-z0-9+.-]. A single-letter scheme
///      followed by ':\' or ':/' on Windows is a drive letter, not a scheme.
///   3. A '+' inside the scheme splits outer+inner ("crypto+file" -> crypto over file).
///   4. Protocol-specific `rest` parsing is the protocol's job, not the splitter's.
pub fn split_url(s: &str) -> Url;
```

**Rule U1 — the default scheme is `file` and only `file`.** A bare path never resolves to anything
else. **Rule U2 — a `file` open never follows a symlink out of an explicitly restricted root** when
the caller supplies one (used by `concat` list files and by HLS local playlists). **Rule U3 — the
Windows drive-letter case is handled in the splitter**, not in `file`, because every nested protocol
would otherwise have to re-handle it.

### 2.3.2 Whitelisting — the security boundary

Every nested open passes `ProtocolEnv`. The gate:

```
allowed(scheme) =
      scheme ∉ blacklist
  AND (whitelist is None  OR  scheme ∈ whitelist  OR  scheme ∈ parent.default_whitelist)
  AND depth < recursion_limit
```

- **W1.** The blacklist always wins.
- **W2.** A demuxer that opens nested URLs (`hls`, `dash`, `concat`, `sdp`, `tee`, `segment`,
  `image2` with a URL pattern) **must** route through `ProtocolEnv`. This is CI-enforced: a lint
  asserts that no `vaco-demux-*`/`vaco-mux-*` crate depends on `vaco-protocol-file` or any concrete
  protocol crate directly — only on `vaco-protocol-core`.
- **W3.** The default whitelist for a remote playlist excludes `file`. A hostile `.m3u8` served over
  HTTP cannot read `/etc/passwd`. This is the single most important security property in the whole
  I/O layer and it gets a dedicated conformance case per playlist format.
- **W4.** `depth` increments on every nested open, including protocol-over-protocol
  (`cache:async:https://…` is depth 3).

## 2.4 All 57 protocols, tiered

Research §4 tabulates 57 registrations across 56 named rows (the extra registration is a
platform-conditional alias; we enumerate the rows). Tiers mirror §4's format tiers.

### P1 — default build, local (12)

| Protocol | Spec / basis | Implementation | pw |
|---|---|---|---|
| `file` | — | `std::fs`. Options `truncate`, `blocksize`, `follow` (tail a growing file). | 0.5 |
| `pipe` | — | `pipe:<fd>`; 0/1/2 by name. Not seekable. | 0.3 |
| `fd` | — | Wrap an inherited descriptor. Unix only (`OwnedFd`). | 0.2 |
| `cache` | — | Read-through cache to a temp file, making an unseekable source seekable. Option `read_ahead_limit`. Genuinely valuable: it is what makes `ffprobe http://…` on a non-range server work. | 1.5 |
| `async` | — | Read-ahead thread with a bounded ring. Our version is a `std::thread` plus `sync_channel`, no condvar hand-rolling. | 1.5 |
| `concat` | — | `concat:a\|b\|c` sequential byte concatenation with a virtual seek across members. | 1.0 |
| `concatf` | — | Same, list read from a file. Shares 90% with `concat`. | 0.3 |
| `subfile` | — | Byte-range window over a nested URL. Used by the `dvdvideo` and Blu-ray paths and by manual byte-range extraction. | 0.5 |
| `data` | RFC 2397 | `data:` URI, base64 or percent-encoded. Pure parsing. | 0.4 |
| `crypto` | — | AES-128/256-CTR over a nested URL. RustCrypto `aes` + `ctr`. Options `key`, `iv`, `decrypt`. | 1.0 |
| `md5` | — | Write-only sink that emits the MD5 of everything written. RustCrypto `md-5`. | 0.2 |
| `tee` | — | Fan-out writes to several nested URLs. | 0.6 |

**Subtotal ≈ 8 pw.** None of these need a network stack, none need a crate we do not already have,
and all twelve are v0.1-or-v0.2 work.

### P2 — default build, network core (10)

| Protocol | Spec | Crate vs ours | pw |
|---|---|---|---|
| `tcp` | — | `std::net::TcpStream` + `socket2` for `tcp_nodelay`, `tcp_mss`, `tcp_keepalive`, send/recv buffer sizes, and bind-to-local-address. Options per research §4.4. | 1.5 |
| `udp` | RFC 768 | `std::net::UdpSocket` + `socket2` for multicast join/leave, source-specific multicast (`sources`), TTL, DSCP, `reuse`, `broadcast`, and the receive FIFO (`fifo_size`, `overrun_nonfatal`). The FIFO is a bounded ring with a reader thread — the option surface in research §4.6 exists because UDP capture drops packets otherwise. | 3.0 |
| `udplite` | RFC 3828 | `udp` plus `udplite_coverage`; needs a raw socket option `socket2` may not expose — a small `setsockopt` shim would be FFI, so this is **Linux-only via `socket2::Socket::set_...` if available, otherwise T3**. VERIFY at implementation time. | 0.5 |
| `unix` | — | `std::os::unix::net`. Options `listen`, `timeout`. | 0.5 |
| `tls` | RFC 8446 / 5246 | **`rustls`** — see §2.6.3 for the provider problem. | 2.5 |
| `http` | RFC 9110/9112 | **`ureq`** for transport (blocking, pure Rust, rustls-based), our own layer for Range/seek, reconnect, ICY, persistent connections, chunked POST. §2.6.2. | 4.0 |
| `https` | — | `http` over `tls`. Same crate. | 0.2 |
| `httpproxy` | RFC 9110 §9.3.6 | HTTP `CONNECT` tunnel. Ours. | 0.5 |
| `srtp` | RFC 3711 | AES-CM/HMAC-SHA1 with externally supplied keys (`srtp_in_params` etc.), no DTLS-SRTP key exchange at this layer. RustCrypto. | 2.0 |
| `rtp` | RFC 3550 | UDP pair (RTP + RTCP), multicast, `write_to_source`, Pro-MPEG FEC hook. The *payload* framing is the RTP demuxer's job (§3.4.12), not the protocol's. | 3.0 |

**Subtotal ≈ 18 pw.**

### P3 — default build, second wave (7)

`prompeg` (SMPTE 2022-1 FEC over UDP, 1.5 pw), `ftp` (RFC 959, 2 pw), `gopher`/`gophers`
(RFC 1436, 0.7 pw), `icecast` (source-client publishing over HTTP, 1 pw), `ipfs_gateway`/
`ipns_gateway` (URL rewriting onto `https`, 0.5 pw), `shared` (a shared-memory ring for multi-process
UDP fan-out — needs `memmap2`, which is FFI-free but uses `unsafe` internally; **T3, and only if
someone asks**).

### P4 — spec-available but substantial (5)

| Protocol | Spec status | Recommendation |
|---|---|---|
| `rtmp`, `rtmps`, `rtmpt`, `ffrtmphttp` | **Adobe published the "Real-Time Messaging Protocol Chunk Stream" specification publicly in 2012.** This is a genuine public spec — research §4 marks the family "reverse-engineered" because FFmpeg's implementation predates the publication, but *we* are not bound by that history. | Implement natively from the Adobe specification. ~10 pw for chunk stream + AMF0/AMF3 + handshake + play/publish. T3: real demand (every ingest endpoint still speaks it) but nothing in v0.1–v0.3 needs it. |
| `dtls` | RFC 6347 / 9147 | Needed only by `whip`. `rustls` has no DTLS. Native DTLS is ~6 pw and is the gating item for WebRTC ingest. **T4 for v1.0**; revisit if WHIP becomes a requirement. |
| `sctp` | RFC 4960 | Requires OS SCTP sockets, absent on macOS and Windows. **T4.** |
| `mmsh`, `mmst` | Microsoft published **[MS-MMSP]** under the Open Specification Promise. Spec-available despite research §4's "reverse-engineered" note, which again reflects FFmpeg's history rather than today's availability. | **T4** — the spec exists, but Windows Media streaming is functionally dead. Implement only on request. |

### P5 — SRT and RIST (2) — see §2.7

### P6 — excluded by D10 Gate 1 (13)

`bluray` (libbluray, GPL as well as FFI), `libamqp`, `libcurl`, `librist`, `librtmp`/`librtmpe`/
`librtmps`/`librtmpt`/`librtmpte`, `libsrt`, `libssh`, `libsmbclient` (also requires GPLv3),
`libzmq`, `android_content` (JNI). Every one of these is a binding to a C library and is out on
purity grounds regardless of licence. Four of them have native replacements in P4/P5 (`rtmp`,
`srt`, `rist`); the rest are simply absent, and `docs/why-some-protocols-are-not-included.md`
says so with the reason.

`ffrtmpcrypt` / `rtmpe` / `rtmpte` are a distinct case: **RTMPE's Diffie-Hellman handshake is
Adobe's unpublished obfuscation scheme and has no public specification.** It is a T5 item in
plan 15's sense and we do not implement it. `rtmps` (RTMP over real TLS) covers the same security
need properly and we do implement that.

### Protocol tier roll-up

| Tier | Count | Effort |
|---|---:|---:|
| P1 local, default | 12 | 8 pw |
| P2 network core, default | 10 | 18 pw |
| P3 default, second wave | 7 | 6 pw |
| P4 spec-available, later | 5 | 22 pw |
| P5 SRT + RIST | 2 | 22 pw |
| P6 excluded (Gate 1) | 13 + 3 RTMPE variants | — |
| **Total rows** | **56** | **76 pw** |

## 2.5 The OS-interface carve-out, stated explicitly

D10 Gate 1 forbids `-sys` crates and FFI. Taken literally it forbids `std` (which calls libc) and
therefore forbids sockets, files and threads. That is obviously not the intent, and plan 15 §8 already
made the same argument for hardware: **Gate 1 forbids FFI to third-party libraries that implement
functionality we could implement ourselves. It does not forbid the operating system's own API.**

The carve-out, written down so nobody has to re-litigate it per crate:

| Allowed | Reason |
|---|---|
| `std` (files, sockets, threads, time) | The OS is the boundary. |
| `libc` as a transitive dependency of `std`/`socket2` | Same. |
| `socket2` (MIT OR Apache-2.0) | A thin, safe wrapper over `setsockopt`. Nothing it does could be done in pure Rust — multicast join is a kernel operation. Contains `unsafe` for the syscall boundary; D10's "our code, not the process" caveat applies. |
| `rustix` (if preferred over `socket2`) | Same, and it is `#![no_std]`-capable with a syscall-direct backend on Linux. Assess at adoption. |

| Still forbidden | Reason |
|---|---|
| `libsrt-sys`, `librist-sys`, `libcurl`, `libssh`, `libsmbclient`, `libbluray`, `librtmp` | Third-party implementations of things we can implement. |
| `ring`, `aws-lc-rs` | **Compile C and assembly in their build scripts.** See §2.6.3 — this is the one place the carve-out does not save us. |

## 2.6 Crate assessments for the network layer

### 2.6.1 What we do not need

`quinn` is on D10's "back on the table" list, but **QUIC is not in FFmpeg's protocol inventory at
all** — there is no `http3` and no `quic` row in research §4. Adopting `quinn` would be building a
capability the reference does not have, which by definition cannot help byte-identity. Recorded here
so the roadmap does not carry it as an assumed dependency. (If Media-over-QUIC or WebTransport
becomes a goal, revisit; the crate clears all three gates comfortably.)

`hyper` clears the gates but is **async-only in 1.x**, and plan 14 §7.1 commits the scheduler to OS
threads plus bounded channels, explicitly not tokio. Adopting hyper means adopting a runtime for one
protocol. Rejected on model fit (D10's "the crate's model does not fit ours" clause), not on quality.

`reqwest` inherits hyper's runtime requirement. Same verdict.

### 2.6.2 HTTP — `ureq` behind the D11 adapter

| Gate | Assessment |
|---|---|
| 1 — pure Rust | **Pass.** No `-sys`, no build-script native compilation (with the rustls backend; the `native-tls` feature must be off). |
| 2 — licence | **Pass.** MIT OR Apache-2.0. |
| 3 — trusted | **Pass.** Widely adopted, actively maintained, blocking-by-design, shallow tree. |
| Model fit | **Good.** Blocking API matches our scheduler exactly. |

What `ureq` gives us: connection handling, TLS integration, redirects, chunked transfer, headers,
proxies. What it does **not** give us and we write: byte-range seeking with the "issue a new request
on a long seek, read-and-discard on a short one" policy; the reconnect state machine
(`reconnect`, `reconnect_at_eof`, `reconnect_streamed`, `reconnect_delay_max`,
`reconnect_delay_total_max`, `reconnect_max_retries`, `reconnect_on_http_error`,
`reconnect_on_network_error`, `respect_retry_after`); ICY/SHOUTcast metadata interleaving
(`icy`, `icy_metadata_headers`, `icy_metadata_packet`); persistent-connection reuse across HLS
segment fetches (`http_persistent`, `http_multiple`); chunked POST for the Icecast and HLS/DASH
upload paths; and `-listen 1` server mode.

That is a lot of "ours", and it is the right split: the ~40 options in research §4.1 are almost all
*policy*, and policy is where behavioural fidelity lives. **D11 boundary**: `ureq` appears in
`crates/protocol/vaco-protocol-http/Cargo.toml` and nowhere else; no `ureq` type crosses the crate's
public API. Predicted fidelity grade: **Equivalent** — header casing and the exact retry timing are
not observable through `vaco-probe` output, but redirect-following limits and range behaviour are.
Server mode is T3 and ours entirely (a ~400-line HTTP/1.1 responder).

### 2.6.3 TLS — an unresolved conflict that needs a decision

`rustls` itself passes every gate: Apache-2.0 OR ISC OR MIT, pure Rust, best-in-class maintenance.
**Its crypto providers do not.**

| Provider | Gate 1 | Notes |
|---|---|---|
| `ring` | **Fail** | `build.rs` compiles C and per-architecture assembly (BoringSSL-derived). Also a per-file composite licence needing a `cargo-deny` clarify entry (register §9 open task 4). |
| `aws-lc-rs` | **Fail** | `aws-lc-sys` builds a BoringSSL fork with cmake. A `-sys` crate by name and by nature. |
| `rustls-rustcrypto` | **Pass** | Pure Rust over the RustCrypto family. Upstream describes it as not production-hardened; performance is materially below `ring` for AES-GCM on hardware with AES-NI, because RustCrypto's AES falls back to a bitsliced software path without `unsafe` intrinsics. |
| `graviola` | Pass on licence, **fail on Gate 1's letter** | Pure-Rust-plus-assembly; the assembly is `global_asm!`, not a C build script. Arguably clears Gate 1's *intent* (no foreign library) while violating its letter (assembly). Young. |

Three options, each with a real cost:

- **(A) `rustls` + `rustls-rustcrypto`, in the default build.** Keeps Gate 1 intact and unqualified.
  Costs: TLS throughput likely 3–10× lower than `ring` on AES-GCM (measure before committing), and
  we ship a provider its own authors flag as not production-hardened for a security-critical path.
  For a media tool that mostly does TLS on HLS/DASH manifest and segment fetches, throughput may not
  actually matter — segment fetch is usually bandwidth-bound, not CPU-bound. **This is measurable
  and should be measured before the decision, not after.**
- **(B) A narrow, written Gate-1 exception for the TLS crypto provider only,** on the same reasoning
  D9 uses to promote hardware acceleration: cryptography is a domain where using audited,
  widely-deployed code is *safer* than the pure alternative, and the exception is one crate deep and
  auditable. Ship `ring`, with the `cargo-deny` clarify entry, an entry in `docs/dependencies.md`,
  and a CI assertion that no other C-compiling crate enters the graph.
- **(C) `https` is not in the default build.** Unacceptable — it removes HLS and DASH, which are
  most of what people point a media tool at in 2026.

**Recommendation: run the benchmark, then choose (A) if the throughput gap is under ~2× for
realistic segment sizes, else (B).** Either way this is an amendment to D10 and needs to be recorded
there, not buried in a subsystem plan. Structurally the choice is cheap to reverse: `rustls`'s
provider is a runtime-installable trait object, so `vaco-protocol-tls` selects it behind a feature
and nothing above it changes. That is D11 working exactly as designed.

Certificate roots: `rustls-platform-verifier` (uses the OS trust store; some FFI on macOS/Windows —
same OS carve-out) or `webpki-roots` (a compiled-in Mozilla root set, pure Rust, but stale between
releases and wrong for corporate MITM proxies). **Recommend `rustls-platform-verifier` with
`webpki-roots` as a fallback feature**, matching what users expect from `curl` and from FFmpeg's
schannel/securetransport backends.

### 2.6.4 Everything else

| Crate | Use | Gates | Predicted D11 grade |
|---|---|---|---|
| `socket2` | UDP multicast, socket options | Pass (OS carve-out) | n/a — not a media crate |
| RustCrypto `aes`, `ctr`, `cbc`, `sha1`, `sha2`, `md-5`, `hmac` | CENC, HLS AES-128, SRTP, `crypto:`, hash muxers | Pass | **Exact** — these compute standardised functions; there is no interpretation |
| `flate2` + `miniz_oxide` | Matroska zlib content encoding, compressed `moov`, SWF | Pass | **Exact** |
| `lzma-rs` | Matroska LZMA content encoding (rare) | Pass | Exact |
| `quick-xml` | DASH MPD, TTML, SAMI, Smooth Streaming manifests | Pass | **Exact** — a parser; all interpretation is ours |
| `ureq` | HTTP transport | Pass | Equivalent (§2.6.2) |
| `rustls` | TLS | Pass, provider excepted | n/a |
| `memchr` | start-code and sync-byte scanning | Pass | Exact — but measure against a `fearless_simd` kernel first (D12); we may want it in `vaco-simd` instead |

## 2.7 SRT — the recommendation

libsrt is MPL-2.0 and denied by D3/D10 Gate 2, and the `srt-rs`/`libsrt-sys` bindings inherit it.
Research §5 records this and stops at "implemented natively or dropped". The plan takes a position.

**SRT has a public specification.** `draft-sharabayko-srt` (IETF Internet-Draft, "The SRT Protocol",
published by Haivision engineers and covering handshake, ARQ, encryption, timestamp-based packet
delivery and congestion control) is a public document. **Therefore SRT is clean-room implementable,
and "absent" would be a choice rather than a necessity.**

**Recommendation: native, tier T3, ~12 pw.** Reasons:

1. SRT is the de facto contribution transport for live broadcast in 2026. A tool that cannot ingest
   SRT is excluded from a whole professional workflow — this is not a long-tail format.
2. The protocol is genuinely implementable: it is UDT-derived ARQ over UDP with AES-CTR encryption
   and a well-documented handshake. Nothing in it needs `unsafe`, and the state machine is exactly
   the kind of thing Rust is good at.
3. There is a plausible **backend** to start from: `srt-protocol` (part of russelltg's `srt-rs`) is a
   **sans-io** state-machine crate — no I/O ownership, which is precisely the shape D10's "does the
   model fit ours" test asks for, since we drive the socket. Licence is believed Apache-2.0 and
   **must be verified at adoption** in the style of register §9's open tasks; Gate 3 (adoption,
   maintenance) is the weak point and will likely fail. Treat it as an accelerant and an oracle, not
   as the answer.
4. Under D11 it costs nothing to hedge: `vaco-protocol-srt` exposes our `Protocol` trait with
   `backend-external` / `backend-native` features, and the tests are the acceptance criteria for
   whichever wins.

**Do not implement RIST from `librist`** for the same Gate-1 reason; RIST's specifications
(VSF TR-06-1 Simple Profile, TR-06-2 Main Profile) are public and it is a smaller job than SRT
(~10 pw), but demand is lower. T4.

---

# 3. Container plans

Common to every format crate: `#![forbid(unsafe_code)]`; a `cargo-fuzz` target from the day the
crate lands (D6); a `docs/formats/<name>.md` page in the same change; a provenance trailer naming
the specification and section each PR was written from (D7); and a conformance entry in
`tests/conformance/probe/<name>.toml` (correctness §1.5).

## 3.1 MP4 / MOV — `vaco-demux-mp4`, `vaco-mux-mp4`

The largest single item in the subsystem. Upstream's `mov.c` is ~12.5k lines and `movenc.c` ~9.6k
(research §6) — together roughly a fifth of libavformat's format code.

### 3.1.1 Governing specifications

| Area | Document |
|---|---|
| Base file format | **ISO/IEC 14496-12** (ISO Base Media File Format), 2022 edition |
| MP4 specifics | **ISO/IEC 14496-14** (MP4 file format) |
| AVC/HEVC/VVC carriage, `avcC`/`hvcC`/`vvcC` | **ISO/IEC 14496-15** |
| MPEG-4 systems, `esds` / DecoderConfigDescriptor / object-type indications | **ISO/IEC 14496-1** |
| QuickTime-only atoms (`gmhd`, `tapt`, `clef`, `cmov`, `tmcd`, `wave`, `chan`) | Apple, **QuickTime File Format Specification** (published; developer archive) |
| Common encryption | **ISO/IEC 23001-7** (`cenc`, `cbc1`, `cens`, `cbcs`) |
| HEIF / AVIF still and tiled images | **ISO/IEC 23008-12**, **AOM AVIF specification** |
| 3GPP / 3GPP2 profiles and `udta` boxes | **3GPP TS 26.244**, **3GPP2 C.S0050** |
| AV1 in ISOBMFF (`av1C`) | AOM, *AV1 Codec ISO Media File Format Binding* v1.2.0 |
| VP8/VP9 (`vpcC`) | WebM Project, *VP Codec ISO Media File Format Binding* |
| Opus (`dOps`) | Xiph, *Encapsulation of Opus in ISO Base Media File Format* |
| FLAC (`dfLa`) | Xiph, *Encapsulation of FLAC in ISOBMFF* |
| Dolby Vision (`dvcC`/`dvvC`/`dvwC`) | Dolby, *Dolby Vision Streams Within the ISO Base Media File Format* (public) |
| CMAF | **ISO/IEC 23000-19** |
| Timed text (`tx3g`, `wvtt`, `stpp`) | ISO/IEC 14496-30, 3GPP TS 26.245 |

Everything here is a published standard. **MP4 is the format where clean-room is least fraught** —
which is convenient, because it is also the one D5 needs first.

### 3.1.2 Box structures to parse

Grouped by v0.1 necessity.

**Tier 1 (v0.1, non-negotiable).**

```
ftyp                                     brand, minor version, compatible brands
moov
 ├ mvhd                                  timescale, duration, matrix, next_track_id, rate, volume
 ├ iods                                  (skipped; profile bytes only)
 ├ trak
 │  ├ tkhd                               track_ID, duration, layer, alternate_group, volume,
 │  │                                    3×3 matrix, width/height (16.16), flags(enabled/in-movie)
 │  ├ tref                               chap / hint / cdsc / dpnd / fall / subt / vdep / vplx
 │  ├ edts ▸ elst                        edit list — §3.1.5
 │  └ mdia
 │     ├ mdhd                            timescale (this is the stream time base), duration,
 │     │                                 packed ISO-639-2/T language, quality
 │     ├ elng                            extended language tag (BCP-47) — wins over mdhd when present
 │     ├ hdlr                            handler type: vide/soun/subt/text/sbtl/hint/meta/tmcd/clcp
 │     └ minf
 │        ├ vmhd | smhd | hmhd | nmhd | gmhd(▸ gmin, text, tmcd)
 │        ├ dinf ▸ dref                  data references — §3.1.10 (security)
 │        └ stbl
 │           ├ stsd  ▸ SampleEntry[]      per entry: format fourcc, data_reference_index, then
 │           │                            VisualSampleEntry (w/h, depth, compressorname) or
 │           │                            AudioSampleEntry v0/v1/v2 (channels, samplesize,
 │           │                            samplerate 16.16, v1 sound-description extensions)
 │           │                            plus extension boxes:
 │           │        avcC hvcC vvcC av1C vpcC dvcC dvvC dvwC esds dOps dfLa dac3 dec3 dmlp
 │           │        alac wave(▸ frma, esds, chan) btrt pasp colr clap fiel gama chnl srat
 │           │        clli mdcv SmDm CoLL ccst sinf(▸ frma schm schi(▸ tenc)) glbl
 │           ├ stts                       decode deltas, run-length coded
 │           ├ ctts                       composition offsets, v0 unsigned / v1 signed
 │           ├ cslg                       composition-to-decode shift
 │           ├ stss                       sync-sample (keyframe) table; absent = all sync
 │           ├ stsc                       sample-to-chunk runs
 │           ├ stsz | stz2                sample sizes (stz2: 4/8/16-bit fields)
 │           ├ stco | co64                chunk offsets (32/64-bit)
 │           ├ sdtp                       per-sample dependency flags
 │           ├ sbgp / sgpd                sample groups (roll, rap, seig, sync, tele, prol)
 │           ├ saiz / saio / senc         CENC auxiliary info — §3.1.8
 │           └ padb, stsh, subs
 ├ mvex ▸ mehd, trex, trep                fragmentation defaults
 └ udta ▸ meta ▸ (hdlr, keys, ilst), chpl, and the 3GPP asset boxes
moof ▸ mfhd, traf ▸ (tfhd, tfdt, trun[], sbgp, sgpd, saiz, saio, senc, subs)
mdat, free, skip, wide, junk
sidx, ssix, prft, emsg, mfra ▸ (tfra, mfro)
meta (top level) ▸ hdlr, pitm, iloc, iinf, iprp ▸ (ipco, ipma), iref, idat   — HEIF/AVIF
pssh                                     DRM initialisation data
```

**Tier 2 (v0.2+).** `cmov`/`dcom`/`cmvd` (zlib-compressed `moov`, QuickTime legacy), `tapt`
(`clef`/`prof`/`enof` — QuickTime clean aperture), `wave` atom nesting, `rtp `/`hnti` hint tracks,
`skip`-embedded `free` metadata, `uuid` extension boxes (Smooth Streaming `tfxd`/`tfrf`, XMP,
Sony/Canon camera metadata), `keys`-indexed metadata with non-`mdta` namespaces.

### 3.1.3 Demux stages

1. **Probe.** Score 100 when the first box is `ftyp` with a recognised major brand
   (`isom mp41 mp42 avc1 iso2 iso4 iso5 iso6 iso8 mp71 qt   3gp4 3gp5 3g2a M4V M4A M4P mif1 msf1
   avif heic heix hevc hevx crx isml ccff dash cmfc`); 90 for `ftyp` with an unknown brand;
   75 for a leading `moov`/`mdat`/`pnot`/`wide`/`free`/`skip` chain that leads to a valid `moov`;
   0 otherwise. A separate weaker score applies when only `mdat` is visible in the probe window.
2. **Open, walk top level.** A box header is `size:u32, type:[u8;4]`, with `size == 1` meaning a
   following `u64` largesize and `size == 0` meaning "to end of file". `uuid` type carries a
   16-byte extended type. Validate `size >= header_len` and `pos + size <= parent_end` on every box;
   an unbounded box inside a bounded parent is corrupt.
3. **Find `moov`.** If `moov` precedes `mdat`, streaming works. If it follows, we must seek; on an
   unseekable input this fails unless `mvex` is present (fragmented). Report the distinction in the
   error, because "this file needs `-movflags +faststart` to stream" is the single most common user
   question about MP4.
4. **Build tracks.** One `Stream` per `trak` whose handler is `vide`/`soun`/`subt`/`sbtl`/`text`/
   `clcp`/`tmcd`/`meta`. `hint` tracks are skipped by default. `Stream::id = tkhd.track_ID`,
   `time_base = 1/mdhd.timescale`.
5. **Codec identification.** `stsd` sample-entry fourcc → `CodecId` via `vaco-format-isom`'s
   movvideo/movaudio/movsubtitle tables; for `mp4v`/`mp4a`/`mp4s`, refine through `esds`'s
   object-type indication (ISO/IEC 14496-1 Table 5). Extradata comes from the configuration box with
   the right `ExtraFlavour` (`Avcc`, `Hvcc`, `Av1C`, `VpCodecConfig`, `EsdsAsc`, `OpusHead`,
   `FlacStreamInfo`) — this is exactly the flavour distinction plan 15 §1.1 introduced, and MP4 is
   where it earns its keep.
6. **Sample table expansion — lazily.** This is the biggest structural improvement over upstream.
   FFmpeg materialises a per-sample array; a 4-hour 30 fps file has 432 000 video samples and ~660
   000 audio frames, so the tables cost tens of megabytes per track. We keep the *compressed* tables
   and expose a cursor:

   ```rust
   pub struct SampleCursor<'a> {
       stts: RunCursor<'a>,   // (count, delta) runs
       ctts: RunCursor<'a>,   // (count, offset) runs
       stsc: ChunkCursor<'a>, // (first_chunk, samples_per_chunk, sample_description_index) runs
       stsz: SizeCursor<'a>,  // uniform or per-sample
       stco: OffsetTable<'a>,
       stss: SyncCursor<'a>,
       index: u64, dts: i64, chunk: u32, offset_in_chunk: u64,
   }
   impl<'a> SampleCursor<'a> {
       pub fn next(&mut self) -> Option<SampleRef>;   // O(1) amortised
       pub fn seek_to_sample(&mut self, n: u64);      // O(log runs) — binary search over runs
       pub fn seek_to_dts(&mut self, dts: i64) -> u64;// O(log runs)
       pub fn count(&self) -> u64;                    // from stsz/stts, without expansion
   }
   pub struct SampleRef { pub index: u64, pub pos: u64, pub size: u32,
                          pub dts: i64, pub cts_offset: i32, pub sync: bool,
                          pub stsd_index: u32 }
   ```

   Memory is O(number of runs), typically a few hundred entries. `nb_frames` (which `vaco-probe`
   prints) comes from `count()` without touching the data. A materialised `Vec<SampleRef>` is built
   **only** when the file's tables are pathologically fragmented (runs ≈ samples) or when profiling
   shows the cursor is hot — behind a `materialise_threshold` internal constant, not an option.
7. **Packet ordering.** Maintain one cursor per track; each `read_packet` picks the next sample to
   emit. **Rule MP4-O1**: pick the track whose next sample has the smallest DTS rescaled to
   `TIME_BASE_Q`; break ties by smallest file offset; break remaining ties by track index.
   **VERIFY-M1** — packet order is part of `-show_packets` byte output, so this is directly
   testable: build an MP4 whose chunks are deliberately out of DTS order (write it with an
   interleaver that emits all video then all audio) and compare packet order.
   **Rule MP4-O2**: a track whose next sample offset is inside a region we have already passed does
   *not* cause a backward seek unless the offset delta exceeds the short-seek threshold — for a
   non-interleaved file this converts a pathological seek storm into sequential reads.
8. **Fragmented input.** After `moov` (possibly empty, with only `mvex`), each `moof` is parsed and
   its `trun` entries appended to the per-track cursor as an additional run source. `tfhd` supplies
   defaults; `tfdt` supplies the fragment's base decode time (and is the only reliable way to place
   a fragment on the timeline — `default_base_is_moof` and `base_data_offset` govern *byte*
   addressing, not time). Streaming fMP4 over a pipe works: read `moof`, read `mdat`, emit, repeat.
9. **Attached pictures.** `udta ▸ meta ▸ ilst ▸ covr` becomes a `Stream` with
   `Disposition::ATTACHED_PIC`, `nb_frames = 1`, and `attached_pic` populated.
10. **Stream groups.** A HEIF/AVIF `grid` derived item becomes a `StreamGroupKind::TileGrid` whose
    member streams are the tile items. This is what `vaco-probe -show_stream_groups` prints for
    `.avif`/`.heic` inputs, which are in the MOV demuxer's extension list, so v0.1 needs it.

### 3.1.4 Timestamps

- `dts` accumulates `stts` deltas from 0. `pts = dts + ctts_offset`.
- `ctts` version 0 offsets are **unsigned**; version 1 are **signed**. With version 0 and B-frames,
  the first sample's PTS exceeds its DTS by the reorder depth, so upstream shifts DTS negative to
  keep PTS starting at zero. `cslg` (composition-to-decode box), when present, states the shift
  explicitly and should be preferred over inferring it.
- **Rule MP4-T1**: if `cslg` is present, `dts_shift = -cslg.composition_to_dts_shift`. Else if any
  `ctts` v0 offset would make the minimum PTS negative, `dts_shift = -max(ctts offsets in the first
  `delay+1` samples)`. Else 0. **VERIFY-M2**: this affects `start_time` and every packet DTS, so it
  is checked on a B-frame MP4 muxed by three different tools (x264+MP4Box, the reference binary,
  and a hardware encoder), each of which writes `ctts` differently.
- `nb_frames` = sample count. `duration` = `mdhd.duration` rescaled, or the sum of `stts` deltas
  when `mdhd.duration` is 0 (VERIFY-T3 covers the disagreement case).

### 3.1.5 Edit lists — the notorious part

`elst` entries are `(segment_duration in movie timescale, media_time in media timescale,
media_rate_integer.media_rate_fraction)`.

- **E1 — empty edit** (`media_time == -1`): inserts `segment_duration` of blank at the start. The
  stream's `start_time` becomes positive by that amount. This is how a track is delayed relative to
  the movie.
- **E2 — first non-empty edit's `media_time` is a trim.** Samples with `pts < media_time` are
  dropped or, for audio, converted to a `skip_samples` packet side-data trim on the first surviving
  packet. The canonical case is AAC encoder delay: `media_time = 1024` (or 2112 for some encoders),
  meaning "discard the priming samples". `vaco-probe -show_packets` prints packet side data, so
  this is byte-visible.
- **E3 — presentation offset.** After trimming, the presented timeline starts at the movie-timescale
  position of the edit, not at `media_time`. Concretely: `pts_presented = pts_media - media_time +
  sum(previous segment_durations)`.
- **E4 — `media_rate != 1`** is a speed change. Upstream handles rate-1 edits properly and does
  something approximate otherwise. We implement rate 1 exactly, and for any other rate we emit a
  warning and treat it as rate 1 — **and we record that as a known divergence** (correctness §1.4)
  rather than pretending. VERIFY-M3: does the reference do the same?
- **E5 — multiple non-empty edits** are a genuine edit decision list: the presented stream is a
  concatenation of media segments in arbitrary order. Upstream's `advanced_editlist` option (default
  on) handles the general case; with it off, only the first edit is honoured. We implement the
  general case: the cursor is driven by an edit-segment iterator that maps presentation time to
  media time. This is more code than the simple path but it is bounded and it removes a permanent
  source of divergence.
- **E6 — `ignore_editlist`** demuxer option disables E1–E5 entirely.
- **E7 — edit lists and seeking.** Seek targets are in *presented* time; the cursor must invert E3.
  Getting this wrong makes seeking in trimmed files land one edit-boundary off, which is the
  classic "audio starts a frame late after seeking" bug.
- **E8 — fragmented files with edit lists** are ill-defined by the specification: `elst` lives in
  `moov`, `tfdt` in each fragment, and the two coordinate systems are not formally reconciled.
  Report a warning, apply `elst` to the assembled timeline, and add a conformance case. This is a
  place where the reference's behaviour is the specification in practice.

### 3.1.6 Metadata mapping

| Source | Handling |
|---|---|
| `udta ▸ meta ▸ ilst` with iTunes four-char keys (`©nam ©ART ©alb ©day ©cmt ©gen ©too ©wrt aART trkn disk covr cpil tmpo gnre desc ldes purd`) | Mapped through `vaco-format-isom`'s conversion table to canonical keys (`title artist album date comment genre encoder …`). `trkn`/`disk` are binary and become `track`/`disc` as `n/total`. `covr` becomes an attached picture. `gnre` (numeric) and `©gen` (text) both map to `genre`, text winning. |
| `----` freeform atoms (`mean`/`name`/`data`) | Key = `name` value, verbatim, so `----:com.apple.iTunes:iTunNORM` becomes a metadata key |
| `meta ▸ keys` + `ilst` index pairing (QuickTime/iOS) | Keys are full reverse-DNS strings (`com.apple.quicktime.location.ISO6709`, `…make`, `…model`, `…creationdate`). Mapped where a canonical key exists, passed through otherwise |
| 3GPP `udta` boxes (`titl auth perf gnre dscp albm yrrc loci rtng clsf kywd cprt`) | Each is a full box with a language code; mapped to canonical keys, language recorded |
| `©xyz` / `loci` | Geographic location → `location` |
| `tkhd` matrix | `DisplayMatrix` stream side data; `vaco-probe` derives `rotation` from it |
| `tmcd` track | The timecode value → `timecode` metadata on the associated video track (via `tref ▸ tmcd`) |
| `udta ▸ chpl` (Nero) | Chapters, 100 ns units |
| `tref ▸ chap` → a text track | Chapters, from that track's samples. Both sources may be present; **VERIFY-M4** which wins |
| `mvhd`/`mdhd` creation/modification time | `creation_time`/`modification_time`, seconds since 1904-01-01 UTC. **Suppressed under `bitexact`** |
| `hdlr` name field | `handler_name`, printed by `vaco-probe` in stream tags |
| `mdhd` packed language / `elng` | `language` stream tag; `elng` (BCP-47) wins |

Language packing: `mdhd`'s 15-bit field holds three 5-bit values, each `char - 0x60`, giving
ISO-639-2/T. The value `0x55C4` ("und") and the value 0 both mean unspecified; a value whose
high bit is set is a Macintosh language code, not packed ISO-639, and needs the legacy table.

### 3.1.7 Seeking

- Byte-exact via the sample tables: `SampleCursor::seek_to_dts` on the reference track, then find the
  nearest preceding sync sample via `stss` (absent `stss` = every sample is sync), then position all
  other tracks to their nearest preceding sample at or before that DTS.
- The demuxer implements `Demuxer::seek` directly, so §1.8's generic paths never run for
  non-fragmented MP4. `FormatFlags` therefore include neither `GENERIC_INDEX` nor `NOBINSEARCH`.
- **Fragmented, seekable, with `mfra`**: `tfra` is a per-track random-access table (time → moof
  offset). Use it.
- **Fragmented, seekable, with `sidx` but no `mfra`**: `sidx` gives (subsegment duration, size)
  runs; accumulate to find the containing subsegment. `ssix` subdivides it. This is the DASH
  on-demand profile's index and is the fast path for `-fflags +fastseek`.
- **Fragmented, seekable, with neither**: walk `moof` boxes from the start, reading only headers
  (each `moof` is followed by an `mdat` whose size lets us skip it). O(fragments) seeks but each is
  cheap. Build an index as we go (§1.8.3 I3) so the second seek is fast.
- **Fragmented, unseekable**: forward-discard only.

### 3.1.8 Encryption

- `stsd ▸ sinf` gives `frma` (the original format fourcc — the sample entry itself becomes `encv`/
  `enca`), `schm` (scheme type: `cenc`, `cbc1`, `cens`, `cbcs`; version), and `schi ▸ tenc`
  (`default_isProtected`, `default_Per_Sample_IV_Size`, `default_KID`, and for pattern schemes
  `default_crypt_byte_block`/`default_skip_byte_block`).
- `senc` (or `saiz`+`saio` pointing into `mdat`) gives per-sample IVs and subsample maps
  (`bytes_of_clear_data` / `bytes_of_protected_data` pairs).
- `pssh` boxes carry per-DRM-system initialisation data (Widevine, PlayReady, FairPlay system IDs).
- **Without a key**: the demuxer reports the stream with its *original* codec (from `frma`), sets
  the `Encrypted` stream side data, attaches per-packet encryption side data, and emits packets
  unmodified. `vaco-probe` on an encrypted file must produce the same output as the reference,
  which it can, because neither decrypts.
- **With `-decryption_key`**: AES-128-CTR (`cenc`/`cens`) or AES-128-CBC (`cbc1`/`cbcs`) per
  subsample, using RustCrypto. Pattern schemes (`cens`/`cbcs`) encrypt `crypt_byte_block` 16-byte
  blocks then skip `skip_byte_block`.
- **Mux side**: `encryption_scheme=cenc-aes-ctr` with `encryption_key`/`encryption_kid` writes
  `sinf`/`tenc`/`senc`/`pssh`. The IV sequence must be deterministic under `bitexact` or byte
  comparison is impossible — **we derive IVs from a counter seeded by the KID**, and document it,
  because a random IV would make every muxed file differ. VERIFY-M5: what does the reference do?
  If it uses a random IV, encrypted mux is excluded from C0 and gets a `container-structure`
  (correctness C2) comparison instead.

### 3.1.9 Mux stages

1. `init`: validate codecs against `query_codec` at the current `strict` level; assign track IDs
   (`use_stream_ids_as_track_ids` controls whether input stream ids are reused); pick timescales
   (`movie_timescale` default 1000; `video_track_timescale` overrides per video track; audio uses
   the sample rate).
2. `write_header`: `ftyp` with brand from the `brand` option or the profile
   (`mp4`→`isom`, `3gp`→`3gp4`, `ipod`→`M4V `, `ismv`→`isml`, `avif`→`avif`); optionally an
   `empty_moov` (fragmented) or a reserved `free` box (`faststart` pre-allocation); then `mdat`
   with a placeholder size.
3. Packets accumulate as sample metadata per track while payload bytes stream into `mdat`.
   Interleaving is chunk-based (§1.9 N5) — a chunk is a run of one track's samples, sized by
   `chunk_duration`/`chunk_size`.
4. `write_trailer`: close `mdat` (patching its size, or promoting to a 64-bit `largesize` if it
   exceeded 4 GiB), then build every `stbl` table with run-length compaction, then `moov`.
5. **`faststart`**: `moov` is built into a `DynBuf`, then the whole `mdat` is shifted forward by
   `moov.len()` and `moov` written at the front. Chunk offsets in `stco`/`co64` must be adjusted by
   exactly that amount, which is why the shift is done after the tables are built and the tables
   are patched rather than rebuilt. Requires a seekable, rewritable output.
6. **Fragmented**: `movflags` `empty_moov`, `frag_keyframe`, `frag_duration`, `frag_size`,
   `frag_every_frame`, `frag_custom`, `separate_moof`, `default_base_moof`, `omit_tfhd_offset`,
   `global_sidx`, `skip_sidx`, `skip_trailer`, `dash`, `cmaf`, `hybrid_fragmented`,
   `delay_moov`, `frag_discont`. Each fragment = `moof` + `mdat`; `tfdt` written always for DASH/CMAF.
7. The full `movflags` set from research §3.10 is implemented as a bitmask option with the exact
   constant names, plus `moov_size` (an integer living in the same unit group — a genuine upstream
   oddity that we reproduce because the CLI surface must match).
8. **Determinism.** Under `-fflags +bitexact`: no `creation_time`/`modification_time` (write 0), no
   writing-application `©too`/`hdlr` name containing a version, no random `uuid`. With those
   suppressed, MP4 mux output is a pure function of the packets and options, and correctness §1.2
   C0 applies.

### 3.1.10 The notoriously awkward parts

| Problem | Our handling |
|---|---|
| `dref` external data references (samples stored in another file) | **Refused by default.** An external `dref` entry with `self_contained` clear makes the track unreadable and we say so. Following it is a file-system read triggered by file content — the same class of hole as W3. An `-allow_external_refs` option exists, is off, and is excluded from the default build's documentation examples. |
| Multiple `stsd` entries per track | Each sample carries `stsd_index`. Where entries share a `CodecId`, we switch extradata mid-stream via packet side data (`NewExtradata`). Where they do not, we report the first and warn. VERIFY-M6 against a file with `avc1` and `hvc1` entries in one track. |
| `mdhd.timescale == 0` | Corrupt; the track is dropped with a warning. A division-by-zero in a naive parser and a guaranteed fuzz finding. |
| `stsc` whose first entry's `first_chunk != 1`, or non-monotonic `first_chunk` | Rejected as corrupt for that track. |
| `stz2` with field size 4 (two samples per byte) | Handled; a common source of off-by-one. |
| Chunk offsets pointing outside the file, or overlapping | Validated lazily at read time, not eagerly: a truncated file must still report its streams. |
| `moov` appearing twice | The first wins; the second is skipped with a warning. |
| `cmov` (zlib-compressed `moov`) | `flate2`, with a decompressed-size cap. Tier 2. |
| `mdat` shorter than the sample table promises | Samples past EOF are dropped; `nb_frames` still reports the table's count, matching what the reference prints. VERIFY-M7. |
| `avc1` vs `avc3` (`hvc1` vs `hev1`) | `avc3`/`hev1` allow in-band parameter sets. Extradata may be absent; the parser refines from the first keyframe. Mux side, the sample entry chosen determines whether `extract_extradata` runs (§1.10 B5). |
| AAC in `esds` with implicit SBR/PS | The AudioSpecificConfig may declare 22.05 kHz when the real output is 44.1 kHz. The container reports what it stores; the decoder resolves it. `vaco-probe` prints the container value. |
| `sidx` with `reference_type == 1` (hierarchical index) | Recursed, with a depth cap of 4. |
| `tfhd` with `default_base_is_moof` unset and no `base_data_offset` | Base is the *start of the previous `mdat`*, per 14496-12. A classic mis-implementation. |
| HEIF derived items (`grid`, `iden`, `iovl`) | `grid` → `TileGrid` stream group. `iden`/`iovl` → warn, expose the referenced items as streams. |
| Negative `tkhd` width/height, or 16.16 values whose fractional part is non-zero | Preserved as a `Rational` for `sample_aspect_ratio` derivation; never truncated silently. |
| `elst` with `segment_duration == 0` | Per spec means "to the end of the media". Handled explicitly. |

### 3.1.11 Effort

| Package | pw |
|---|---:|
| `vaco-format-isom` (shared box/tag/`esds`/language helpers) | 4 |
| Demux: box walk, track build, sample cursor, packet output | 8 |
| Demux: edit lists, `cslg`/`ctts` timestamp rules, seek | 4 |
| Demux: fragmented (`moof`/`tfdt`/`trun`/`sidx`/`mfra`) | 3 |
| Demux: metadata, chapters, cover art, timecode, side data | 3 |
| Demux: CENC reporting + decryption | 2 |
| Demux: HEIF/AVIF item model + tile grid | 2 |
| **Demux total** | **22** (incl. isom) |
| Mux: header/tables/trailer, chunked interleave | 7 |
| Mux: faststart, fragmented, all `movflags` | 5 |
| Mux: metadata, CENC write, profile variants (ipod/ismv/f4v/psp/3gp/3g2/avif) | 4 |
| **Mux total** | **16** |

## 3.2 Matroska / WebM — `vaco-demux-matroska`, `vaco-mux-matroska`

### 3.2.1 Governing specifications

| Area | Document |
|---|---|
| EBML | **RFC 8794** (Extensible Binary Meta Language) |
| Matroska | **RFC 9559** (Matroska Media Container, 2024) — this is the crucial one: Matroska now has a real, complete IETF specification, including the codec-mapping registry in §27 |
| WebM profile | WebM Project, *WebM Container Guidelines* |
| Codec mappings | RFC 9559 §27, plus the WebM Project's *Codec Mappings* for `V_VP9`/`A_OPUS` |
| Encryption | WebM Project, *WebM Encryption* (AES-CTR) |
| VP9 in WebM | WebM Project, *VP9 Codec ISO/WebM Bitstream Features* |
| WebM DASH | WebM Project, *WebM DASH Specification* |

**Matroska is the easiest of the three v0.1 formats to clean-room**, because RFC 9559 is recent,
complete, and written to be implementable. The codec-ID table — the one piece that would otherwise
be "copy FFmpeg's table" — is normative in §27, so D9's "constant tables come from the
specification" is satisfied directly.

### 3.2.2 Element structures

```
EBML                       EBMLVersion, EBMLReadVersion, EBMLMaxIDLength, EBMLMaxSizeLength,
                           DocType ("matroska" | "webm"), DocTypeVersion, DocTypeReadVersion
Segment (often unknown-size)
 ├ SeekHead                SeekID + SeekPosition; may be at both ends
 ├ Info                    TimestampScale (ns, default 1_000_000), Duration (float!),
 │                         DateUTC, MuxingApp, WritingApp, SegmentUUID, Title,
 │                         PrevUUID/NextUUID/SegmentFilename (linked segments)
 ├ Tracks ▸ TrackEntry     TrackNumber, TrackUID, TrackType (1 video 2 audio 3 complex
 │                         16 logo 17 subtitle 18 buttons 32 control 33 metadata),
 │                         CodecID, CodecPrivate, CodecDelay (ns), SeekPreRoll (ns),
 │                         DefaultDuration (ns), Name, Language (ISO-639-2),
 │                         LanguageBCP47, FlagDefault/Forced/Enabled/HearingImpaired/
 │                         VisualImpaired/TextDescriptions/Original/Commentary,
 │                         TrackTimestampScale (deprecated), MaxBlockAdditionID,
 │                         BlockAdditionMapping ▸ (BlockAddIDType/Value/ExtraData),
 │   ├ Video               PixelWidth/Height, PixelCrop{Bottom,Top,Left,Right},
 │   │                     DisplayWidth/Height/Unit, FlagInterlaced, FieldOrder,
 │   │                     StereoMode, AlphaMode, ColourSpace (FourCC),
 │   │                     Colour ▸ {MatrixCoefficients, BitsPerChannel, ChromaSubsampling*,
 │   │                       CbSubsampling*, ChromaSiting*, Range, TransferCharacteristics,
 │   │                       Primaries, MaxCLL, MaxFALL,
 │   │                       MasteringMetadata ▸ {Primary{R,G,B}Chromaticity{X,Y},
 │   │                         WhitePointChromaticity{X,Y}, LuminanceMax, LuminanceMin}},
 │   │                     Projection ▸ {ProjectionType, ProjectionPrivate, Pose{Yaw,Pitch,Roll}}
 │   ├ Audio               SamplingFrequency (float), OutputSamplingFrequency, Channels, BitDepth
 │   └ ContentEncodings ▸ ContentEncoding ▸ {ContentEncodingOrder/Scope/Type,
 │                         ContentCompression ▸ {ContentCompAlgo, ContentCompSettings},
 │                         ContentEncryption ▸ {ContentEncAlgo, ContentEncKeyID,
 │                           ContentEncAESSettings ▸ AESSettingsCipherMode}}
 ├ Chapters ▸ EditionEntry ▸ ChapterAtom ▸ {ChapterUID, ChapterTimeStart/End,
 │                         ChapterFlagHidden/Enabled, ChapterDisplay ▸ {ChapString,
 │                         ChapLanguage, ChapLanguageBCP47, ChapCountry}}
 ├ Cluster (often unknown-size)
 │   ├ Timestamp
 │   ├ SimpleBlock         track number (vint), 16-bit signed relative timestamp,
 │   │                     flags (keyframe, invisible, lacing 2 bits, discardable)
 │   └ BlockGroup ▸ {Block, BlockDuration, ReferenceBlock, ReferencePriority,
 │                   BlockAdditions ▸ BlockMore ▸ {BlockAddID, BlockAdditional},
 │                   DiscardPadding, CodecState}
 ├ Cues ▸ CuePoint ▸ {CueTime, CueTrackPositions ▸ {CueTrack, CueClusterPosition,
 │                    CueRelativePosition, CueDuration, CueBlockNumber}}
 ├ Attachments ▸ AttachedFile ▸ {FileDescription, FileName, FileMediaType, FileData, FileUID}
 └ Tags ▸ Tag ▸ {Targets ▸ {TargetTypeValue, TargetType, TagTrackUID, TagEditionUID,
                            TagChapterUID, TagAttachmentUID},
                 SimpleTag ▸ {TagName, TagLanguage, TagLanguageBCP47, TagDefault,
                              TagString, TagBinary, nested SimpleTag}}
```

### 3.2.3 EBML parsing — the rules that bite

- **K1 — variable-length integers.** Element IDs keep their length marker; sizes strip it. Max ID
  length 4 and max size length 8 by default, overridable by the EBML header (and both are attack
  surface — cap them at the header's declared values and reject larger).
- **K2 — unknown-size elements.** A size field of all ones (`0x01FF…FF` for length 8) means unknown.
  RFC 9559 §6.2 defines the termination rule: an unknown-size element ends immediately before the
  first element that is not a valid child of it at the current nesting level. Implementing this
  needs the element-schema table (which element may be a child of which, and at what level) —
  generated from RFC 9559's element list into `vaco-demux-matroska/src/schema.rs`. **Live-streamed
  Matroska and WebM-over-HTTP both use unknown-size Segments and Clusters**, so this is not an
  exotic path.
- **K3 — unknown elements.** Skip by size. An unknown element with unknown size is unrecoverable;
  abort the parent.
- **K4 — `Void` and `CRC-32`.** `Void` is padding. `CRC-32` is the first child of a master element
  and covers the rest of that element; verified when `err_detect` includes `crccheck`, using
  `IoContext::start_checksum`.
- **K5 — level-0 recovery.** On a corrupt element, scan forward for a known level-1 element ID
  (`Cluster`, `Tracks`, `Cues`, `Info`, `Tags`, `Attachments`, `Chapters`, `SeekHead`) and resume.
  This is what makes truncated/damaged MKVs still play.

### 3.2.4 Demux stages

1. **Probe.** EBML magic `1A 45 DF A3` → parse the EBML header → `DocType`. Score 100 for
   `matroska` or `webm`; 75 for an EBML file with an unrecognised DocType (some other EBML format).
   The `webm` demuxer name is an alias for the same implementation; the DocType is recorded and is
   what `vaco-probe` reports as `format_name`.
2. **`read_header`.** Parse EBML header; enter `Segment`; if `SeekHead` exists, use it to jump
   directly to `Info`, `Tracks`, `Cues`, `Tags`, `Chapters`, `Attachments`. If not (or if it is
   wrong — common), linear-scan level-1 elements, skipping `Cluster` bodies by size, until all of
   `Info` and `Tracks` are found. A second `SeekHead` at the end of the file is followed when the
   first points to it.
3. **Time base.** `TimestampScale` is nanoseconds per tick, default 1 000 000 → time base 1/1000.
   **Every track shares it.** `Stream::time_base = TimestampScale / 1e9` reduced.
4. **Codec mapping.** `CodecID` string → `CodecId` via RFC 9559 §27's registry
   (`V_MPEG4/ISO/AVC` → h264 with `Avcc` extradata, `V_MPEGH/ISO/HEVC` → hevc with `Hvcc`,
   `V_AV1` → av1 with `Av1C`, `V_VP8`/`V_VP9`, `A_OPUS` with `OpusHead`, `A_VORBIS` with the
   three Xiph headers packed Xiph-style, `A_AAC` with `EsdsAsc`, `A_FLAC` with `FlacStreamInfo`,
   `A_PCM/*`, `S_TEXT/*`, `S_HDMV/PGS`, `S_DVBSUB`, `S_VOBSUB`, `V_MS/VFW/FOURCC` and
   `A_MS/ACM` — the last two carry a `BITMAPINFOHEADER`/`WAVEFORMATEX` in `CodecPrivate` and are
   why `vaco-demux-matroska` depends on `vaco-format-riff`).
5. **Cluster/Block parsing.** Packet timestamp = `(Cluster.Timestamp + block_relative) ×
   TimestampScale`, expressed in the stream time base — so in the default case, simply
   `Cluster.Timestamp + block_relative`, in milliseconds.
6. **Lacing.** `SimpleBlock`/`Block` flags bits 1–2: 0 none, 1 Xiph, 3 fixed-size, 2 EBML. All three
   pack N frames into one block:
   - *Xiph*: sizes as sequences of 255-bytes; last frame's size is the remainder.
   - *Fixed*: frame count only; sizes are equal (`total / count`, must divide exactly).
   - *EBML*: first size as a vint, subsequent as signed vint deltas.
   Each laced frame becomes its own `Packet`. **Timestamps for laced frames**: the first gets the
   block timestamp; subsequent ones get `+ i × DefaultDuration` if `DefaultDuration` is set, else
   they are spaced by dividing `BlockDuration` by the frame count, else they inherit the block
   timestamp and are marked as having no independent PTS. Rule MKV-L1, and **VERIFY-K1** on a
   Vorbis-in-MKV file with EBML lacing, which is the common real case.
7. **Content encodings**, applied in `ContentEncodingOrder` order, highest first:
   - `ContentCompAlgo` 0 = zlib (`flate2`), 1 = bzlib (**not implemented — `bzip2-rs` is decode-only
     and this is vanishingly rare; warn and drop the track**), 2 = lzo1x (**not implemented; no
     permissive pure-Rust LZO decoder clears Gate 3 — warn and drop**), 3 = header stripping
     (`ContentCompSettings` bytes are prepended to every frame; used to strip repeated AAC/RealVideo
     headers). Header stripping is common and must work.
   - `ContentEncryption` with `AESSettingsCipherMode = 1` (CTR): each frame is prefixed by a signal
     byte; if bit 0 is set, an 8-byte IV follows. Decryption with a supplied key; without one, the
     packets carry encryption side data as in MP4.
8. **`CodecDelay` / `SeekPreRoll` / `DiscardPadding`** — three different trims that are constantly
   confused:
   - `CodecDelay` (ns, per track): the codec's own start delay. Subtracted from every timestamp on
     that track, and recorded as `params.initial_padding`. For Opus it is the pre-skip.
   - `SeekPreRoll` (ns, per track): how far *before* a seek target the decoder must start to produce
     correct output. Recorded as `params.seek_preroll`; consumed by the seek path, not by timestamps.
   - `DiscardPadding` (ns, per block): trailing samples to discard from *this* block. Becomes
     `skip_samples` packet side data with the trailing count set.
   All three are byte-visible through `vaco-probe -show_packets`/`-show_streams`.
9. **Duration.** `Info/Duration` is a **float** in TimestampScale units. DD3 requires we convert it
   once, deterministically: `duration_ticks = round(Duration)` as an `i64`, using round-half-away.
   **VERIFY-K2**: a file whose `Duration` is 12345.6789 — does the reference truncate or round?
   Additionally, files written by mkvmerge for streaming carry a per-track `DURATION` tag
   (`HH:MM:SS.nnnnnnnnn`); when `Info/Duration` is absent we use the maximum of those.
10. **Cues → index.** `CueTime` + `CueClusterPosition` (+ `CueRelativePosition` inside the cluster)
    become index entries flagged keyframe. Absent Cues, seeking falls back to §1.8 S5 binary search
    over cluster positions, which works because Cluster IDs are findable by scanning.
11. **Tags.** `TargetTypeValue` scopes a tag: 70 collection, 60 edition/season, 50 album/movie,
    40 part, 30 track/song, 20 movement, 10 shot. Tags targeting a `TagTrackUID` become stream
    metadata; untargeted ones (or `TargetTypeValue` 50 with no UID) become container metadata;
    tags targeting a `TagChapterUID` become chapter metadata. Nested `SimpleTag`s are flattened as
    `PARENT/CHILD`. **VERIFY-K3** on the flattening separator.
12. **Attachments** become streams with `MediaType::Attachment`, `params.codec` derived from
    `FileMediaType` (`application/x-truetype-font` → `ttf`, `otf`, `application/x-font` …), the
    file data as the single packet, and `filename`/`mimetype` metadata.
13. **Chapters.** `EditionEntry` → chapters. Multiple editions exist; the default (or first) edition
    wins, and the others are ignored with a warning. Ordered chapters (`ChapterFlagOrdered`) and
    linked segments are **not** implemented — warn, and record as a known divergence.
14. **`webm_dash_manifest`** is a demuxer *mode*, not a separate parser: it reports the
    DASH-relevant properties (cue positions, initialisation range, bandwidth) as container
    metadata. ~0.5 pw on top of the demuxer.

### 3.2.5 Mux stages

EBML header → `Segment` (unknown size for `live=1`, else a reserved 8-byte size patched at the end)
→ `SeekHead` (reserved, patched) → `Info` → `Tracks` → `Tags` → `Attachments` → `Chapters` →
`Cluster`s → `Cues` → patch `SeekHead` and `Info/Duration`.

- Clusters are bounded by `cluster_size_limit` and `cluster_time_limit`, and a new cluster **must**
  start when the block-relative timestamp would exceed the 16-bit signed range. For video, a new
  cluster starts on a keyframe.
- Master elements whose size is not known in advance are written to a `DynBuf`, measured, then
  emitted. This is the primary consumer of `DynBuf` (§2.2).
- `reserve_index_space` pre-allocates a `Void` for `Cues`; `cues_to_front` writes `Cues` before the
  first cluster (requires either the reservation or a full rewrite).
- `default_mode` (`infer` / `infer_no_subs` / `passthrough`) controls how `FlagDefault` is derived
  from input dispositions.
- `write_crc32` adds CRC-32 elements. `allow_raw_vfw` and `flipped_raw_rgb` handle the VFW
  compatibility corners.
- **Determinism.** Under `bitexact`: `MuxingApp`/`WritingApp` become fixed strings without version
  numbers, `DateUTC` is omitted, and `SegmentUUID`/`TrackUID`/`FileUID`/`ChapterUID` — which are
  normally random — become deterministic values derived from the stream index. **Without that last
  substitution Matroska output can never be byte-identical**, so it is load-bearing; VERIFY-K4
  confirms what the reference substitutes (a fixed zero UID, a counter, or omission).

### 3.2.6 The awkward parts

| Problem | Handling |
|---|---|
| Unknown-size Segment *and* Cluster in a live stream | K2's schema-driven termination. The single hardest piece of Matroska parsing and the one with the most fuzz surface. |
| Lacing timestamp derivation | MKV-L1, VERIFY-K1. |
| `Duration` as a float | DD3 + VERIFY-K2. |
| `TimestampScale` ≠ 1 000 000 | Everything is in ticks; there is no special case, but a fixture with `TimestampScale = 100` is in the corpus because most implementations assume milliseconds. |
| `V_MS/VFW/FOURCC` and `A_MS/ACM` | Depend on `vaco-format-riff`. The `BITMAPINFOHEADER`'s `biCompression` FourCC drives codec selection. |
| Cues pointing at the wrong track or at a non-existent position | Validated against the file size at parse; bad entries dropped, not fatal. |
| A `SeekHead` that lies | Positions are validated (target position must hold the claimed element ID); on mismatch, fall back to linear scan. |
| Blocks with a track number not in `Tracks` | Dropped with a rate-limited warning. |
| Multiple `EditionEntry` / ordered chapters / linked segments | First edition only; ordered chapters not honoured; documented divergence. |
| `BlockAddID` payloads (VP9 alpha, HDR10+ dynamic metadata, Dolby Vision RPU) | Exposed as packet side data. `BlockAdditionMapping` gives the type. |
| `A_VORBIS` `CodecPrivate` (Xiph-packed three headers) | Unpacked at header time into the flavour the decoder expects. |
| `S_TEXT/UTF8` vs `S_TEXT/ASS` vs `S_TEXT/WEBVTT` newline and timing conventions | Handled in the subtitle codec crates, but the container must not mangle bytes. |
| Files with the `Tracks` element after the first `Cluster` | Legal and rare; handled by the linear scan, which does not stop at the first cluster. |

### 3.2.7 Effort

| Package | pw |
|---|---:|
| EBML reader + schema table + unknown-size termination + recovery | 3 |
| Demux: header, tracks, codec mapping, colour/HDR metadata | 3 |
| Demux: clusters, lacing, content encodings, encryption reporting | 3 |
| Demux: cues/seek, tags, chapters, attachments, `webm_dash_manifest` | 2 |
| **Demux total** | **11** |
| Mux: EBML writer, clusters, cues, seekhead patching, all options | 6 |
| Mux: WebM profile, `webm_chunk`, determinism work | 2 |
| **Mux total** | **8** |

## 3.3 MPEG-TS — `vaco-demux-mpegts`, `vaco-mux-mpegts`

### 3.3.1 Governing specifications

| Area | Document |
|---|---|
| Transport stream, PES, PSI, PCR | **ISO/IEC 13818-1** (= ITU-T H.222.0) |
| Stream types, descriptors | 13818-1 Table 2-34 (stream_type) and §2.6 (descriptors) |
| DVB service information | **ETSI EN 300 468** (SDT, NIT, EIT, BAT, TDT/TOT, and the DVB descriptors) |
| DVB subtitling / teletext carriage | **ETSI EN 300 743**, **ETSI EN 300 472** |
| ATSC PSIP | **ATSC A/65** |
| AC-3 / E-AC-3 carriage | **ATSC A/52** Annex, ETSI TS 102 366 Annex |
| Splice information | **ANSI/SCTE 35** |
| AVC / HEVC / VVC / AV1 carriage | 13818-1 Amendments; AOM *AV1 MPEG-2 TS binding* |
| Blu-ray M2TS (192-byte packets) | BDA *System Description Blu-ray Disc Read-Only Format* — partially public; the 4-byte `TP_extra_header` is documented in public write-ups. Treat the timestamp field as observed-behaviour, not spec-derived, and say so in the provenance trailer. |

### 3.3.2 Structures

```
TS packet (188 / 192 m2ts / 204 with RS parity)
  sync 0x47 | TEI | PUSI | transport_priority | PID(13)
  transport_scrambling_control(2) | adaptation_field_control(2) | continuity_counter(4)
  adaptation_field: length, discontinuity_indicator, random_access_indicator,
                    ES_priority, PCR_flag(48b: 33-bit base @90k + 6 reserved + 9-bit ext @27MHz),
                    OPCR_flag, splicing_point_flag(splice_countdown),
                    transport_private_data, adaptation_field_extension
PSI section (in payload, after pointer_field on a PUSI packet)
  table_id | section_syntax_indicator | section_length(12)
  | table_id_extension | version_number(5) | current_next_indicator
  | section_number | last_section_number | <body> | CRC_32 (MPEG-2, poly 0x04C11DB7, non-reflected)
  PAT   (PID 0x0000, table 0x00): program_number -> PMT PID; program 0 -> NIT PID
  CAT   (PID 0x0001, table 0x01): CA descriptors
  PMT   (table 0x02): PCR_PID, program_info descriptors,
                      per-ES: stream_type, elementary_PID, ES_info descriptors
  NIT   (PID 0x0010, table 0x40/0x41)   SDT/BAT (PID 0x0011, 0x42/0x46/0x4A)
  EIT   (PID 0x0012)   TDT/TOT (PID 0x0014)
PES packet
  0x000001 | stream_id | PES_packet_length (0 = unbounded, video only)
  | '10' | scrambling | priority | data_alignment | copyright | original
  | PTS_DTS_flags | ESCR | ES_rate | DSM_trick | additional_copy_info | PES_CRC | PES_extension
  | header_data_length | PTS(33b) | DTS(33b) | …
```

### 3.3.3 Demux stages

1. **Probe.** Find the packet size by testing 188, 192, 204: for each candidate stride, count how
   many of the next N positions hold `0x47`. Score `min(100, 25 + 8·runs)` per §1.5.3, capped by
   how much buffer we have. `mpegtsraw` scores slightly lower than `mpegts` so `mpegts` wins the
   tie (§1.5 R6 priority). `resync_size` bounds how far we search for the first sync byte.
2. **`read_header`.** Read up to `probesize` bytes, collecting PSI. `scan_all_pmts` (default on
   during probing) makes us wait for every PMT referenced by the PAT, so all programs appear.
   `skip_unknown_pmt` skips PMTs not referenced by the PAT.
3. **Programs.** Each PAT entry becomes a `Program` with `program_num`, `pmt_pid`, `pcr_pid`,
   `pmt_version` — all four are printed by `vaco-probe -show_programs`, so all four must be exact.
4. **Streams.** Each PMT elementary entry becomes a `Stream` with `id = elementary_PID`.
   `stream_type` → `CodecId` through 13818-1 Table 2-34, **refined by descriptors**:
   - registration descriptor (tag 0x05) format identifier: `AC-3`, `EAC3`, `HEVC`, `VC-1`, `AV01`,
     `Opus`, `KLVA`, `ID3 `, `CUEI`, `HDMV`.
   - ISO 639 language descriptor (0x0A) → stream `language` tag and audio type
     (0=undefined, 1=clean effects, 2=hearing impaired, 3=visual impaired) → `Disposition`.
   - DVB AC-3 (0x6A) / enhanced AC-3 (0x7A) / DTS (0x7B) / AAC (0x7C) descriptors.
   - teletext (0x56) and subtitling (0x59) descriptors: **one descriptor can declare several
     logical subtitle streams on one PID**, each with its own language and page number. This is why
     a single teletext PID can produce five subtitle streams.
   - ATSC AC-3 audio descriptor (0x81) in the ATSC private range.
   - HEVC video descriptor (0x38), AVC video descriptor (0x28), DVB extension descriptors.
5. **PES reassembly.** Per PID, accumulate payload from a PUSI packet until either
   `PES_packet_length` bytes are collected or the next PUSI arrives on that PID. For video with
   `PES_packet_length == 0` (legal and universal), only the next PUSI terminates the packet — which
   means a video packet is always one PES-header-to-next-PES-header span, and a truncated stream's
   last packet is emitted at EOF. `max_packet_size` bounds the accumulation so a corrupt stream
   cannot allocate without limit.
6. **Timestamps.** PES PTS/DTS are 33-bit at 90 kHz → `time_base = 1/90000`,
   `pts_wrap_bits = 33`. §1.7 R7–R10 own the wrap handling, with wrap state on the `Program`.
7. **Continuity.** The 4-bit `continuity_counter` increments per packet with payload on a PID. A gap
   means loss; the affected PES packet is flagged `CORRUPT` and, under
   `err_detect` including `explode`, becomes an error. `discontinuity_indicator` in the adaptation
   field means a *legitimate* discontinuity (a splice, a new recording): reset the CC expectation
   and, per §1.7 R22, pass the timestamp jump through untouched because MPEG-TS declares
   `FormatFlags::TS_DISCONT`.
8. **`start_time`** = the smallest PTS across streams after wrap correction (§1.7 R12).
   `compute_pcr` derives packet positions from the PCR instead, for CBR streams.
9. **Duration** is `FromPts` (§1.7 R15) — there is no container duration field. The tail read is the
   only way, and it is why `duration_probesize` matters most for TS.
10. **Seek** is §1.8 S5 binary search using `read_timestamp`, which scans forward from a byte
    position for the next PUSI packet on the target PID and reads its PTS. Then S6-style resync to
    the next sync byte. `seek2any` allows landing on a non-RAI packet.
11. **PMT version changes.** A new `version_number` on a PMT means the program's composition
    changed. Options: `merge_pmt_versions` (map the new PIDs onto the existing streams where the
    stream type matches), `skip_changes` (ignore the new PMT entirely), `skip_clear` (ignore
    CA-descriptor changes). Default behaviour creates *new* streams, which is why a long recording
    of a channel that re-multiplexes can end up with a dozen streams. Reproduce exactly; this is
    high on the list of things users notice.
12. **Scrambling.** `transport_scrambling_control != 0` means CA-scrambled. We do not descramble
    (there is no legal or clean way). Packets are flagged and, if every packet on a PID is
    scrambled, the stream is still reported — which is what a user needs to see.
13. **`mpegtsraw`** exposes the raw 188-byte packets as a single data stream, for PID-level analysis
    and for `rtp_mpegts` passthrough. It shares the packet layer and none of the PES layer.
14. **M2TS.** 192-byte packets with a 4-byte prefix carrying a 30-bit arrival timestamp at 27 MHz.
    Detected in the probe. The arrival timestamp is exposed as packet side data and used for nothing
    else.

### 3.3.4 Mux stages

1. PID assignment: `mpegts_start_pid` (default 0x0100) for elementary streams,
   `mpegts_pmt_start_pid` (default 0x1000) for PMTs. `mpegts_transport_stream_id`,
   `mpegts_original_network_id`, `mpegts_service_id`, `mpegts_service_type`.
2. Periodic tables: PAT every `pat_period`, SDT every `sdt_period`, NIT (if `nit`) every
   `nit_period`, PMT with the PAT. `pat_pmt_at_frames` forces emission at each keyframe instead of
   on a timer. `tables_version` sets the initial version number, `resend_headers` forces a rewrite.
3. PCR on `mpegts_pcr_pid` (default: the first video PID) every `pcr_period` (default 20 ms), or
   more often if `muxrate` requires it.
4. `muxrate` non-zero = CBR: null packets (PID 0x1FFF) pad to rate, and PCR values are computed
   from byte position. `muxrate` zero = VBR: PCR is derived from timestamps. **CBR is the mode where
   byte-identical output is easiest to achieve and hardest to get right**, because every byte
   position feeds back into the PCR value.
5. PES packetisation: `pes_payload_size` (default 2930, chosen so a PES packet plus header fills an
   integral number of TS packets), `omit_video_pes_length` (write 0 for video), `latm` (AAC in LATM
   rather than ADTS), `initial_discontinuity`, `omit_rai`, `mpegts_copyts`, `system_b`,
   `mpegts_m2ts_mode`.
6. Auto-BSF: length-prefixed H.264/HEVC/VVC → Annex B; raw AAC ASC → ADTS or LATM (§1.10 B5).
7. **Determinism.** Every source of nondeterminism in TS muxing is already an option (PID
   assignment, all the periods, `muxrate`, `tables_version`), so pinning them makes output a pure
   function of input. Correctness §1.2 C0 already names "MPEG-TS with pinned PCR/PAT/PMT settings"
   as an exact-bytes case; this section is what makes that claim true.

### 3.3.5 The awkward parts

| Problem | Handling |
|---|---|
| `PES_packet_length == 0` for video | Terminate on next PUSI; bound by `max_packet_size`. |
| 33-bit PTS wrap every ~26.5 h | §1.7 R7–R10, with the seek interaction (R10) as the hard case. |
| Sections spanning TS packets, and `pointer_field` | A per-PID section assembler with a 4096-byte bound (`section_length` is 12 bits). |
| A section whose `section_length` exceeds what is available | Held until more arrives; discarded on CC discontinuity. |
| PID reuse after a PMT version change | §3.3.3 item 11. |
| Streams declared in the PMT but carrying no packets | Reported with no packets; `nb_frames` unset. This is correct and users find it surprising. |
| Programs sharing an elementary PID | A `Stream` appears in several `Program::streams` lists. The model allows it; the CLI's `p:` stream specifier depends on it. |
| One teletext PID carrying several logical subtitle streams | Descriptor-driven stream splitting (§3.3.3 item 4). |
| Teletext PTS that lags the video by a fixed offset | `fix_teletext_pts` option. |
| Null packets and stuffing | Dropped before the PES layer. |
| CRC-32 failures | Governed by `err_detect`; default is to warn and drop the section. |
| Files starting mid-packet | `resync_size`-bounded scan for the sync pattern. |
| 204-byte packets with Reed-Solomon parity | Stride detected; the parity bytes are ignored (we do not FEC-correct). |
| AV1 in TS | Very rare; the AOM binding exists and the `AV01` registration descriptor identifies it. |
| SCTE-35 splice information | Exposed as a data stream, not interpreted. |

### 3.3.6 Effort

| Package | pw |
|---|---:|
| `vaco-format-mpegts-tables` (packet, adaptation field, section framing, CRC, PSI structures, stream_type and descriptor tables) | 3 |
| Demux: PSI, programs, streams, descriptors | 3 |
| Demux: PES reassembly, timestamps, wrap, continuity, discontinuity | 3 |
| Demux: seek, duration estimation, PMT-version options, m2ts, `mpegtsraw` | 3 |
| **Demux total** | **12** (incl. tables) |
| Mux: PID assignment, PSI generation, PES packetisation, PCR, CBR/VBR, all 24 options | 7 |

---

## 3.4 Second tier

Progressively less detail. Each is a `vaco-demux-<name>` / `vaco-mux-<name>` crate pair.

### 3.4.1 MPEG-PS — `mpegps` demux, `mpeg1system`/`mpeg1vcd`/`mpeg2dvd`/`mpeg2svcd`/`mpeg2vob` mux

**Specs**: ISO/IEC 11172-1 (MPEG-1 systems), ISO/IEC 13818-1 §2.5 (program stream). Pack headers
(`0x000001BA`), system headers (`0x000001BB`), PES packets with the same PTS/DTS encoding as TS,
program stream map and directory. SCR instead of PCR. Private stream 1 (`0xBD`) carries AC-3, DTS,
LPCM and DVD subtitles with a one-byte sub-stream id — that sub-stream demultiplexing is the only
genuinely fiddly part, and it is where `vobsub` comes from. Seeking is byte-position binary search
on SCR. Shares timestamp code with TS through `vaco-format-mpeg-common`. **Demux 4 pw, mux 5 pw**
(the mux family's VCD/SVCD/DVD profiles have rigid multiplexing constraints — fixed pack sizes,
mandated padding, muxrate — which is most of the work).

### 3.4.2 AVI — `vaco-demux-avi`, `vaco-mux-avi`

**Specs**: Microsoft RIFF/AVI (published as part of the Multimedia Programming Interface and Data
Specifications, and in the DirectShow documentation), plus the OpenDML AVI 2.0 extension
(published by the OpenDML committee) for files over 2 GiB.

`RIFF/AVI ` → `LIST/hdrl` → `avih` (main header: microseconds per frame, total frames, streams) →
per stream `LIST/strl` → `strh` (fourcc type `vids`/`auds`/`txts`/`mids`, handler, dwScale/dwRate →
time base, dwStart, dwLength, dwSampleSize) + `strf` (`BITMAPINFOHEADER` or `WAVEFORMATEX` — hence
`vaco-format-riff`) + optional `strd`, `strn`, `indx` (OpenDML super index) → `LIST/movi` with
`##db`/`##dc`/`##wb`/`##tx` chunks → `idx1` (legacy index).

Awkward parts: OpenDML `indx`/`ix##` two-level indexes and `RIFF/AVIX` continuation chunks;
`dwSampleSize != 0` audio where one chunk is many samples and timestamps come from a running sample
count, not from the chunk index; interleaved-but-lying `idx1` entries whose offsets are relative to
either the file or the `movi` chunk (both conventions exist in the wild and the disambiguation is by
probing the first entry); non-interleaved files where the whole audio stream follows the whole video
stream; `strh.dwStart` as a per-stream delay; VBR MP3 in AVI where `dwSampleSize` lies.
**Demux 4 pw, mux 2 pw.**

### 3.4.3 ASF / WMV — `vaco-demux-asf`, `vaco-mux-asf`

**Specs**: Microsoft published the *Advanced Systems Format (ASF) Specification* (Revision 01.20.06)
publicly; it is also covered by [MS-ASF] under the Open Specification Promise. So ASF is
spec-implementable despite research §2.1's "de facto" note.

GUID-keyed object tree: Header Object → File Properties, Stream Properties (per stream, with a
type-specific block that is `BITMAPINFOHEADER`/`WAVEFORMATEX` again), Header Extension, Codec List,
Extended Content Description, Content Description, Marker, Script Command, Stream Bitrate
Properties, Extended Stream Properties, Language List, Metadata; Data Object with fixed-size packets
carrying multiple payloads with optional fragmentation; Simple Index / Index Objects.

Awkward parts: the two demuxer implementations upstream (`asf` frame-based and `asf_o`
object-based) exist because the format's payload-parsing rules are ambiguous — we implement one,
correctly, and register both names against it unless a conformance case proves they differ (**VERIFY-A1**);
the unreliable header start time (§1.7 R13); DRM'd files (report, do not attempt); payload
fragmentation across packets with a per-payload media-object number; the `Preroll` field, which
offsets every timestamp. **Demux 5 pw, mux 2 pw.**

### 3.4.4 FLV — `vaco-demux-flv`, `vaco-mux-flv`

**Specs**: Adobe *Video File Format Specification v10.1* (published), plus the community-maintained
*Enhanced RTMP / E-RTMP* specification for the modern extensions (HEVC, AV1, VP9, Opus, FLAC and
multitrack in FLV — genuinely public, maintained by the Veovera Software Organization).

Header (9 bytes) → back-pointer-prefixed tags (type 8 audio, 9 video, 18 script). `onMetaData`
AMF0 script tag gives duration, dimensions, frame rate, and a `keyframes` array that doubles as an
index. Video tag header carries frame type and codec id; AVC/HEVC packets carry a
`AVCPacketType`/composition time offset. `live_flv` is the same parser without seeking; `kux` is a
Youku FLV variant.

Awkward parts: AMF0/AMF3 parsing (small but easy to get wrong on the ECMA array vs strict array
distinction); the `keyframes` index being routinely wrong; the enhanced-RTMP FourCC-based codec
signalling coexisting with the legacy 4-bit codec ids; timestamps as 24-bit + an 8-bit extension
byte in a strange order. **Demux 3 pw, mux 2 pw.**

### 3.4.5 Ogg — `vaco-demux-ogg`, `vaco-mux-ogg` (+ `oga`/`ogv`/`opus`/`spx` aliases)

**Specs**: RFC 3533 (Ogg encapsulation), RFC 7845 (Opus in Ogg), RFC 5334 (Ogg media types),
Xiph's Vorbis I specification, the Theora specification, RFC 9639 (FLAC-in-Ogg mapping),
Xiph *Ogg Skeleton*.

Pages (`OggS`, version, header type, granule position, serial number, page sequence, CRC-32 with
Ogg's own polynomial, segment table) → packets (assembled from lacing values, continued across
pages). Each logical stream has its own serial number; chained and multiplexed streams both occur.

The genuinely hard part is **granule position → timestamp**, which is *codec-specific*: Vorbis
counts samples with a pre-roll convention on the first page; Theora packs a keyframe number and an
offset in bit fields whose split comes from the setup header; Opus counts 48 kHz samples with
`pre_skip` from `OpusHead`; FLAC counts samples; Speex counts samples. So the Ogg demuxer needs a
per-codec granule interpreter table — the one place where a container needs codec knowledge, and it
is resolved the same way as parsers (§1.0): a `GranuleInterpreter` trait implemented per mapping
inside the Ogg crate itself, since the logic is container-side, not decode-side.

Awkward parts: chained streams (a new `OggS` with a new serial number mid-file means a whole new set
of streams — upstream reports this as a stream change; we must match); page-spanning packets with
continuation flags; the end-of-stream granule being a *negative* trim for Opus/Vorbis;
`ogg` vs `oga` vs `ogv` vs `opus` vs `spx` mux aliases differing only in extension and default codec.
**Demux 5 pw, mux 3 pw.**

### 3.4.6 WAV / W64 / AIFF / CAF / AU / VOC — one crate, `vaco-format-audio-simple`

**Specs**: Microsoft RIFF/WAVE (published, plus EBU Tech 3285 for BWF and ITU-R BS.2088 for RF64),
Sony *Wave64* specification, Apple *Audio Interchange File Format* v1.3 and AIFF-C, Apple
*Core Audio Format Specification*, Sun/NeXT `.au` (de facto, documented in the audiofile literature),
Creative *Voice File* format (de facto, documented).

These share so much structure that one crate carrying six demuxers and six muxers is the right
factoring. WAV needs: `fmt ` (including `WAVEFORMATEXTENSIBLE` with the channel mask and sub-format
GUID), `data`, `fact`, `LIST/INFO` metadata, `cue `/`adtl` markers → chapters, `bext` (Broadcast
Wave), `ds64` (RF64 for >4 GiB), `iXML`, ID3 chunks, and the "data size 0 or 0xFFFFFFFF means read to
EOF" streaming convention. AIFF needs `COMM` (with the 80-bit IEEE 754 extended sample rate — a
genuinely fiddly conversion), `SSND` with offset and block size, AIFF-C compression types, `ID3 `,
and `MARK`/`INST`. CAF needs `desc`, `pakt` (variable packet table), `chan`, `free`, `data` with an
edit count. **Combined: demux 4 pw, mux 3 pw.**

### 3.4.7 MXF — `vaco-demux-mxf`, `vaco-mux-mxf`

**Specs**: SMPTE ST 377-1 (MXF file format), ST 379-2 (generic container), ST 381 (MPEG in MXF),
ST 382 (AES3/BWF in MXF), ST 384 (uncompressed picture), ST 386 (D-10), ST 422 (JPEG 2000),
RP 210 (metadata dictionary), ST 2067 (IMF), plus the operational pattern specs (ST 378 OP1a,
ST 391 OP-Atom).

KLV (Key-Length-Value) throughout, with 16-byte SMPTE universal labels. Partitions (header, body,
footer) each with a partition pack; header metadata as a set of KLV-coded structural-metadata sets
(Preface → ContentStorage → Package (Material/Source) → Track → Sequence → StructuralComponent
(SourceClip/Timecode) → Descriptor (FileDescriptor subclasses)); essence containers as either
frame-wrapped or clip-wrapped KLV elements; the Index Table Segment for random access; Random Index
Pack in the footer.

MXF is the third-largest format in libavformat (~4.4k demux + 3.8k mux) and the reason is that
its metadata model is a full object graph with references, not a tree. Our approach: parse the
structural metadata into an arena with `InstanceUID`-keyed resolution, then walk it. The essence
descriptors map to `CodecParameters` through a label table derived from ST 381/382/384/386/422 and
RP 210 — spec-derived, per D9.

Awkward parts: OP-Atom (one essence per file, with the picture and each audio channel in separate
files — the demuxer must be told about the others, or discover them by filename convention);
clip-wrapped essence (the whole track is one enormous KLV value, so packet boundaries come from the
index table, not from KLV); D-10 (SMPTE 386) constant-bitrate MPEG with fixed frame sizes;
partial/growing files with no footer; the "essence container label says one thing, the descriptor
says another" cases; timecode tracks and the `start_timecode` metadata everyone in broadcast cares
about. **Demux 14 pw, mux 12 pw. T2, not v0.1.**

### 3.4.8 image2 and the pipe family — `vaco-demux-image2`, `vaco-mux-image2`

Not a container: a pseudo-format that maps a filename pattern (`%03d`, `%d`, glob, or a single file)
onto a stream of one-image-per-packet. Options: `start_number`, `start_number_range`, `pattern_type`
(`sequence`/`glob`/`glob_sequence`/`none`), `frame_size`, `ts_from_file`, `export_path_metadata`,
`framerate`, `loop`, `video_size`, `pixel_format`.

The ~42 `image_<codec>_pipe` demuxers are one generic implementation parameterised by a per-codec
**frame splitter** (find where one image ends and the next begins in a concatenated byte stream) —
JPEG by SOI/EOI, PNG by signature + IEND, GIF by header + trailer, and so on. That splitter table is
the whole of the work. Registration is one entry per enabled image codec.

Awkward parts: `%d` expansion and the `-start_number` interaction with missing files (upstream stops
at the first gap unless `start_number_range` allows a search); glob ordering (must be the platform's
sort, not the shell's); the `ts_from_file` mode which reads file mtimes and is therefore
non-deterministic and excluded from the conformance corpus.
**Demux 3 pw, mux 1 pw.**

### 3.4.9 Raw / elementary streams — `vaco-demux-raw`, `vaco-mux-raw`

48 demuxers and 40 muxers from two macro families plus a per-codec set.

- **PCM family** (21 demux / 17 mux registrations): one generic implementation parameterised by
  sample format, with `sample_rate`, `channels` and `ch_layout` options. Timestamps come from a
  running sample count. **1 pw total.**
- **Raw video family** (`rawvideo`, `bitpacked`, `v210`, `v210x`, `yuv4mpegpipe`): frame size from
  `video_size` + `pixel_format`, or from the Y4M header. **1.5 pw.**
- **Bitstream families** (`h264`, `hevc`, `vvc`, `evc`, `av1`/`obu`, `m4v`, `mpegvideo`, `h261`,
  `h263`, `cavsvideo`, `avs2`, `avs3`, `dirac`, `dnxhd`, `vc1`, `mjpeg`, `mjpeg_2000`, `aac`,
  `ac3`, `eac3`, `dts`, `flac`, `mp3`, `loas`, `spdif`, `g72x`, `amr`, `gsm`): each is
  "find the sync pattern, hand whole access units to the parser, let the parser supply timestamps".
  A generic `RawDemuxer<S: SyncFinder>` plus one small `SyncFinder` per family covers all of them.
  The probe functions are the substance: each must count consecutive valid headers to score.
  **4 pw for the family.**
- `s337m`, `bit`, `data`. **0.5 pw.**

These matter more than their size suggests: **D5 needs the H.264/HEVC/AV1/AAC/Opus paths to exist as
parsers even though no raw-ES demuxer is in the v0.1 acceptance corpus**, and they are the cheapest
possible integration test for those parsers.

### 3.4.10 HLS — `vaco-demux-hls`, `vaco-mux-hls`

**Spec**: RFC 8216 (HTTP Live Streaming), plus the ongoing `draft-pantos-hls-rfc8216bis` for
low-latency and the newer tags, and Apple's *HLS Authoring Specification* for the profile rules.

Demux: fetch the playlist; if it is a master playlist, enumerate `#EXT-X-STREAM-INF` variants and
`#EXT-X-MEDIA` renditions and pick per `live_start_index`/user selection; fetch media playlists;
each segment is opened as a nested demuxer (MPEG-TS or fMP4, chosen by
`#EXT-X-MAP`/`hls_segment_type`); packets are re-timestamped onto a continuous timeline across
`#EXT-X-DISCONTINUITY`. Live playlists are reloaded (`max_reload`, `m3u8_hold_counters`). Key
handling: `#EXT-X-KEY` `METHOD=AES-128` (whole-segment AES-128-CBC with an IV from the tag or the
media sequence number) and `METHOD=SAMPLE-AES` (Apple's per-sample scheme; audio IVs arrive as
timed ID3 metadata, video is NAL-structure-aware). `allowed_extensions`/
`allowed_segment_extensions`/`extension_picky` plus the protocol whitelist (§2.3.2 W3) are the
security surface, and it is the most attacked surface in the whole subsystem.

Mux: segment the output, write media playlists, optionally a master playlist
(`var_stream_map`/`cc_stream_map`), handle `hls_flags`' 15 distinct constants, `hls_playlist_type`,
`strftime` naming, and the encryption options. Reuses the `segment` muxer's machinery.

**Demux 8 pw, mux 8 pw. T1 but v0.4+, because it needs the HTTP protocol and both segment formats.**

### 3.4.11 DASH — `vaco-demux-dash`, `vaco-mux-dash`

**Spec**: ISO/IEC 23009-1 (MPEG-DASH), plus DVB-DASH (ETSI TS 103 285) and the DASH-IF
Interoperability Points for the profile rules.

Demux: parse the MPD XML with `quick-xml` — `Period` → `AdaptationSet` → `Representation`, with
`SegmentTemplate` (with `$Number$`/`$Time$`/`$Bandwidth$`/`$RepresentationID$` substitution and
`SegmentTimeline`), `SegmentList`, or `SegmentBase` (`sidx`-indexed byte ranges for the on-demand
profile). Static and dynamic (live) MPDs; `availabilityStartTime` and wall-clock-driven segment
selection for live, which is inherently non-deterministic and is excluded from the byte-exact
corpus. `ContentProtection` with CENC `default_KID` and `pssh`; `cenc_decryption_key(s)`.

Mux: the inverse, plus `adaptation_sets` grouping, `use_template`/`use_timeline`, `single_file`
with `sidx`, `ldash`/`lhls` low-latency modes, and an optional HLS master playlist alongside.

**Demux 8 pw, mux 8 pw. T1, v0.5.**

### 3.4.12 RTP / RTSP / SDP — `vaco-demux-rtp`, `vaco-demux-rtsp`, `vaco-mux-rtp`, `vaco-mux-rtsp`

**Specs**: RFC 3550 (RTP/RTCP), RFC 3551 (audio/video profile), RFC 2326 and RFC 7826 (RTSP 1.0/2.0),
RFC 8866 (SDP), RFC 4566 (SDP offer/answer), plus one payload-format RFC per handler.

Three layers, cleanly separable:

1. **Transport** — the `rtp`/`srtp` protocols (§2.4 P2), plus RTSP's interleaved-over-TCP mode
   (`$` framing) and its HTTP tunnelling mode.
2. **Session** — SDP parsing (`m=`, `a=rtpmap`, `a=fmtp`, `a=control`, `a=range`, `b=`,
   `a=recvonly`), RTSP's DESCRIBE/SETUP/PLAY/PAUSE/TEARDOWN/OPTIONS/GET_PARAMETER state machine,
   Digest and Basic authentication, transport negotiation (`rtsp_transport` ∈ udp / tcp /
   udp_multicast / http / https), keepalive, and the reorder queue (`reorder_queue_size`).
3. **Payload depacketisation** — 28 handlers, dispatched by static payload type or by
   `a=rtpmap` encoding name. Each is small and independently testable:

| Handler group | RFCs | Handlers |
|---|---|---|
| H.26x | RFC 6184 (H.264), RFC 7798 (HEVC), RFC 4629 (H.263-2000), RFC 2429 (H.263-1998), RFC 2190 (H.263 legacy), RFC 4587 (H.261) | `h264`, `hevc`, `h263_1998`, `h263_2000`, `h263_rfc2190`, `h261` |
| MPEG | RFC 2250 (MPEG-1/2 A/V and TS), RFC 3640 (`mpeg4-generic`), RFC 6416 (MP4V-ES, MP4A-LATM), RFC 5219 (robust MPEG audio) | `mpeg_video`, `mpeg_audio`, `mpeg_audio_robust`, `mpegts`, `mpeg4_generic`, `mp4v_es`, `mp4a_latm` |
| Open codecs | RFC 7587 (Opus), RFC 5215 (Vorbis), RFC 5215 (Theora), AOM RTP AV1 spec, RFC 7741 (VP8), draft-ietf-payload-vp9 | `opus`, `vorbis`, `theora`, `av1`, `vp8`, `vp9` |
| Speech | RFC 4867 (AMR-NB/WB), RFC 3952 (iLBC), RFC 3558 (QCELP), RFC 3551 (G.726) | `amr_nb`, `amr_wb`, `ilbc`, `qcelp`, `g726_16/24/32/40`, `g726le_*` |
| Other | RFC 2435 (JPEG), RFC 4175 (uncompressed), VC-2 HQ RTP spec, RFC 6469 (DV), ATSC A/52 RTP (AC-3) | `jpeg`, `vc2hq`, `dv`, `ac3` |
| Proprietary | — | `qdm2`, `svq3` — **reverse-engineered, no public spec: T4, not implemented** |

The packetiser (mux) side mirrors it, one per payload family, plus `rtpenc_chain` for the generic
fallback and `rtp_mpegts` for TS-in-RTP.

**RDT** (RealNetworks Data Transport) is a separate, undocumented transport used by legacy
RealMedia streaming: **T4, not implemented.**

**Total: transport+session 8 pw, 26 implementable depacketisers 8 pw, packetisers 6 pw = 22 pw.
T2, v0.6+.**

### 3.4.13 The meta-formats — `concat`, `ffmetadata`, `segment`, `tee`, `fifo`

Not containers; they are compositional operators over other formats, and they share one crate,
`vaco-format-meta`.

- **`concat` demuxer** reads an `ffconcat` list file (`file`, `duration`, `inpoint`, `outpoint`,
  `file_packet_metadata`, `stream`, `exact_stream_id`, `option`) and opens each entry as a nested
  demuxer, splicing timestamps. `safe` (default 1) rejects absolute paths and `..` — a security
  control, not a convenience. `segment_time_metadata`, `auto_convert`.
- **`ffmetadata`** demux and mux: FFmpeg's own INI-like metadata sidecar. Trivial, and the format
  is defined by its own documentation, which is a public interface fact.
- **`segment`/`stream_segment`** wrap a real muxer and cut it at boundaries determined by
  `segment_time`, `segment_times`, `segment_frames`, `segment_atclocktime` (excluded from the
  deterministic corpus), or keyframes; write a list file in one of six formats.
- **`tee`** fans one packet stream out to several muxers with per-output `select`, `onfail`,
  `use_fifo`, `fifo_options` and `bsfs` directives, parsed from its pseudo-URL syntax.
- **`fifo`** wraps a muxer in a thread with a bounded queue so a slow network sink cannot stall the
  encoder; options for recovery, timeshift and overflow policy.

**6 pw combined.** `segment` and `tee` are prerequisites for HLS and DASH mux.

### 3.4.14 The rest of the second tier, briefly

| Format | Spec | Tier | pw (dem/mux) |
|---|---|---|---|
| NUT | NUT open container specification (published by the NUT project) | T1 | 3 / 2 |
| DV | SMPTE 314M, IEC 61834 | T1 | 2 / 1 |
| GXF | SMPTE 360M | T2 | 3 / 3 |
| IMF | SMPTE ST 2067 (composition playlist over MXF) | T2 | 4 / — |
| SWF | Adobe *SWF File Format Specification* v19 (published) | T2 | 2 / 2 |
| mpjpeg | RFC 2046 `multipart/x-mixed-replace` | T1 | 0.5 / 0.5 |
| IAMF | AOM *Immersive Audio Model and Formats* | T2 | 4 / 3 |
| SPDIF / s337m | IEC 61937, SMPTE ST 337 | T2 | 1.5 / 1 |
| Subtitle text formats (srt, ass, webvtt, microdvd, mpl2, subviewer, vplayer, jacosub, pjs, sami, realtext, lrc, aqtitle, mpsub, stl) | Mostly community-documented; WebVTT is a W3C spec, STL is EBU Tech 3264, SCC is CEA-608 | T1/T2 | 6 / 4 |
| Subtitle bitmap formats (dvbsub, dvbtxt, sup/PGS, vobsub) | EN 300 743, EN 300 472; PGS is thoroughly publicly documented despite being reverse-engineered originally | T2 | 4 / 2 |
| Utility muxers (crc, framecrc, framemd5, framehash, hash, md5, streamhash, uncodedframecrc, null, mkvtimestamp_v2) | FFmpeg-defined; their output format is an interface fact | **T1 and needed early** — correctness §1.2's C3/C4 comparison modes are defined in terms of them | 2 |
| APNG, GIF, ICO muxers | APNG spec, GIF89a, MS ICO | T2 | 2 |

## 3.5 The long tail

Everything not named above. §4 gives the counts; the policy is plan 15 §3.5's, applied to formats:

1. **Omit (the default).** `vaco` names the format, says there is no demuxer, and points at
   `docs/why-some-formats-are-not-included.md`. This is the honest consequence of D7, not a failure.
2. **Two-team clean room** for the handful with real user demand. Ordered by value:
   RealMedia (`rm`/`rmvb` — an enormous installed base of archived content), Monkey's Audio (`ape`),
   WavPack (`wv` — actually has a published format description at wavpack.com, so it may be T3 not
   T4; **VERIFY-L1**), TrueHD/MLP framing, Musepack, TAK, Bink, Smacker, Windows TV Recording
   (`wtv`), LXF. Budget 2.5× normal effort per plan 15 §3.5. **Post-v1.0.**
3. **Independent reverse engineering from samples**, producing a *published* specification document.
   Collapses into (2) in practice, but the published spec is a public good and strengthens the
   clean-room evidence trail.
4. **Out-of-process delegation** — the same escape hatch plan 15 §3.5 item 4 builds for codecs. If a
   user has a tool that reads a format, `vaco` can pipe through it. The mechanism is already being
   built for codecs (`vaco-codec-exec`); extending it to formats is ~1 pw.

---

# 4. Tiering across all 368 demuxers and 186 muxers

## 4.1 Definitions

| Tier | Meaning | Build presence |
|---|---|---|
| **T1** | Default build. Formats people actually encounter, all spec-backed. | `default` |
| **T2** | Useful, spec-backed, not on the critical path. | `default` for most; `full-rf` for the bulky ones (MXF, IMF, IAMF) |
| **T3** | Long tail with *some* public documentation — de-facto formats, community-documented text formats, vendor specifications of limited interest. | `full-rf`, opt-in `format-<name>` |
| **T4** | **Cannot be done cleanly.** No public specification exists; FFmpeg's source *is* the specification. | Not implemented. Named and refused, with a documented reason. |

T4 is exactly plan 15 §3.5's T5 applied to containers, and the same four responses apply (§3.5
above).

## 4.2 Demuxers — 368

| Research section | Total | T1 | T2 | T3 | T4 |
|---|---:|---:|---:|---:|---:|
| §2.1 General-purpose containers | 23 | 19 | 1 | 0 | 3 |
| §2.2 Broadcast / professional | 7 | 2 | 2 | 0 | 3 |
| §2.3 Audio containers | 91 | 20 | 13 | 4 | 54 |
| §2.4 Image / image2 | 46 | 2 | 42 | 0 | 2 |
| §2.5 Subtitle | 23 | 7 | 6 | 8 | 2 |
| §2.6 Game / FMV / legacy | 123 | 0 | 1 | 1 | 121 |
| §2.7 Scripting frontends | 5 | 0 | 0 | 0 | 5 |
| §2.8 Raw / elementary streams | 48 | 44 | 2 | 0 | 2 |
| §2.9 Playlist / concat | 2 | 2 | 0 | 0 | 0 |
| **Total** | **368** | **96** | **67** | **13** | **192** |

**192 of 368 demuxers — 52% — cannot be clean-roomed.** That number is the honest headline of this
section and it should be stated plainly in user-facing documentation rather than discovered by users
one file at a time. It matches plan 15's finding for codecs (~half the decoder inventory is T5)
because it describes the same phenomenon: FFmpeg accumulated these formats over 25 years by people
reverse-engineering samples, and the artefact of that work is the code, not a document.

Composition of the 192: 121 game/FMV/legacy containers; 54 audio formats (Monkey's Audio, Musepack,
TAK, Shorten, ATRAC/OpenMG, aptX, TrueHD/MLP framing, the console-audio family — BRSTM, BFSTM, FSB,
HCA, MSF, XVAG, VAG, SVAG, MCA, GENH — and two dozen others); 5 scripting frontends (which are
external-library bindings and fail D10 Gate 1 independently); 3 general-purpose (RealMedia, IVR,
SAP); 3 broadcast (LXF, WTV, DVD-Video); 2 image (`image2_alias_pix`, `image2_brender_pix`); 2 raw
(`bit`, `v210x`); 2 subtitle (MCC, TED captions).

Two of these deserve a second look at implementation time and are marked **VERIFY-L1**: WavPack
publishes a format description at wavpack.com, and TTA has an open specification — both are listed
as "reverse-engineered" in research §2.3 but may in fact be T3.

## 4.3 Muxers — 186

| Research section | Total | T1 | T2 | T3 | T4 |
|---|---:|---:|---:|---:|---:|
| §3.1 General-purpose | 12 | 9 | 1 | 0 | 2 |
| §3.2 Streaming / segmented / ABR | 22 | 15 | 5 | 2 | 0 |
| §3.3 Broadcast / professional | 4 | 2 | 1 | 0 | 1 |
| §3.4 Audio containers / raw audio | 41 | 20 | 9 | 4 | 8 |
| §3.5 Image / image2 | 5 | 2 | 3 | 0 | 0 |
| §3.6 Subtitle | 9 | 5 | 3 | 1 | 0 |
| §3.7 Raw / elementary streams | 40 | 38 | 1 | 1 | 0 |
| §3.8 Utility / integrity / test | 12 | 11 | 0 | 0 | 1 |
| §3.9 Game / legacy / misc | 41 | 12 | 5 | 4 | 20 |
| **Total** | **186** | **114** | **28** | **12** | **32** |

**Only 32 of 186 muxers — 17% — are unreachable.** The asymmetry with demuxers is real and worth
understanding: nobody needs to *write* a Bink file, so upstream never wrote a Bink muxer. The muxer
list is dominated by raw elementary streams and PCM (78 of 186 between §3.4 and §3.7), which are
trivial, and by the MP4/Matroska/TS/HLS/DASH family, which is where all the work is.

## 4.4 Crate decomposition

~40 format crates, following architecture §3's "one crate per container family" and grouping the
long tail rather than exploding it.

| Crate | Registrations | Tier |
|---|---:|---|
| `vaco-demux-mp4` / `vaco-mux-mp4` | 1 / 9 (mov, mp4, ipod, ismv, f4v, psp, 3gp, 3g2, avif) | T1 |
| `vaco-demux-matroska` / `vaco-mux-matroska` | 2 / 4 (matroska, webm, matroska_audio, webm_chunk) | T1 |
| `vaco-demux-mpegts` / `vaco-mux-mpegts` | 2 / 1 | T1 |
| `vaco-demux-mpegps` / `vaco-mux-mpegps` | 2 / 5 | T1 |
| `vaco-demux-avi` / `vaco-mux-avi` | 1 / 1 | T1 |
| `vaco-demux-asf` / `vaco-mux-asf` | 2 / 2 | T1 |
| `vaco-demux-flv` / `vaco-mux-flv` | 3 / 1 | T1 |
| `vaco-demux-ogg` / `vaco-mux-ogg` | 1 / 5 | T1 |
| `vaco-format-audio-simple` (wav, w64, aiff, caf, au, voc, sox, ircam, rso) | 9 / 9 | T1/T2 |
| `vaco-demux-raw` / `vaco-mux-raw` (bitstream + PCM + rawvideo families) | 48 / 40 | T1 |
| `vaco-demux-image2` / `vaco-mux-image2` (incl. the 42 pipes) | 44 / 2 | T1/T2 |
| `vaco-format-subtitle-text` | 15 / 6 | T1/T2 |
| `vaco-format-subtitle-bitmap` | 4 / 2 | T2 |
| `vaco-demux-hls` / `vaco-mux-hls` | 1 / 1 | T1 |
| `vaco-demux-dash` / `vaco-mux-dash` | 2 / 3 | T1 |
| `vaco-format-rtp` (rtp, rtsp, sdp, sap-read, 26 depacketisers, packetisers) | 3 / 3 | T2 |
| `vaco-format-meta` (concat, ffmetadata, segment, stream_segment, tee, fifo) | 2 / 5 | T1 |
| `vaco-mux-utility` (crc, framecrc, framemd5, framehash, hash, md5, streamhash, uncodedframecrc, null, mkvtimestamp_v2) | 0 / 10 | **T1, early** |
| `vaco-demux-mxf` / `vaco-mux-mxf` | 1 / 3 | T2 |
| `vaco-format-nut` | 1 / 1 | T1 |
| `vaco-format-dv` | 1 / 1 | T1 |
| `vaco-format-gxf`, `-imf` | 2 / 1 | T2 |
| `vaco-format-iamf` | 1 / 1 | T2 |
| `vaco-format-spdif` | 2 / 1 | T2 |
| `vaco-format-swf` | 2 / 2 | T2 |
| `vaco-format-misc-audio` (T3 audio containers) | ~13 / ~9 | T3 |
| `vaco-format-misc` (T3 remainder) | ~10 / ~7 | T3 |
| `vaco-mux-smoothstreaming`, `-whip`, `-hds` | 0 / 3 | T2/T3 |
| `vaco-format-mpjpeg` | 1 / 1 | T1 |

Plus the eleven shared-helper crates in §6.

---

# 5. Build or buy, per D10 and D11

## 5.1 The structural argument, first

Plan 15 §4A concluded that wrapping a good codec crate is often right. **For containers the same
reasoning produces the opposite answer**, and it is worth stating why before looking at individual
crates.

A codec crate is measured against a bitstream specification that pins the output exactly. If
`claxon` decodes FLAC correctly, it produces the same samples FFmpeg produces, because there is only
one correct answer. The specification is the shared oracle.

A container has no such oracle for the things `vaco-probe` prints. ISO/IEC 14496-12 does not say
what `start_time` is when the edit list is empty and `ctts` is version 0. RFC 9559 does not say how
to derive `r_frame_rate` from a millisecond time base. Those are *interpretations*, and FFmpeg's
interpretation is the target. An external crate that parses the file perfectly correctly will still
disagree with us on a dozen reported fields, because it was never trying to agree.

So the D11 grade prediction for any third-party demuxer is **Divergent by construction**, and
"Divergent blocks the crate from the default build and schedules a native implementation". The
efficient move is to skip the intermediate step.

**The corollary is the good news.** Because muxing is deterministic — a pure function of packets,
options and the bitexact flag — **containers are the one subsystem where D6's "byte-identical"
requirement is fully achievable and fully testable**. Correctness §1.2's C0 mode already names
deterministic remuxes as exact-byte cases. Every divergence here is a bug with a specific cause, not
a tolerance to be negotiated. That makes this plan's acceptance criteria unusually sharp, and it is
why §7's v0.1 gate can be "zero unexplained byte diffs" rather than "within tolerance".

## 5.2 Container and format crates assessed

| Crate | Licence | Gate 1 | Gate 2 | Gate 3 | Model fit | Predicted D11 grade | Verdict |
|---|---|---|---|---|---|---|---|
| `matroska` (tuffy) — the one D10 names | MIT OR Apache-2.0 | Pass | Pass | **Weak** — small, low adoption, sporadic releases | **Poor.** It reads Info/Tracks/Chapters metadata; it does not produce packets, does not handle lacing, content encodings, `DiscardPadding`/`CodecDelay`, cue-based seeking, or unknown-size clusters, and owns its own I/O | **Divergent** — it cannot express most of what we need, so the grade is not even measurable | **Do not adopt.** Keep as a **dev-dependency oracle**: a test that cross-checks our track metadata against it on the corpus is cheap and catches whole classes of parse error |
| `matroska-demuxer` | MIT | Pass | Pass | Weak | Better than `matroska` — it does produce frames — but no content encodings, no lacing edge cases, no attachment/tag model | **Divergent** | Do not adopt; oracle only |
| `ebml-iterable` / `ebml` | MIT | Pass | Pass | Weak | The EBML layer is ~400 lines and we need exact control over K2 (unknown-size termination) and K5 (recovery), which is precisely what a generic EBML crate abstracts away | n/a | **Build** |
| `mp4` (alfg) / `mp4-rust` | MIT | Pass | Pass | Weak | No fragmented seek, no edit lists, no CENC, no QuickTime atoms, eager sample-table materialisation | **Divergent** | Do not adopt |
| `mp4parse` (Mozilla) | **MPL-2.0** | Pass | **Fail** | Strong | Would have been the strongest candidate — it is the parser Firefox ships | — | **Excluded on licence (D3/D10 Gate 2)** |
| Symphonia `symphonia-format-*` | **MPL-2.0** | Pass | **Fail** | Strong | Covers ogg, mkv, mp4, wav, aiff, caf | — | **Excluded on licence.** Register §11 already calls this the single most consequential dependency decision, and it lands hardest here |
| `mpeg2ts-reader` (dholroyd) | MIT | Pass | Pass | **Pass** — production use, maintained | Genuinely good for the TS packet + PSI section layer. But it is a push parser and our resync/`err_detect`/PMT-version semantics permeate the layer | **Equivalent** for section parsing; **Divergent** for error recovery, which is exactly where `vaco-probe` output differs on real broadcast captures | **Build**, and use it as a **differential oracle** in the TS fuzz target — two independent parsers disagreeing on a mutated stream is a high-signal finding |
| `ogg` (est31) | BSD-3-Clause | Pass | Pass | Pass — stable, used by `lewton` | Good: the page layer is exactly what it does, and it does not own the codec-specific granule interpretation (which we must do anyway) | **Equivalent** | **Marginal adopt.** The Ogg page layer is ~300 lines; the value is mostly in its CRC and lacing edge cases being battle-tested. Decide at implementation time; either way it sits behind `vaco-demux-ogg` per D11 |
| `hound` (WAV) | Apache-2.0 | Pass | Pass | Pass | Poor: no `LIST/INFO`, no `bext`, no RF64/`ds64`, no `cue`/`adtl`, no ID3-in-RIFF, no `WAVEFORMATEXTENSIBLE` channel mask exposure | **Divergent** — half the fields `vaco-probe` prints are absent | Do not adopt |
| `id3` | MIT | Pass | Pass | **Pass** — mature, widely used | Good. Reads/writes v2.2/2.3/2.4 including unsynchronisation, APIC, CHAP/CTOC | **Equivalent** — the frame-ID → canonical-key mapping and the `id3v2_priv.` prefixing are ours regardless, layered on top | **Adopt** behind `vaco-format-id3`, with our own conversion table. Measure; if the grade comes back Divergent the native fallback is ~2 pw |
| `m3u8-rs` | MIT | Pass | Pass | Marginal | HLS playlist parsing. FFmpeg's tolerance of malformed playlists is idiosyncratic and security-relevant | **Divergent** likely | Do not adopt — the parser is small and the tolerance rules *are* the feature |
| `quick-xml` | MIT | Pass | Pass | **Pass** — very widely used, shallow | Excellent. A pull parser; all interpretation is ours | **Exact** | **Adopt** for DASH MPD, TTML, SAMI, Smooth Streaming |
| `flate2` + `miniz_oxide` | MIT/Apache/Zlib | Pass | Pass | Pass | Matroska zlib encoding, `cmov`, SWF | **Exact** | **Adopt** |
| `lzma-rs` | MIT | Pass | Pass | Marginal | Matroska LZMA content encoding, very rare | Exact | Adopt behind a feature |
| RustCrypto (`aes`, `ctr`, `cbc`, `sha1`, `sha2`, `md-5`, `hmac`) | MIT OR Apache-2.0 | Pass | Pass | Pass | CENC, HLS AES-128, SRTP, `crypto:`, hash muxers | **Exact** | **Adopt** |
| `url` | MIT OR Apache-2.0 | Pass | Pass | Pass | Only for protocols that speak real RFC 3986 (§2.3.1) | n/a | Adopt, scoped |
| `ureq`, `rustls`, `socket2` | — | see §2.6 | | | | | Adopt, scoped |
| `crc` / `crc32fast` | MIT/Apache | Pass | Pass | Pass | We need MPEG-2 CRC-32 (non-reflected, poly 0x04C11DB7) for TS sections *and* zlib CRC-32 for Matroska *and* Ogg's variant. Three polynomials, ~60 lines total, and `crc32fast` uses `unsafe` for SIMD | Exact | **Build**, in `vaco-core`, as a `fearless_simd` kernel if profiling justifies it (D12) |

## 5.3 The two CI rules that make this real

Both are already required by D11 and architecture §12; restated here in format-specific terms:

1. **Single-owner rule.** `quick-xml` appears in exactly one `Cargo.toml` under `crates/`
   (`vaco-demux-dash`), `flate2` in exactly one, `ureq` in exactly one. A second occurrence fails
   the build. Where two crates genuinely need the same third-party crate, the answer is a shared
   Vaco crate that owns it — which is how `vaco-format-riff` and `vaco-format-isom` came to exist
   anyway.
2. **No foreign type in a public API.** Enforced by review plus a lint on the crates that wrap
   something: their public items must reference only `vaco-*` and `core`/`std` types.

## 5.4 The honest summary

Of the crates D10 §"what this means in practice" names as back on the table, exactly one —
`matroska` — is a container crate, and it does not survive contact with what we need. Everything
else useful here is *infrastructure* (XML, compression, crypto, HTTP, TLS) rather than
container logic. **The container and protocol layer is ours, ~230 person-weeks of it, and the
dependency policy reduces the periphery rather than the core** — which is exactly what D10 §"what
this means in practice" predicted, with "no pure-Rust MP4/Matroska/MPEG-TS muxer at FFmpeg's level"
named explicitly.

---

# 6. Shared helpers

Research §5.5 identifies the cross-format dependency map. These become eleven crates, split out
precisely because several formats depend on each — and because splitting them is what lets eleven
people work in parallel in week 1.

| Crate | Contents | Consumed by |
|---|---|---|
| **`vaco-format-riff`** | RIFF/LIST chunk reader and writer; `BITMAPINFOHEADER`, `WAVEFORMATEX`, `WAVEFORMATEXTENSIBLE` parse/serialise (incl. the channel mask and sub-format GUID); the video FourCC ↔ `CodecId` table and the WAVE format-tag ↔ `CodecId` table; RIFF `INFO` metadata conversion. **Provenance**: the tag tables are derived from Microsoft's published `mmreg.h`/AVI documentation, the vendor registration for each FourCC, and the specification that defines each codec — never from FFmpeg's table (D9). | `avi`, `wav`, `w64`, `asf`, `matroska` (`V_MS/VFW/FOURCC`, `A_MS/ACM`), `swf`, `nut` (codec tags), `rawvideo`, `mov` (some sample entries) |
| **`vaco-format-isom`** | ISOBMFF box header read/write (32/64-bit sizes, `uuid`, full-box version/flags); box-type constants; MOV/MP4 sample-entry ↔ `CodecId` tables for video, audio and subtitle; MPEG-4 object-type indications (14496-1 Table 5); `esds`/DecoderConfigDescriptor/DecoderSpecificInfo parse and serialise; packed ISO-639-2/T language codec; 16.16 and 2.30 fixed-point helpers; the 3×3 display matrix and its rotation derivation. | `mp4` demux+mux, `mxf` (wrapped MPEG-4 codec identification), `hls`/`dash` (fMP4 segments), `smoothstreaming`, `avif`/`heif` |
| **`vaco-format-mpegts-tables`** | TS packet header and adaptation field; PSI section framing with `pointer_field` handling and MPEG-2 CRC-32; PAT/PMT/CAT/SDT/NIT/EIT/TDT structures; the `stream_type` ↔ `CodecId` table (13818-1 Table 2-34 + registration descriptors); the descriptor tag registry (13818-1 §2.6 + EN 300 468 + ATSC A/65). | `mpegts` demux+mux, `mpegtsraw`, `rtp_mpegts`, `hls`/`dash` when segmenting to TS, `m2ts` |
| **`vaco-format-mpeg-common`** | Start-code scanning, PES header parse/serialise, 33-bit timestamp codec, SCR/PCR encoding. Shared by TS and PS so the timestamp handling cannot drift between them. | `mpegts`, `mpegps`, `mpegenc` family, `spdif`, `vobsub` |
| **`vaco-format-id3`** | ID3v1 and ID3v2.2/2.3/2.4 read and write; unsynchronisation; the per-version frame-ID tables and their canonical-key conversion; `APIC` → attached picture; `CHAP`/`CTOC` → chapters; `id3v2_priv.` prefixing for unmapped frames. Wraps the `id3` crate behind the D11 boundary (§5.2). | `mp3`, `aiff`, `wav`, `asf` (read), `flv`, `hls` (timed ID3 for Sample-AES), any format opting into `InputFlags::ID3V2_AUTO` |
| **`vaco-format-metadata`** | The canonical metadata key set (`title artist album date comment genre track disc encoder language …`), the `MetadataConv` table type, and the conversion driver. Every container ships its own table; the driver is shared. | Everything |
| **`vaco-format-nalu`** | Annex-B ↔ length-prefixed conversion; `AVCDecoderConfigurationRecord`, `HEVCDecoderConfigurationRecord`, `VVCDecoderConfigurationRecord`, `AV1CodecConfigurationRecord`, `VPCodecConfigurationRecord`, `OpusHead`, `dfLa`, `dvcC`/`dvvC` build and parse. **Container-side byte layout only** — the semantic parsing of an SPS lives in `vaco-parse-h2645` at layer 4 and reaches us only through `ParserProvider` (§1.0). | `mp4`, `matroska`, `mpegts`, `hls`, `dash`, `rtp`, raw `h264`/`hevc`/`vvc`/`obu` demuxers, `flv` |
| **`vaco-format-vorbiscomment`** | Vorbis comment parse/serialise; FLAC `METADATA_BLOCK_PICTURE` (base64-wrapped) → attached picture. | `ogg` (Vorbis/Opus/FLAC/Theora/Speex), `flac`, `matroska` (some tags) |
| **`vaco-format-apetag`** | APEv1/v2 tag read and write, including the leading/trailing placement rules and the ID3v1 coexistence rule. | `ape`, `wv`, `mpc`, `tta`, `mp3` |
| **`vaco-format-avlanguage`** | ISO 639-1/639-2B/639-2T normalisation and BCP-47 handling, so `eng` / `en` / `en-US` / `Macintosh code 0` all resolve consistently. | `matroska`, `mp4`, `mxf`, `asf`, `mpegts`, subtitle formats |
| **`vaco-format-replaygain`** | ReplayGain tag parsing and side-data emission from all four conventions (Vorbis comment, APE tag, ID3 `TXXX`, LAME header). | `wav`, `mp3`, `ape`, `flac`, `ogg`, `wv` |

`vaco-format-spdif` (IEC 61937 burst framing, shared by the `spdif` mux/demux and the `s337m`
demuxer) is folded into `vaco-format-spdif` as its own format crate rather than a helper, since it
has only two consumers and they are both formats.

**Dependency shape**: every helper depends only on `vaco-core`, `vaco-codec-core` (3a) and
`vaco-io`. No helper depends on another helper except `vaco-format-id3` → `vaco-format-metadata` and
`vaco-format-vorbiscomment` → `vaco-format-metadata`. This keeps them all buildable and testable in
parallel from week 1.

---

# 7. The v0.1 delivery plan

D5: demux MP4/MOV, Matroska/WebM and MPEG-TS; parse H.264/HEVC/AV1/AAC/Opus stream headers (parse
only, no decode); emit the complete `vaco-probe` writer surface byte-identically for the covered
sections. Zero encoders, zero filters, zero muxers.

## 7.1 What this subsystem must deliver — and what it must not

**Needed.**

| Component | Scope for v0.1 |
|---|---|
| `vaco-io` | `IoContext` read path, buffering, seek, size, short seek, sticky EOF/error, `bytes_read` accounting. Write path only for the `md5`/`crc` protocol sinks used by the harness. |
| `vaco-protocol-core` | URL splitting, dispatch, whitelist/blacklist plumbing. |
| Protocols | `file`, `pipe`, `fd`, `data`, `md5`. **No network.** |
| `vaco-format-core` | Full object model (§1.1); probing (§1.5) including tie-break calibration; stream discovery (§1.6); the whole demux-side timestamp model (§1.7 R1–R24); duration estimation (R14–R18); index and generic/binary/format seek (§1.8) — needed by `-read_intervals`; the 40 generic options (§1.11), of which the mux-only ones are parsed and inert. |
| Shared helpers | `vaco-format-isom`, `vaco-format-mpegts-tables`, `vaco-format-mpeg-common`, `vaco-format-nalu`, `vaco-format-metadata`, `vaco-format-avlanguage`, and `vaco-format-riff` (Matroska's `V_MS/VFW/FOURCC` and `A_MS/ACM` tracks need it, and the corpus will contain one). |
| Demuxers | `mp4` (edit lists, fragmented, CENC *reporting*, metadata, chapters, cover art, HEIF tile groups), `matroska`/`webm` (lacing, content encodings, cues, tags, chapters, attachments, `CodecDelay`/`SeekPreRoll`/`DiscardPadding`), `mpegts`/`mpegtsraw` (PSI, programs, PES, descriptors, seek, duration). |
| Conformance | `tests/conformance/probe/{isobmff,matroska,mpegts}.toml`, the tie-break corpus, and the timestamp matrix from §1.7.8 restricted to its demux half. |
| Fuzz | One target per demuxer plus one per shared helper, from the day each lands (D6). |

**Deferred, explicitly.**

- Every muxer, and therefore §1.9 interleaving, §1.10 BSF-in-muxer, and §1.7's M1–M28 mux chain.
  The `Muxer` trait is *defined* in v0.1 (it must be, since `vaco-format-core`'s surface is being
  frozen) but has no implementations except the utility sinks the harness needs.
- All network protocols, TLS, HLS, DASH, RTP/RTSP.
- All other containers, including AVI/ASF/FLV/Ogg/WAV — which is worth stating because they look
  cheap and would silently expand the acceptance matrix.
- `use_wallclock_as_timestamps`, `-listen`, device formats.
- Ordered chapters, multi-edition Matroska, `media_rate != 1` edit lists, MP4 external `dref` — all
  documented divergences from day one rather than silent gaps.

## 7.2 Week-by-week

Four workstreams, ~2 engineers each: ≈112 pw of content (§8.4 — Wave F0, the demux half of F1, the
six v0.1 helpers from F1b, and all of F2) over 14 calendar weeks. At four engineers total the same
content takes ~28 weeks. Runs alongside plan 14's CLI work and plan 15's Wave 1b parser work, which
supplies `ParserProvider`'s implementation.

| Wk | E1 — core framework | E2 — MP4 | E3 — Matroska | E4 — MPEG-TS + I/O |
|---:|---|---|---|---|
| 1 | `vaco-io` `IoContext` read path, `file`/`pipe`/`fd`/`data` protocols | `vaco-format-isom`: box headers, tag tables, `esds`, language | EBML reader, element schema table, unknown-size termination (K2) | `vaco-format-mpegts-tables`: packet, adaptation field, section framing, CRC |
| 2 | Object model §1.1 frozen; `Demuxer`/`Muxer` traits; descriptors; registry seam | `vaco-format-nalu` config records | Matroska header, `Info`, `Tracks`, codec mapping from RFC 9559 §27 | PSI: PAT/PMT/descriptors; `stream_type` table |
| 3 | Probing §1.5 incl. the padded `ProbeData` cursor and the score conventions | MP4 box walk, track build, `stsd` → `CodecParameters` | Clusters, `SimpleBlock`/`BlockGroup`, keyframe flags | PES reassembly; 33-bit timestamps |
| 4 | `FormatOptions` (all 40) + `vaco-opts` wiring; `vaco-format-metadata` driver | `SampleCursor` (lazy sample tables) + `nb_frames` | Lacing (all three modes) + MKV-L1 timestamps | Programs, `Program` model, PMT version handling |
| 5 | Stream discovery §1.6 loop, limits, determinism rules DD1–DD4 | Packet output, MP4-O1/O2 ordering | Content encodings (zlib, header stripping); encryption reporting | Continuity, discontinuity, `TS_DISCONT` behaviour |
| 6 | Timestamp model §1.7 R1–R13 (rescale, NOPTS, wrap, `start_time`) | `ctts`/`cslg`, MP4-T1 dts shift | `CodecDelay`/`SeekPreRoll`/`DiscardPadding`; `Duration` float rule | Wrap state on `Program` (R7–R10); `read_timestamp` |
| 7 | §1.7 R14–R24 (duration estimation, generation, monotonic repair) | Edit lists E1–E8 | Cues → index; seek | Binary-search seek (S5), resync (S6), duration `FromPts` |
| 8 | Index + seek §1.8 (S1–S9, I1–I4); `-read_intervals` contract C1–C6 | Fragmented: `moof`/`tfhd`/`tfdt`/`trun`/`sidx`/`mfra` | Tags, chapters, attachments, `webm_dash_manifest` mode | Teletext/subtitle descriptor stream splitting; `mpegtsraw`; m2ts |
| 9 | `ParserProvider` integration with plan 15's parsers; `r_frame_rate`/`avg_frame_rate` estimator | Metadata: `ilst`, `keys`, 3GPP, `chpl`/`chap`, `covr`, `tmcd`, matrix | Colour/HDR metadata → `StreamSideData`; block additions | Descriptor-driven metadata and dispositions |
| 10 | Probe tie-break calibration harness (`just calibrate-probe`); the VERIFY-P/T/S experiments | CENC reporting + decryption; HEIF/AVIF items + `TileGrid` | Recovery (K5), CRC-32 elements, corrupt-file behaviour | AC-3/E-AC-3/AAC/HEVC descriptor refinement; SCTE-35 as data |
| 11 | Fuzz targets for core + all three demuxers; corpus minimisation | MP4 conformance bring-up | MKV conformance bring-up | TS conformance bring-up |
| 12 | Conformance triage — all three families, full plan 14 §5.6 matrix | triage | triage | triage |
| 13 | triage | triage | triage | triage |
| 14 | Zero-diff gate; `docs/formats/*`; divergence allowlist review | docs | docs | docs |

Weeks 12–14 look like a lot of triage. They are, deliberately: plan 14 §5.6's matrix is ~9 000
invocations and every field that differs is a real behavioural question that has to be answered with
an experiment. Budgeting three weeks for it is the difference between "v0.1 slips by a month" and
"v0.1 ships".

## 7.3 Acceptance criteria

Additive to plan 14 §5.6's, which govern the writer side.

1. **Zero unexplained byte differences** across the full plan 14 §5.6 matrix for the MP4/MOV,
   Matroska/WebM and MPEG-TS corpora, with `conformance/known-gaps.toml` containing nothing outside
   the reviewed divergence allowlist.
2. **The `-show_frames` question (§1.6.5) is resolved** and the matrix reflects the resolution.
3. **Every VERIFY item in this document is either answered and its answer recorded, or explicitly
   deferred with an owner.** The list: P1–P7, T1–T5, S1, M1–M7, K1–K4, A1, N1, L1. That is 27
   experiments, each small, and they are the real content of weeks 10–13.
4. **`probe_score` is exact for every corpus file**, including the deliberately ambiguous
   tie-break corpus. This is the proof that §1.5's model is right.
5. **Determinism gate**: every corpus file probed 100 times, on 1 and 16 threads, produces
   byte-identical output. This tests DD1–DD4 directly and is cheap to run.
6. **Fuzz gate**: 24 hours per demuxer with no panic, no OOM (RSS capped at 512 MiB), no hang
   (10 s per input), and no debug-mode arithmetic overflow. Per correctness §2.2.
7. **A truncation ladder**: every corpus file truncated at 1%, 5%, 10%, …, 95% of its length,
   probed by both binaries, exit codes and output compared. Truncation is the single most common
   real-world corruption and it exercises every error path at once.
8. **A memory ceiling**: probing a 4-hour 30 fps MP4 uses under 64 MiB RSS. This is the lazy sample
   cursor (§3.1.3 item 6) earning its place, and it is a regression test, not an aspiration.

## 7.4 Risks specific to v0.1

| Risk | Mitigation |
|---|---|
| The timestamp model is wrong in a way that only shows up in triage, and fixing it churns all three demuxers | Land §1.7 in weeks 6–7 with its own unit-test suite built from synthetic streams *before* any demuxer depends on it. The rules are numbered so a fix is localised. |
| `find_stream_info`'s stopping conditions differ from the reference, changing which streams appear on TS | VERIFY-P7 in week 10 is on the critical path; do it early rather than at triage time. |
| MP4 is simply larger than budgeted (upstream is 12.5k lines) | The lazy cursor and the edit-list generality are the two places to cut if week 10 looks bad; both have simple degraded modes (materialise the table; honour only the first edit) that are correct for the common case. |
| The tie-break calibration turns up ordering we cannot reproduce with a single priority integer | Fall back to an explicit ordered list in `formats.toml` rather than a priority key. Cheap, and it is data, not code. |
| Plan 15's parsers slip, so `ParserProvider` has no implementation | Every discovery path degrades gracefully with `NoParsers` (§1.0). The affected fields (`profile`, `level`, and `pix_fmt` where the container omits it) become a known gap with a named milestone rather than a blocker. |

---

# 8. Parallelisable work breakdown

Same contract as plan 15 §7 and plan 16 §8: every package is independently assignable, has an
explicit dependency list, and does not require coordination outside those dependencies.

**Definition of done, uniformly**: implementation + unit tests + a `cargo-fuzz` target + a criterion
bench where there is a hot path + the `docs/` page + a conformance entry at the stated gate + a
provenance trailer naming the specification and section + (for any wrapped crate) a fidelity grade
in `docs/format-status.md`.

IDs are prefixed to avoid collision with plan 15 (`F-`/`D-`/`C-`/`X-`/`B-`/`P-`/`H-`/`T2-`/`T3-`)
and plan 16 (phase-numbered).

## 8.1 Wave F0 — foundations (blocks nearly everything; ~4 people, ~5 weeks)

| ID | Package | Deps | pw | Notes |
|---|---|---|---|---|
| IO-01 | `vaco-io`: `IoContext` read+write, buffering, short seek, sticky state, checksums | `vaco-core` | 4 | Blocks every format. Freeze the surface after review. |
| IO-02 | `vaco-io`: `DynBuf`, `DataMarker` typed writes, cancellation | IO-01 | 2 | `DynBuf` blocks every muxer. |
| IO-03 | `vaco-protocol-core`: URL grammar, dispatch, whitelist/blacklist, nested-open depth | IO-01 | 3 | W1–W4 are security properties; review hard. |
| FW-01 | `vaco-format-core`: object model §1.1 (`Stream`, `Program`, `Chapter`, `StreamGroup`, `Metadata`, dispositions, side data) | `vaco-codec-core` (plan 15 F-01) | 3 | **Blocks everything. Do first, review hard, then freeze.** |
| FW-02 | `Demuxer`/`Muxer` traits, `DemuxCtx`/`MuxCtx`, descriptors, `ParserProvider`/`BsfProvider` seams, registry codegen | FW-01 | 2 | The §1.0 layering amendment lands here. |
| FW-03 | Probing §1.5: padded `ProbeData`, scoring, retry, forced format, whitelist, tie-break table + `just calibrate-probe` | FW-02 | 3 | Directly byte-verified via `probe_score`. |
| FW-11 | The 40 generic options + `vaco-opts` wiring + `-h demuxer=`/`-h muxer=` introspection | FW-02, `vaco-opts` | 2 | |

## 8.2 Wave F1 — the framework's hard parts (partly serial; ~3 people)

| ID | Package | Deps | pw | Parallel? |
|---|---|---|---|---|
| FW-04 | Stream discovery §1.6: the loop, limits, DD1–DD4, per-format analyse-duration defaults | FW-03 | 5 | after FW-03 |
| FW-05 | Timestamp model §1.7 R1–R13: rescaling, NOPTS, wrap state, `start_time` | FW-01 | 4 | yes |
| FW-06 | Timestamp model §1.7 R14–R24: duration estimation, generation, monotonic repair, fill-in | FW-05, FW-04 | 4 | after FW-05 |
| FW-07 | Seek §1.8: index, generic seek, binary search, byte seek, flags, the `-ss` contract | FW-04, FW-05 | 6 | after FW-05 |
| FW-12 | Metadata/chapter/program/stream-group model + `MetadataConv` driver + `vaco-format-metadata` | FW-01 | 3 | yes |
| FW-08 | Muxer core: init/header/packet/trailer state machine, M1–M28, `avoid_negative_ts`, monotonicity | FW-02, IO-02 | 4 | yes |
| FW-09 | Interleaving §1.9: per-DTS, chunked, sparse escape, custom policies | FW-08 | 4 | after FW-08 |
| FW-10 | BSF-in-muxer §1.10 | FW-08, plan 15 B-01 | 2 | after FW-08 |

## 8.3 Wave F1b — shared helpers (fully parallel once FW-01/FW-02 land; ~6 people)

| ID | Package | Deps | pw |
|---|---|---|---|
| SH-01 | `vaco-format-isom` | FW-02 | 4 |
| SH-02 | `vaco-format-riff` | FW-02 | 3 |
| SH-03 | `vaco-format-mpegts-tables` | FW-02 | 3 |
| SH-04 | `vaco-format-mpeg-common` (start codes, PES, 33-bit timestamps, SCR/PCR) | FW-02 | 2 |
| SH-05 | `vaco-format-nalu` (config records, Annex-B ↔ length-prefixed) | FW-02 | 3 |
| SH-06 | `vaco-format-id3` (wraps `id3`, D11 boundary, our conversion table) | FW-12 | 3 |
| SH-07 | `vaco-format-vorbiscomment` + FLAC picture | FW-12 | 1.5 |
| SH-08 | `vaco-format-apetag` | FW-12 | 1 |
| SH-09 | `vaco-format-avlanguage` | FW-01 | 1 |
| SH-10 | `vaco-format-replaygain` | FW-12 | 0.5 |

## 8.4 Wave F2 — the v0.1 demuxers (delivers D5; ~3 people, parallel)

| ID | Package | Deps | pw |
|---|---|---|---|
| FM-01 | `vaco-demux-mp4`: box walk, tracks, `SampleCursor`, packet output, MP4-O1/O2 | SH-01, SH-05, FW-04 | 8 |
| FM-02 | `vaco-demux-mp4`: edit lists, `ctts`/`cslg`, seek | FM-01, FW-07 | 4 |
| FM-03 | `vaco-demux-mp4`: fragmented (`moof`/`tfdt`/`trun`/`sidx`/`mfra`) | FM-01 | 3 |
| FM-04 | `vaco-demux-mp4`: metadata, chapters, cover art, timecode, matrix, colour side data | FM-01, FW-12, SH-09 | 3 |
| FM-05 | `vaco-demux-mp4`: CENC reporting + decryption; HEIF/AVIF items + `TileGrid` | FM-01 | 4 |
| FM-06 | `vaco-demux-matroska`: EBML reader, schema, unknown-size termination, recovery | FW-02 | 3 |
| FM-07 | `vaco-demux-matroska`: header, tracks, codec mapping, colour/HDR, clusters, lacing, content encodings | FM-06, SH-02, SH-09 | 5 |
| FM-08 | `vaco-demux-matroska`: cues/seek, tags, chapters, attachments, delay/preroll/padding, `webm_dash_manifest` | FM-07, FW-07, FW-12 | 3 |
| FM-09 | `vaco-demux-mpegts`: PSI, programs, descriptors, streams | SH-03, SH-04 | 4 |
| FM-10 | `vaco-demux-mpegts`: PES, timestamps, wrap, continuity/discontinuity, `mpegtsraw`, m2ts | FM-09, FW-05 | 4 |
| FM-11 | `vaco-demux-mpegts`: seek, duration `FromPts`, PMT-version options | FM-10, FW-07, FW-06 | 4 |
| XF-01 | `tests/conformance/probe/{isobmff,matroska,mpegts}.toml` + corpus + the 26 VERIFY experiments | FM-01..FM-11, plan 13 X-01 | 6 |
| XF-02 | Fuzz targets for core, IO, and the three demuxers + corpus minimisation | FM-01..FM-11 | 3 |
| PR-01 | Protocols `file`, `pipe`, `fd`, `data`, `md5` | IO-03 | 1.5 |

**Wave F2 subtotal: 55.5 pw.** With Wave F0 (19 pw), the framework half of Wave F1 needed by
demuxing (FW-04/05/06/07/12 — 22 pw) and the v0.1 helpers (SH-01, SH-02, SH-03, SH-04, SH-05, SH-09 —
16 pw), **v0.1's formats content is ~112 pw**. That is the 14-week schedule in §7.2 at eight
engineers, or ~28 weeks at four.

## 8.5 Wave F3 — the v0.2 muxers and the second-tier containers

| ID | Package | Deps | pw |
|---|---|---|---|
| FM-20 | `vaco-mux-utility` (crc, framecrc, framemd5, framehash, hash, md5, streamhash, uncodedframecrc, null, mkvtimestamp_v2) | FW-08 | 2 |
| FM-21 | `vaco-mux-mp4`: tables, chunked interleave, trailer, faststart | FW-09, SH-01 | 7 |
| FM-22 | `vaco-mux-mp4`: fragmented, all `movflags`, CENC write, profile variants | FM-21 | 5 |
| FM-23 | `vaco-mux-mp4`: metadata write, `avif`/`ipod`/`ismv`/`f4v`/`psp`/`3gp`/`3g2` | FM-21, FW-12 | 4 |
| FM-24 | `vaco-mux-matroska` (+ webm, matroska_audio, webm_chunk) | FW-09, FM-06 | 8 |
| FM-25 | `vaco-mux-mpegts` | FW-09, SH-03 | 7 |
| FM-26 | `vaco-demux-raw` + `vaco-mux-raw` (48 + 40 registrations: PCM, rawvideo, bitstream families) | FW-04, FW-09 | 6 |
| FM-27 | `vaco-format-audio-simple` (wav, w64, aiff, caf, au, voc, sox, ircam, rso — demux + mux) | SH-02, SH-06, SH-10 | 7 |
| FM-28 | `vaco-demux-avi` + `vaco-mux-avi` | SH-02 | 6 |
| FM-29 | `vaco-demux-flv` + `vaco-mux-flv` | SH-05 | 5 |
| FM-30 | `vaco-demux-ogg` + `vaco-mux-ogg` (+ oga/ogv/opus/spx) | SH-07 | 8 |
| FM-31 | `vaco-demux-asf` + `vaco-mux-asf` | SH-02 | 7 |
| FM-32 | `vaco-demux-mpegps` + `vaco-mux-mpegps` family (mpeg1system/vcd/dvd/svcd/vob) | SH-04 | 9 |
| FM-33 | `vaco-format-meta` (concat, ffmetadata, segment, stream_segment, tee, fifo) | FW-09, IO-03 | 6 |
| FM-34 | `vaco-format-subtitle-text` (15 demux / 6 mux) | FW-02 | 6 |
| FM-35 | `vaco-demux-image2` + `vaco-mux-image2` incl. the 42 pipe splitters | FW-04 | 4 |
| FM-36 | `vaco-format-nut` | FW-09 | 5 |
| FM-37 | `vaco-format-dv` | SH-02 | 3 |
| FM-38 | `vaco-format-mpjpeg` | FW-04 | 1 |

## 8.6 Wave F4 — protocols and streaming

| ID | Package | Deps | pw |
|---|---|---|---|
| PR-02 | `cache`, `subfile`, `concat`, `concatf`, `tee`, `async` | IO-03 | 5 |
| PR-03 | `crypto` (AES-CTR over a nested URL) | IO-03 | 1.5 |
| PR-04 | `tcp`, `udp`, `udplite`, `unix` (+ `socket2`) | IO-03 | 4.5 |
| PR-05 | `tls` via `rustls` + the provider decision (§2.6.3) + root store | PR-04 | 3 |
| PR-06 | `http`/`https` (wrapping `ureq`) + range/seek/reconnect/ICY/persistent/chunked-POST | PR-05 | 6 |
| PR-07 | `httpproxy`, `ftp`, `gopher`, `gophers`, `icecast`, `ipfs_gateway`, `ipns_gateway` | PR-06 | 4.5 |
| PR-08 | `rtp`, `srtp`, `prompeg` | PR-04 | 5 |
| FM-40 | `vaco-format-rtp`: RTSP/SDP session layer + transport modes | PR-08 | 8 |
| FM-41 | `vaco-format-rtp`: 26 implementable depacketisers | FM-40, SH-05 | 8 |
| FM-42 | `vaco-format-rtp`: packetisers + `rtp_mpegts` | FM-41 | 6 |
| FM-43 | `vaco-demux-hls` | PR-06, FM-03, FM-25, SH-06 | 8 |
| FM-44 | `vaco-mux-hls` | FM-33, FM-22 | 8 |
| FM-45 | `vaco-demux-dash` (`quick-xml`) | PR-06, FM-03 | 8 |
| FM-46 | `vaco-mux-dash` | FM-33, FM-22 | 8 |
| PR-09 | `rtmp`/`rtmps`/`rtmpt`/`ffrtmphttp` native, from the Adobe specification | PR-05 | 10 |
| PR-10 | `srt` native, from `draft-sharabayko-srt` (§2.7) | PR-04 | 12 |

## 8.7 Wave F5 — T2 and T3

| ID | Package | Deps | pw |
|---|---|---|---|
| FM-50 | `vaco-demux-mxf` | SH-01, FW-07 | 14 |
| FM-51 | `vaco-mux-mxf` (+ d10, opatom) | FM-50 | 12 |
| FM-52 | `vaco-format-subtitle-bitmap` (dvbsub, dvbtxt, sup/PGS, vobsub) | SH-04 | 6 |
| FM-53 | `vaco-format-iamf` | SH-01 | 7 |
| FM-54 | `vaco-format-spdif` + `s337m` | SH-04 | 2.5 |
| FM-55 | `vaco-format-gxf` + `vaco-format-imf` | SH-01 | 10 |
| FM-56 | `vaco-format-swf` | SH-02 | 4 |
| FM-57 | `vaco-mux-smoothstreaming`, `-hds`, `-whip` | FM-22, PR-06 | 8 |
| FM-58 | `vaco-format-misc-audio` (T3 audio containers) | FW-04, SH-08 | 10 |
| FM-59 | `vaco-format-misc` (T3 remainder) | FW-04 | 10 |
| PR-11 | `rist` native (VSF TR-06-1/2) | PR-04 | 10 |
| PR-12 | `sctp`, `shared`, `dtls` | PR-04 | 8 |
| XF-03 | Format-wide conformance expansion: remux byte-identity matrix across every T1 muxer | FM-2x | 6 |
| XF-04 | The differential fuzzer for formats (correctness §2.4): mutate real media, feed both, assert agreement | XF-02 | 4 |
| XF-05 | `docs/formats/*` completeness + `docs/why-some-formats-are-not-included.md` | all | 4 |

## 8.8 Roll-up

| Wave | pw |
|---|---:|
| F0 — foundations | 19 |
| F1 — framework hard parts | 32 |
| F1b — shared helpers | 22 |
| F2 — v0.1 demuxers + conformance | 55.5 |
| F3 — muxers and second-tier containers | 106 |
| F4 — protocols and streaming | 105.5 |
| F5 — T2/T3 and cross-cutting | 115.5 |
| **Total** | **455.5 pw** |

Of which **~112 pw is v0.1** (§8.4) and ~204 pw gets us through v0.2's muxers and the second-tier
containers. For comparison, plan 15 puts codecs at roughly 1 050 pw across T1–T3, plus ~550 pw of
T4/T5 programmes it recommends deferring past v1.0. Containers and protocols at ~455 pw is the right
ratio against libavformat's share of upstream's source.

## 8.9 What this means for staffing

| Contributors on formats | F0+F1+F1b (73 pw) | F2 → v0.1 (55.5 pw) | F3+F4+F5 (327 pw) | Realistic v1.0 formats |
|---:|---|---|---|---|
| 2 | 9 months | 6.5 months | 38 months | ~4.4 years |
| 4 | 4.5 months | 3.5 months | 19 months | ~2.3 years |
| 8 | 3 months (F0 does not parallelise below ~4) | 2 months | 10 months | ~1.3 years |

The v0.1 column is the one that matters for scheduling, because D5 gates everything: **four
engineers reach the v0.1 gate about eight months after starting from nothing, eight engineers in
about five.** The §7.2 fourteen-week table describes the second of those, and it assumes F0/F1/F1b
are already done.

**F0 is the hard floor.** FW-01 and FW-02 cannot be usefully parallelised, and every one of the 40
format crates waits on them. As in plan 15, getting `vaco-format-core`'s surface right is worth more
than any other five weeks in this subsystem — and unlike the codec core, this one has a hard external
deadline, because D5's v0.1 ships on top of it.

---

# 9. Open questions and verification owed

## 9.1 Decisions that need escalation

1. **The TLS crypto-provider conflict (§2.6.3).** D10 Gate 1 excludes both production `rustls`
   providers. Needs a benchmark, then either a D10 amendment (narrow exception) or acceptance of
   `rustls-rustcrypto`'s performance. **Blocking for `https`, and therefore for HLS/DASH.**
2. **The OS-interface carve-out (§2.5).** Implicit in D2's hardware allowlist and plan 15 §8's
   reasoning, but never written into D10. Should be, since it governs `socket2`, `std`, and
   `rustls-platform-verifier`.
3. **SRT (§2.7).** Recommendation is native, T3, ~12 pw, from `draft-sharabayko-srt`. Needs a
   yes/no so the roadmap can carry it or not.
4. **The `-show_frames` scoping problem (§1.6.5).** A formats/CLI boundary issue. Recommendation:
   move `-show_frames` to v0.2. Needs plan 14's agreement before the v0.1 corpus is frozen.
5. **The layering amendment (§1.0).** `vaco-codec-core` must sit below `vaco-format-core`. A
   one-line change to `layers.toml` plus a ban rule, but it contradicts architecture §3 as written.

## 9.2 Black-box experiments owed

Each is small, each settles a rule, and all 27 belong to weeks 10–13 of §7.2. Recorded here as a
checklist so none is forgotten.

| ID | Question | Experiment |
|---|---|---|
| P1 | Does the MIME bonus apply to a zero-scoring demuxer? | Serve WebM over HTTP with a correct `Content-Type`, a `.bin` extension and mangled EBML magic; read `probe_score` |
| P2 | `PROBE_BUF_MIN` / `PROBE_BUF_MAX` | Bisect the offset at which a format's only magic stops being found |
| P3 | Does `-f <name>` report `probe_score` 100? | `ffprobe -f matroska x.mkv` |
| P4 | `fpsprobesize` default | Stream whose frame rate changes after frame N; bisect N |
| P5 | `r_frame_rate` snapping tolerance and standard-rate list | Matroska (ms time base) at 23.976 / 29.97 / 59.94 / 119.88 fps |
| P6 | `r_frame_rate` for single-frame streams | Single-frame MP4, MKV, and a TS with one video PES |
| P7 | Per-format `analyzeduration` defaults | TS whose second program's audio first appears at t s; bisect t |
| T1 | Seek across a 33-bit PTS wrap | TS with segments straddling 2^33; seek past the wrap; compare `-show_packets` |
| T2 | Container `start_time` = min or max over streams? | MP4 with audio at 0.000000 and video at 0.041708 |
| T3 | `mvhd.duration` vs the longest track's duration | MP4 where they disagree by 2 s |
| T4 | `duration_probesize` default | Progressively truncate a TS tail until `duration` goes wrong |
| T5 | `avoid_negative_ts` byte effects | Remux a negative-DTS MP4 to TS with all four values |
| S1 | Does the reference forward-discard on an unseekable input, and is it bounded? | `cat big.ts \| ffprobe -ss 3600 -i -` |
| M1 | MP4 packet emission order | MP4 written with all-video-then-all-audio chunking |
| M2 | `ctts` v0/v1 and `cslg` DTS shift | B-frame MP4s from three different muxers |
| M3 | `elst` with `media_rate != 1` | Hand-built MP4 with a rate-2 edit |
| M4 | `chpl` vs `tref chap` precedence | MP4 carrying both, with different chapter titles |
| M5 | CENC IV generation determinism on mux | Mux the same input twice with `encryption_scheme=cenc-aes-ctr`; diff |
| M6 | Multiple `stsd` entries in one track | MP4 with `avc1` and `hvc1` entries in one track |
| M7 | `nb_frames` when `mdat` is truncated | Truncate an MP4's `mdat` to half |
| K1 | Laced-frame timestamp derivation | Vorbis-in-MKV with EBML lacing and no `BlockDuration` |
| K2 | `Info/Duration` float rounding | MKV with `Duration = 12345.6789` |
| K3 | Nested `SimpleTag` flattening separator | MKV with a two-level tag |
| K4 | What does `bitexact` substitute for MKV random UIDs? | Mux the same input twice with `-fflags +bitexact`; diff |
| A1 | Do `asf` and `asf_o` differ observably? | Run both over the ASF corpus and diff |
| N1 | Interleave tie-break for equal DTS | Two streams with identical DTS, remuxed to MKV and to MP4 |
| L1 | Are WavPack and TTA actually spec-documented (T3, not T4)? | Read wavpack.com's format description and the TTA specification; reclassify if so |

## 9.3 Things this plan deliberately does not solve

- **Device formats** (`avfoundation`, `v4l2`, `dshow`, `alsa`, `x11grab`, …). Research 06 covers
  them; they are `AVFMT_NOFILE` formats that own their own I/O, they are inherently
  non-deterministic, and they are out of scope for v1.0. The `NOFILE` flag exists in `FormatFlags`
  so the model can express them later.
- **DVD and Blu-ray disc structure** (`dvdvideo`, `bluray`). Both require GPL C libraries; both are
  T4; both have an out-of-process delegation answer (§3.5 item 4).
- **Ordered chapters and linked Matroska segments**, **`media_rate != 1` MP4 edits**, and
  **MP4 external `dref`**. All three are documented divergences from day one, in the correctness
  §1.4 allowlist, with the reason recorded.
- **Live DASH and live HLS byte-exactness.** Both depend on wall-clock time by design and cannot be
  in a byte-exact corpus; they get `container-structure` (correctness C2) comparison instead.
