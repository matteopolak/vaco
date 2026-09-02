//! The Configuration Record's CRC (RFC 9043 §4.3.2/§4.9.3).
//!
//! Non-reflected (MSB-first) CRC-32, generator polynomial `0x04C11DB7`
//! (RFC 9043's `0x104C11DB7` with its degree-32 leading bit dropped), `init =
//! 0`, no post-inversion -- measured against a real `ffmpeg`-encoded FFV1
//! Configuration Record's own trailing 4 bytes, which bring this exact
//! variant to zero (the reflected zlib/gzip/PNG variant that shares the same
//! generator polynomial does not).

fn table() -> [u32; 256] {
    std::array::from_fn(|i| {
        let mut c = u32::try_from(i).unwrap_or(0) << 24;
        let mut j = 0;
        while j < 8 {
            c = if c & 0x8000_0000 != 0 {
                (c << 1) ^ 0x04C1_1DB7
            } else {
                c << 1
            };
            j += 1;
        }
        c
    })
}

/// A fresh CRC over `data` (RFC 9043 §4.9.3: init 0, no post-inversion).
#[must_use]
pub(crate) fn crc32_ffv1(data: &[u8]) -> u32 {
    let t = table();
    let mut c = 0u32;
    for &b in data {
        let idx = (((c >> 24) ^ u32::from(b)) & 0xFF) as usize;
        c = t.get(idx).copied().unwrap_or(0) ^ (c << 8);
    }
    c
}

/// Whether `data`'s trailing 4 bytes are a valid CRC parity for the record
/// (RFC 9043 §4.3.2: the whole record, including the parity, CRCs to zero).
#[must_use]
pub(crate) fn extradata_crc_ok(data: &[u8]) -> bool {
    crc32_ffv1(data) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_length_record_has_a_trivially_ok_crc() {
        assert!(extradata_crc_ok(&[]));
    }

    #[test]
    fn flipping_a_byte_breaks_the_crc() {
        // Appending a payload's own CRC as big-endian parity continues this
        // non-reflected, non-inverted register to exactly zero -- the same
        // property `vaco-codec-ffv1`'s own encoder relies on to build a
        // record's trailing 4 bytes.
        let payload = *b"ffv1-configuration-record";
        let mut data = payload.to_vec();
        data.extend_from_slice(&crc32_ffv1(&payload).to_be_bytes());
        assert!(extradata_crc_ok(&data));

        let mut broken = data.clone();
        if let Some(b) = broken.first_mut() {
            *b ^= 0xFF;
        }
        assert!(!extradata_crc_ok(&broken));
    }
}
