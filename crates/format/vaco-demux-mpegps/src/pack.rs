//! Pack headers and system headers (ISO/IEC 11172-1 §2.4.3.2/.3, ISO/IEC
//! 13818-1 §2.5.3.2/.3).
//!
//! A program stream is a sequence of *packs*, each starting with the
//! `pack_start_code` (`0x000001BA`). Two incompatible syntaxes exist for the
//! bytes that follow, distinguished by the top bits of the first byte after
//! the start code:
//!
//! * **MPEG-1** (top nibble `0010`): a fixed 8-byte body — no stuffing field
//!   exists at all, so a fixed 12-byte pack header including the start code.
//!   This is what the reference's `mpeg` and `vcd` muxers write.
//! * **MPEG-2** (top 2 bits `01`): a 10-byte body plus a `stuffing_length`
//!   (0–7) count of `0xFF` stuffing bytes. This is what `vob`, `svcd` and
//!   `dvd` write.
//!
//! Measured against `ffmpeg -f mpeg` / `-f vob` output (2026-08-23): the
//! discriminating bits are exactly as ISO/IEC 13818-1 §2.5.3.3 states, and
//! `vob`/`svcd`/`dvd` pack headers are byte-identical in shape — the
//! difference between those three muxers is fixed pack size and system
//! header content, not pack header syntax.
//!
//! The first pack in a program stream is immediately followed, in every
//! sample measured, by a system header (`0x000001BB`) — optional per spec but
//! universal in practice, and this crate treats a stream lacking a system
//! header before the first PES packet as merely undeclared, never an error.

use vaco_core::{Error, Result};

/// `pack_start_code`.
pub const PACK_START_CODE: [u8; 4] = [0x00, 0x00, 0x01, 0xBA];
/// `system_header_start_code`.
pub const SYSTEM_HEADER_START_CODE: [u8; 4] = [0x00, 0x00, 0x01, 0xBB];
/// `MPEG_program_end_code`, written once by the muxer at end of stream.
pub const PROGRAM_END_CODE: [u8; 4] = [0x00, 0x00, 0x01, 0xB9];
/// `program_stream_map` `stream_id`.
pub const SID_PROGRAM_STREAM_MAP: u8 = 0xBC;

/// The two pack-header syntaxes a program stream can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackSyntax {
    /// ISO/IEC 11172-1 §2.4.3.2: fixed 12 bytes, no `program_mux_rate`
    /// stuffing field, no SCR extension.
    Mpeg1,
    /// ISO/IEC 13818-1 §2.5.3.3: 14 fixed bytes plus 0–7 stuffing bytes, a
    /// 9-bit SCR extension.
    Mpeg2,
}

/// A decoded pack header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackHeader {
    pub syntax: PackSyntax,
    /// System Clock Reference base: 33 bits, 90 kHz.
    pub scr_base: i64,
    /// SCR extension: 9 bits, 27 MHz sub-tick. Always 0 for [`PackSyntax::Mpeg1`].
    pub scr_ext: u16,
    /// `program_mux_rate`: units of 50 bytes/second. 0 is legal and means
    /// "not indicated" in some encoders despite the spec calling it
    /// mandatory — measured on hand-built streams from other tools; the
    /// reference itself always writes a nonzero value.
    pub mux_rate: u32,
    /// Total size of this pack header in bytes, start code included. The
    /// next byte in the stream is the first byte after the header.
    pub len: usize,
}

impl PackHeader {
    /// Parse the pack header at the start of `buf`, which must begin with
    /// [`PACK_START_CODE`].
    ///
    /// Returns `Ok(None)` when `buf` does not hold a complete header yet — a
    /// caller reading incrementally should treat that as "need more input",
    /// not as corruption.
    ///
    /// # Errors
    /// [`Error::InvalidData`] when the start code is missing or the syntax
    /// discriminator bits match neither known form.
    pub fn parse(buf: &[u8]) -> Result<Option<Self>> {
        if buf.len() < 4 {
            return Ok(None);
        }
        if buf.get(..4) != Some(&PACK_START_CODE[..]) {
            return Err(Error::InvalidData("mpegps: missing pack_start_code"));
        }
        let Some(&b0) = buf.get(4) else {
            return Ok(None);
        };
        if b0 & 0xF0 == 0x20 {
            Self::parse_mpeg1(buf)
        } else if b0 & 0xC0 == 0x40 {
            Self::parse_mpeg2(buf)
        } else {
            Err(Error::InvalidData(
                "mpegps: pack header matches neither MPEG-1 nor MPEG-2 syntax",
            ))
        }
    }

