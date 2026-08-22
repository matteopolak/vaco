//! Unit and property tests.
//!
//! Three kinds of thing are checked here, and they have different oracles:
//!
//! * **String tables** are checked against values observed from the reference
//!   binary (ffmpeg/ffprobe 8.1), recorded in the tables below. These are the
//!   D6 contract: they appear verbatim in `-show_streams` output.
//! * **Coefficients** are checked against the specification's own printed
//!   values, and — where H.273 says a constant is a rounded derivation — against
//!   the derivation recomputed from the primaries.
//! * **Structure** (round trips, inverses, continuity) is checked two ways,
//!   split by whether the domain can be enumerated.
//!
//! # Exhaustive where the domain is finite, `proptest` where it is not
//!
//! All 256 code-point bytes, all 17 × 12 matrix/primary pairs, all 65 bit
//! depths and every printed name are **enumerated**. Sampling those would be
//! strictly weaker than enumerating them, so they stay hand-written loops.
//!
//! Arbitrary text and the continuous floating-point ranges get `proptest`,
//! where it earns its keep for shrinking: "the round trip fails somewhere in
//! `0..=1` for BT.2020 12-bit" is not a bug report, and "it fails at 0.0181"
//! is. The two sections are marked below.
#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::float_cmp,
    clippy::integer_division,
    clippy::needless_range_loop,
    clippy::many_single_char_names,
    reason = "a test that unwraps a None or indexes out of range is a failing \
              test, which is the correct outcome, and the lints exist to stop \
              library code panicking on hostile input. Exact float comparison \
              is meaningful where the value under test is exactly representable. \
              A 3x3 matrix product reads as row/column indices and nothing \
              else; rewriting it as nested `enumerate` calls makes it harder to \
              check against the algebra it is testing."
)]

use proptest::prelude::*;

use crate::{
    ChromaLocation, ColorInfo, ColorPrimaries, ColorRange, MatrixCoefficients,
    TransferCharacteristic,
};

/// Absolute tolerance for a value that has been through two transcendental
/// functions. Every curve in H.273 maps roughly `0..=1` to `0..=1`, so an
/// absolute bound is the meaningful one.
const EPS: f64 = 1e-9;

fn close(a: f64, b: f64, eps: f64) -> bool {
    (a - b).abs() <= eps
}

// ------------------------------------------------------------- name tables
//
// Observed from ffmpeg 8.1 by writing each code point into an H.264 VUI with
// the `h264_metadata` bitstream filter and reading it back with
// `ffprobe -show_entries stream=...`. Every unlisted code point printed
// `unknown`.

const PRIMARIES_OUTPUT: &[(u8, &str)] = &[
    (1, "bt709"),
    (2, "unknown"),
    (4, "bt470m"),
    (5, "bt470bg"),
    (6, "smpte170m"),
    (7, "smpte240m"),
    (8, "film"),
    (9, "bt2020"),
    (10, "smpte428"),
    (11, "smpte431"),
    (12, "smpte432"),
    (22, "ebu3213"),
];

const TRANSFER_OUTPUT: &[(u8, &str)] = &[
    (1, "bt709"),
    (2, "unknown"),
    (4, "bt470m"),
    (5, "bt470bg"),
    (6, "smpte170m"),
    (7, "smpte240m"),
    (8, "linear"),
    (9, "log100"),
    (10, "log316"),
    (11, "iec61966-2-4"),
    (12, "bt1361e"),
    (13, "iec61966-2-1"),
    (14, "bt2020-10"),
    (15, "bt2020-12"),
    (16, "smpte2084"),
    (17, "smpte428"),
    (18, "arib-std-b67"),
];

const MATRIX_OUTPUT: &[(u8, &str)] = &[
    (0, "gbr"),
    (1, "bt709"),
    (2, "unknown"),
    (4, "fcc"),
    (5, "bt470bg"),
    (6, "smpte170m"),
    (7, "smpte240m"),
    (8, "ycgco"),
    (9, "bt2020nc"),
    (10, "bt2020c"),
    (11, "smpte2085"),
    (12, "chroma-derived-nc"),
    (13, "chroma-derived-c"),
    (14, "ictcp"),
    (15, "ipt-c2"),
    (16, "ycgco-re"),
    (17, "ycgco-ro"),
];

#[test]
fn output_names_match_the_reference() {
    for &(code, name) in PRIMARIES_OUTPUT {
        assert_eq!(ColorPrimaries::from_u8(code).unwrap().name(), name);
    }
    for &(code, name) in TRANSFER_OUTPUT {
        assert_eq!(TransferCharacteristic::from_u8(code).unwrap().name(), name);
    }
    for &(code, name) in MATRIX_OUTPUT {
        assert_eq!(MatrixCoefficients::from_u8(code).unwrap().name(), name);
    }
    for (code, name) in [(0u8, "unknown"), (1, "tv"), (2, "pc")] {
        assert_eq!(ColorRange::from_u8(code).unwrap().name(), name);
    }
    for (code, name) in [
        (0u8, "unspecified"),
        (1, "left"),
        (2, "center"),
        (3, "topleft"),
        (4, "top"),
        (5, "bottomleft"),
        (6, "bottom"),
    ] {
        assert_eq!(ChromaLocation::from_u8(code).unwrap().name(), name);
    }
}

