//! Constant tables from ISO/IEC 11172-3 Annex B and ISO/IEC 13818-3 Annex B,
//! parsed programmatically from the standards' own text rather than
//! hand-transcribed, to keep transcription error out of the one place it is
//! hardest to notice.

#![allow(
    clippy::unreadable_literal,
    clippy::excessive_precision,
    reason = "spec tables, not authored numbers"
)]

pub(crate) const SYNTHESIS_WINDOW: [f32; 512] = [
    0.0,
    -1.5259e-05,
    -1.5259e-05,
    -1.5259e-05,
    -1.5259e-05,
    -1.5259e-05,
    -1.5259e-05,
    -3.0518e-05,
    -3.0518e-05,
    -3.0518e-05,
    -3.0518e-05,
    -4.5776e-05,
    -4.5776e-05,
    -6.1035e-05,
    -6.1035e-05,
    -7.6294e-05,
    -7.6294e-05,
    -9.1553e-05,
    -0.000106812,
    -0.000106812,
    -0.00012207,
    -0.000137329,
    -0.000152588,
    -0.000167847,
    -0.000198364,
    -0.000213623,
    -0.000244141,
    -0.000259399,
    -0.000289917,
    -0.000320435,
    -0.000366211,
    -0.000396729,
    -0.000442505,
    -0.000473022,
    -0.000534058,
    -0.000579834,
    -0.00062561,
    -0.000686646,
    -0.000747681,
    -0.000808716,
    -0.00088501,
    -0.000961304,
    -0.001037598,
    -0.001113892,
    -0.001205444,
    -0.001296997,
    -0.00138855,
    -0.001480103,
    -0.001586914,
    -0.001693726,
    -0.001785278,
    -0.001907349,
    -0.00201416,
    -0.002120972,
    -0.002243042,
    -0.002349854,
    -0.002456665,
    -0.002578735,
    -0.002685547,
    -0.002792358,
    -0.00289917,
    -0.002990723,
    -0.003082275,
    -0.003173828,
    0.003250122,
    0.003326416,
    0.003387451,
    0.003433228,
    0.003463745,
    0.003479004,
    0.003479004,
    0.003463745,
    0.003417969,
    0.003372192,
    0.00328064,
    0.003173828,
    0.003051758,
    0.002883911,
    0.002700806,
    0.002487183,
    0.002227783,
    0.001937866,
    0.001617432,
    0.001266479,
    0.000869751,
    0.000442505,
    -3.0518e-05,
    -0.000549316,
    -0.001098633,
    -0.001693726,
    -0.002334595,
    -0.003005981,
    -0.003723145,
    -0.004486084,
    -0.0052948,
    -0.006118774,
    -0.007003784,
    -0.007919312,
    -0.008865356,
    -0.009841919,
    -0.010848999,
    -0.011886597,
    -0.012939453,
    -0.014022827,
    -0.01512146,
    -0.016235352,
    -0.017349243,
    -0.018463135,
    -0.019577026,
    -0.020690918,
    -0.021789551,
    -0.022857666,
    -0.023910522,
    -0.024932861,
    -0.025909424,
    -0.02684021,
    -0.02772522,
    -0.028533936,
    -0.029281616,
    -0.029937744,
    -0.030532837,
    -0.031005859,
    -0.031387329,
    -0.031661987,
    -0.031814575,
    -0.031845093,
    -0.031738281,
    -0.031478882,
    0.031082153,
    0.030517578,
    0.029785156,
    0.028884888,
    0.027801514,
    0.026535034,
    0.025085449,
    0.023422241,
    0.021575928,
    0.01953125,
    0.01725769,
    0.014801025,
    0.012115479,
    0.009231567,
    0.006134033,
    0.002822876,
    -0.000686646,
    -0.004394531,
    -0.00831604,
    -0.012420654,
    -0.016708374,
    -0.021179199,
    -0.025817871,
    -0.030609131,
    -0.035552979,
    -0.040634155,
    -0.045837402,
    -0.051132202,
    -0.056533813,
    -0.06199646,
    -0.067520142,
    -0.073059082,
    -0.07862854,
    -0.084182739,
    -0.089706421,
    -0.095169067,
    -0.100540161,
    -0.105819702,
    -0.110946655,
    -0.115921021,
    -0.120697021,
    -0.125259399,
    -0.129562378,
    -0.133590698,
    -0.137298584,
    -0.140670776,
    -0.143676758,
    -0.146255493,
    -0.148422241,
    -0.150115967,
    -0.151306152,
    -0.15196228,
    -0.152069092,
    -0.151596069,
    -0.150497437,
    -0.148773193,
    -0.146362305,
    -0.143264771,
    -0.139450073,
    -0.134887695,
    -0.129577637,
    -0.123474121,
    -0.116577148,
    -0.108856201,
    0.100311279,
    0.090927124,
    0.080688477,
    0.069595337,
    0.057617187,
    0.044784546,
    0.031082153,
    0.01651001,
    0.001068115,
    -0.015228271,
    -0.03237915,
    -0.050354004,
    -0.069168091,
    -0.088775635,
    -0.109161377,
    -0.130310059,
    -0.152206421,
    -0.174789429,
    -0.198059082,
    -0.221984863,
    -0.246505737,
    -0.271591187,
    -0.297210693,
    -0.323318481,
    -0.349868774,
    -0.376800537,
    -0.404083252,
    -0.431655884,
    -0.459472656,
    -0.487472534,
    -0.515609741,
    -0.543823242,
    -0.572036743,
    -0.600219727,
    -0.628295898,
    -0.656219482,
    -0.683914185,
    -0.71131897,
    -0.738372803,
    -0.765029907,
    -0.791213989,
    -0.816864014,
    -0.841949463,
    -0.866363525,
    -0.890090942,
    -0.91305542,
    -0.935195923,
    -0.956481934,
    -0.976852417,
    -0.996246338,
    -1.01461792,
    -1.03193665,
    -1.04815674,
    -1.06321716,
    -1.07711792,
    -1.08978271,
    -1.10121155,
    -1.1113739,
    -1.120224,
    -1.12774658,
    -1.13392639,
    -1.13876343,
    -1.14221191,
    -1.14428711,
    1.14498901,
    1.14428711,
    1.14221191,
    1.13876343,
    1.13392639,
    1.12774658,
    1.120224,
    1.1113739,
    1.10121155,
    1.08978271,
    1.07711792,
    1.06321716,
    1.04815674,
    1.03193665,
    1.01461792,
    0.996246338,
    0.976852417,
    0.956481934,
    0.935195923,
    0.91305542,
    0.890090942,
    0.866363525,
    0.841949463,
    0.816864014,
    0.791213989,
    0.765029907,
    0.738372803,
    0.71131897,
    0.683914185,
    0.656219482,
    0.628295898,
    0.600219727,
    0.572036743,
    0.543823242,
    0.515609741,
    0.487472534,
    0.459472656,
    0.431655884,
    0.404083252,
    0.376800537,
    0.349868774,
    0.323318481,
    0.297210693,
    0.271591187,
    0.246505737,
    0.221984863,
    0.198059082,
    0.174789429,
    0.152206421,
    0.130310059,
    0.109161377,
    0.088775635,
    0.069168091,
    0.050354004,
    0.03237915,
    0.015228271,
    -0.001068115,
    -0.01651001,
    -0.031082153,
    -0.044784546,
    -0.057617187,
    -0.069595337,
    -0.080688477,
    -0.090927124,
    0.100311279,
    0.108856201,
    0.116577148,
    0.123474121,
    0.129577637,
    0.134887695,
    0.139450073,
    0.143264771,
    0.146362305,
    0.148773193,
    0.150497437,
    0.151596069,
    0.152069092,
    0.15196228,
    0.151306152,
    0.150115967,
    0.148422241,
    0.146255493,
    0.143676758,
    0.140670776,
    0.137298584,
    0.133590698,
    0.129562378,
    0.125259399,
    0.120697021,
    0.115921021,
    0.110946655,
    0.105819702,
    0.100540161,
    0.095169067,
    0.089706421,
    0.084182739,
    0.07862854,
    0.073059082,
    0.067520142,
    0.06199646,
    0.056533813,
    0.051132202,
    0.045837402,
    0.040634155,
    0.035552979,
    0.030609131,
    0.025817871,
    0.021179199,
    0.016708374,
    0.012420654,
    0.00831604,
    0.004394531,
    0.000686646,
    -0.002822876,
    -0.006134033,
    -0.009231567,
    -0.012115479,
    -0.014801025,
    -0.01725769,
    -0.01953125,
    -0.021575928,
    -0.023422241,
    -0.025085449,
    -0.026535034,
    -0.027801514,
    -0.028884888,
    -0.029785156,
    -0.030517578,
    0.031082153,
    0.031478882,
    0.031738281,
    0.031845093,
    0.031814575,
    0.031661987,
    0.031387329,
    0.031005859,
    0.030532837,
    0.029937744,
    0.029281616,
    0.028533936,
    0.02772522,
    0.02684021,
    0.025909424,
    0.024932861,
    0.023910522,
    0.022857666,
    0.021789551,
    0.020690918,
    0.019577026,
    0.018463135,
    0.017349243,
    0.016235352,
    0.01512146,
    0.014022827,
    0.012939453,
    0.011886597,
    0.010848999,
    0.009841919,
    0.008865356,
    0.007919312,
    0.007003784,
    0.006118774,
    0.0052948,
    0.004486084,
    0.003723145,
    0.003005981,
    0.002334595,
    0.001693726,
    0.001098633,
    0.000549316,
    3.0518e-05,
    -0.000442505,
    -0.000869751,
    -0.001266479,
    -0.001617432,
    -0.001937866,
    -0.002227783,
    -0.002487183,
    -0.002700806,
    -0.002883911,
    -0.003051758,
    -0.003173828,
    -0.00328064,
    -0.003372192,
    -0.003417969,
    -0.003463745,
    -0.003479004,
    -0.003479004,
    -0.003463745,
    -0.003433228,
    -0.003387451,
    -0.003326416,
    0.003250122,
    0.003173828,
    0.003082275,
    0.002990723,
    0.00289917,
    0.002792358,
    0.002685547,
    0.002578735,
    0.002456665,
    0.002349854,
    0.002243042,
    0.002120972,
    0.00201416,
    0.001907349,
    0.001785278,
    0.001693726,
    0.001586914,
    0.001480103,
    0.00138855,
    0.001296997,
    0.001205444,
    0.001113892,
    0.001037598,
    0.000961304,
    0.00088501,
    0.000808716,
    0.000747681,
    0.000686646,
    0.00062561,
    0.000579834,
    0.000534058,
    0.000473022,
    0.000442505,
    0.000396729,
    0.000366211,
    0.000320435,
    0.000289917,
    0.000259399,
    0.000244141,
    0.000213623,
    0.000198364,
    0.000167847,
    0.000152588,
    0.000137329,
    0.00012207,
    0.000106812,
    0.000106812,
    9.1553e-05,
    7.6294e-05,
    7.6294e-05,
    6.1035e-05,
    6.1035e-05,
    4.5776e-05,
    4.5776e-05,
    3.0518e-05,
    3.0518e-05,
    3.0518e-05,
    3.0518e-05,
    1.5259e-05,
    1.5259e-05,
    1.5259e-05,
    1.5259e-05,
    1.5259e-05,
    1.5259e-05,
];
// Scalefactor-band boundary tables, machine-generated from ISO/IEC 11172-3
// Annex B Table 3-B.8 (long: 23 boundaries / 22 bands; short: 13 boundaries /
// 12 bands, one window) and ISO/IEC 13818-3 Annex B Table B.2 for the
// low-sample-rate rates.
//
// The MPEG-1 long tables' final `576` was missing until it was measured back
// in: the generated rows stopped at the *21st* boundary, so the last band
// (418..576 at 44.1 kHz, 384..576 at 48 kHz, 550..576 at 32 kHz) had no
// window in requantisation's `sfb.windows(2)` and every spectral line in it
// stayed zero. Measured against ffmpeg 9.0.1 on full-band pink noise: our
// output was 70 dB down on the reference's above 16.03 kHz at both 44.1 and
// 48 kHz — the frequency each table's 21st boundary happens to sit at, which
// is why the symptom looked like a fixed-Hz lowpass rather than a table
// error. The low-sample-rate tables below always had their `576` and were
// unaffected in practice, their last band being above anything a real
// encoder codes at those rates.
//
// Only 21 scalefactors are transmitted for a long block, so the last band's
// `scalefac`/`pretab` lookups fall off the end of their tables and read 0 —
// which is what the standard specifies for it, not an accident of indexing.

