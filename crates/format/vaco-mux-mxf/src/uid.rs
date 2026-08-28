//! Generating `InstanceUID`s (16 bytes) and Package UMIDs (32 bytes).
//!
//! # What these need to be, and what they do not
//!
//! `InstanceUID` only needs to be unique among the sets this crate writes
//! into *one* file — nothing in `vaco-demux-mxf` or the reference demuxer
//! needs it to be globally unique. A UMID (SMPTE ST 330) conventionally
//! aims for archival, cross-file uniqueness, but this crate's correctness
//! bar (this file's own graph resolves consistently, and a `SourceClip`'s
//! reference matches the Source Package it names) only needs uniqueness
//! *within* the file plus reasonable hygiene *across* files from repeated
//! runs of this muxer. That is why this module generates IDs from a
//! per-file counter mixed with real wall-clock entropy (via `vaco_time`,
//! per D18 — clock access only through that crate) rather than pulling in
//! a random-number-generator dependency this project does not otherwise
//! need (D-constraint: no new external dependencies).
//!
//! # UMID shape
//!
//! A real UMID's first 12 bytes are a fixed "UMID Universal Label" — the
//! same 12 bytes on every real UMID this workspace has measured
//! (`vaco-demux-mxf`'s corpus), since it identifies "this is a SMPTE UMID"
//! rather than anything file-specific. Bytes 12 onward (length, instance
//! number, and the material/generation number) vary per package. This
//! module reuses the measured 12-byte label prefix and fills the remaining
//! 20 bytes from the counter/time mix.

use vaco_time::unix_nanos;

/// The UMID's own fixed 12-byte label, measured off a real `ffmpeg -f mxf`
/// package UMID (`vaco-demux-mxf`'s corpus; the label is a SMPTE-registered
/// constant, not a per-file value, so seeing it once fixes it for every
/// file this crate writes).
const UMID_LABEL_PREFIX: [u8; 12] = [
    0xad, 0xab, 0x44, 0x24, 0x2f, 0x25, 0x4d, 0xc7, 0x92, 0xff, 0x00, 0x0d,
];

/// A per-muxer-instance counter, mixed with wall-clock entropy, so that
/// `InstanceUid`/`Umid` values are unique within one file and vary across
/// separate runs of this muxer without a random-number dependency.
#[derive(Debug)]
pub(crate) struct IdGenerator {
    counter: u32,
    /// Low 64 bits of `vaco_time::unix_nanos()` at construction, or `0` if
    /// the platform cannot answer — degrades to "unique within this file
    /// only", which is still this module's real correctness bar (see
    /// module docs).
    entropy: u64,
}

impl IdGenerator {
    #[must_use]
    pub(crate) fn new() -> Self {
        let entropy = unix_nanos().map_or(0, |n| n as u64);
        Self {
            counter: 0,
            entropy,
        }
    }

    fn next_counter(&mut self) -> u32 {
        let v = self.counter;
        self.counter = self.counter.wrapping_add(1);
        v
    }

    /// A 16-byte `InstanceUID`, distinct from every other one this
    /// generator has produced.
    #[must_use]
    pub(crate) fn instance_uid(&mut self) -> [u8; 16] {
        let n = self.next_counter();
        let mut out = [0u8; 16];
        out[..8].copy_from_slice(&self.entropy.to_be_bytes());
        out[8..12].copy_from_slice(&n.to_be_bytes());
        out
    }

    /// A 32-byte Package UMID: the measured 12-byte label, then a length
    /// byte (`0x13`, matching a real UMID's own value there), then 19
    /// bytes derived from the counter and entropy.
    #[must_use]
    pub(crate) fn package_umid(&mut self) -> [u8; 32] {
        let n = self.next_counter();
        let mut out = [0u8; 32];
        out[..12].copy_from_slice(&UMID_LABEL_PREFIX);
        out[12] = 0x13;
        out[13..21].copy_from_slice(&self.entropy.to_be_bytes());
        out[21..25].copy_from_slice(&n.to_be_bytes());
        out
    }
}

impl Default for IdGenerator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn successive_instance_uids_differ() {
        let mut g = IdGenerator::new();
        let a = g.instance_uid();
        let b = g.instance_uid();
        assert_ne!(a, b);
    }

    #[test]
    fn successive_umids_differ_and_keep_the_measured_label() {
        let mut g = IdGenerator::new();
        let a = g.package_umid();
        let b = g.package_umid();
        assert_ne!(a, b);
        assert_eq!(&a[..12], &UMID_LABEL_PREFIX);
        assert_eq!(&b[..12], &UMID_LABEL_PREFIX);
    }
}
