//! The codec framework: what a decoder, encoder, parser and bitstream filter are.
//!
//! Per D14.1 this crate sits *below* `vaco-format-core`, because demuxers need
//! codec parameters and bitstream parsers. Nothing here knows about any specific
//! codec — concrete codecs are leaf crates that implement these traits.
//!
//! # What is in here
//!
//! | Module | Contents |
//! |---|---|
//! | [`machine`] | [`Machine`], the send/receive state machine every component shares |
//! | [`protocol`] | [`SendReceive`], the adapters onto the three trait faces, and [`Validated`] |
//! | [`parser`] | [`ParserDriver`], the harness that drives a [`Parser`] over a byte stream |
//! | [`caps`] | [`Caps`] — what an implementation can do, checked before it is built |
//! | [`params`] | [`CodecParameters`], profiles, levels and their tables |
//! | [`picture`] | frame-threading primitives: guard-padded row bands published through `OnceLock` |
//! | [`threading`] | the three threading axes, and the split-decoder frame-threading traits |
//! | [`mock`] | a reference codec that exercises every corner of the protocol |
//!
//! # The one idea worth reading first
//!
//! Packet-to-frame is genuinely N:M. One packet can yield several frames, several
//! packets can yield none while a reorder buffer fills, and draining at end of
//! stream can yield many. That is why the API is send/receive rather than
//! `decode(packet) -> Frame`, and why [`Machine`] exists: the rules are the same
//! for decoders, encoders and bitstream filters, so they are written down once,
//! executed once, and — through [`Validated`] — *enforced* once.

#![forbid(unsafe_code)]

use vaco_core::{MediaType, Rational, Result};
use vaco_frame::Frame;
use vaco_packet::Packet;

pub mod caps;
pub mod machine;
pub mod mock;
pub mod params;
pub mod parser;
pub mod picture;
pub mod protocol;
pub mod threading;

pub use caps::{Caps, CodecProperties};
pub use machine::{Accept, Machine, Stage};
pub use params::{
    AudioParameters, CodecParameters, FieldOrder, Level, LevelConstraints, LevelEntry, LevelQuery,
    LevelTable, Profile, ProfileEntry, ProfileTable, VideoParameters,
};
pub use parser::ParserDriver;
pub use picture::{
    BandMut, BandRangeMut, BlockRef, BlockScratch, PictureRef, PictureSpec, PictureWriter,
    PlaneSpec, PlaneView, ProgressPicture,
};
pub use protocol::{
    AsBitstreamFilter, AsDecoder, AsEncoder, DecoderProtocol, OnViolation, SendReceive, Validated,
    Violation, validate_decoder,
};
pub use threading::{
    CancelToken, FrameTask, FrameThreadedDecoder, SliceThreadedDecoder, SplitOutcome, TaskCtx,
    Threading,
};

/// Identifies a codec independently of who implements it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum CodecId {
    H264,
    Hevc,
    Av1,
    Vp8,
    Vp9,
    Aac,
    /// AAC in the LATM/LOAS syntax, which the reference treats as a separate
    /// codec rather than a framing of `Aac`: `ffmpeg -codecs` lists both, and
    /// ffprobe prints `codec_name=aac_latm`. Reproducing that spelling is why
    /// this is a variant and not a flag on `Aac`.
    AacLatm,
    Opus,
    Flac,
    Vorbis,
    Mp3,
    Pcm,
    Png,
    Jpeg,
    // ... generated
}

/// One row of the codec identity table.
///
/// Hand-written for now; plan 15 §1.1 has this generated from `codecs.toml`, the
/// same technique `vaco-pixfmt` uses, so that name, long name, media type and
/// properties cannot drift from the enum. The shape here is what that generator
/// must emit.
struct CodecEntry {
    id: CodecId,
    /// CLI-stable short name. An interface fact, not an implementation detail.
    name: &'static str,
    long_name: &'static str,
    media_type: MediaType,
    properties: CodecProperties,
}

const fn entry(
    id: CodecId,
    name: &'static str,
    long_name: &'static str,
    media_type: MediaType,
    properties: CodecProperties,
) -> CodecEntry {
    CodecEntry {
        id,
        name,
        long_name,
        media_type,
        properties,
    }
}

const V: MediaType = MediaType::Video;
const A: MediaType = MediaType::Audio;

