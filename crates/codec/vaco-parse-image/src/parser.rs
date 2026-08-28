//! The shared shape every format in this crate reduces to: **the whole input
//! is one image**, so [`Parser::parse`] never has to find a boundary.
//!
//! # Why every one of the six formats can share this
//!
//! Every container that carries a still image in this workspace —
//! `image2` (one file per pattern match), the `image2pipe` splitters
//! (`vaco-demux-image2::pipe`), a single-frame MP4/Matroska sample — already
//! delimits one image as one packet before a `vaco-parse-*` crate ever sees
//! it. `vaco-parse-opus` and `vaco-parse-vpx` document the identical
//! contract for the identical reason (no self-framing byte stream exists for
//! their formats either); this crate has an even stronger version of it,
//! since a still image format has no concept of "the next one" to find a
//! boundary before at all.

use vaco_codec_core::{CodecParameters, Parser};
use vaco_core::Result;
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

/// One format's header reader: bytes in, a stream description out.
pub trait ImageHeader: Send {
    /// Read whatever this format's header states. `None` means the bytes do
    /// not even look like this format (a truncated or corrupt file) —
    /// [`ImageParser`] still emits the packet either way, since a demuxer
    /// that already recognised the format by its magic is offering bytes to
    /// describe, not asking permission to keep them.
    fn parse(data: &[u8]) -> Option<CodecParameters>;
}

/// Wraps one [`ImageHeader`] reader as a [`Parser`]. Every still-image
/// format in this crate is one of these, parameterised by `H`.
#[derive(Debug)]
pub struct ImageParser<H> {
    budget: Budget,
    params: Option<CodecParameters>,
    _header: core::marker::PhantomData<H>,
}

impl<H: ImageHeader> ImageParser<H> {
    /// A parser bounded by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            budget: Budget::new(limits),
            params: None,
            _header: core::marker::PhantomData,
        }
    }
}

impl<H: ImageHeader + 'static> Parser for ImageParser<H> {
    fn parse(&mut self, input: &[u8]) -> Result<(Option<Packet>, usize)> {
        if input.is_empty() {
            return Ok((None, 0));
        }
        if let Some(found) = H::parse(input) {
            if let Some(existing) = &mut self.params {
                existing.fill_from(&found);
            } else {
                self.params = Some(found);
            }
        }
        let mut packet = Packet::from_slice(&mut self.budget, input)?;
        // A still image is always independently decodable — there is no
        // reference frame for one to depend on, in any of the six formats
        // this crate covers.
        packet.flags = PacketFlags::KEY;
        Ok((Some(packet), input.len()))
    }

    fn parameters(&self) -> Option<&CodecParameters> {
        self.params.as_ref()
    }
}