pub(crate) const SFB_LONG_32000: [u16; 23] = [
    0, 4, 8, 12, 16, 20, 24, 30, 36, 44, 54, 66, 82, 102, 126, 156, 194, 240, 296, 364, 448, 550,
    576,
];
#[allow(
    dead_code,
    reason = "reserved for the short-block decode path, not yet implemented"
)]
pub(crate) const SFB_SHORT_32000: [u16; 13] = [0, 4, 8, 12, 16, 22, 30, 42, 58, 78, 104, 138, 180];
pub(crate) const SFB_LONG_44100: [u16; 23] = [
    0, 4, 8, 12, 16, 20, 24, 30, 36, 44, 52, 62, 74, 90, 110, 134, 162, 196, 238, 288, 342, 418,
    576,
];
#[allow(
    dead_code,
    reason = "reserved for the short-block decode path, not yet implemented"
)]
pub(crate) const SFB_SHORT_44100: [u16; 13] = [0, 4, 8, 12, 16, 22, 30, 40, 52, 66, 84, 106, 136];
pub(crate) const SFB_LONG_48000: [u16; 23] = [
    0, 4, 8, 12, 16, 20, 24, 30, 36, 42, 50, 60, 72, 88, 106, 128, 156, 190, 230, 276, 330, 384,
    576,
];
#[allow(
    dead_code,
    reason = "reserved for the short-block decode path, not yet implemented"
)]
pub(crate) const SFB_SHORT_48000: [u16; 13] = [0, 4, 8, 12, 16, 22, 28, 38, 50, 64, 80, 100, 126];
// `SFB_LONG_16000`/`22050`/`24000` are the MPEG-2 low-sample-rate long
// tables (`Vaco-Spec-Ref: iso-13818-3` Annex B, referenced from Table B.2 —
// this crate's extracted copy of ISO/IEC 13818-3 does not include Annex B's
// own numeric tables, only the clauses that reference them, so these were
// already present before this pass and are used here without a fresh
// primary-text re-check of the exact values; empirically verified below by
// decoding real `ffmpeg`-produced MPEG-2 low-sample-rate files).
// Every `SFB_SHORT_*` waits on the short-block decode gap `layer3.rs`'s
// module doc names and is not consulted by any decode path yet.
pub(crate) const SFB_LONG_16000: [u16; 23] = [
    0, 6, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 116, 140, 168, 200, 238, 284, 336, 396, 464, 522,
    576,
];
#[allow(
    dead_code,
    reason = "reserved for the short-block decode path, not yet implemented"
)]
pub(crate) const SFB_SHORT_16000: [u16; 14] =
    [0, 4, 8, 12, 18, 26, 36, 48, 62, 80, 104, 134, 174, 192];
