//! The codec framework: what a decoder, encoder, parser and bitstream filter are.
//!
//! Per D14.1 this crate sits *below* `vaco-format-core`, because demuxers need
//! codec parameters and bitstream parsers. Nothing here knows about any specific
//! codec — concrete codecs are leaf crates that implement these traits.

use vaco_core::{MediaType, Rational, Result};
use vaco_frame::Frame;
use vaco_packet::Packet;

pub mod caps;
pub mod params;

pub use caps::Caps;
pub use params::{CodecParameters, Level, Profile};

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
    Opus,
    Flac,
    Vorbis,
    Mp3,
    Pcm,
    Png,
    Jpeg,
    // ... generated
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
/// parsing parameter sets, never reconstructing pixels.
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

/// Transforms packets without decoding: format conversion, metadata rewriting,
/// extradata extraction.
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
