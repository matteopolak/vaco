//! The tile/superblock/partition/mode-info/residual walk (AV1 spec §5.11,
//! §6.10, §7.4–§7.12) and the [`vaco_codec_core::Decoder`] wiring.

use vaco_codec_core::{Decoder, DecoderDesc};
use vaco_core::{Error, MediaType, Result};
use vaco_frame::Frame;
use vaco_limits::Limits;
use vaco_packet::Packet;

/// The AV1 decoder. See the crate root doc for exactly what it decodes.
#[derive(Debug)]
pub struct Av1Decoder {
    #[allow(dead_code, reason = "wired in as the tile loop lands")]
    limits: Limits,
}

impl Av1Decoder {
    #[must_use]
    pub const fn new(limits: Limits) -> Self {
        Self { limits }
    }
}

impl Decoder for Av1Decoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        let _ = packet;
        Err(Error::Unsupported("vaco-codec-av1: tile decode not yet wired"))
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        Err(Error::Unsupported("vaco-codec-av1: tile decode not yet wired"))
    }

    fn flush(&mut self) {}
}

/// `vaco-component.toml`'s decoder registration point.
pub static AV1_DECODER: DecoderDesc = DecoderDesc {
    name: "av1",
    long_name: "AV1 (intra-only; AV1 Bitstream & Decoding Process Specification v1.0.0 with Errata 1)",
    id: vaco_codec_core::CodecId::Av1,
    media_type: MediaType::Video,
    caps: vaco_codec_core::Caps::empty(),
    supported_rates: &[],
    make: |limits| Box::new(Av1Decoder::new(limits)),
};