pub(crate) const SFB_LONG_22050: [u16; 23] = [
    0, 6, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 116, 140, 168, 200, 238, 284, 336, 396, 464, 522,
    576,
];
#[allow(
    dead_code,
    reason = "reserved for the short-block decode path, not yet implemented"
)]
pub(crate) const SFB_SHORT_22050: [u16; 14] =
    [0, 4, 8, 12, 18, 24, 32, 42, 56, 74, 100, 132, 174, 192];
pub(crate) const SFB_LONG_24000: [u16; 23] = [
    0, 6, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 114, 136, 162, 194, 232, 278, 332, 394, 464, 540,
    576,
];
#[allow(
    dead_code,
    reason = "reserved for the short-block decode path, not yet implemented"
)]
pub(crate) const SFB_SHORT_24000: [u16; 14] =
    [0, 4, 8, 12, 18, 26, 36, 48, 62, 80, 104, 136, 180, 192];
// MPEG-2.5 (unofficial-but-universal; not part of any ISO standard — ISO/IEC
// 13818-3 defines MPEG-2 only) is not covered by any primary text this crate
// has access to. Every public description of the extension claims it reuses
// MPEG-2's own long-block scalefactor-band tables unchanged for the
// corresponding halved sample rate (8000 Hz shares 16000's table, 11025
// shares 22050's, 12000 shares 24000's) rather than defining new geometry.
// TESTED AND FOUND WRONG for at least two of the three rates: decoding real
// `ffmpeg`-produced MPEG-2.5 fixtures against these tables measured
// correlation ~0.10-0.32 at 8000 Hz and ~0.79 at 12000 Hz (both clearly
// broken, confirmed independent of bitrate), while 11025 Hz measured ~0.98 —
// read as a fixture that doesn't exercise the wrong bands rather than
// confirmation the sharing is correct, given the other two rates falsify the
// premise outright. Consequently `layer3::decode` rejects all of
// `Version::Mpeg25` with `Error::Unsupported` before these constants are
// ever reached; they are kept (rather than deleted) only as a record of the
// assumption that was tried and disproven, should someone later find the
// actual MPEG-2.5 geometry to implement correctly.
pub(crate) const SFB_LONG_8000: [u16; 23] = SFB_LONG_16000;
pub(crate) const SFB_LONG_11025: [u16; 23] = SFB_LONG_22050;
pub(crate) const SFB_LONG_12000: [u16; 23] = SFB_LONG_24000;
// Layer II bit-allocation tables, machine-generated from ISO/IEC 11172-3
// Annex B Tables 3-B.2a-d and Table 3-B.4.

