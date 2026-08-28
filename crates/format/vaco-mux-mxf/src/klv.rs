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

/// As [`write`], but with `ber::encode_minimal`'s length prefix (short form
/// when possible, else the smallest long form) instead of the fixed
/// 4-/8-byte form — the convention a real file uses for the Primer Pack,
/// the Random Index Pack, and most of the structural-metadata graph
/// (`ber.rs`'s own module docs have the measurement).
///
/// # Errors
/// Propagates I/O failure.
pub(crate) fn write_minimal(io: &mut IoWriter, key: &[u8; 16], value: &[u8]) -> Result<()> {
    io.write(key)?;
    io.write(ber::encode_minimal(value.len() as u64).as_slice())?;
    io.write(value)
}

/// Write one structural-metadata set (`ul::structural_set_key`'s own key
/// shape), choosing [`write`] or [`write_minimal`] by the set's own class
/// byte (`key[14]`) — measured against two real fixtures (`ber.rs`'s module
/// docs): every essence descriptor class (`MPEGVideoDescriptor`,
/// `AES3PCMDescriptor`, and, grouped with the other two rather than
/// independently re-checked, D-10's `CDCIEssenceDescriptor`) keeps this
/// crate's fixed-width form; every other structural set — `Preface`,
/// `Identification`, `ContentStorage`, both `Package`s, `Track`, `Sequence`,
/// `SourceClip`, `TimecodeComponent`, `MultipleDescriptor` — uses the
/// minimal form instead.
///
/// # Errors
/// Propagates I/O failure.
pub(crate) fn write_structural_set(io: &mut IoWriter, key: &[u8; 16], value: &[u8]) -> Result<()> {
    let class = key[14];
    if matches!(
        class,
        crate::ul::class::MPEG_VIDEO_DESCRIPTOR
            | crate::ul::class::AES3_PCM_DESCRIPTOR
            | crate::ul::class::CDCI_ESSENCE_DESCRIPTOR
    ) {
        write(io, key, value)
    } else {
        write_minimal(io, key, value)
    }
}

/// Pad the current position out to the next multiple of `kag_size` with one
/// Fill Item KLV — the KLV Alignment Grid convention a real `ffmpeg -f mxf`
/// file uses in its header region (`ul::FILL_ITEM`'s own doc comment has
/// the measurement). `kag_size <= 1` means "no alignment", the same
/// convention this crate's own `PartitionPackFields`/`partition::write`
/// used before KAG support existed.
///
/// If the gap to the next boundary is smaller than the smallest KLV this
/// crate can express (16-byte key + this crate's fixed 4-byte BER length
/// prefix = 20 bytes), padding to *that* boundary is impossible with a
/// single Fill Item; this falls through to the boundary after it instead,
/// which is the same shape a real writer would need whenever alignment and
/// minimum-KLV-size collide — not observed in practice against any real
/// fixture this session, since every real gap measured was comfortably
/// larger than 20 bytes.
///
/// # Errors
/// Propagates I/O failure.
pub(crate) fn pad_to_kag(io: &mut IoWriter, kag_size: u64) -> Result<()> {
    if kag_size <= 1 {
        return Ok(());
    }
    let pos = io.pos();
    let rem = pos % kag_size;
    if rem == 0 {
        return Ok(());
    }
    let mut gap = kag_size - rem;
    if gap < 20 {
        gap += kag_size;
    }
    let value_len = gap - 20;
    let mut value = Vec::new();
    value.extend(std::iter::repeat_n(0u8, value_len as usize));
    write(io, &crate::ul::FILL_ITEM, &value)
}