/// D17: the output table and the option table are not the same table.
///
/// If someone "simplifies" the two into one, this fails — which is the point.
#[test]
fn output_names_and_option_names_diverge_exactly_where_the_reference_does() {
    // `-color_trc bt470m` is rejected by the reference; `gamma22` selects 4.
    assert_eq!(TransferCharacteristic::from_u8(4).unwrap().name(), "bt470m");
    assert_eq!(TransferCharacteristic::from_name("bt470m"), None);
    assert_eq!(
        TransferCharacteristic::from_name("gamma22"),
        Some(TransferCharacteristic::Gamma22)
    );
    assert_eq!(
        TransferCharacteristic::from_u8(5).unwrap().name(),
        "bt470bg"
    );
    assert_eq!(TransferCharacteristic::from_name("bt470bg"), None);

    // `-colorspace gbr` is rejected; `rgb` selects 0.
    assert_eq!(MatrixCoefficients::from_u8(0).unwrap().name(), "gbr");
    assert_eq!(MatrixCoefficients::from_name("gbr"), None);
    assert_eq!(
        MatrixCoefficients::from_name("rgb"),
        Some(MatrixCoefficients::Identity)
    );

    // Primaries 22 is `jedec-p22` on the command line, `ebu3213` in output.
    // Unusually, the reference accepts both spellings as options.
    assert_eq!(ColorPrimaries::from_u8(22).unwrap().name(), "ebu3213");
    assert_eq!(
        ColorPrimaries::from_name("jedec-p22"),
        Some(ColorPrimaries::Ebu3213)
    );
    assert_eq!(
        ColorPrimaries::from_name("ebu3213"),
        Some(ColorPrimaries::Ebu3213)
    );

    // And the unspecified value prints "unknown" everywhere except chroma
    // location, which prints "unspecified".
    assert_eq!(ColorPrimaries::default().name(), "unknown");
    assert_eq!(TransferCharacteristic::default().name(), "unknown");
    assert_eq!(MatrixCoefficients::default().name(), "unknown");
    assert_eq!(ColorRange::default().name(), "unknown");
    assert_eq!(ChromaLocation::default().name(), "unspecified");
}

#[test]
fn option_aliases_resolve() {
    for (name, expected) in [
        ("smpte428_1", ColorPrimaries::Smpte428),
        ("unspecified", ColorPrimaries::Unspecified),
    ] {
        assert_eq!(ColorPrimaries::from_name(name), Some(expected));
    }
    for (name, expected) in [
        ("log", TransferCharacteristic::Log100),
        ("log_sqrt", TransferCharacteristic::Log316),
        ("iec61966_2_4", TransferCharacteristic::Iec61966_2_4),
        ("bt1361", TransferCharacteristic::Bt1361e),
        ("iec61966_2_1", TransferCharacteristic::Iec61966_2_1),
        ("bt2020_10bit", TransferCharacteristic::Bt2020_10),
        ("bt2020_12bit", TransferCharacteristic::Bt2020_12),
        ("smpte428_1", TransferCharacteristic::Smpte428),
    ] {
        assert_eq!(TransferCharacteristic::from_name(name), Some(expected));
    }
    for (name, expected) in [
        ("ycocg", MatrixCoefficients::YCgCo),
        ("bt2020_ncl", MatrixCoefficients::Bt2020Ncl),
        ("bt2020_cl", MatrixCoefficients::Bt2020Cl),
    ] {
        assert_eq!(MatrixCoefficients::from_name(name), Some(expected));
    }
    for (name, expected) in [
        ("mpeg", ColorRange::Limited),
        ("limited", ColorRange::Limited),
        ("jpeg", ColorRange::Full),
        ("full", ColorRange::Full),
    ] {
        assert_eq!(ColorRange::from_name(name), Some(expected));
    }
}

/// The reference's option parser is case-sensitive and rejects `BT709`.
#[test]
fn names_are_case_sensitive() {
    assert_eq!(ColorPrimaries::from_name("BT709"), None);
    assert_eq!(TransferCharacteristic::from_name("BT709"), None);
    assert_eq!(MatrixCoefficients::from_name("YCGCO"), None);
    assert_eq!(ChromaLocation::from_name("Left"), None);
}

#[test]
fn reserved_code_points_are_rejected() {
    for v in [0u8, 3, 13, 21, 23, 100, 255] {
        assert_eq!(ColorPrimaries::from_u8(v), None, "primaries {v}");
    }
    for v in [0u8, 3, 19, 100, 255] {
        assert_eq!(TransferCharacteristic::from_u8(v), None, "transfer {v}");
    }
    for v in [3u8, 18, 100, 255] {
        assert_eq!(MatrixCoefficients::from_u8(v), None, "matrix {v}");
    }
    assert_eq!(ColorRange::from_u8(3), None);
    assert_eq!(ChromaLocation::from_u8(7), None);
}

#[test]
fn all_lists_are_complete_and_ordered() {
    let codes: Vec<u8> = ColorPrimaries::all().iter().map(|p| p.to_u8()).collect();
    assert_eq!(
        codes,
        PRIMARIES_OUTPUT.iter().map(|e| e.0).collect::<Vec<_>>()
    );
    let codes: Vec<u8> = TransferCharacteristic::all()
        .iter()
        .map(|t| t.to_u8())
        .collect();
    assert_eq!(
        codes,
        TRANSFER_OUTPUT.iter().map(|e| e.0).collect::<Vec<_>>()
    );
    let codes: Vec<u8> = MatrixCoefficients::all()
        .iter()
        .map(|m| m.to_u8())
        .collect();
    assert_eq!(codes, MATRIX_OUTPUT.iter().map(|e| e.0).collect::<Vec<_>>());
}

// ------------------------------------------------------------- chromaticity

/// The defining property of the normalisation: R = G = B = 1 is the reference
/// white, and its Y is 1.
#[test]
fn white_maps_to_the_reference_white() {
    for &p in ColorPrimaries::all() {
        let (Some(m), Some(c)) = (p.rgb_to_xyz(), p.chromaticity()) else {
            assert_eq!(p, ColorPrimaries::Unspecified);
            continue;
        };
        let xyz = [
            m[0][0] + m[0][1] + m[0][2],
            m[1][0] + m[1][1] + m[1][2],
            m[2][0] + m[2][1] + m[2][2],
        ];
        assert!(
            close(xyz[1], 1.0, 1e-12),
            "{p:?}: Y of white was {}",
            xyz[1]
        );
        // ST 428-1's white is equal-energy by construction; for everything else
        // check the chromaticity of the result against the stated white point.
        let sum = xyz[0] + xyz[1] + xyz[2];
        assert!(close(xyz[0] / sum, c.white.0, 1e-12), "{p:?}: white x");
        assert!(close(xyz[1] / sum, c.white.1, 1e-12), "{p:?}: white y");
    }
}

