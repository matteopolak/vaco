//! The 16-byte packet header every GXF packet shares (SMPTE 360-2009 clause
//! 6, Table 2): a fixed 5-byte leader, a type byte, a 4-byte big-endian
//! total length (header included), 4 reserved bytes, and a fixed 2-byte
//! trailer.
//!
//! ```text
//! offset  value        usage
//! 0x00    00 00 00 00  packet leader
//! 0x04    01
//! 0x05    variable     packet type (Table 2)
//! 0x06    variable     packet length, MSB first, header included
//! 0x0A    00 00 00 00  reserved
//! 0x0E    E1 E2        packet trailer
//! ```
//!
//! Measured directly against a real `ffmpeg -f gxf` file this session
//! (`tests/fixtures/ffmpeg_pal_mpeg2_pcm.gxf`) byte-for-byte before writing
//! this, in addition to the published standard.

use vaco_core::{Error, Result};
use vaco_io::IoContext;

/// Map packet: metadata about the tracks and the material.
pub const PKT_MAP: u8 = 0xBC;
/// Media packet: one field/frame of video, or a vector of audio or time
/// code samples.
pub const PKT_MEDIA: u8 = 0xBF;
/// Field locator table: a coarse seek index. Optional; this crate reads it
/// only well enough to skip it (see the crate's top-level docs).
pub const PKT_FLT: u8 = 0xFC;
/// Unified material format: a redundant, complete restatement of the
/// material, required by the Standard but not needed to demux a stream
/// whose MAP/MEDIA packets are already being read directly.
pub const PKT_UMF: u8 = 0xFD;
/// End of stream: header only, no payload.
pub const PKT_EOS: u8 = 0xFB;

/// Largest single packet this crate will read into memory. A real-world
/// video frame is a few hundred KB at most (`ffmpeg -f gxf`'s own MPEG-2
/// packets in this session's fixtures top out under 40 KB); an FLT is
/// capped by the Standard itself at 1000 entries (~8 KB); an audio packet
/// is a fixed 65,536 bytes (32,768 16-bit samples). 64 MiB is generous
/// headroom over all of these while still refusing a declared-length
/// attack before it is read, the same posture
/// `vaco-demux-mxf::MAX_PACKET_BYTES` takes for the analogous risk.
pub const MAX_PACKET_BYTES: u64 = 64 << 20;

/// A parsed packet header, positioned just after the trailer (i.e. at the
/// start of the payload).
#[derive(Debug, Clone, Copy)]
pub struct PacketHeader {
    pub packet_type: u8,
    /// Total packet length in bytes, header included. Always `>= 16` —
    /// checked here, not left for a caller to discover via underflow.
    pub length: u64,
}

impl PacketHeader {
    /// Bytes remaining after the header: what a caller should read (for
    /// `MAP`/`MEDIA`/`FLT`/`UMF`) or skip (for a packet type it does not
    /// interpret).
    #[must_use]
    pub const fn payload_len(&self) -> u64 {
        self.length - 16
    }
}

/// Read one packet header from `io`, leaving the cursor at the start of its
/// payload.
///
/// # Errors
/// [`Error::InvalidData`] if the leader or trailer bytes do not match the
/// Standard's fixed values, or the declared length is less than the header
/// size. [`Error::LimitExceeded`] past [`MAX_PACKET_BYTES`]. Propagates
/// [`Error::UnexpectedEof`] at genuine end of file (the normal way a GXF
/// reader without a trailing `EOS` packet notices it is done).
pub fn read_header(io: &mut IoContext) -> Result<PacketHeader> {
    let mut leader = [0u8; 5];
    io.read_exact(&mut leader)?;
    if leader != [0x00, 0x00, 0x00, 0x00, 0x01] {
        return Err(Error::InvalidData(
            "gxf: packet leader does not match the fixed 00 00 00 00 01",
        ));
    }
    let packet_type = io.r8()?;
    let length = u64::from(io.rb32()?);
    io.skip(4)?; // reserved
    let mut trailer = [0u8; 2];
    io.read_exact(&mut trailer)?;
    if trailer != [0xE1, 0xE2] {
        return Err(Error::InvalidData(
            "gxf: packet trailer does not match the fixed E1 E2",
        ));
    }
    if length < 16 {
        return Err(Error::InvalidData(
            "gxf: packet length is shorter than its own 16-byte header",
        ));
    }
    if length > MAX_PACKET_BYTES {
        return Err(Error::LimitExceeded {
            limit: "gxf_packet_bytes",
            requested: length,
            cap: MAX_PACKET_BYTES,
        });
    }
    Ok(PacketHeader {
        packet_type,
        length,
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_io::{IoOptions, MemorySource};

    fn header_bytes(packet_type: u8, length: u32) -> Vec<u8> {
        let mut v = vec![0x00, 0x00, 0x00, 0x00, 0x01, packet_type];
        v.extend_from_slice(&length.to_be_bytes());
        v.extend_from_slice(&[0, 0, 0, 0]);
        v.extend_from_slice(&[0xE1, 0xE2]);
        v
    }

    #[test]
    fn reads_a_map_header_matching_the_real_fixture() {
        // The exact 16 bytes at offset 0 of `ffmpeg_pal_mpeg2_pcm.gxf`.
        let bytes = header_bytes(PKT_MAP, 352);
        let mut io =
            IoContext::new(Box::new(MemorySource::new(bytes)), &IoOptions::default()).unwrap();
        let h = read_header(&mut io).unwrap();
        assert_eq!(h.packet_type, PKT_MAP);
        assert_eq!(h.length, 352);
        assert_eq!(h.payload_len(), 336);
    }

    #[test]
    fn a_bad_leader_is_rejected() {
        let mut bytes = header_bytes(PKT_MEDIA, 16);
        bytes[4] = 0x02;
        let mut io =
            IoContext::new(Box::new(MemorySource::new(bytes)), &IoOptions::default()).unwrap();
        assert!(read_header(&mut io).is_err());
    }

    #[test]
    fn a_bad_trailer_is_rejected() {
        let mut bytes = header_bytes(PKT_EOS, 16);
        bytes[15] = 0x00;
        let mut io =
            IoContext::new(Box::new(MemorySource::new(bytes)), &IoOptions::default()).unwrap();
        assert!(read_header(&mut io).is_err());
    }

    #[test]
    fn a_length_shorter_than_the_header_is_rejected() {
        let bytes = header_bytes(PKT_EOS, 10);
        let mut io =
            IoContext::new(Box::new(MemorySource::new(bytes)), &IoOptions::default()).unwrap();
        assert!(matches!(read_header(&mut io), Err(Error::InvalidData(_))));
    }

    #[test]
    fn an_oversized_length_is_rejected_before_allocating() {
        let bytes = header_bytes(PKT_MEDIA, u32::MAX);
        let mut io =
            IoContext::new(Box::new(MemorySource::new(bytes)), &IoOptions::default()).unwrap();
        assert!(matches!(
            read_header(&mut io),
            Err(Error::LimitExceeded { .. })
        ));
    }
}
