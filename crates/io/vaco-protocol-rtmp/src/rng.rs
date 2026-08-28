//! Filler bytes for the handshake's padding.
//!
//! Neither handshake scheme requires the padding to be unpredictable: the
//! plain scheme never inspects it at all, and the digest scheme's security
//! (such as it is — this is a compatibility check, not real encryption key
//! agreement, since neither side of a plain `rtmp:` session does the DH
//! exchange the digest scheme was designed to protect) rests entirely on
//! the HMAC key, not on the padding's entropy. A small deterministic
//! generator avoids a `rand` dependency for content nothing ever
//! authenticates.

/// `SplitMix64` (Steele, Lea & Flood 2014's public-domain generator).
pub(crate) struct Filler(u64);

impl Filler {
    /// Seed from the wall clock where available, a fixed constant on a
    /// platform where it is not (wasm without a clock shim) — either way
    /// this is filler, not a key.
    #[must_use]
    pub(crate) fn new() -> Self {
        let seed = vaco_time::unix_nanos().map_or(0x9e37_79b9_7f4a_7c15, |n| n as u64);
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Fill `buf` with non-cryptographic filler bytes.
    pub(crate) fn fill(&mut self, buf: &mut [u8]) {
        let mut chunks = buf.chunks_mut(8);
        for chunk in &mut chunks {
            let bytes = self.next_u64().to_le_bytes();
            let n = chunk.len().min(bytes.len());
            if let (Some(dst), Some(src)) = (chunk.get_mut(..n), bytes.get(..n)) {
                dst.copy_from_slice(src);
            }
        }
    }
}

impl Default for Filler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn fills_the_whole_buffer() {
        let mut buf = [0u8; 1528];
        Filler::new().fill(&mut buf);
        // Not all-zero: SplitMix64 never returns a long run of zero blocks
        // for a real seed, so this is a smoke test against a no-op fill,
        // not a statistical claim.
        assert!(buf.iter().any(|b| *b != 0));
    }

    #[test]
    fn odd_length_buffer_is_fully_filled() {
        let mut buf = [0u8; 13];
        Filler::new().fill(&mut buf);
        assert!(buf.iter().any(|b| *b != 0));
    }
}