    fn parse_mpeg1(buf: &[u8]) -> Result<Option<Self>> {
        const LEN: usize = 12;
        let Some(body) = buf.get(4..LEN) else {
            return Ok(None);
        };
        let scr_base = decode_scr_33_mpeg1(body)?;
        let mux_rate = decode_mux_rate(body.get(5..8).ok_or(Error::UnexpectedEof)?)?;
        Ok(Some(Self {
            syntax: PackSyntax::Mpeg1,
            scr_base,
            scr_ext: 0,
            mux_rate,
            len: LEN,
        }))
    }

    fn parse_mpeg2(buf: &[u8]) -> Result<Option<Self>> {
        const FIXED_LEN: usize = 14;
        let Some(body) = buf.get(4..FIXED_LEN) else {
            return Ok(None);
        };
        let (scr_base, scr_ext) = decode_scr_33_mpeg2(body)?;
        let mux_rate = decode_mux_rate(body.get(6..9).ok_or(Error::UnexpectedEof)?)?;
        let stuffing_len = usize::from(*body.get(9).ok_or(Error::UnexpectedEof)?) & 0x07;
        let total = FIXED_LEN
            .checked_add(stuffing_len)
            .ok_or(Error::InvalidData("mpegps: pack header length overflow"))?;
        if buf.len() < total {
            return Ok(None);
        }
        Ok(Some(Self {
            syntax: PackSyntax::Mpeg2,
            scr_base,
            scr_ext,
            mux_rate,
            len: total,
        }))
    }
}

/// Decode the 33-bit SCR field of an MPEG-1 pack header (ISO/IEC 11172-1
/// §2.4.3.2). `body` is the 8 bytes following the pack start code; only the
/// first five participate.
///
/// Verified against `ffmpeg -f mpeg` output (2026-08-23): decoded SCR is
/// monotonically increasing across consecutive packs at a rate consistent
/// with the encoded `mux_rate`, which a wrong bit split would not produce.
fn decode_scr_33_mpeg1(body: &[u8]) -> Result<i64> {
    let b0 = *body.first().ok_or(Error::UnexpectedEof)?;
    let b1 = *body.get(1).ok_or(Error::UnexpectedEof)?;
    let b2 = *body.get(2).ok_or(Error::UnexpectedEof)?;
    let b3 = *body.get(3).ok_or(Error::UnexpectedEof)?;
    let b4 = *body.get(4).ok_or(Error::UnexpectedEof)?;
    // byte0: '0010'(4) SCR[32:30](3) marker(1)
    // byte1: SCR[29:22](8)
    // byte2: SCR[21:15](7) marker(1)
    // byte3: SCR[14:7](8)
    // byte4: SCR[6:0](7) marker(1)
    let scr32_30 = u64::from((b0 >> 1) & 0x07);
    let scr29_22 = u64::from(b1);
    let scr21_15 = u64::from((b2 >> 1) & 0x7F);
    let scr14_7 = u64::from(b3);
    let scr6_0 = u64::from((b4 >> 1) & 0x7F);
    let v = (scr32_30 << 30) | (scr29_22 << 22) | (scr21_15 << 15) | (scr14_7 << 7) | scr6_0;
    Ok(v.cast_signed())
}

/// Decode the 33-bit SCR base and 9-bit SCR extension of an MPEG-2 pack
/// header (ISO/IEC 13818-1 §2.5.3.3). `body` is the 10 bytes following the
/// pack start code; only the first six participate.
///
/// Verified the same way as [`decode_scr_33_mpeg1`], against `ffmpeg -f vob`.
fn decode_scr_33_mpeg2(body: &[u8]) -> Result<(i64, u16)> {
    let b0 = *body.first().ok_or(Error::UnexpectedEof)?;
    let b1 = *body.get(1).ok_or(Error::UnexpectedEof)?;
    let b2 = *body.get(2).ok_or(Error::UnexpectedEof)?;
    let b3 = *body.get(3).ok_or(Error::UnexpectedEof)?;
    let b4 = *body.get(4).ok_or(Error::UnexpectedEof)?;
    let b5 = *body.get(5).ok_or(Error::UnexpectedEof)?;
    // byte0: '01'(2) SCR[32:30](3) marker(1) SCR[29:28](2)
    // byte1: SCR[27:20](8)
    // byte2: SCR[19:15](5) marker(1) SCR[14:13](2)
    // byte3: SCR[12:5](8)
    // byte4: SCR[4:0](5) marker(1) SCR_ext[8:7](2)
    // byte5: SCR_ext[6:0](7) marker(1)
    let scr32_30 = u64::from((b0 >> 3) & 0x07);
    let scr29_28 = u64::from(b0 & 0x03);
    let scr27_20 = u64::from(b1);
    let scr19_15 = u64::from((b2 >> 3) & 0x1F);
    let scr14_13 = u64::from(b2 & 0x03);
    let scr12_5 = u64::from(b3);
    let scr4_0 = u64::from((b4 >> 3) & 0x1F);
    let scr_ext_hi = u16::from(b4 & 0x03);
    let scr_ext_lo = u16::from(b5 >> 1);
    let v = (scr32_30 << 30)
        | (scr29_28 << 28)
        | (scr27_20 << 20)
        | (scr19_15 << 15)
        | (scr14_13 << 13)
        | (scr12_5 << 5)
        | scr4_0;
    Ok((v.cast_signed(), (scr_ext_hi << 7) | scr_ext_lo))
}