#[derive(Debug, Clone, Copy)]
pub(crate) struct AllocRow {
    pub nbal: u8,
    pub nlevels: &'static [u32],
}

pub(crate) const LAYER2_TABLE_A: &[AllocRow] = &[
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 7, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383, 32767, 65535,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 7, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383, 32767, 65535,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 7, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383, 32767, 65535,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 65535,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 65535,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 65535,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 65535,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 65535,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 65535,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 65535,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 65535,
        ],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 7, 9, 15, 31, 65535],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 7, 9, 15, 31, 65535],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 7, 9, 15, 31, 65535],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 7, 9, 15, 31, 65535],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 7, 9, 15, 31, 65535],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 7, 9, 15, 31, 65535],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 7, 9, 15, 31, 65535],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 7, 9, 15, 31, 65535],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 7, 9, 15, 31, 65535],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 7, 9, 15, 31, 65535],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 7, 9, 15, 31, 65535],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 7, 9, 15, 31, 65535],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 65535],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 65535],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 65535],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 65535],
    },
];

pub(crate) const LAYER2_TABLE_B: &[AllocRow] = &[
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 7, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383, 32767, 65535,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 7, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383, 32767, 65535,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 7, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383, 32767, 65535,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 65535,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 65535,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 65535,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 65535,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 65535,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 65535,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 65535,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 65535,
        ],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 7, 9, 15, 31, 65535],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 7, 9, 15, 31, 65535],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 7, 9, 15, 31, 65535],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 7, 9, 15, 31, 65535],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 7, 9, 15, 31, 65535],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 7, 9, 15, 31, 65535],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 7, 9, 15, 31, 65535],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 7, 9, 15, 31, 65535],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 7, 9, 15, 31, 65535],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 7, 9, 15, 31, 65535],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 7, 9, 15, 31, 65535],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 7, 9, 15, 31, 65535],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 65535],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 65535],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 65535],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 65535],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 65535],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 65535],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 65535],
    },
];

