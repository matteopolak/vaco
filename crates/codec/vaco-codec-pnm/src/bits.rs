//! MSB-first 1-bit-per-pixel packing, shared by PBM's raw raster and PAM's
//! `BLACKANDWHITE` tuple type (which the reference decodes into the same
//! bit-packed pixel format even though PAM's own raster is byte-per-sample —
//! see `vaco-codec-pnm`'s crate docs).

use vaco_core::{Error, Result};

/// Bytes needed for one row of `width` 1-bit samples, padded to a whole byte.
pub(crate) const fn row_bytes_for_bits(width: u32) -> usize {
    (width as usize).div_ceil(8)
}

/// Set bit `x` of row `y` in a buffer strided by `stride`.
pub(crate) fn set_bit(
    buf: &mut [u8],
    stride: usize,
    y: usize,
    x: usize,
    value: bool,
) -> Result<()> {
    let byte_off = y.saturating_mul(stride).saturating_add(x >> 3);
    let slot = buf
        .get_mut(byte_off)
        .ok_or(Error::InvalidData("pnm: bit out of bounds"))?;
    let mask = 0x80u8 >> (x % 8);
    if value {
        *slot |= mask;
    } else {
        *slot &= !mask;
    }
    Ok(())
}

/// Read bit `x` of row `y` in a buffer strided by `stride`.
pub(crate) fn get_bit(buf: &[u8], stride: usize, y: usize, x: usize) -> Result<bool> {
    let byte_off = y.saturating_mul(stride).saturating_add(x >> 3);
    let byte = buf
        .get(byte_off)
        .copied()
        .ok_or(Error::InvalidData("pnm: bit out of bounds"))?;
    let mask = 0x80u8 >> (x % 8);
    Ok(byte & mask != 0)
}