/// Decode `program_mux_rate` from its three-byte field (22 bits, no marker
/// bits inside — only a trailing pair closing the pack header).
fn decode_mux_rate(b: &[u8]) -> Result<u32> {
    let b0 = *b.first().ok_or(Error::UnexpectedEof)?;
    let b1 = *b.get(1).ok_or(Error::UnexpectedEof)?;
    let b2 = *b.get(2).ok_or(Error::UnexpectedEof)?;
    Ok((u32::from(b0) << 14) | (u32::from(b1) << 6) | (u32::from(b2) >> 2))
}

/// One `P-STD` bound entry in a system header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamBound {
    pub stream_id: u8,
    /// `STD_buffer_bound_scale`: unit is 128 bytes when set, 1024 when clear.
    pub buffer_scale: bool,
    pub buffer_size_bound: u16,
}

impl StreamBound {
    /// The buffer bound in bytes.
    #[must_use]
    pub const fn buffer_bytes(&self) -> u32 {
        (self.buffer_size_bound as u32) * if self.buffer_scale { 128 } else { 1024 }
    }
}

/// A decoded system header (§2.5.3.5 / §2.4.3.3).
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent field the spec defines; a state \
              machine or nested enums would not model five orthogonal \
              signalling bits any more clearly"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemHeader {
    pub rate_bound: u32,
    pub audio_bound: u8,
    pub fixed_flag: bool,
    pub csps_flag: bool,
    pub system_audio_lock_flag: bool,
    pub system_video_lock_flag: bool,
    pub video_bound: u8,
    pub packet_rate_restriction_flag: bool,
    pub streams: Vec<StreamBound>,
    /// Total size of this element in bytes, start code and length field
    /// included.
    pub len: usize,
}

