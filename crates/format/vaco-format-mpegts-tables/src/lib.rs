//! MPEG-TS packet framing and PSI/SI table parsing.
//!
//! The shared layer under `vaco-demux-mpegts`, `vaco-mux-mpegts`, `rtp_mpegts`
//! and HLS/DASH segmenting to TS. It registers nothing itself — it is a helper
//! crate, like `vaco-format-isom`.
//!
//! # What is in here
//!
//! | Module | Contents |
//! |---|---|
//! | [`packet`] | the 188-byte transport packet, adaptation field, PCR, stride detection |
//! | [`crc`] | the MPEG-2 section CRC-32, and only that |
//! | [`section`] | PSI section framing: `pointer_field`, packets that span, the reassembler |
//! | [`descriptor`] | the descriptor loop and the dozen descriptors that change output |
//! | [`psi`] | PAT, PMT, CAT, SDT |
//! | [`stream_type`] | `stream_type` × registration identifier → codec |
//! | [`text`] | DVB text decoding (ISO 6937 and friends) |
//!
//! # The two properties everything here holds to
//!
//! **No I/O.** Every entry point takes a `&[u8]` a caller already has. That is
//! what lets the whole PSI layer be fuzzed without a file, and it is why
//! `vaco-demux-mpegts` owns the sync-byte search rather than this crate.
//!
//! **No allocation from a length field.** The one buffer in the crate is
//! [`section::SectionAssembler`]'s fixed 4 KiB array, sized by the twelve-bit
//! `section_length` field's own ceiling rather than by anything an input
//! declares. Everything else is a borrowed view. There is therefore no input
//! to this crate that can make it allocate, which removes plan 13 §2.2.2's
//! dominant bug class from the section layer by construction rather than by
//! bounding it.
//!
//! ```
//! use vaco_format_mpegts_tables::packet::{TsPacket, PacketStride};
//! use vaco_format_mpegts_tables::psi::Pat;
//! use vaco_format_mpegts_tables::section::{Section, SectionAssembler};
//!
//! // One transport packet carrying a complete PAT.
//! let mut buf = [0xFFu8; 188];
//! buf[..4].copy_from_slice(&[0x47, 0x40, 0x00, 0x10]);   // PUSI, PID 0, payload only
//! let pat_section: [u8; 16] = [
//!     0x00, 0xB0, 0x0D, 0x00, 0x01, 0xC1, 0x00, 0x00,
//!     0x00, 0x01, 0xE1, 0x00, 0xE8, 0xF9, 0x5E, 0x7D,
//! ];
//! buf[4] = 0;                                            // pointer_field
//! buf[5..5 + 16].copy_from_slice(&pat_section);
//!
//! let Some(pkt) = TsPacket::parse(&buf) else { unreachable!() };
//! let mut asm = SectionAssembler::new();
//! let mut programs = Vec::new();
//! asm.push(pkt.header.payload_unit_start, pkt.payload, |raw| {
//!     if let Some(s) = Section::new(raw)
//!         && let Some(pat) = Pat::parse(&s)
//!     {
//!         programs.extend(pat.entries().map(|e| (e.program_number, e.pid)));
//!     }
//! });
//! assert_eq!(programs, vec![(1, 0x100)]);
//! assert_eq!(PacketStride::Ts.stride(), 188);
//! ```

#![forbid(unsafe_code)]

pub mod crc;
pub mod descriptor;
pub mod packet;
pub mod psi;
pub mod section;
pub mod stream_type;
pub mod text;

pub use crc::{crc32, section_crc_ok};
pub use descriptor::{Descriptor, DescriptorIter, Iso639Entry, SubtitlingEntry, TeletextEntry};
pub use packet::{
    AdaptationField, PCR_HZ, PTS_HZ, PacketStride, Pcr, TS_PACKET_SIZE, TS_WRAP_BITS, TsHeader,
    TsPacket, find_stride,
};
pub use psi::{Cat, Pat, PatEntry, Pmt, PmtStream, Sdt, SdtService};
pub use section::{Section, SectionAssembler, SectionHeader};
pub use stream_type::{Resolved, TsCodec, resolve};

/// The presentation time base every MPEG-TS stream uses: 1/90000.
///
/// Fixed by the format, not chosen: PTS and DTS are 33-bit counters of 90 kHz
/// ticks, so a demuxer that reported anything else would be rescaling for no
/// reason and losing exactness doing it.
pub const TIME_BASE: vaco_core::Rational = vaco_core::Rational::new(1, 90_000);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_time_base_is_the_one_the_format_fixes() {
        assert_eq!(TIME_BASE.num, 1);
        assert_eq!(TIME_BASE.den, 90_000);
        assert_eq!(i64::from(TIME_BASE.den), packet::PTS_HZ);
    }

    #[test]
    fn the_pcr_clock_is_an_exact_multiple_of_the_presentation_clock() {
        assert_eq!(packet::PCR_HZ, packet::PTS_HZ * packet::PCR_EXT_PER_TICK);
    }
}