const CODECS: &[CodecEntry] = &[
    entry(
        CodecId::H264,
        "h264",
        "H.264 / AVC / MPEG-4 AVC",
        V,
        CodecProperties::LOSSY
            .union(CodecProperties::REORDER)
            .union(CodecProperties::FIELDS),
    ),
    entry(
        CodecId::Hevc,
        "hevc",
        "H.265 / HEVC",
        V,
        CodecProperties::LOSSY
            .union(CodecProperties::REORDER)
            .union(CodecProperties::FIELDS),
    ),
    entry(
        CodecId::Av1,
        "av1",
        "Alliance for Open Media AV1",
        V,
        CodecProperties::LOSSY.union(CodecProperties::REORDER),
    ),
    entry(CodecId::Vp8, "vp8", "On2 VP8", V, CodecProperties::LOSSY),
    entry(
        CodecId::Vp9,
        "vp9",
        "Google VP9",
        V,
        CodecProperties::LOSSY.union(CodecProperties::REORDER),
    ),
    entry(
        CodecId::Aac,
        "aac",
        "AAC (Advanced Audio Coding)",
        A,
        CodecProperties::LOSSY,
    ),
    entry(
        CodecId::AacLatm,
        "aac_latm",
        "AAC LATM (Advanced Audio Coding LATM syntax)",
        A,
        CodecProperties::LOSSY,
    ),
    entry(CodecId::Opus, "opus", "Opus", A, CodecProperties::LOSSY),
    entry(
        CodecId::Flac,
        "flac",
        "FLAC (Free Lossless Audio Codec)",
        A,
        CodecProperties::LOSSLESS,
    ),
    entry(
        CodecId::Vorbis,
        "vorbis",
        "Vorbis",
        A,
        CodecProperties::LOSSY,
    ),
    entry(
        CodecId::Mp3,
        "mp3",
        "MP3 (MPEG audio layer 3)",
        A,
        CodecProperties::LOSSY,
    ),
    entry(
        CodecId::Pcm,
        "pcm",
        "PCM (uncompressed)",
        A,
        CodecProperties::LOSSLESS.union(CodecProperties::INTRA_ONLY),
    ),
    entry(
        CodecId::Png,
        "png",
        "PNG (Portable Network Graphics)",
        V,
        CodecProperties::LOSSLESS.union(CodecProperties::INTRA_ONLY),
    ),
    entry(
        CodecId::Jpeg,
        "mjpeg",
        "Motion JPEG",
        V,
        CodecProperties::LOSSY.union(CodecProperties::INTRA_ONLY),
    ),
];

impl CodecId {
    fn entry(self) -> Option<&'static CodecEntry> {
        CODECS.iter().find(|e| e.id == self)
    }

    /// The CLI-stable short name, e.g. `"av1"`.
    ///
    /// Names and their documented semantics are interface facts and are used
    /// freely under D7; only expression is off limits.
    #[must_use]
    pub fn name(self) -> &'static str {
        self.entry().map_or("unknown", |e| e.name)
    }

    /// The human-readable name `vaco -codecs` prints.
    #[must_use]
    pub fn long_name(self) -> &'static str {
        self.entry().map_or("unknown codec", |e| e.long_name)
    }

    /// Video, audio, subtitle or data.
    #[must_use]
    pub fn media_type(self) -> MediaType {
        self.entry().map_or(MediaType::Data, |e| e.media_type)
    }

    /// What the *format* implies, before any implementation is chosen.
    #[must_use]
    pub fn properties(self) -> CodecProperties {
        self.entry()
            .map_or(CodecProperties::empty(), |e| e.properties)
    }

    /// Resolve a CLI name. Case-insensitive, because option values are.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        CODECS
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(name))
            .map(|e| e.id)
    }

    /// Every codec this build knows about, in table order.
    pub fn all() -> impl Iterator<Item = Self> {
        CODECS.iter().map(|e| e.id)
    }
}

/// Decode: compressed packets in, frames out.
///
/// The send/receive shape is not decoration. A decoder's packet-to-frame
/// relationship is genuinely N:M — one packet can yield several frames, several
/// packets can yield none while a reorder buffer fills, and flushing at EOF can
/// yield many. A `decode(packet) -> Frame` signature cannot express that, which
/// is why this API is shaped the way it is.
pub trait Decoder: Send {
    /// Submit a packet, or `None` to begin draining at end of stream.
    ///
    /// # Errors
    /// [`vaco_core::Error::OutputPending`] means frames must be drained via
    /// [`Decoder::receive_frame`] before more input is accepted.
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()>;

    /// Take the next available frame.
    ///
    /// # Errors
    /// [`vaco_core::Error::NeedMoreInput`] means send another packet;
    /// [`vaco_core::Error::Eof`] means draining is complete.
    fn receive_frame(&mut self) -> Result<Frame>;

