//! Decoder checks from Bluetooth SIG's official SBC conformance bitstreams.
//!
//! Each packet below is one independently decodable frame selected from the
//! published A2DP 1.3.2 conformance archive. The PCM oracle was produced by
//! decoding that one frame with `ffmpeg 9.0.1` as a black box.

#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]

use vaco_codec_simple_audio::sbc::{DecoderState, decode};
use vaco_core::Error;
use vaco_limits::{Budget, Limits};

const MONO_4: [u8; 42] = [
    0x9c, 0xf2, 0x12, 0xdd, 0xc8, 0x34, 0x36, 0xf1, 0x23, 0x54, 0x09, 0x2a, 0x46, 0x2b, 0x81, 0x89,
    0x1d, 0x5f, 0xa8, 0x56, 0xc1, 0xf5, 0x8a, 0x7d, 0x6d, 0x1f, 0x60, 0x07, 0xd8, 0xa6, 0x15, 0xff,
    0x79, 0x60, 0x5d, 0x51, 0x78, 0xd2, 0x75, 0xa4, 0xbf, 0x91,
];
const MONO_4_PCM: [i16; 64] = [
    0, 4, 0, -23, -55, -67, -33, 57, 165, 178, 4, -269, -387, -132, 517, 1362, 1966, 1596, -64,
    -2494, -4504, -5147, -4490, -2960, -1408, -124, 508, 774, 755, 725, 682, 681, 698, 741, 772,
    812, 842, 850, 811, 733, 629, 481, 256, -45, -380, -711, -1032, -1334, -1584, -1758, -1852,
    -1863, -1789, -1635, -1425, -1159, -848, -521, -227, 33, 284, 537, 742, 842,
];

const DUAL_4: [u8; 72] = [
    0x9c, 0xf6, 0x10, 0x22, 0xc8, 0x24, 0xc8, 0x34, 0x4f, 0x70, 0x1e, 0x72, 0xa7, 0x11, 0x73, 0xce,
    0xad, 0x63, 0x77, 0xe5, 0x9f, 0x1f, 0x76, 0x60, 0x9a, 0xdf, 0x77, 0x1c, 0x8b, 0x9f, 0x71, 0xa3,
    0x75, 0xe0, 0x62, 0x5d, 0x72, 0x9e, 0x52, 0x5f, 0x7f, 0xe0, 0x5a, 0x1f, 0x93, 0x1e, 0x6d, 0x1f,
    0xa2, 0xe2, 0x71, 0xa0, 0x9c, 0x9d, 0x63, 0x1e, 0x75, 0x1d, 0x4b, 0x9c, 0x51, 0xa3, 0x39, 0xe3,
    0x2e, 0x1a, 0x20, 0x9b, 0x38, 0x64, 0x27, 0x25,
];
const DUAL_4_PCM: [i16; 128] = [
    0, 0, 3, 5, 0, 0, -16, -30, -37, -72, -46, -89, -22, -45, 46, 68, 134, 197, 150, 206, 17, -8,
    -202, -338, -302, -472, -123, -142, 348, 681, 984, 1753, 1432, 2490, 1122, 2081, -249, 116,
    -2202, -2795, -3702, -5299, -4003, -6341, -3124, -5814, -1597, -4347, -7, -2730, 1252, -1538,
    2031, -970, 2398, -872, 2505, -965, 2482, -1043, 2408, -1040, 2331, -959, 2259, -842, 2194,
    -725, 2146, -629, 2135, -552, 2153, -493, 2159, -466, 2106, -481, 1971, -509, 1759, -519, 1466,
    -526, 1096, -585, 677, -749, 265, -1017, -93, -1345, -383, -1698, -606, -2050, -764, -2385,
    -853, -2664, -882, -2853, -839, -2927, -722, -2889, -538, -2753, -315, -2534, -52, -2249, 245,
    -1933, 564, -1618, 875, -1346, 1189, -1134, 1541, -988, 1936, -884, 2301, -830, 2554, -863,
];

const STEREO_8: [u8; 36] = [
    0x9c, 0x4b, 0x30, 0x30, 0xba, 0x97, 0x53, 0x21, 0xc9, 0x97, 0x53, 0x10, 0x8a, 0xbb, 0x0b, 0x8c,
    0xde, 0xfb, 0x94, 0x7e, 0xdb, 0x85, 0xc2, 0xeb, 0x8c, 0x68, 0xcc, 0x8c, 0xa0, 0xdb, 0x03, 0x2a,
    0x48, 0x3f, 0x86, 0x50,
];
const STEREO_8_PCM: [i16; 64] = [
    0, 0, 0, 0, 1, 0, 1, 0, 0, 0, -4, -2, -9, -2, -11, -1, -8, 3, 0, 9, 10, 15, 15, 16, 10, 9, -2,
    -2, -8, -13, 3, -15, 30, -6, 58, 8, 68, 18, 49, 16, 3, 2, -54, -14, -97, -19, -102, -3, -64,
    27, -9, 50, 10, 34, -47, -41, -163, -157, -249, -270, -206, -324, -9, -285,
];

