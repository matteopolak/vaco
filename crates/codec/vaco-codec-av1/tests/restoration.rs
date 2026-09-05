//! Scalar restoration checks; the independent dav1d vectors cover actual filter values.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "bounded test fixtures"
)]

use vaco_codec_av1::framebuf::Plane;
use vaco_codec_av1::restoration::{PlaneConfig, RestorationUnit, restore_plane};
use vaco_limits::{Budget, Limits};

fn plane(budget: &mut Budget, w: usize, h: usize, value: u16) -> Plane {
    let mut p = Plane::new(budget, w, h).unwrap();
    for y in 0..h {
        for x in 0..w {
            p.set(x, y, value);
        }
    }
    p
}

#[test]
fn unit_geometry_merges_short_tails_and_shifts_vertical_boundaries() {
    let config = PlaneConfig {
        width: 95,
        height: 161,
        bit_depth: 8,
        unit_size: 64,
        subsampling_y: false,
    };
    assert_eq!(config.unit_counts(), (1, 3));
    assert_eq!(config.unit_index(94, 55), 0);
    assert_eq!(config.unit_index(94, 56), 1);
    assert_eq!(config.unit_index(94, 119), 1);
    assert_eq!(config.unit_index(94, 120), 2);
    let chroma = PlaneConfig {
        width: 48,
        height: 81,
        unit_size: 32,
        subsampling_y: true,
        ..config
    };
    assert_eq!(chroma.unit_counts(), (2, 3));
    assert_eq!(chroma.unit_index(32, 27), 1);
    assert_eq!(chroma.unit_index(32, 28), 3);
}

#[test]
fn constant_plane_rounding_matches_dav1d_at_each_bit_depth() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut reference = std::fs::read(fixtures.join("restoration-constant-8.u16le")).unwrap();
    reference.extend(std::fs::read(fixtures.join("restoration-constant-high.u16le")).unwrap());
    let mut expected = reference.chunks_exact(2);
    for bit_depth in [8, 10, 12] {
        let mut budget = Budget::new(Limits::default());
        let value = (1 << bit_depth) - 1;
        let input = plane(&mut budget, 7, 9, value);
        let config = PlaneConfig {
            width: 7,
            height: 9,
            bit_depth,
            unit_size: 64,
            subsampling_y: false,
        };
        let unit = RestorationUnit::Wiener {
            vertical: [-5, -23, -17],
            horizontal: [10, 8, 46],
        };
        let output = restore_plane(&input, &input, config, &[unit], &mut budget).unwrap();
        assert_eq!(
            output.as_slice(),
            input.as_slice(),
            "Wiener depth={bit_depth}"
        );
        for set in 0..16 {
            let unit = RestorationUnit::SelfGuided {
                set,
                xqd: [-32, 31],
            };
            let output = restore_plane(&input, &input, config, &[unit], &mut budget).unwrap();
            for &actual in output.as_slice() {
                let bytes = expected.next().unwrap();
                assert_eq!(
                    actual,
                    u16::from_le_bytes([bytes[0], bytes[1]]),
                    "constant depth={bit_depth} set={set}"
                );
            }
        }
    }
    assert!(expected.next().is_none());
    assert!(expected.remainder().is_empty());
}

#[test]
fn invalid_configuration_and_coefficients_are_rejected() {
    let mut budget = Budget::new(Limits::default());
    let input = plane(&mut budget, 4, 4, 100);
    let config = PlaneConfig {
        width: 4,
        height: 4,
        bit_depth: 8,
        unit_size: 64,
        subsampling_y: false,
    };
    for bad in [
        PlaneConfig {
            unit_size: 0,
            ..config
        },
        PlaneConfig {
            bit_depth: 16,
            ..config
        },
        PlaneConfig { width: 5, ..config },
        PlaneConfig {
            height: 0,
            ..config
        },
    ] {
        assert!(restore_plane(&input, &input, bad, &[RestorationUnit::None], &mut budget).is_err());
    }
    for unit in [
        RestorationUnit::SelfGuided {
            set: 16,
            xqd: [0, 0],
        },
        RestorationUnit::SelfGuided {
            set: 0,
            xqd: [32, 0],
        },
        RestorationUnit::Wiener {
            vertical: [11, 0, 0],
            horizontal: [0; 3],
        },
    ] {
        assert!(restore_plane(&input, &input, config, &[unit], &mut budget).is_err());
    }
    assert!(restore_plane(&input, &input, config, &[], &mut budget).is_err());
}

