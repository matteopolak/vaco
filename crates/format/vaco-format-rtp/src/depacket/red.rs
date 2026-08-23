//! RFC 2198: redundant audio data (`a=rtpmap:<pt> red/<rate>`).
//!
//! `RED` is not a codec — it is a wrapper that carries the current frame
//! plus one or more *older* copies of previous frames (at different payload
//! types, in principle) ahead of it, so a receiver can recover a frame lost
//! to a single dropped packet. This module extracts only the **primary**
//! (most recent, always-last) block and hands it to the wrapped
//! depacketiser for the primary encoding; the redundant copies are simply
//! discarded rather than used for loss concealment — implementing recovery
//! would mean buffering and re-ordering output across calls, which nothing
//! in this workspace's demuxer pipeline currently has a slot for.

use vaco_core::{Error, Result};

use super::Depacketizer;

/// RFC 2198 §3: strips every redundant block, keeping only the primary
/// one, and forwards it to `inner`.
pub struct RedDepacketizer {
    inner: Box<dyn Depacketizer>,
}

impl std::fmt::Debug for RedDepacketizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedDepacketizer").finish_non_exhaustive()
    }
}

impl RedDepacketizer {
    #[must_use]
    pub fn new(inner: Box<dyn Depacketizer>) -> Self {
        Self { inner }
    }
}

impl Depacketizer for RedDepacketizer {
    fn push(&mut self, marker: bool, timestamp: u32, payload: &[u8]) -> Result<Option<Vec<u8>>> {
        let mut redundant_len_total = 0usize;
        let mut rest = payload;
        loop {
            let first = *rest.first().ok_or(Error::InvalidData(
                "RTP RED payload ran out while reading a header",
            ))?;
            if first & 0x80 == 0 {
                // Primary block header: 1 byte, no length (runs to the end).
                rest = rest.get(1..).ok_or(Error::InvalidData(
                    "RTP RED payload has no primary header byte",
                ))?;
                break;
            }
            let more: [u8; 3] =
                rest.get(1..4)
                    .and_then(|s| s.try_into().ok())
                    .ok_or(Error::InvalidData(
                        "RTP RED redundant header runs past the payload",
                    ))?;
            let block_len = usize::from(u16::from_be_bytes([more[1] & 0x03, more[2]]));
            redundant_len_total = redundant_len_total
                .checked_add(block_len)
                .ok_or(Error::InvalidData("RTP RED block length overflows"))?;
            rest = rest.get(4..).ok_or(Error::InvalidData(
                "RTP RED payload ran out after a redundant header",
            ))?;
        }
        let data = rest.get(redundant_len_total..).ok_or(Error::InvalidData(
            "RTP RED redundant block lengths exceed the payload",
        ))?;
        self.inner.push(marker, timestamp, data)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use crate::depacket::raw::Identity;

    #[test]
    fn extracts_primary_block_with_no_redundancy() {
        let mut d = RedDepacketizer::new(Box::new(Identity));
        let mut payload = vec![0x00u8]; // primary header, PT low bits ignored
        payload.extend_from_slice(b"primary-data");
        assert_eq!(
            d.push(true, 0, &payload).unwrap(),
            Some(b"primary-data".to_vec())
        );
    }

    #[test]
    fn skips_one_redundant_block() {
        let mut d = RedDepacketizer::new(Box::new(Identity));
        // Redundant header: F=1, PT=0, timestamp offset=0, block length=4.
        let mut payload = vec![0x80u8, 0x00, 0x00, 0x04];
        payload.push(0x00); // primary header
        payload.extend_from_slice(b"redx"); // redundant block (4 bytes)
        payload.extend_from_slice(b"primary"); // primary block
        let out = d.push(true, 0, &payload).unwrap().unwrap();
        assert_eq!(out, b"primary".to_vec());
    }

    proptest::proptest! {
        #[test]
        fn push_never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256)) {
            let mut d = RedDepacketizer::new(Box::new(Identity));
            let _ = d.push(true, 0, &bytes);
        }
    }
}
