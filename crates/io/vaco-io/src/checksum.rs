//! Running checksums over consumed bytes.
//!
//! Matroska `CRC-32` elements, MPEG-TS section CRC, PNG chunk CRC and the
//! `crc`/`md5` muxers all want "checksum everything I read from here to there".
//! [`IoContext::start_checksum`](crate::IoContext::start_checksum) opens such a
//! region; every byte subsequently *consumed* is fed in.
//!
//! The implementations are bit-serial rather than table-driven. Regions are
//! small (a TS section is at most 1021 bytes, an EBML element header a
//! handful), the tables would be the only indexing in the crate, and the
//! polynomials are the ones the formats specify.

/// Which checksum a region accumulates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ChecksumKind {
    /// CRC-32/ISO-HDLC, the reflected `0xEDB8_8320` form used by zlib, PNG and
    /// Matroska's `CRC-32` element.
    Crc32Ieee,
    /// CRC-32/MPEG-2: forward `0x04C1_1DB7`, init all-ones, no final xor. The
    /// MPEG-TS PSI section CRC.
    Crc32Mpeg2,
    /// Adler-32 (RFC 1950).
    Adler32,
}

/// A checksum in progress.
#[derive(Debug, Clone, Copy)]
pub struct Checksum {
    kind: ChecksumKind,
    state: u32,
    /// Position at which the region started, for diagnostics.
    start: u64,
}

impl Checksum {
    /// Open a region at `start`.
    #[must_use]
    pub const fn new(kind: ChecksumKind, start: u64) -> Self {
        let state = match kind {
            ChecksumKind::Crc32Ieee | ChecksumKind::Crc32Mpeg2 => 0xFFFF_FFFF,
            ChecksumKind::Adler32 => 1,
        };
        Self { kind, state, start }
    }

    /// The kind being accumulated.
    #[must_use]
    pub const fn kind(&self) -> ChecksumKind {
        self.kind
    }

    /// The position the region started at.
    #[must_use]
    pub const fn start(&self) -> u64 {
        self.start
    }

    /// Feed consumed bytes.
    pub fn update(&mut self, data: &[u8]) {
        self.state = match self.kind {
            ChecksumKind::Crc32Ieee => crc32_reflected(self.state, data),
            ChecksumKind::Crc32Mpeg2 => crc32_forward(self.state, data),
            ChecksumKind::Adler32 => adler32(self.state, data),
        };
    }

    /// The value so far, with whatever final transform the kind specifies.
    #[must_use]
    pub const fn value(&self) -> u64 {
        let v = match self.kind {
            ChecksumKind::Crc32Ieee => !self.state,
            ChecksumKind::Crc32Mpeg2 | ChecksumKind::Adler32 => self.state,
        };
        v as u64
    }
}

fn crc32_reflected(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8u8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    crc
}

fn crc32_forward(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc ^= u32::from(b) << 24;
        for _ in 0..8u8 {
            let mask = (crc >> 31).wrapping_neg();
            crc = (crc << 1) ^ (0x04C1_1DB7 & mask);
        }
    }
    crc
}

fn adler32(state: u32, data: &[u8]) -> u32 {
    // 65521, the largest prime below 2^16 (RFC 1950 §9).
    const BASE: u32 = 65_521;
    let mut a = state & 0xFFFF;
    let mut b = (state >> 16) & 0xFFFF;
    for &byte in data {
        a = (a + u32::from(byte)) % BASE;
        b = (b + a) % BASE;
    }
    (b << 16) | a
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use super::*;

    #[test]
    fn crc32_ieee_matches_known_vectors() {
        // The standard "check" value: CRC-32/ISO-HDLC of "123456789".
        let mut c = Checksum::new(ChecksumKind::Crc32Ieee, 0);
        c.update(b"123456789");
        assert_eq!(c.value(), 0xCBF4_3926);
    }

    #[test]
    fn crc32_mpeg2_matches_known_vectors() {
        let mut c = Checksum::new(ChecksumKind::Crc32Mpeg2, 0);
        c.update(b"123456789");
        assert_eq!(c.value(), 0x0376_E6E7);
    }

    #[test]
    fn adler32_matches_known_vectors() {
        let mut c = Checksum::new(ChecksumKind::Adler32, 0);
        c.update(b"123456789");
        assert_eq!(c.value(), 0x091E_01DE);
    }

    #[test]
    fn split_updates_equal_one_update() {
        for kind in [
            ChecksumKind::Crc32Ieee,
            ChecksumKind::Crc32Mpeg2,
            ChecksumKind::Adler32,
        ] {
            let data: Vec<u8> = (0..=255u8).collect();
            let mut whole = Checksum::new(kind, 0);
            whole.update(&data);
            let mut split = Checksum::new(kind, 0);
            for chunk in data.chunks(7) {
                split.update(chunk);
            }
            assert_eq!(whole.value(), split.value(), "{kind:?}");
        }
    }
}
