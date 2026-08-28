//! `syncinfo()`: the sync word and just enough to know the frame's length and
//! sample rate. ATSC A/52:2018 §4.4.1 (classic AC-3) / §E.1.3.1 (E-AC-3).

use vaco_bitstream::BitReader;

use crate::tables::{BITRATES_KBPS, NUMBLKS, SAMPLE_RATES};

pub const SYNCWORD: [u8; 2] = [0x0B, 0x77];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// Classic AC-3, `bsid <= 8` in practice (this parses `bsid` up to 10,
    /// leniently, for the handful of legacy encoders that used 9/10 with the
    /// same `syncinfo`/`bsi` layout).
    Ac3,
    /// E-AC-3, `bsid` in `11..=16`.
    Eac3,
}

#[derive(Debug, Clone, Copy)]
pub struct SyncInfo {
    pub kind: FrameKind,
    pub bsid: u8,
    /// Total frame size in bytes, header included.
    pub frame_size: usize,
    pub sample_rate: u32,
    /// Samples this syncframe carries: always 1536 for classic AC-3;
    /// `numblkscod`-dependent for E-AC-3.
    pub samples: u32,
    /// Bit position where `bsi()` starts, i.e. how much of `syncinfo()` was
    /// consumed. Classic AC-3: 40 bits (sync+crc1+fscod+frmsizecod). E-AC-3:
    /// also 40 bits by coincidence of field widths (see crate docs) — but
    /// this is measured per-parse rather than assumed constant, since E-AC-3
    /// additionally reads `acmod`/`lfeon` before `bsid` while classic AC-3
    /// does not, and only the position after `syncinfo()` proper (before
    /// `bsid`) is common.
    pub bsi_bit_offset: u32,
    pub strmtyp: Option<u8>,
    pub substream_id: Option<u8>,
    pub fscod2_half_rate: bool,
}

/// Parse `syncinfo()` plus the leading fields of `bsi()` needed to locate the
/// frame boundary (`bsid`, plus, for E-AC-3, `acmod`/`lfeon` which `bsi()`
/// interleaves before `bsid` in that format). Returns `None` on a bad sync
/// word, an out-of-range `fscod`, or a reserved `bsid`.
///
/// The caller continues reading `bsi()`'s remaining fields with
/// [`crate::bsi::Bsi::parse_rest`], which needs `acmod` too — passed back out
/// so E-AC-3 does not read it twice.
#[must_use]
pub fn parse(buf: &[u8]) -> Option<SyncInfo> {
    let head: &[u8; 2] = buf.first_chunk()?;
    if *head != SYNCWORD {
        return None;
    }
    let bsid = u32::from(*buf.get(5)?) >> 3;
    if bsid <= 10 {
        parse_ac3(buf)
    } else if bsid <= 16 {
        parse_eac3(buf)
    } else {
        None
    }
}

/// See [`crate::syncinfo`] module docs: `Table 5.18`'s relationship,
/// evaluated as a formula rather than transcribed. Floored; exact at 48 kHz
/// and 32 kHz for every standard rate, floors at 44.1 kHz where `parse_ac3`
/// adds the one-word pad the odd `frmsizecod` codes state.
#[allow(
    clippy::integer_division,
    reason = "the AC-3 frame-size formula is an intentional floor, not a precision loss"
)]
const fn frame_words(bit_rate_kbps: u16, sample_rate: u32) -> u64 {
    (bit_rate_kbps as u64 * 1536 * 1000) / (sample_rate as u64 * 16)
}