#[test]
fn primaries_map_to_their_own_chromaticities() {
    for &p in ColorPrimaries::all() {
        // ST 428-1 has y = 0 primaries; the generic property does not apply.
        if matches!(p, ColorPrimaries::Unspecified | ColorPrimaries::Smpte428) {
            continue;
        }
        let m = p.rgb_to_xyz().unwrap();
        let c = p.chromaticity().unwrap();
        for (col, want) in [(0, c.red), (1, c.green), (2, c.blue)] {
            let (x, y, z) = (m[0][col], m[1][col], m[2][col]);
            let sum = x + y + z;
            assert!(close(x / sum, want.0, 1e-12), "{p:?} col {col}: x");
            assert!(close(y / sum, want.1, 1e-12), "{p:?} col {col}: y");
        }
    }
}

#[test]
fn xyz_round_trips() {
    for &p in ColorPrimaries::all() {
        let (Some(fwd), Some(inv)) = (p.rgb_to_xyz(), p.xyz_to_rgb()) else {
            continue;
        };
        for i in 0..3 {
            for j in 0..3 {
                let got: f64 = (0..3).map(|k| inv[i][k] * fwd[k][j]).sum();
                let want = f64::from(u8::from(i == j));
                assert!(close(got, want, 1e-12), "{p:?} [{i}][{j}] = {got}");
            }
        }
    }
}

