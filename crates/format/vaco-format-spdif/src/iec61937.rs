//! The burst header IEC 61937 (S/PDIF) and SMPTE 337M share.
//!
//! IEC 61937 is, structurally, a specific 16-bit-word profile of SMPTE
//! 337M's non-PCM-in-PCM burst encapsulation — both wrap a compressed audio
//! frame as `Pa Pb Pc Pd <payload> <zero padding>` inside what looks like a
//! PCM stream, using the same sync words and the same `Pc`/`Pd` fields. That
//! shared shape is what lives here; `spdif.rs` and `s337m.rs` each add only
//! what is specific to their own byte width and supported data types.
//!
//! # What is measured, and against what
//!
//! Every constant below was read directly off real bytes:
//! `ffmpeg -i in.ac3 -c copy -bitexact out.spdif` (ffmpeg 8.1), for AC-3 at
//! two different bitrates and two different PCM sample rates. See
//! `spdif.rs`'s module docs for the numbers.

use vaco_core::{Error, Result};

/// First sync word. Written/read as a plain `u16` in the stream's chosen
/// byte order — a `big_endian` flag, not a byte pattern of its own; see
/// [`BurstHeader::parse`]/[`BurstHeader::write`].
pub const PA: u16 = 0xF872;
/// Second sync word.
pub const PB: u16 = 0x4E1F;

/// `Pa`, `Pb`, `Pc`, `Pd`: four 16-bit words, always.
pub const HEADER_LEN: usize = 8;

/// IEC 61937 data-type 1: AC-3. The only data type this crate can verify
/// end-to-end, because it is the only one `ffmpeg -f spdif`'s own muxer
/// (its `Default audio codec: ac3`) and demuxer both round-trip.
pub const DATA_TYPE_AC3: u16 = 1;

/// This module deliberately has no `Endian` type of its own — `vaco-scale`
/// already defines one (`vaco_scale::geometry::Endian`, "byte order of a
/// multi-byte container"), which is the same universal concept a second
/// enum here would just restate under a different name (D19). Rather than
/// pull a video-scaling crate into an audio container crate's dependency
/// graph for one two-variant enum, every byte-order-sensitive function here
/// takes a plain `big_endian: bool` — measured to matter in exactly one
/// place (see `spdif.rs`'s `-spdif_flags be` docs), which a bare `bool`
/// says as plainly as a named type would.
const fn read_u16(b: [u8; 2], big_endian: bool) -> u16 {
    if big_endian {
        u16::from_be_bytes(b)
    } else {
        u16::from_le_bytes(b)
    }
}

const fn write_u16(v: u16, big_endian: bool) -> [u8; 2] {
    if big_endian { v.to_be_bytes() } else { v.to_le_bytes() }
}

/// The four-word burst preamble.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BurstHeader {
    pub pc: u16,
    pub pd: u16,
}

impl BurstHeader {
    /// Parse `Pa Pb Pc Pd` from the first 8 bytes of `buf`, with 16-bit
    /// words in big-endian order if `big_endian` else little-endian. `None`
    /// if the sync words do not match — a normal "not a burst here" answer,
    /// not a parse error, since a demuxer scans forward through padding
    /// looking for the next one.
    #[must_use]
    pub fn parse(buf: &[u8], big_endian: bool) -> Option<Self> {
        let pa = read_u16(buf.get(0..2)?.try_into().ok()?, big_endian);
        let pb = read_u16(buf.get(2..4)?.try_into().ok()?, big_endian);
        if pa != PA || pb != PB {
            return None;
        }
        let pc = read_u16(buf.get(4..6)?.try_into().ok()?, big_endian);
        let pd = read_u16(buf.get(6..8)?.try_into().ok()?, big_endian);
        Some(Self { pc, pd })
    }

    /// Serialise `Pa Pb Pc Pd`, big-endian words if `big_endian` else
    /// little-endian.
    #[must_use]
    pub fn write(self, big_endian: bool) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[0..2].copy_from_slice(&write_u16(PA, big_endian));
        out[2..4].copy_from_slice(&write_u16(PB, big_endian));
        out[4..6].copy_from_slice(&write_u16(self.pc, big_endian));
        out[6..8].copy_from_slice(&write_u16(self.pd, big_endian));
        out
    }

    /// The low 7 bits of `Pc`: the data-type field. Bit 15 (error flag) and
    /// the stream-number/data-type-dependent bits above bit 6 are not
    /// interpreted by this crate — every burst measured while writing it
    /// (AC-3, MPEG-1 layer 2/3, DTS, E-AC-3) had them all zero.
    #[must_use]
    pub const fn data_type(self) -> u16 {
        self.pc & 0x7F
    }

    /// AC-3's payload length in bytes. Measured: `Pd` counts **bits** for
    /// this data type — a 192 kb/s and a 384 kb/s AC-3 elementary stream at
    /// 48 kHz produced `Pd` = 6144 and 12288, which are exactly 8× their
    /// real frame sizes (768 and 1536 bytes). Not assumed to generalise to
    /// any other data type — see the module docs.
    ///
    /// # Errors
    /// [`Error::InvalidData`] if `data_type()` is not [`DATA_TYPE_AC3`].
    pub const fn ac3_payload_len_bytes(self) -> Result<usize> {
        if self.data_type() != DATA_TYPE_AC3 {
            return Err(Error::InvalidData(
                "iec61937: not an AC-3 burst, cannot use the bits-not-bytes Pd convention",
            ));
        }
        Ok((self.pd as usize) >> 3)
    }
}

/// Read `payload_len` payload bytes out of `buf` (which must be at least
/// that long), undoing the 16-bit word byte-swap the reference applies by
/// default (see `spdif.rs`'s module docs for why AC-3's own sync word
/// `0x0B77` appears in the burst as bytes `77 0B`, not `0B 77`).
///
/// A trailing odd byte, if `payload_len` is odd, is copied through
/// unswapped — this never triggers for AC-3 (its frame sizes are always
/// even), but a partial word does not warrant a panic.
#[must_use]
pub fn unswap_payload(buf: &[u8], payload_len: usize) -> Vec<u8> {
    // No `Vec::with_capacity`: `clippy.toml` bans it workspace-wide so every
    // sized allocation goes through `vaco_limits::Budget` instead. This one
    // does not need that — `payload_len` is a `u16`-derived value, at most
    // 8191 bytes, an amount `Vec::push`'s own growth handles with at most a
    // couple of reallocations — so a plain `Vec::new()` is the honest
    // reflection of "this is not the allocation the budget exists to guard".
    let mut out = Vec::new();
    let mut i = 0usize;
    while i.saturating_add(1) < payload_len {
        if let (Some(&a), Some(&b)) = (buf.get(i), buf.get(i + 1)) {
            out.push(b);
            out.push(a);
        }
        i = i.saturating_add(2);
    }
    if payload_len % 2 == 1
        && let Some(&last) = buf.get(payload_len - 1)
    {
        out.push(last);
    }
    out
}

/// The inverse of [`unswap_payload`]: pack elementary-stream bytes into the
/// swapped-16-bit-word form the reference writes by default.
#[must_use]
pub fn swap_payload(data: &[u8]) -> Vec<u8> {
    // Same transform in both directions: swap every adjacent pair.
    unswap_payload(data, data.len())
}