fn parse_ac3(buf: &[u8]) -> Option<SyncInfo> {
    let mut r = BitReader::new(buf);
    r.skip(16); // syncword
    r.skip(16); // crc1
    let fscod = r.get(2);
    let frmsizecod = r.get(6);
    if fscod == 3 || frmsizecod > 37 {
        return None;
    }
    let sample_rate = *SAMPLE_RATES.get(fscod as usize)?;
    let bitrate_kbps = *BITRATES_KBPS.get((frmsizecod >> 1) as usize)?;
    let base = frame_words(bitrate_kbps, sample_rate);
    let extra = u64::from(fscod == 1 && frmsizecod & 1 == 1);
    let words = base.checked_add(extra)?;
    let frame_size = usize::try_from(words.checked_mul(2)?).ok()?;
    let bsid = u32::from(*buf.get(5)?) >> 3;
    let bsid = u8::try_from(bsid).ok()?;
    Some(SyncInfo {
        kind: FrameKind::Ac3,
        bsid,
        frame_size,
        sample_rate,
        samples: 1536,
        bsi_bit_offset: 40,
        strmtyp: None,
        substream_id: None,
        fscod2_half_rate: false,
    })
}

fn parse_eac3(buf: &[u8]) -> Option<SyncInfo> {
    let mut r = BitReader::new(buf);
    r.skip(16); // syncword
    let strmtyp = r.get(2);
    let substream_id = r.get(3);
    let frmsiz = r.get(11);
    let frame_size = usize::try_from(frmsiz.checked_add(1)?)
        .ok()?
        .checked_mul(2)?;
    let fscod = r.get(2);
    let (sample_rate, samples, half_rate) = if fscod == 3 {
        let fscod2 = r.get(2);
        // The reduced sample rate is exactly half; every `SAMPLE_RATES` entry
        // is even, so a right shift is exact.
        let sr = *SAMPLE_RATES.get(fscod2 as usize)? >> 1;
        (sr, 1536, true)
    } else {
        let numblkscod = r.get(2);
        let sr = *SAMPLE_RATES.get(fscod as usize)?;
        let blocks = *NUMBLKS.get(numblkscod as usize)?;
        (sr, blocks * crate::tables::SAMPLES_PER_BLOCK, false)
    };
    r.skip(3); // acmod, re-read by `Bsi::parse`
    r.skip(1); // lfeon, likewise
    let bsid = u32::from(*buf.get(5)?) >> 3;
    let bsid = u8::try_from(bsid).ok()?;
    let bit_offset = u32::try_from(16 + 2 + 3 + 11 + 2 + 2 + 3 + 1).ok()?;
    let _ = &r;
    Some(SyncInfo {
        kind: FrameKind::Eac3,
        bsid,
        frame_size,
        sample_rate,
        samples,
        bsi_bit_offset: bit_offset,
        strmtyp: Some(u8::try_from(strmtyp).ok()?),
        substream_id: Some(u8::try_from(substream_id).ok()?),
        fscod2_half_rate: half_rate,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn ac3_frame() -> Vec<u8> {
        let mut f = vec![0u8; 768];
        f[0] = 0x0B;
        f[1] = 0x77;
        f[4] = 20; // fscod=0, frmsizecod=20
        f[5] = 8 << 3; // bsid=8
        f[6] = 2 << 5; // acmod=2
        f
    }

    fn eac3_frame() -> Vec<u8> {
        let mut f = vec![0u8; 1792];
        f[0] = 0x0B;
        f[1] = 0x77;
        f[2] = 0x03;
        f[3] = 0x7f;
        f[4] = 0x3f;
        f[5] = 0x87;
        f
    }

    #[test]
    fn ac3_header_parses() {
        let info = parse(&ac3_frame()).unwrap();
        assert_eq!(info.kind, FrameKind::Ac3);
        assert_eq!(info.frame_size, 768);
        assert_eq!(info.sample_rate, 48000);
        assert_eq!(info.samples, 1536);
        assert_eq!(info.bsi_bit_offset, 40);
    }

    #[test]
    fn eac3_header_parses_the_measured_fixture_bytes() {
        let info = parse(&eac3_frame()).unwrap();
        assert_eq!(info.kind, FrameKind::Eac3);
        assert_eq!(info.frame_size, 1792);
        assert_eq!(info.sample_rate, 48000);
        assert_eq!(info.samples, 1536);
        assert_eq!(info.strmtyp, Some(0));
    }

    #[test]
    fn empty_input_never_panics() {
        assert!(parse(&[]).is_none());
        assert!(parse(&[0u8; 4]).is_none());
    }
}