impl SystemHeader {
    /// Parse the system header at the start of `buf`, which must begin with
    /// [`SYSTEM_HEADER_START_CODE`].
    ///
    /// No [`vaco_limits::Budget`] is needed here: `header_length` is a 16-bit
    /// field, so the body this reads is bounded to 65535 bytes regardless of
    /// what the caller supplies.
    ///
    /// # Errors
    /// [`Error::InvalidData`] on a missing start code or a malformed
    /// stream-bound entry (the two marker bits at its head are checked,
    /// unlike a PES timestamp's markers, because a system header is parsed
    /// once at open and a wrong sync here misframes every entry after it).
    pub fn parse(buf: &[u8]) -> Result<Option<Self>> {
        if buf.len() < 6 {
            return Ok(None);
        }
        if buf.get(..4) != Some(&SYSTEM_HEADER_START_CODE[..]) {
            return Err(Error::InvalidData(
                "mpegps: missing system_header_start_code",
            ));
        }
        let header_length = usize::from(u16::from_be_bytes([
            *buf.get(4).unwrap_or(&0),
            *buf.get(5).unwrap_or(&0),
        ]));
        let total = 6usize
            .checked_add(header_length)
            .ok_or(Error::InvalidData("mpegps: system header length overflow"))?;
        if buf.len() < total {
            return Ok(None);
        }
        let body = buf.get(6..total).ok_or(Error::UnexpectedEof)?;
        let b0 = *body.first().ok_or(Error::UnexpectedEof)?;
        let b1 = *body.get(1).ok_or(Error::UnexpectedEof)?;
        let b2 = *body.get(2).ok_or(Error::UnexpectedEof)?;
        let rate_bound = (u32::from(b0 & 0x7F) << 15) | (u32::from(b1) << 7) | (u32::from(b2) >> 1);
        let b3 = *body.get(3).ok_or(Error::UnexpectedEof)?;
        let audio_bound = b3 >> 2;
        let fixed_flag = b3 & 0x02 != 0;
        let csps_flag = b3 & 0x01 != 0;
        let b4 = *body.get(4).ok_or(Error::UnexpectedEof)?;
        let system_audio_lock_flag = b4 & 0x80 != 0;
        let system_video_lock_flag = b4 & 0x40 != 0;
        let video_bound = b4 & 0x1F;
        let b5 = *body.get(5).ok_or(Error::UnexpectedEof)?;
        let packet_rate_restriction_flag = b5 & 0x80 != 0;

        let mut streams = Vec::new();
        let mut i = 6;
        while let Some(&sid) = body.get(i) {
            // A stream-bound entry's `stream_id` byte has its top bit set
            // (`'1'` per the syntax table); the program_stream_map and any
            // other trailing field do not, and mark the end of the list.
            if sid & 0x80 == 0 {
                break;
            }
            let e1 = *body.get(i + 1).ok_or(Error::InvalidData(
                "mpegps: truncated system header stream-bound entry",
            ))?;
            let e2 = *body.get(i + 2).ok_or(Error::InvalidData(
                "mpegps: truncated system header stream-bound entry",
            ))?;
            if e1 & 0xC0 != 0xC0 {
                return Err(Error::InvalidData(
                    "mpegps: system header stream-bound marker bits are wrong",
                ));
            }
            streams.push(StreamBound {
                stream_id: sid,
                buffer_scale: e1 & 0x20 != 0,
                buffer_size_bound: (u16::from(e1 & 0x1F) << 8) | u16::from(e2),
            });
            i += 3;
        }

        Ok(Some(Self {
            rate_bound,
            audio_bound,
            fixed_flag,
            csps_flag,
            system_audio_lock_flag,
            system_video_lock_flag,
            video_bound,
            packet_rate_restriction_flag,
            streams,
            len: total,
        }))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    /// Byte-for-byte the `mpeg1.mpg` pack header captured from
    /// `ffmpeg -f mpeg` (2026-08-23): `00 00 01 ba 21 00 01 00 01 a1 a1 ad`.
    #[test]
    fn a_measured_mpeg1_pack_header_parses() {
        let buf = [
            0x00, 0x00, 0x01, 0xba, 0x21, 0x00, 0x01, 0x00, 0x01, 0xa1, 0xa1, 0xad,
        ];
        let h = PackHeader::parse(&buf).unwrap().unwrap();
        assert_eq!(h.syntax, PackSyntax::Mpeg1);
        assert_eq!(h.len, 12);
        assert_eq!(h.scr_ext, 0);
    }

    /// Byte-for-byte the `vob1.vob` pack header captured from
    /// `ffmpeg -f vob` (2026-08-23):
    /// `00 00 01 ba 44 00 04 00 04 01 43 37 8b f8`.
    #[test]
    fn a_measured_mpeg2_pack_header_parses() {
        let buf = [
            0x00, 0x00, 0x01, 0xba, 0x44, 0x00, 0x04, 0x00, 0x04, 0x01, 0x43, 0x37, 0x8b, 0xf8,
        ];
        let h = PackHeader::parse(&buf).unwrap().unwrap();
        assert_eq!(h.syntax, PackSyntax::Mpeg2);
        assert_eq!(h.len, 14);
    }

    #[test]
    fn a_missing_start_code_is_refused() {
        let buf = [0x00, 0x00, 0x02, 0xba, 0x21, 0, 0, 0, 0, 0, 0, 0];
        assert!(PackHeader::parse(&buf).is_err());
    }

    #[test]
    fn a_truncated_header_yields_pending_not_error() {
        let buf = [0x00, 0x00, 0x01, 0xba, 0x21, 0x00];
        assert_eq!(PackHeader::parse(&buf).unwrap(), None);
    }

    #[test]
    fn an_unrecognised_syntax_byte_is_refused() {
        let buf = [0x00, 0x00, 0x01, 0xba, 0x00, 0, 0, 0, 0, 0, 0, 0];
        assert!(PackHeader::parse(&buf).is_err());
    }

    /// Measured system header from `vob1.vob`: `00 00 01 bb 00 0c a1 9b c5
    /// 04 21 ff e0 e0 e6 bd c0 20`, two stream-bound entries (video `0xe0`,
    /// private-stream-1 `0xbd`).
    #[test]
    fn a_measured_system_header_parses() {
        let buf = [
            0x00, 0x00, 0x01, 0xbb, 0x00, 0x0c, 0xa1, 0x9b, 0xc5, 0x04, 0x21, 0xff, 0xe0, 0xe0,
            0xe6, 0xbd, 0xc0, 0x20,
        ];
        let h = SystemHeader::parse(&buf).unwrap().unwrap();
        assert_eq!(h.len, 18);
        assert_eq!(h.streams.len(), 2);
        assert_eq!(h.streams[0].stream_id, 0xe0);
        assert_eq!(h.streams[1].stream_id, 0xbd);
    }

    #[test]
    fn a_system_header_needing_more_bytes_yields_none() {
        let buf = [0x00, 0x00, 0x01, 0xbb, 0x00, 0x0c, 0xa1];
        assert_eq!(SystemHeader::parse(&buf).unwrap(), None);
    }
}
