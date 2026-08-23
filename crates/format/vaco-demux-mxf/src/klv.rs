//! Reading one KLV (Key-Length-Value) triplet off an [`IoContext`].
//!
//! This is the layer that turns the [`ber`](crate::ber) decoder — which knows
//! nothing about I/O — into something that can walk a real file. Every method
//! here is bounded: a header's declared length is checked against a cap
//! *before* anything sized by it is allocated, per `vaco-limits`' two-phase
//! reservation model. That is the single most load-bearing property of this
//! module, because every layer above it (partitions, the primer, structural
//! metadata, index tables) trusts a length field that came straight from the
//! file.

use vaco_core::{Error, Result};
use vaco_io::IoContext;
use vaco_limits::Budget;

use crate::ber;
use crate::ul::Ul;

/// A KLV header: the key, the decoded value length, and where the value
/// starts. Reading the value itself is a separate, explicitly bounded step
/// ([`read_value`]) so a caller can decide *whether* to read it at all —
/// essence elements are usually skipped-not-read until a packet is actually
/// requested.
#[derive(Debug, Clone, Copy)]
pub struct KlvHeader {
    pub key: Ul,
    pub length: u64,
    /// Absolute file offset of the key's first byte.
    pub offset: u64,
    /// Absolute file offset of the value's first byte.
    pub value_offset: u64,
}

impl KlvHeader {
    /// Absolute file offset one past the value's last byte — where the next
    /// KLV triplet, if any, begins.
    #[must_use]
    pub const fn end(&self) -> u64 {
        self.value_offset.saturating_add(self.length)
    }
}

/// Read one KLV header: a 16-byte key, then a BER length.
///
/// Does not read the value. On success, `io`'s position is exactly
/// `header.value_offset`.
///
/// # Errors
///
/// [`Error::UnexpectedEof`] if the key or length prefix is truncated.
/// [`Error::InvalidData`] for a malformed BER length (see [`ber::decode`]).
pub fn read_header(io: &mut IoContext) -> Result<KlvHeader> {
    let offset = io.pos();
    let mut key = [0u8; 16];
    io.read_exact(&mut key)?;
    // Peek enough for the widest possible BER length prefix, then advance by
    // exactly what it consumed. `peek` returns fewer bytes than asked only at
    // EOF, which `ber::decode` reports as `UnexpectedEof` on its own — a
    // length prefix genuinely truncated by end-of-file is indistinguishable
    // from one that is merely short, and both are the same error.
    let probe = io.peek(ber::MAX_ENCODED_LEN)?;
    let decoded = ber::decode(probe)?;
    io.skip(decoded.consumed as u64)?;
    let value_offset = io.pos();
    Ok(KlvHeader {
        key: Ul::new(key),
        length: decoded.value,
        offset,
        value_offset,
    })
}

/// Read a header's value fully into memory, charging `budget` for it.
///
/// This is the one place in the crate where a length that came from the file
/// turns into an allocation, so it is the one place that has to get the
/// bound right: `header.length` is checked against `max_value_bytes` (an
/// explicit ceiling the caller picks for *this* kind of set — a primer pack
/// and a header-metadata local set have very different plausible sizes) and
/// against `budget`'s own caps, before a single byte is read.
///
/// # Errors
///
/// [`Error::LimitExceeded`] if `header.length` exceeds `max_value_bytes` or a
/// `budget` cap. [`Error::UnexpectedEof`] if the file has fewer bytes than
/// `header.length` claims.
pub fn read_value(
    io: &mut IoContext,
    budget: &mut Budget,
    header: &KlvHeader,
    max_value_bytes: u64,
) -> Result<Vec<u8>> {
    if header.length > max_value_bytes {
        return Err(Error::LimitExceeded {
            limit: "mxf_klv_value_bytes",
            requested: header.length,
            cap: max_value_bytes,
        });
    }
    let n = usize::try_from(header.length).map_err(|_| Error::LimitExceeded {
        limit: "mxf_klv_value_bytes",
        requested: header.length,
        cap: max_value_bytes,
    })?;
    let mut buf = budget.alloc::<u8>(n)?;
    io.read_exact(&mut buf)?;
    Ok(buf)
}

/// Advance past a header's value without reading it.
///
/// # Errors
/// [`Error::UnexpectedEof`] if the file is shorter than `header.length`.
pub fn skip_value(io: &mut IoContext, header: &KlvHeader) -> Result<()> {
    io.skip(header.length)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::MemorySource;
    use vaco_limits::Limits;

    fn ctx(bytes: Vec<u8>) -> IoContext {
        IoContext::new(
            Box::new(MemorySource::new(bytes)),
            &vaco_io::IoOptions::default(),
        )
        .unwrap()
    }

    #[test]
    fn reads_a_short_form_header() {
        let mut bytes = vec![0xAAu8; 16];
        bytes.push(0x05); // short-form length 5
        bytes.extend_from_slice(b"hello");
        let mut io = ctx(bytes);
        let h = read_header(&mut io).unwrap();
        assert_eq!(h.key.as_bytes(), [0xAA; 16]);
        assert_eq!(h.length, 5);
        assert_eq!(h.offset, 0);
        assert_eq!(h.value_offset, 17);
        let mut budget = Budget::new(Limits::strict());
        let v = read_value(&mut io, &mut budget, &h, 1024).unwrap();
        assert_eq!(v, b"hello");
    }

    #[test]
    fn reads_a_long_form_header_like_a_real_partition_pack() {
        let mut bytes = vec![0xBBu8; 16];
        bytes.extend_from_slice(&[0x83, 0x00, 0x00, 0x04]); // long form, length 4
        bytes.extend_from_slice(b"abcd");
        let mut io = ctx(bytes);
        let h = read_header(&mut io).unwrap();
        assert_eq!(h.length, 4);
        assert_eq!(h.value_offset, 20);
        assert_eq!(h.end(), 24);
    }

    #[test]
    fn oversized_declared_length_is_rejected_before_allocating() {
        let mut bytes = vec![0xCCu8; 16];
        bytes.extend_from_slice(&[0x88, 0, 0, 0, 0, 0, 0, 0, 0]); // declares u64::MAX-ish width 0
        let mut io = ctx(bytes);
        let h = read_header(&mut io).unwrap();
        assert_eq!(h.length, 0);
        // A header whose declared length exceeds a tiny cap must fail before
        // any read is attempted, not merely fail the subsequent read.
        let mut bytes2 = vec![0xDDu8; 16];
        bytes2.extend_from_slice(&[0x84, 0x7f, 0xff, 0xff, 0xff]); // length ~2^31
        let mut io2 = ctx(bytes2);
        let h2 = read_header(&mut io2).unwrap();
        let mut budget = Budget::new(Limits::strict());
        let err = read_value(&mut io2, &mut budget, &h2, 1024).unwrap_err();
        assert!(matches!(err, Error::LimitExceeded { .. }));
    }

    #[test]
    fn truncated_key_is_eof_not_a_panic() {
        let mut io = ctx(vec![0u8; 10]);
        assert!(matches!(read_header(&mut io), Err(Error::UnexpectedEof)));
    }
}