fn sample(x: usize, y: usize, depth: u8, after: bool) -> u16 {
    (((x * 71 + y * 47 + x * y * 13 + usize::from(after) * 39) ^ ((x + y) * 29))
        & ((1 << depth) - 1)) as u16
}

fn oracle_unit(mode: usize, index: usize, chroma: bool) -> RestorationUnit {
    if mode < 16 {
        RestorationUnit::SelfGuided {
            set: ((mode + index) % 16) as u8,
            xqd: [
                -96 + i16::try_from((mode * 11 + index * 7) % 128).unwrap(),
                -32 + i16::try_from((mode * 17 + index * 13) % 128).unwrap(),
            ],
        }
    } else {
        let coefficients = [[-5, -23, -17], [10, 8, 46], [3, -7, 15], [0, 0, 0]];
        let mut horizontal = coefficients[(mode - 16 + index) % 4];
        let mut vertical = coefficients[(mode - 16 + index + 1) % 4];
        if chroma {
            horizontal[0] = 0;
            vertical[0] = 0;
        }
        RestorationUnit::Wiener {
            vertical,
            horizontal,
        }
    }
}

#[test]
fn dav1d_scalar_oracle_matches_all_pixels_across_depths_units_and_stripes() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut reference = std::fs::read(fixtures.join("restoration-dav1d-8.u16le")).unwrap();
    reference.extend(std::fs::read(fixtures.join("restoration-dav1d-high.u16le")).unwrap());
    let mut expected = reference.chunks_exact(2);
    let (mut cases, mut pixels) = (0, 0);
    for bit_depth in [8, 10, 12] {
        for (width, height, subsampling_y, unit_size) in [
            (7, 9, false, 64),
            (13, 65, false, 64),
            (97, 130, false, 64),
            (48, 81, true, 32),
            (135, 129, false, 128),
        ] {
            for mode in 0..20 {
                let mut budget = Budget::new(Limits::default());
                // Distinct padded edges also catch accidental reads past visible geometry.
                let mut before = plane(&mut budget, width + 8, height + 8, 0);
                let mut after = plane(&mut budget, width + 8, height + 8, 0);
                for y in 0..height {
                    for x in 0..width {
                        before.set(x, y, sample(x, y, bit_depth, false));
                        after.set(x, y, sample(x, y, bit_depth, true));
                    }
                }
                let config = PlaneConfig {
                    width,
                    height,
                    subsampling_y,
                    unit_size,
                    bit_depth,
                };
                let (cols, rows) = config.unit_counts();
                let units: Vec<_> = (0..cols * rows)
                    .map(|i| oracle_unit(mode, i, subsampling_y))
                    .collect();
                let output = restore_plane(&before, &after, config, &units, &mut budget).unwrap();
                assert_eq!(output.as_slice().len(), width * height);
                for (index, &actual) in output.as_slice().iter().enumerate() {
                    let bytes = expected.next().unwrap();
                    let wanted = u16::from_le_bytes([bytes[0], bytes[1]]);
                    assert_eq!(
                        actual, wanted,
                        "depth={bit_depth} {width}x{height} mode={mode} pixel={index}"
                    );
                    pixels += 1;
                }
                cases += 1;
            }
        }
    }
    assert!(expected.next().is_none());
    assert!(expected.remainder().is_empty());
    assert_eq!(cases, 300);
    assert_eq!(pixels, 2_089_260);
}

#[test]
fn none_preserves_visible_cdef_pixels_and_out_of_depth_input_is_rejected() {
    let mut budget = Budget::new(Limits::default());
    let before = plane(&mut budget, 12, 12, 16);
    let after = plane(&mut budget, 12, 12, 235);
    let config = PlaneConfig {
        width: 7,
        height: 9,
        bit_depth: 8,
        unit_size: 64,
        subsampling_y: false,
    };
    let output = restore_plane(
        &before,
        &after,
        config,
        &[RestorationUnit::None],
        &mut budget,
    )
    .unwrap();
    assert_eq!(output.width(), 7);
    assert_eq!(output.height(), 9);
    assert!(output.as_slice().iter().all(|v| *v == 235));
    let mut invalid = after;
    invalid.set(6, 8, 256);
    assert!(
        restore_plane(
            &before,
            &invalid,
            config,
            &[RestorationUnit::None],
            &mut budget
        )
        .is_err()
    );
}