#[test]
fn st428_is_the_identity() {
    let m = ColorPrimaries::Smpte428.rgb_to_xyz().unwrap();
    assert_eq!(m, [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
}

/// H.273's literal `Kr`/`Kb` for BT.709 and BT.2020 are the derivation from
/// their own primaries, rounded to four places. Recomputing it is a check on
/// both the chromaticity table and the derivation.
#[test]
fn derived_luma_agrees_with_the_stated_constants() {
    for (p, m) in [
        (ColorPrimaries::Bt709, MatrixCoefficients::Bt709),
        (ColorPrimaries::Bt2020, MatrixCoefficients::Bt2020Ncl),
    ] {
        let (dr, db) = p.chromaticity().unwrap().luma_coefficients().unwrap();
        let (sr, sb) = m.luma_coefficients().unwrap();
        assert!(close(dr, sr, 5e-5), "{p:?}: Kr derived {dr}, stated {sr}");
        assert!(close(db, sb, 5e-5), "{p:?}: Kb derived {db}, stated {sb}");
        // ... and they are NOT equal, which is why the stated value is used.
        assert_ne!(dr, sr);
    }
}

/// BT.601's coefficients are historical, not derived: they belong to the 1953
/// NTSC primaries and were kept when the primaries changed. Asserting the gap
/// stops anyone "fixing" the table by deriving them.
#[test]
fn bt601_luma_is_not_derived_from_its_primaries() {
    let (dr, db) = ColorPrimaries::Smpte170m
        .chromaticity()
        .unwrap()
        .luma_coefficients()
        .unwrap();
    let (sr, sb) = MatrixCoefficients::Smpte170m.luma_coefficients().unwrap();
    assert!((dr - sr).abs() > 0.08, "derived {dr}, stated {sr}");
    assert!((db - sb).abs() > 0.02, "derived {db}, stated {sb}");
}

// ------------------------------------------------------------------ matrices

#[test]
fn bt709_matrix_matches_the_published_values() {
    let f = MatrixCoefficients::Bt709.rgb_to_ycbcr().unwrap();
    assert_eq!(f[0], [0.2126, 1.0 - 0.2126 - 0.0722, 0.0722]);
    assert!(close(f[1][2], 0.5, 0.0));
    assert!(close(f[2][0], 0.5, 0.0));

    // The four numbers everyone quotes for BT.709 YCbCr -> RGB.
    let i = MatrixCoefficients::Bt709.ycbcr_to_rgb().unwrap();
    assert!(close(i[0][2], 1.5748, 1e-12), "{}", i[0][2]);
    assert!(close(i[1][1], -0.187_324, 1e-6), "{}", i[1][1]);
    assert!(close(i[1][2], -0.468_124, 1e-6), "{}", i[1][2]);
    assert!(close(i[2][1], 1.8556, 1e-12), "{}", i[2][1]);
}

#[test]
fn matrices_invert() {
    for &m in MatrixCoefficients::all() {
        for &p in &[ColorPrimaries::Bt709, ColorPrimaries::Bt2020] {
            let (Some(fwd), Some(inv)) = (m.rgb_to_ycbcr_with(p), m.ycbcr_to_rgb_with(p)) else {
                continue;
            };
            for i in 0..3 {
                for j in 0..3 {
                    let got: f64 = (0..3).map(|k| inv[i][k] * fwd[k][j]).sum();
                    let want = f64::from(u8::from(i == j));
                    assert!(close(got, want, 1e-12), "{m:?} [{i}][{j}] = {got}");
                }
            }
        }
    }
}

#[test]
fn primary_colours_land_where_the_scaling_puts_them() {
    for &m in MatrixCoefficients::all() {
        let Some(fwd) = m.rgb_to_ycbcr() else {
            continue;
        };
        if m == MatrixCoefficients::Identity || m == MatrixCoefficients::YCgCo {
            continue;
        }
        let apply = |r: f64, g: f64, b: f64| {
            [
                fwd[0][0] * r + fwd[0][1] * g + fwd[0][2] * b,
                fwd[1][0] * r + fwd[1][1] * g + fwd[1][2] * b,
                fwd[2][0] * r + fwd[2][1] * g + fwd[2][2] * b,
            ]
        };
        // White is achromatic and full luma.
        let w = apply(1.0, 1.0, 1.0);
        assert!(close(w[0], 1.0, EPS) && close(w[1], 0.0, EPS) && close(w[2], 0.0, EPS));
        // Pure blue sits at Cb = +0.5, pure red at Cr = +0.5. That is the whole
        // reason for the 2(1-K) divisors.
        assert!(close(apply(0.0, 0.0, 1.0)[1], 0.5, EPS), "{m:?}");
        assert!(close(apply(1.0, 0.0, 0.0)[2], 0.5, EPS), "{m:?}");
    }
}

#[test]
fn identity_is_the_gbr_permutation() {
    let f = MatrixCoefficients::Identity.rgb_to_ycbcr().unwrap();
    let apply = |v: [f64; 3]| {
        [
            f[0][0] * v[0] + f[0][1] * v[1] + f[0][2] * v[2],
            f[1][0] * v[0] + f[1][1] * v[1] + f[1][2] * v[2],
            f[2][0] * v[0] + f[2][1] * v[1] + f[2][2] * v[2],
        ]
    };
    // (R, G, B) -> (Y, Cb, Cr) = (G, B, R), which is what `gbr` names.
    assert_eq!(apply([0.1, 0.2, 0.3]), [0.2, 0.3, 0.1]);
}

#[test]
fn ycgco_is_the_specified_lifting() {
    let f = MatrixCoefficients::YCgCo.rgb_to_ycbcr().unwrap();
    assert_eq!(f[0], [0.25, 0.5, 0.25]);
    assert_eq!(f[1], [-0.25, 0.5, -0.25]);
    assert_eq!(f[2], [0.5, 0.0, -0.5]);
    // Y = (R + 2G + B) / 4, which is the (0.25, 0.25) luma pair.
    assert_eq!(
        MatrixCoefficients::YCgCo.luma_coefficients(),
        Some((0.25, 0.25))
    );
}

#[test]
fn matrices_without_a_linear_form_say_so() {
    for m in [
        MatrixCoefficients::Unspecified,
        MatrixCoefficients::Bt2020Cl,
        MatrixCoefficients::ChromaDerivedCl,
        MatrixCoefficients::Smpte2085,
        MatrixCoefficients::Ictcp,
        MatrixCoefficients::IptC2,
        MatrixCoefficients::YCgCoRe,
        MatrixCoefficients::YCgCoRo,
    ] {
        assert_eq!(m.rgb_to_ycbcr(), None, "{m:?}");
        assert_eq!(m.ycbcr_to_rgb(), None, "{m:?}");
    }
    assert!(MatrixCoefficients::Bt2020Cl.is_constant_luminance());
    assert!(MatrixCoefficients::ChromaDerivedCl.is_constant_luminance());
    assert!(!MatrixCoefficients::Bt2020Ncl.is_constant_luminance());
    // Constant luminance still has a defined (Kr, Kb) even without a matrix.
    assert_eq!(
        MatrixCoefficients::Bt2020Cl.luma_coefficients(),
        Some((0.2627, 0.0593))
    );
}

#[test]
fn chroma_derived_needs_primaries() {
    let m = MatrixCoefficients::ChromaDerivedNcl;
    assert_eq!(m.luma_coefficients(), None);
    assert_eq!(m.rgb_to_ycbcr(), None);
    let (kr, kb) = m.luma_coefficients_with(ColorPrimaries::Bt2020).unwrap();
    assert!(close(kr, 0.2627, 5e-5) && close(kb, 0.0593, 5e-5));
    // A different primary set gives different coefficients, which is the point.
    let (kr2, _) = m.luma_coefficients_with(ColorPrimaries::Bt470m).unwrap();
    assert!((kr - kr2).abs() > 0.01);
}

// ----------------------------------------------------------------- transfer

#[test]
fn hlg_constants_are_self_consistent() {
    // b = 1 - 4a and c = 0.5 - a*ln(4a) are what make the two segments meet at
    // L = 1/12. The spec prints c to eight places; check the printed value
    // against the expression it abbreviates.
    let a = 0.178_832_77_f64;
    let c = 0.5 - a * (4.0 * a).ln();
    assert!(close(c, 0.559_910_73, 1e-8), "{c}");
    let t = TransferCharacteristic::AribStdB67;
    assert!(close(t.encode(1.0 / 12.0).unwrap(), 0.5, 1e-12));
    assert!(close(t.encode(1.0).unwrap(), 1.0, 1e-7));
    assert_eq!(t.encode(0.0), Some(0.0));
}

#[test]
fn pq_matches_st2084() {
    let t = TransferCharacteristic::Smpte2084;
    // 1.0 is 10000 cd/m^2 and encodes to full signal, exactly.
    assert!(close(t.encode(1.0).unwrap(), 1.0, 1e-12));
    // PQ's encode of zero is NOT zero: it is c1^m. Worth pinning, because a
    // "fix" that clamps it to zero breaks the inverse near black.
    let at_zero = t.encode(0.0).unwrap();
    assert!(at_zero > 0.0 && at_zero < 1e-6, "{at_zero}");
    // 100 cd/m^2 is the SDR reference, at signal ~0.508.
    assert!(close(t.encode(0.01).unwrap(), 0.508_078_4, 1e-6));
}

#[test]
fn known_curve_values() {
    // Every curve maps peak white to peak signal.
    for &t in TransferCharacteristic::all() {
        if t == TransferCharacteristic::Unspecified {
            assert_eq!(t.encode(1.0), None);
            assert_eq!(t.decode(1.0), None);
            continue;
        }
        let v = t.encode(1.0).unwrap();
        let want = if t == TransferCharacteristic::Smpte428 {
            // ST 428-1 normalises to 52.37 rather than 48, so its peak is below
            // 1 by construction.
            (48.0_f64 / 52.37).powf(1.0 / 2.6)
        } else {
            1.0
        };
        assert!(close(v, want, 1e-7), "{t:?} encode(1.0) = {v}");
    }
    assert_eq!(TransferCharacteristic::Linear.encode(0.37), Some(0.37));
    assert_eq!(TransferCharacteristic::Linear.decode(0.37), Some(0.37));
    // sRGB's 12.92 slope near black.
    let srgb = TransferCharacteristic::Iec61966_2_1;
    assert!(close(srgb.encode(0.001).unwrap(), 0.012_92, 1e-12));
    // BT.709's 4.5 slope near black.
    assert!(close(
        TransferCharacteristic::Bt709.encode(0.01).unwrap(),
        0.045,
        1e-12
    ));
    // The log curves floor out.
    assert_eq!(TransferCharacteristic::Log100.encode(0.001), Some(0.0));
    assert!(close(
        TransferCharacteristic::Log100.encode(0.1).unwrap(),
        0.5,
        1e-12
    ));
}

/// The extended-gamut curves are odd (or nearly so) about the origin; the rest
/// are only defined for non-negative light but must not produce NaN there.
#[test]
fn negative_light_never_produces_nan() {
    for &t in TransferCharacteristic::all() {
        if t == TransferCharacteristic::Unspecified {
            continue;
        }
        for l in [-1.0, -0.25, -0.01, -1e-9] {
            let v = t.encode(l).unwrap();
            assert!(!v.is_nan(), "{t:?} encode({l}) = NaN");
            assert!(!t.decode(v).unwrap().is_nan(), "{t:?} decode({v}) = NaN");
        }
    }
}

#[test]
fn hdr_classification() {
    assert!(TransferCharacteristic::Smpte2084.is_hdr());
    assert!(TransferCharacteristic::AribStdB67.is_hdr());
    for t in [
        TransferCharacteristic::Bt709,
        TransferCharacteristic::Iec61966_2_4,
        TransferCharacteristic::Bt1361e,
        TransferCharacteristic::Bt2020_12,
    ] {
        assert!(!t.is_hdr(), "{t:?}");
    }
}

// ------------------------------------------------------------------- levels

#[test]
fn eight_bit_levels_are_the_familiar_ones() {
    let l = ColorRange::Limited.luma_levels(8).unwrap();
    assert_eq!((l.offset, l.scale, l.min, l.max), (16, 219, 0, 255));
    // 16 + 219 = 235, the canonical narrow-range white.
    assert_eq!(l.offset + l.scale, 235);
    let c = ColorRange::Limited.chroma_levels(8).unwrap();
    assert_eq!((c.offset, c.scale), (128, 224));
    // 128 +/- 112 = 16..240.
    assert_eq!(c.offset - c.scale / 2, 16);
    assert_eq!(c.offset + c.scale / 2, 240);

    let l = ColorRange::Full.luma_levels(8).unwrap();
    assert_eq!((l.offset, l.scale, l.max), (0, 255, 255));
    let c = ColorRange::Full.chroma_levels(8).unwrap();
    assert_eq!((c.offset, c.scale), (128, 255));
}

#[test]
fn deeper_levels_scale_by_two() {
    for depth in [10u32, 12, 16] {
        let shift = depth - 8;
        let l = ColorRange::Limited.luma_levels(depth).unwrap();
        assert_eq!((l.offset, l.scale), (16 << shift, 219 << shift));
        let c = ColorRange::Limited.chroma_levels(depth).unwrap();
        assert_eq!((c.offset, c.scale), (128 << shift, 224 << shift));
        let f = ColorRange::Full.chroma_levels(depth).unwrap();
        assert_eq!(f.offset, 1 << (depth - 1));
        assert_eq!(f.scale, (1u32 << depth) - 1);
    }
    // 10-bit narrow range, the values a HEVC pipeline uses constantly.
    let l = ColorRange::Limited.luma_levels(10).unwrap();
    assert_eq!((l.offset, l.offset + l.scale, l.max), (64, 940, 1023));
}

#[test]
fn unspecified_range_quantises_as_limited() {
    for depth in 8..=16 {
        assert_eq!(
            ColorRange::Unspecified.luma_levels(depth),
            ColorRange::Limited.luma_levels(depth)
        );
        assert_eq!(
            ColorRange::Unspecified.chroma_levels(depth),
            ColorRange::Limited.chroma_levels(depth)
        );
    }
}

#[test]
fn unsupported_depths_are_rejected() {
    for depth in [0u32, 1, 7, 33, 64, u32::MAX] {
        assert_eq!(ColorRange::Limited.luma_levels(depth), None, "{depth}");
        assert_eq!(ColorRange::Full.chroma_levels(depth), None, "{depth}");
    }
    assert!(ColorRange::Full.luma_levels(32).is_some());
    assert_eq!(ColorRange::Full.luma_levels(32).unwrap().max, u32::MAX);
}

#[test]
fn full_range_flag_maps_both_ways() {
    assert_eq!(ColorRange::from_full_range_flag(true), ColorRange::Full);
    assert_eq!(ColorRange::from_full_range_flag(false), ColorRange::Limited);
}

// ------------------------------------------------------------ chroma siting

#[test]
fn chroma_offsets() {
    use ChromaLocation as C;
    assert_eq!(C::Unspecified.sample_offset_420(), None);
    assert_eq!(C::Left.sample_offset_420(), Some((0.0, 0.5)));
    assert_eq!(C::Center.sample_offset_420(), Some((0.5, 0.5)));
    assert_eq!(C::TopLeft.sample_offset_420(), Some((0.0, 0.0)));
    assert_eq!(C::Top.sample_offset_420(), Some((0.5, 0.0)));
    assert_eq!(C::BottomLeft.sample_offset_420(), Some((0.0, 1.0)));
    assert_eq!(C::Bottom.sample_offset_420(), Some((0.5, 1.0)));
}

/// H.264/H.265 number the same six positions from zero and have no
/// "unspecified" member, so the mapping is off by one.
#[test]
fn h264_loc_type_is_offset_by_one() {
    for t in 0u8..=5 {
        assert_eq!(
            ChromaLocation::from_h264_loc_type(t),
            ChromaLocation::from_u8(t + 1)
        );
    }
    assert_eq!(ChromaLocation::from_h264_loc_type(6), None);
    assert_eq!(ChromaLocation::from_h264_loc_type(255), None);
}

#[test]
fn color_info_defaults_are_unspecified() {
    let info = ColorInfo::default();
    assert_eq!(info.primaries, ColorPrimaries::Unspecified);
    assert_eq!(info.transfer, TransferCharacteristic::Unspecified);
    assert_eq!(info.matrix, MatrixCoefficients::Unspecified);
    assert_eq!(info.range, ColorRange::Unspecified);
    assert_eq!(info.chroma_location, ChromaLocation::Unspecified);
    assert!(!info.is_fully_specified());

    let info = ColorInfo {
        primaries: ColorPrimaries::Bt709,
        transfer: TransferCharacteristic::Bt709,
        matrix: MatrixCoefficients::Bt709,
        range: ColorRange::Limited,
        chroma_location: ChromaLocation::Left,
    };
    assert!(info.is_fully_specified());
}

// ------------------------------------------------- exhaustive invariant tests
//
// These sweeps are *exhaustive*, not sampled: the domain is small enough to
// enumerate, so enumerating it is strictly stronger than generating from it.
// They stay hand-written for that reason — see the `proptest` section below for
// the properties whose domains cannot be enumerated.

/// Every one of the 256 possible code-point bytes, for all five enums.
#[test]
fn code_points_round_trip() {
    for v in 0u8..=255 {
        if let Some(p) = ColorPrimaries::from_u8(v) {
            assert_eq!(p.to_u8(), v);
        }
        if let Some(t) = TransferCharacteristic::from_u8(v) {
            assert_eq!(t.to_u8(), v);
        }
        if let Some(m) = MatrixCoefficients::from_u8(v) {
            assert_eq!(m.to_u8(), v);
        }
        if let Some(r) = ColorRange::from_u8(v) {
            assert_eq!(r.to_u8(), v);
        }
        if let Some(c) = ChromaLocation::from_u8(v) {
            assert_eq!(c.to_u8(), v);
        }
    }
    // And the enums' own `all()` lists agree with `from_u8` about membership.
    assert_eq!(
        ColorPrimaries::all()
            .iter()
            .filter(|p| ColorPrimaries::from_u8(p.to_u8()) == Some(**p))
            .count(),
        ColorPrimaries::all().len()
    );
}

/// Every printed name, for every variant of every enum. The interesting case is
/// the D17 divergence, which has its own test above.
#[test]
fn every_output_name_is_stable() {
    for &p in ColorPrimaries::all() {
        assert!(!p.name().is_empty());
        if let Some(back) = ColorPrimaries::from_name(p.name()) {
            assert_eq!(back, p, "{p:?} name `{}`", p.name());
        }
    }
    for &t in TransferCharacteristic::all() {
        if let Some(back) = TransferCharacteristic::from_name(t.name()) {
            assert_eq!(back, t, "{t:?} name `{}`", t.name());
        }
    }
    for &m in MatrixCoefficients::all() {
        if let Some(back) = MatrixCoefficients::from_name(m.name()) {
            assert_eq!(back, m, "{m:?} name `{}`", m.name());
        }
    }
    for &r in ColorRange::all() {
        assert_eq!(ColorRange::from_name(r.name()), Some(r));
    }
    for &c in ChromaLocation::all() {
        assert_eq!(ChromaLocation::from_name(c.name()), Some(c));
    }
}

/// Every prefix and every suffix of every real name, plus every one- and
/// two-character string over the alphabet the names are drawn from.
///
/// This is where an accidental `starts_with` or a stray `trim` would show up,
/// and it is exhaustive over exactly the strings that could plausibly collide.
/// Arbitrary text is covered by the `proptest` case below.
#[test]
fn near_miss_names_are_rejected() {
    let check = |s: &str| {
        if let Some(p) = ColorPrimaries::from_name(s) {
            assert!(ColorPrimaries::all().contains(&p), "{s:?}");
        }
        if let Some(t) = TransferCharacteristic::from_name(s) {
            assert!(TransferCharacteristic::all().contains(&t), "{s:?}");
        }
        if let Some(m) = MatrixCoefficients::from_name(s) {
            assert!(MatrixCoefficients::all().contains(&m), "{s:?}");
        }
    };
    check("");
    let alphabet: Vec<char> = "abcdefgilmnoprstuvy0123456789-_.".chars().collect();
    for &a in &alphabet {
        check(&a.to_string());
        for &b in &alphabet {
            check(&format!("{a}{b}"));
        }
    }
    for &t in TransferCharacteristic::all() {
        let n = t.name();
        // Every prefix and suffix must still land inside the enum or nowhere.
        //
        // Deliberately NOT asserted: that a proper prefix never resolves to the
        // same variant. `log` is a declared alias of `log100`, so it does — and
        // that is the reference's option table, not a prefix match leaking
        // through. The two assertions below are the ones that catch a real
        // `starts_with` or `trim`, and they have no such exception.
        for i in 1..n.len() {
            if n.is_char_boundary(i) {
                check(&n[..i]);
                check(&n[i..]);
            }
        }
        assert_eq!(
            TransferCharacteristic::from_name(&format!("{n}x")),
            None,
            "`{n}x` must not resolve"
        );
        assert_eq!(
            TransferCharacteristic::from_name(&format!(" {n}")),
            None,
            "` {n}` must not resolve: the reference does not trim"
        );
        assert_eq!(
            TransferCharacteristic::from_name(&format!("{n} ")),
            None,
            "`{n} ` must not resolve"
        );
    }
}

/// The piecewise curves change branch exactly where H.273 says, and the jump at
/// the break point is the size the specification's rounded constants imply.
///
/// This is the test that catches a mis-transcribed alpha or beta: a wrong
/// constant moves the knee, and a wrong branch condition changes the gap.
/// Enumerated rather than generated, because the knees are six specific points
/// and a generator would find them only by luck.
#[test]
fn transfer_knees_are_where_the_spec_puts_them() {
    for (t, beta, slope) in [
        (TransferCharacteristic::Bt709, 0.018, 4.5),
        (TransferCharacteristic::Smpte170m, 0.018, 4.5),
        (TransferCharacteristic::Bt2020_10, 0.018, 4.5),
        (TransferCharacteristic::Bt2020_12, 0.0181, 4.5),
        (TransferCharacteristic::Smpte240m, 0.0228, 4.0),
        (TransferCharacteristic::Iec61966_2_1, 0.003_130_8, 12.92),
    ] {
        // Just below the break the curve is exactly linear.
        let below = beta * 0.5;
        assert!(
            close(t.encode(below).unwrap(), slope * below, 1e-15),
            "{t:?} linear segment"
        );
        // The two segments differ by less than a thousandth of full scale at
        // the break: enough to matter for an exact inverse, not enough to see.
        let jump = (t.encode(beta).unwrap() - slope * beta).abs();
        assert!(jump < 1e-3, "{t:?} discontinuity of {jump} at the knee");
        // And `decode` picks the branch that makes `decode(encode(l)) == l`.
        for l in [below, beta, beta * 1.001, beta * 100.0] {
            let back = t.decode(t.encode(l).unwrap()).unwrap();
            assert!((back - l).abs() < 1e-6, "{t:?} at {l}: {back}");
        }
    }
}

/// Every matrix against every primary set — 17 × 12 pairs, so enumerated.
/// The pairing matters: `ChromaDerivedNcl` produces a different matrix for each
/// primary set, and that cross-product is exactly where a bad derivation hides.
#[test]
fn matrix_inverses_compose() {
    for &m in MatrixCoefficients::all() {
        for &p in ColorPrimaries::all() {
            let (Some(f), Some(i)) = (m.rgb_to_ycbcr_with(p), m.ycbcr_to_rgb_with(p)) else {
                continue;
            };
            for r in 0..3 {
                for c in 0..3 {
                    let got: f64 = (0..3).map(|k| i[r][k] * f[k][c]).sum();
                    let want = f64::from(u8::from(r == c));
                    assert!((got - want).abs() < 1e-9, "{m:?}/{p:?} [{r}][{c}] = {got}");
                }
            }
        }
    }
}

/// The eight corners of the RGB cube through every matrix. Corners are where
/// the coefficients are least forgiving, and there are only eight of them.
#[test]
fn cube_corners_round_trip() {
    for &m in MatrixCoefficients::all() {
        let (Some(f), Some(inv)) = (m.rgb_to_ycbcr(), m.ycbcr_to_rgb()) else {
            continue;
        };
        for i in 0..8u8 {
            let rgb = [
                f64::from(i & 1),
                f64::from((i >> 1) & 1),
                f64::from((i >> 2) & 1),
            ];
            let back = apply(inv, apply(f, rgb));
            for k in 0..3 {
                assert!(
                    (back[k] - rgb[k]).abs() < 1e-12,
                    "{m:?}: {rgb:?} -> {back:?}"
                );
            }
        }
    }
}

/// Quantisation levels stay inside the representable range at every depth, and
/// every unsupported depth is refused rather than shifted by an out-of-range
/// amount. Exhaustive over 0..=64, which is every depth a `u32` shift can name.
/// Run under `overflow-checks`, this is also the shift-width test.
#[test]
fn levels_fit_their_depth() {
    for depth in 0u32..=64 {
        for &range in ColorRange::all() {
            let (Some(l), Some(c)) = (range.luma_levels(depth), range.chroma_levels(depth)) else {
                assert!(
                    !(8..=32).contains(&depth),
                    "depth {depth} should be supported"
                );
                continue;
            };
            assert_eq!(l.min, 0);
            assert_eq!(l.max, ((1u64 << depth) - 1) as u32);
            assert_eq!(c.max, l.max);
            assert!(u64::from(l.offset) + u64::from(l.scale) <= u64::from(l.max));
            assert!(u64::from(c.offset) + u64::from(c.scale) / 2 <= u64::from(c.max));
        }
    }
}

/// `m · v` for a 3×3 and a colour triple.
fn apply(m: [[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    let mut out = [0.0f64; 3];
    for (o, row) in out.iter_mut().zip(m) {
        *o = row.iter().zip(v).map(|(a, x)| a * x).sum();
    }
    out
}

// ------------------------------------------------------------ property tests
//
// These are the properties whose domain cannot be enumerated: arbitrary text,
// and the continuous floating-point ranges the transfer functions and matrices
// are defined over. `proptest` earns its keep here specifically for shrinking —
// "the round trip fails somewhere in `0..=1` for BT.2020 12-bit" is not a bug
// report, and "it fails at 0.0181" is.
//
// The finite domains above stay exhaustive on purpose. Sampling 256 code points
// instead of enumerating them would be a strictly weaker test.

fn any_primaries() -> impl Strategy<Value = ColorPrimaries> {
    prop::sample::select(ColorPrimaries::all())
}

fn any_transfer() -> impl Strategy<Value = TransferCharacteristic> {
    prop::sample::select(TransferCharacteristic::all())
}

fn any_matrix() -> impl Strategy<Value = MatrixCoefficients> {
    prop::sample::select(MatrixCoefficients::all())
}

proptest! {
    /// Arbitrary text must never panic and must never resolve to a value
    /// outside the enum. The generator produces plenty of multi-byte UTF-8,
    /// which is what proves nothing slices mid-character.
    #[test]
    fn from_name_is_total(s in ".{0,32}") {
        if let Some(p) = ColorPrimaries::from_name(&s) {
            prop_assert!(ColorPrimaries::all().contains(&p));
            prop_assert_eq!(ColorPrimaries::from_u8(p.to_u8()), Some(p));
        }
        if let Some(t) = TransferCharacteristic::from_name(&s) {
            prop_assert!(TransferCharacteristic::all().contains(&t));
            prop_assert_eq!(TransferCharacteristic::from_u8(t.to_u8()), Some(t));
        }
        if let Some(m) = MatrixCoefficients::from_name(&s) {
            prop_assert!(MatrixCoefficients::all().contains(&m));
            prop_assert_eq!(MatrixCoefficients::from_u8(m.to_u8()), Some(m));
        }
        if let Some(r) = ColorRange::from_name(&s) {
            prop_assert!(ColorRange::all().contains(&r));
        }
        if let Some(c) = ChromaLocation::from_name(&s) {
            prop_assert!(ChromaLocation::all().contains(&c));
            prop_assert!(c.sample_offset_420().is_some() || c == ChromaLocation::Unspecified);
        }
    }

    /// `decode` inverts `encode` everywhere the forward function is injective.
    #[test]
    fn transfer_round_trips(t in any_transfer(), l in 0.0f64..=1.0) {
        let Some(v) = t.encode(l) else {
            prop_assert_eq!(t, TransferCharacteristic::Unspecified);
            return Ok(());
        };
        prop_assert!(!v.is_nan(), "{:?}: encode({}) is NaN", t, l);
        // The log curves collapse everything below their floor onto zero, so
        // they are not injective there.
        let floor = match t {
            TransferCharacteristic::Log100 => 0.01,
            TransferCharacteristic::Log316 => 0.003_162_277_660_168_379_3,
            _ => 0.0,
        };
        if l < floor {
            return Ok(());
        }
        let back = t.decode(v).unwrap();
        // 1e-6 rather than 1e-12: H.273 prints the piecewise curves' break
        // points and scale factors rounded, so the two segments do not meet
        // exactly and a value inside the resulting gap cannot round-trip better
        // than the width of the gap. Measured worst case is ~6e-7, on BT.2020
        // 12-bit. `transfer_knees_are_where_the_spec_puts_them` pins the knees
        // themselves.
        prop_assert!(
            (back - l).abs() < 1e-6,
            "{:?}: {} -> {} -> {} (error {})", t, l, v, back, back - l
        );
    }

    /// Monotonic, because every one of these curves is a code-value assignment
    /// and a non-monotonic one would fold two light levels onto one code.
    ///
    /// Checked above the break points: H.273's rounded constants make the
    /// curves genuinely discontinuous there, and the size of that jump is
    /// asserted separately rather than smoothed over here.
    #[test]
    fn transfer_is_monotonic(
        t in any_transfer(),
        a in 0.1f64..=1.0,
        d in 0.0f64..=0.9,
    ) {
        let b = (a + d).min(1.0);
        let (Some(va), Some(vb)) = (t.encode(a), t.encode(b)) else {
            prop_assert_eq!(t, TransferCharacteristic::Unspecified);
            return Ok(());
        };
        prop_assert!(vb >= va, "{:?}: encode({}) = {} > encode({}) = {}", t, a, va, b, vb);
    }

    /// Both directions of a curve stay finite over the extended domain the
    /// wide-gamut rows are defined on, including negative light.
    #[test]
    fn transfer_stays_finite(t in any_transfer(), x in -1.0f64..=1.5) {
        let (Some(v), Some(l)) = (t.encode(x), t.decode(x)) else {
            prop_assert_eq!(t, TransferCharacteristic::Unspecified);
            return Ok(());
        };
        prop_assert!(!v.is_nan(), "{:?}: encode({}) is NaN", t, x);
        prop_assert!(!l.is_nan(), "{:?}: decode({}) is NaN", t, x);
    }

    /// A round trip through Y'CbCr and back must return the same colour, for
    /// any colour in the cube. The corners are covered exhaustively above.
    #[test]
    fn ycbcr_round_trips(
        m in any_matrix(),
        r in 0.0f64..=1.0,
        g in 0.0f64..=1.0,
        b in 0.0f64..=1.0,
    ) {
        let (Some(f), Some(inv)) = (m.rgb_to_ycbcr(), m.ycbcr_to_rgb()) else {
            return Ok(());
        };
        let rgb = [r, g, b];
        let back = apply(inv, apply(f, rgb));
        for k in 0..3 {
            prop_assert!(
                (back[k] - rgb[k]).abs() < 1e-12,
                "{:?}: {:?} -> {:?}", m, rgb, back
            );
        }
    }

    /// The same, through the XYZ matrices, over the whole primaries set.
    #[test]
    fn xyz_round_trips_for_any_colour(
        p in any_primaries(),
        r in 0.0f64..=1.0,
        g in 0.0f64..=1.0,
        b in 0.0f64..=1.0,
    ) {
        let (Some(fwd), Some(inv)) = (p.rgb_to_xyz(), p.xyz_to_rgb()) else {
            prop_assert_eq!(p, ColorPrimaries::Unspecified);
            return Ok(());
        };
        let rgb = [r, g, b];
        let back = apply(inv, apply(fwd, rgb));
        for k in 0..3 {
            prop_assert!(
                (back[k] - rgb[k]).abs() < 1e-12,
                "{:?}: {:?} -> {:?}", p, rgb, back
            );
        }
    }

    /// Y' is a convex combination of R', G' and B', so it never leaves the
    /// signal range, and Cb/Cr never leave -0.5..=0.5. A kernel sizes its
    /// intermediate precision on exactly this bound.
    #[test]
    fn ycbcr_stays_in_range(
        m in any_matrix(),
        p in any_primaries(),
        r in 0.0f64..=1.0,
        g in 0.0f64..=1.0,
        b in 0.0f64..=1.0,
    ) {
        let Some(f) = m.rgb_to_ycbcr_with(p) else { return Ok(()) };
        // The identity "matrix" is a permutation: all three of its outputs are
        // full-range, which is exactly why they quantise like luma.
        if m == MatrixCoefficients::Identity {
            return Ok(());
        }
        let [y, cb, cr] = apply(f, [r, g, b]);
        prop_assert!((-1e-12..=1.0 + 1e-12).contains(&y), "{:?}: Y = {}", m, y);
        prop_assert!(cb.abs() <= 0.5 + 1e-12, "{:?}: Cb = {}", m, cb);
        prop_assert!(cr.abs() <= 0.5 + 1e-12, "{:?}: Cr = {}", m, cr);
    }
}
