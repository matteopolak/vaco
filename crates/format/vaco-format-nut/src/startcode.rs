//! The five 8-byte startcodes and the file signature, computed exactly as
//! the specification defines them (`0x..low48.. + ((('N'<<8)+tag)<<48)`),
//! not copied as opaque byte constants — the formula is public and
//! reproducing it this way is what makes the constant self-documenting.

/// `"nut/multimedia container\0"` — the first 25 bytes of every NUT file.
pub const FILE_ID_STRING: &[u8] = b"nut/multimedia container\0";

const fn startcode(low48: u64, tag: u8) -> u64 {
    low48 + ((((b'N' as u64) << 8) + tag as u64) << 48)
}

pub const MAIN_STARTCODE: u64 = startcode(0x7A56_1F5F_04AD, b'M');
pub const STREAM_STARTCODE: u64 = startcode(0x1140_5BF2_F9DB, b'S');
pub const SYNCPOINT_STARTCODE: u64 = startcode(0xE4AD_EECA_4569, b'K');
pub const INDEX_STARTCODE: u64 = startcode(0xDD67_2F23_E64E, b'X');
pub const INFO_STARTCODE: u64 = startcode(0xAB68_B596_BA78, b'I');

#[cfg(test)]
mod tests {
    use super::*;

    /// Measured directly off a real `ffmpeg -f nut` file's first bytes
    /// after the signature: `4E 4D 7A 56 1F 5F 04 AD`.
    #[test]
    fn the_main_startcode_matches_the_measured_bytes() {
        assert_eq!(
            MAIN_STARTCODE.to_be_bytes(),
            [0x4E, 0x4D, 0x7A, 0x56, 0x1F, 0x5F, 0x04, 0xAD]
        );
    }

    /// Measured: the stream header immediately following, `4E 53 11 40 5B
    /// F2 F9 DB`.
    #[test]
    fn the_stream_startcode_matches_the_measured_bytes() {
        assert_eq!(
            STREAM_STARTCODE.to_be_bytes(),
            [0x4E, 0x53, 0x11, 0x40, 0x5B, 0xF2, 0xF9, 0xDB]
        );
    }

    /// Measured: the info packet startcode, `4E 49 AB 68 B5 96 BA 78`.
    #[test]
    fn the_info_startcode_matches_the_measured_bytes() {
        assert_eq!(
            INFO_STARTCODE.to_be_bytes(),
            [0x4E, 0x49, 0xAB, 0x68, 0xB5, 0x96, 0xBA, 0x78]
        );
    }

    #[test]
    fn every_startcode_begins_with_n() {
        for sc in [
            MAIN_STARTCODE,
            STREAM_STARTCODE,
            SYNCPOINT_STARTCODE,
            INDEX_STARTCODE,
            INFO_STARTCODE,
        ] {
            assert_eq!(sc.to_be_bytes()[0], b'N');
        }
    }
}