    /// Discard buffered state after a seek. Does not change configuration.
    fn flush(&mut self);
}

/// Encode: frames in, compressed packets out. Mirrors [`Decoder`].
pub trait Encoder: Send {
    /// # Errors
    /// [`vaco_core::Error::OutputPending`] means drain before sending more.
    fn send_frame(&mut self, frame: Option<&Frame>) -> Result<()>;

    /// # Errors
    /// [`vaco_core::Error::NeedMoreInput`] or [`vaco_core::Error::Eof`].
    fn receive_packet(&mut self) -> Result<Packet>;

    fn flush(&mut self);
}

/// Splits a byte stream into packets and reads enough header syntax to describe
/// the stream, without decoding it.
///
/// This is what v0.1 needs: `vaco-probe` reports stream properties, which means
/// parsing parameter sets, never reconstructing pixels. The distinction is also
/// load-bearing legally (plan 15 §1.6): parsing an H.264 SPS implements no
/// decoder, so header-parsing crates ship in the default build while the
/// decoders do not.
///
/// # The end-of-stream convention
///
/// An empty `input` slice means "no more bytes will ever arrive": emit any
/// partially buffered final unit, or return `(None, 0)`. [`ParserDriver`] is
/// what applies that convention, and what checks the parser does not claim to
/// have consumed more than it was given.
pub trait Parser: Send {
    /// Consume input, returning a complete packet when one is available and how
    /// many bytes were used.
    ///
    /// # Errors
    /// [`vaco_core::Error::InvalidData`] when the stream cannot be resynchronised.
    fn parse(&mut self, input: &[u8]) -> Result<(Option<Packet>, usize)>;

    /// Stream properties discovered so far, if enough header data has been seen.
    fn parameters(&self) -> Option<&CodecParameters>;
}

/// So a boxed parser is itself a [`Parser`], and can be handed to anything
/// generic over `P: Parser` — [`ParserDriver`] above all.
///
/// Without this, a caller that obtains a parser dynamically (from a registry
/// lookup, or `ParserProvider::parser_for`, which can only return a `Box<dyn
/// Parser>` because the codec is not known until runtime) cannot use the driver
/// at all, and has to re-implement the end-of-stream convention and the
/// consumed-bytes check by hand. `vaco-format-core`'s stream discovery hit
/// exactly that and was doing so.
impl<P: Parser + ?Sized> Parser for Box<P> {
    fn parse(&mut self, input: &[u8]) -> Result<(Option<Packet>, usize)> {
        (**self).parse(input)
    }

    fn parameters(&self) -> Option<&CodecParameters> {
        (**self).parameters()
    }
}

/// Transforms packets without decoding: format conversion, metadata rewriting,
/// extradata extraction.
///
/// Deliberately the same state machine as [`Decoder`]: learning it once covers
/// all three faces.
pub trait BitstreamFilter: Send {
    /// # Errors
    /// [`vaco_core::Error::OutputPending`].
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()>;

    /// # Errors
    /// [`vaco_core::Error::NeedMoreInput`] or [`vaco_core::Error::Eof`].
    fn receive_packet(&mut self) -> Result<Packet>;
}

/// Static description of a codec implementation.
///
/// The registry stores descriptors and constructs implementations on demand, so
/// `-h decoder=h264` can print capabilities without instantiating anything.
#[derive(Debug, Clone, Copy)]
pub struct DecoderDesc {
    pub name: &'static str,
    pub long_name: &'static str,
    pub id: CodecId,
    pub media_type: MediaType,
    pub caps: Caps,
    /// Frame rates, sample rates or pixel formats the implementation accepts;
    /// empty means unconstrained.
    pub supported_rates: &'static [Rational],
}

impl DecoderDesc {
    /// Whether this implementation may appear in a default build.
    ///
    /// D4 requires CI to assert that nothing patent-encumbered is *reachable*
    /// from the default feature set, and to assert it on the compiled artefact
    /// rather than on intent. [`Caps::PATENT_ENCUMBERED`] is the runtime half of
    /// that assertion and this is the predicate CI evaluates over the registry.
    #[must_use]
    pub const fn is_default_build_safe(&self) -> bool {
        !self.caps.contains(Caps::PATENT_ENCUMBERED)
    }

    /// Whether the implementation supports a given rate, treating an empty
    /// table as "unconstrained".
    #[must_use]
    pub fn supports_rate(&self, rate: Rational) -> bool {
        self.supported_rates.is_empty() || self.supported_rates.contains(&rate)
    }
}