const JOINT_8: [u8; 69] = [
    0x9c, 0x9d, 0x38, 0xa6, 0xfe, 0xba, 0x75, 0x22, 0x11, 0xa7, 0x42, 0x01, 0x11, 0xe1, 0xd8, 0x26,
    0x5c, 0x8f, 0xe3, 0x29, 0xf6, 0xc2, 0x72, 0xb5, 0x23, 0x51, 0x95, 0xa4, 0xbc, 0x63, 0xb8, 0x9c,
    0x61, 0x2d, 0x90, 0x44, 0x73, 0xc8, 0x18, 0x68, 0xe9, 0xe4, 0x3a, 0x73, 0x36, 0x88, 0xdb, 0x31,
    0x9f, 0x46, 0x64, 0x5b, 0x18, 0x59, 0x29, 0x2a, 0x1a, 0x1b, 0x56, 0x81, 0x91, 0x2d, 0xde, 0xdc,
    0x0d, 0x2b, 0x09, 0xee, 0xf2,
];
const JOINT_8_PCM: [i16; 128] = [
    0, 0, -1, -3, 0, -3, 1, -2, 0, 0, -4, 2, -4, 10, 0, 26, 11, 47, 20, 62, 26, 69, 25, 62, 16, 38,
    2, 0, -11, -43, -14, -76, 1, -81, 36, -54, 70, -6, 71, 31, 20, 35, -47, 33, -78, 67, -59, 135,
    -20, 186, -8, 156, -51, 8, -146, -257, -273, -608, -382, -971, -430, -1268, -378, -1415, -179,
    -1321, 112, -1009, 307, -645, 180, -466, -341, -559, -944, -635, -1190, -278, -913, 633, -244,
    1885, 535, 3114, 1237, 4068, 1839, 4710, 2305, 5029, 2541, 5000, 2603, 4769, 2606, 4537, 2595,
    4389, 2662, 4392, 2801, 4501, 2911, 4603, 2893, 4614, 2707, 4499, 2423, 4295, 2102, 4036, 1795,
    3766, 1458, 3464, 1008, 3063, 510, 2622, 10, 2169, -437, 1754, -767, 1428, -985, 1188,
];

fn assert_matches_oracle(packet: &[u8], expected: &[i16], rate: u32, channels: u32) {
    let mut state = DecoderState::default();
    let mut budget = Budget::new(Limits::permissive());
    let decoded = decode(&mut budget, &mut state, packet).unwrap();
    assert_eq!(decoded.sample_rate, rate);
    assert_eq!(decoded.channels, channels);
    assert_eq!(
        decoded.samples_per_channel as usize * channels as usize,
        expected.len()
    );
    assert_eq!(decoded.interleaved.len(), expected.len());

    let max_error = decoded
        .interleaved
        .iter()
        .zip(expected)
        .map(|(actual, oracle)| (i32::from(*actual) - i32::from(*oracle)).abs())
        .max()
        .unwrap();
    assert!(max_error <= 2, "maximum PCM error was {max_error} LSB");
}

#[test]
fn decodes_official_mono_four_subband_frame() {
    assert_matches_oracle(&MONO_4, &MONO_4_PCM, 48_000, 1);
}

#[test]
fn decodes_official_dual_channel_four_subband_frame() {
    assert_matches_oracle(&DUAL_4, &DUAL_4_PCM, 48_000, 2);
}

#[test]
fn decodes_official_stereo_eight_subband_frame() {
    assert_matches_oracle(&STEREO_8, &STEREO_8_PCM, 32_000, 2);
}

#[test]
fn decodes_official_joint_stereo_eight_subband_frame() {
    assert_matches_oracle(&JOINT_8, &JOINT_8_PCM, 44_100, 2);
}

#[test]
fn rejects_a_corrupt_crc_before_synthesis() {
    let mut packet = MONO_4;
    packet[3] ^= 1;
    let result = decode(
        &mut Budget::new(Limits::permissive()),
        &mut DecoderState::default(),
        &packet,
    );
    assert!(matches!(result, Err(Error::InvalidData(_))));
}

#[test]
fn rejects_a_bitpool_below_the_profile_minimum() {
    let mut packet = MONO_4;
    packet[2] = 1;
    let result = decode(
        &mut Budget::new(Limits::permissive()),
        &mut DecoderState::default(),
        &packet,
    );
    assert!(matches!(result, Err(Error::InvalidData(_))));
}

#[test]
fn rejects_a_truncated_audio_payload() {
    let result = decode(
        &mut Budget::new(Limits::permissive()),
        &mut DecoderState::default(),
        &MONO_4[..MONO_4.len() - 1],
    );
    assert!(matches!(result, Err(Error::UnexpectedEof)));
}
