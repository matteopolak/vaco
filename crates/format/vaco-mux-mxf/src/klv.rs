//! Writing one Key-Length-Value triplet.

use vaco_core::Result;
use vaco_io::IoWriter;

use crate::ber;

/// Write `key` (16 bytes), then `value.len()`'s BER length, then `value`.
///
/// # Errors
/// Propagates I/O failure.
pub(crate) fn write(io: &mut IoWriter, key: &[u8; 16], value: &[u8]) -> Result<()> {
    io.write(key)?;
    io.write(ber::encode(value.len() as u64).as_slice())?;
    io.write(value)
}