pub(crate) const LAYER2_TABLE_C: &[AllocRow] = &[
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 5, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383, 32767,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 5, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383, 32767,
        ],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 9, 15, 31, 63, 127],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 9, 15, 31, 63, 127],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 9, 15, 31, 63, 127],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 9, 15, 31, 63, 127],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 9, 15, 31, 63, 127],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 9, 15, 31, 63, 127],
    },
];

pub(crate) const LAYER2_TABLE_D: &[AllocRow] = &[
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 5, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383, 32767,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 5, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383, 32767,
        ],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 9, 15, 31, 63, 127],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 9, 15, 31, 63, 127],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 9, 15, 31, 63, 127],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 9, 15, 31, 63, 127],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 9, 15, 31, 63, 127],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 9, 15, 31, 63, 127],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 9, 15, 31, 63, 127],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 9, 15, 31, 63, 127],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 9, 15, 31, 63, 127],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 9, 15, 31, 63, 127],
    },
];

/// (`nlevels` -> (C, D, grouped, `samples_per_codeword`, `bits_per_codeword`)),
/// ISO/IEC 11172-3 Table 3-B.4.
#[derive(Debug, Clone, Copy)]
pub(crate) struct QuantClass {
    pub nlevels: u32,
    pub c: f32,
    pub d: f32,
    pub grouped: bool,
    pub bits_per_codeword: u8,
}

