//! `avcC` (ISO/IEC 14496-15 §5.3.3.1) → Annex-B `CodecPrivateData`.
//!
//! Smooth Streaming's `Manifest` states each video `QualityLevel`'s SPS/PPS
//! as `CodecPrivateData`: the raw NAL units, each preceded by a 4-byte
//! `00 00 00 01` Annex-B start code, concatenated and hex-encoded — measured
//! against real `ffmpeg -f smoothstreaming` output (`provenance/sources.toml`,
//! `ffmpeg-smoothstreaming-mux-probe`). `vaco-format-core`'s own
//! `CodecParameters::extradata` for an H.264 stream carries the *other*
//! representation instead — the `avcC` configuration record itself — so this
//! module's whole job is unpacking one into the other.
//!
//! This is a small, local, self-contained parser rather than a dependency on
//! `vaco-parse-h264`: per D14.1, a format/mux crate reaches codec-level
//! parsing only through the injected `ParserProvider` seam, never a direct
//! crate dependency, and `avcC`'s box layout is simple enough that hand
//! parsing it here is less machinery than routing through that seam for a
//! handful of length-prefixed byte copies.

/// One Annex-B start code, prepended to every NAL unit this module emits.
const START_CODE: [u8; 4] = [0x00, 0x00, 0x00, 0x01];

/// Unpack an `avcC` configuration record into Annex-B bytes: every SPS then
/// every PPS, each preceded by a 4-byte `00 00 00 01` start code.
///
/// Returns `None` when `extradata` is too short to be a valid `avcC` record,
/// or when a length prefix would run past the end of the buffer — a
/// malformed or absent extradata is something the caller should report as
/// `Unsupported`/`InvalidData`, not something this module should guess at.
///
/// # Layout (ISO/IEC 14496-15 §5.3.3.1.2)
///
/// ```text
/// configurationVersion        u8   (always 1)
/// AVCProfileIndication         u8
/// profile_compatibility        u8
/// AVCLevelIndication           u8
/// reserved(6) + lengthSizeMinusOne(2)   u8   (ignored here: Smooth Streaming
///                                              fragments always use 4-byte
///                                              NAL lengths, measured)
/// reserved(3) + numOfSequenceParameterSets(5)  u8
/// { u16 sps_length, u8[sps_length] sps }  × numOfSequenceParameterSets
/// numOfPictureParameterSets    u8
/// { u16 pps_length, u8[pps_length] pps }  × numOfPictureParameterSets
/// ```
#[must_use]
pub fn avcc_to_annexb(extradata: &[u8]) -> Option<Vec<u8>> {
    if extradata.len() < 6 || extradata.first() != Some(&1) {
        return None;
    }
    let mut out = Vec::new();
    let mut pos = 5usize;

    let num_sps = usize::from(*extradata.get(pos)? & 0x1f);
    pos += 1;
    for _ in 0..num_sps {
        pos = push_nal(extradata, pos, &mut out)?;
    }

    let num_pps = usize::from(*extradata.get(pos)?);
    pos += 1;
    for _ in 0..num_pps {
        pos = push_nal(extradata, pos, &mut out)?;
    }

    Some(out)
}

/// Read one `u16`-length-prefixed NAL at `pos`, append it (with its start
/// code) to `out`, and return the position just past it.
fn push_nal(data: &[u8], pos: usize, out: &mut Vec<u8>) -> Option<usize> {
    let len_bytes: [u8; 2] = data.get(pos..pos + 2)?.try_into().ok()?;
    let len = usize::from(u16::from_be_bytes(len_bytes));
    let nal = data.get(pos + 2..pos + 2 + len)?;
    out.extend_from_slice(&START_CODE);
    out.extend_from_slice(nal);
    Some(pos + 2 + len)
}

/// Lower-case hex, matching the case measured in real `ffmpeg` output.
#[must_use]
pub fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    /// The exact bytes `vaco-mux-dash`'s own roundtrip test uses for a
    /// minimal H.264 `avcC`: version 1, one SPS (`67 42 00 0A`), one PPS
    /// (`68 CE`).
    fn sample_avcc() -> Vec<u8> {
        vec![
            1, 0x42, 0x00, 0x0A, 0xFF, 0xE1, 0x00, 0x04, 0x67, 0x42, 0x00, 0x0A, 0x01, 0x00, 0x02,
            0x68, 0xCE,
        ]
    }

    #[test]
    fn unpacks_sps_and_pps_with_start_codes() {
        let annexb = avcc_to_annexb(&sample_avcc()).unwrap();
        assert_eq!(
            annexb,
            vec![
                0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x0A, // SPS
                0x00, 0x00, 0x00, 0x01, 0x68, 0xCE, // PPS
            ]
        );
    }

    #[test]
    fn matches_the_real_ffmpeg_smoothstreaming_manifest_fixture() {
        // `mss-samples/out.ism/Manifest`, `QualityLevel` for the video track:
        // CodecPrivateData="0000000167f4000d919b28283f6022000003000200000300641e28532c0000000168ebe3c44844"
        let avcc = vec![
            0x01, 0xf4, 0x00, 0x0d, 0xff, 0xe1, 0x00, 0x19, 0x67, 0xf4, 0x00, 0x0d, 0x91, 0x9b,
            0x28, 0x28, 0x3f, 0x60, 0x22, 0x00, 0x00, 0x03, 0x00, 0x02, 0x00, 0x00, 0x03, 0x00,
            0x64, 0x1e, 0x28, 0x53, 0x2c, 0x01, 0x00, 0x06, 0x68, 0xeb, 0xe3, 0xc4, 0x48, 0x44,
        ];
        let annexb = avcc_to_annexb(&avcc).unwrap();
        assert_eq!(
            to_hex(&annexb),
            "0000000167f4000d919b28283f6022000003000200000300641e28532c0000000168ebe3c44844"
        );
    }

    #[test]
    fn rejects_truncated_or_non_avcc_input() {
        assert!(avcc_to_annexb(&[]).is_none());
        assert!(
            avcc_to_annexb(&[0, 1, 2, 3, 4, 5]).is_none(),
            "version byte must be 1"
        );
        assert!(
            avcc_to_annexb(&[1, 0, 0, 0, 0, 5]).is_none(),
            "5 SPS declared, none present"
        );
    }
}