pub(crate) const QUANT_CLASSES: &[QuantClass] = &[
    QuantClass {
        nlevels: 3,
        c: 1.33333333333,
        d: 0.5,
        grouped: true,
        bits_per_codeword: 5,
    },
    QuantClass {
        nlevels: 5,
        c: 1.6,
        d: 0.5,
        grouped: true,
        bits_per_codeword: 7,
    },
    QuantClass {
        nlevels: 7,
        c: 1.14285714286,
        d: 0.25,
        grouped: false,
        bits_per_codeword: 3,
    },
    QuantClass {
        nlevels: 9,
        c: 1.77777777777,
        d: 0.5,
        grouped: true,
        bits_per_codeword: 10,
    },
    QuantClass {
        nlevels: 15,
        c: 1.06666666666,
        d: 0.125,
        grouped: false,
        bits_per_codeword: 4,
    },
    QuantClass {
        nlevels: 31,
        c: 1.03225806452,
        d: 0.0625,
        grouped: false,
        bits_per_codeword: 5,
    },
    QuantClass {
        nlevels: 63,
        c: 1.01587301587,
        d: 0.03125,
        grouped: false,
        bits_per_codeword: 6,
    },
    QuantClass {
        nlevels: 127,
        c: 1.00787401575,
        d: 0.015625,
        grouped: false,
        bits_per_codeword: 7,
    },
    QuantClass {
        nlevels: 255,
        c: 1.00392156863,
        d: 0.0078125,
        grouped: false,
        bits_per_codeword: 8,
    },
    QuantClass {
        nlevels: 511,
        c: 1.00195694716,
        d: 0.00390625,
        grouped: false,
        bits_per_codeword: 9,
    },
    QuantClass {
        nlevels: 1023,
        c: 1.00097751711,
        d: 0.001953125,
        grouped: false,
        bits_per_codeword: 10,
    },
    QuantClass {
        nlevels: 2047,
        c: 1.00048851979,
        d: 0.0009765625,
        grouped: false,
        bits_per_codeword: 11,
    },
    QuantClass {
        nlevels: 4095,
        c: 1.00024420024,
        d: 0.00048828125,
        grouped: false,
        bits_per_codeword: 12,
    },
    QuantClass {
        nlevels: 8191,
        c: 1.00012208522,
        d: 0.00024414063,
        grouped: false,
        bits_per_codeword: 13,
    },
    QuantClass {
        nlevels: 16383,
        c: 1.00006103888,
        d: 0.00012207031,
        grouped: false,
        bits_per_codeword: 14,
    },
    QuantClass {
        nlevels: 32767,
        c: 1.00003051851,
        d: 6.103516e-05,
        grouped: false,
        bits_per_codeword: 15,
    },
    QuantClass {
        nlevels: 65535,
        c: 1.00001525902,
        d: 3.051758e-05,
        grouped: false,
        bits_per_codeword: 16,
    },
];

/// Layer I, II scalefactor table, ISO/IEC 11172-3 Table 3-B.1 (63 entries).
pub(crate) const LAYER12_SCALEFACTORS: [f32; 63] = [
    2.0,
    1.5874010519682,
    1.25992104989487,
    1.0,
    0.7937005259841,
    0.62996052494744,
    0.5,
    0.39685026299205,
    0.31498026247372,
    0.25,
    0.19842513149602,
    0.15749013123686,
    0.125,
    0.09921256574801,
    0.07874506561843,
    0.0625,
    0.04960628287401,
    0.03937253280921,
    0.03125,
    0.024803141437,
    0.01968626640461,
    0.015625,
    0.0124015707185,
    0.0098431332023,
    0.0078125,
    0.00620078535925,
    0.00492156660115,
    0.00390625,
    0.00310039267963,
    0.00246078330058,
    0.001953125,
    0.00155019633981,
    0.00123039165029,
    0.0009765625,
    0.00077509816991,
    0.00061519582514,
    0.00048828125,
    0.00038754908495,
    0.00030759791257,
    0.000244140625,
    0.00019377454248,
    0.00015379895629,
    0.0001220703125,
    9.688727124e-05,
    7.689947814e-05,
    6.103515625e-05,
    4.844363562e-05,
    3.844973907e-05,
    3.051757813e-05,
    2.422181781e-05,
    1.922486954e-05,
    1.525878906e-05,
    1.21109089e-05,
    9.61243477e-06,
    7.62939453e-06,
    6.05545445e-06,
    4.80621738e-06,
    3.81469727e-06,
    3.02772723e-06,
    2.40310869e-06,
    1.90734863e-06,
    1.51386361e-06,
    1.20155435e-06,
];

/// Layer III `pretab`, ISO/IEC 11172-3 Table 3-B.6 (21 entries).
pub(crate) const PRETAB: [u8; 21] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 3, 2,
];
/// Layer II low-sample-rate bit allocation, ISO/IEC 13818-3 Annex B Table B.1
/// (one table for all three low sample rates, unlike MPEG-1's four).
pub(crate) const LAYER2_TABLE_LSF: &[AllocRow] = &[
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383,
        ],
    },
    AllocRow {
        nbal: 4,
        nlevels: &[
            3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383,
        ],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 9, 15, 31, 63, 127],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 9, 15, 31, 63, 127],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 9, 15, 31, 63, 127],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 9, 15, 31, 63, 127],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 9, 15, 31, 63, 127],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 9, 15, 31, 63, 127],
    },
    AllocRow {
        nbal: 3,
        nlevels: &[3, 5, 9, 15, 31, 63, 127],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 9],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 9],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 9],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 9],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 9],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 9],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 9],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 9],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 9],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 9],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 9],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 9],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 9],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 9],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 9],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 9],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 9],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 9],
    },
    AllocRow {
        nbal: 2,
        nlevels: &[3, 5, 9],
    },
    AllocRow {
        nbal: 0,
        nlevels: &[],
    },
    AllocRow {
        nbal: 0,
        nlevels: &[],
    },
];
/// Layer III alias-reduction butterfly coefficients, ISO/IEC 11172-3 Table
/// 3-B.9: `ci`, from which `cs = 1/sqrt(1+ci^2)` and `ca = ci/sqrt(1+ci^2)`.
pub(crate) const ALIAS_CI: [f32; 8] = [
    -0.6, -0.535, -0.33, -0.185, -0.095, -0.041, -0.0142, -0.0037,
];

/// Layer III `scalefac_compress` (0..15) to `(slen1, slen2)` bit widths,
/// `Vaco-Spec-Ref: iso-11172-3` §2.4.1.7's worked table (16 rows, under the
/// `[[table]]` provenance threshold).
pub(crate) const SCALEFAC_COMPRESS: [(u8, u8); 16] = [
    (0, 0),
    (0, 1),
    (0, 2),
    (0, 3),
    (3, 0),
    (1, 1),
    (1, 2),
    (1, 3),
    (2, 1),
    (2, 2),
    (2, 3),
    (3, 1),
    (3, 2),
    (3, 3),
    (4, 2),
    (4, 3),
];
