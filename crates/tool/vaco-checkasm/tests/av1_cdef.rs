//! Full direction/strength differential against pinned dav1d C output.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "fixed-size deterministic oracle corpus"
)]

use vaco_checkasm::{Differential, Kernel};
use vaco_codec_av1::cdef::{FilterParams, filter_block, find_direction};

#[derive(Clone, Debug)]
struct Case {
    input: [u16; 144],
    params: FilterParams,
    expected: Vec<u32>,
}

struct CdefOracle;

impl Kernel for CdefOracle {
    const NAME: &'static str = "vaco-codec-av1::cdef-dav1d";
    type Case = Case;
    type Lane = u32;

    fn cases() -> Vec<Case> {
        let fixture = include_bytes!("fixtures/av1-cdef-dav1d.bin");
        let mut cases = Vec::new();
        for bit_depth in [8, 10, 12] {
            for damping in 2..=6 {
                for direction in 0..8 {
                    for primary in 0..16 {
                        for secondary in [0, 1, 2, 4] {
                            let id = cases.len();
                            let mut input = [0; 144];
                            let mut state = 0x9e37_79b9u32 ^ id as u32;
                            for sample in &mut input {
                                state ^= state << 13;
                                state ^= state >> 17;
                                state ^= state << 5;
                                *sample = ((if id & 1 == 1 {
                                    96 + state % 33
                                } else {
                                    state & 255
                                }) as u16)
                                    << (bit_depth - 8)
                                    | ((state >> 8) as u16 & ((1 << (bit_depth - 8)) - 1));
                            }
                            let params = FilterParams {
                                width: if id % 3 == 0 { 8 } else { 4 },
                                height: if id % 3 == 2 { 4 } else { 8 },
                                edges: ((id / 3) % 16) as u8,
                                bit_depth,
                                direction,
                                primary: primary << (bit_depth - 8),
                                secondary: secondary << (bit_depth - 8),
                                damping: damping + bit_depth - 8,
                            };
                            let record = &fixture[id * 136..(id + 1) * 136];
                            let mut expected = vec![
                                u32::from_le_bytes(record[..4].try_into().unwrap()),
                                u32::from_le_bytes(record[4..8].try_into().unwrap()),
                            ];
                            expected.extend(
                                record[8..]
                                    .chunks_exact(2)
                                    .map(|b| u32::from(u16::from_le_bytes(b.try_into().unwrap()))),
                            );
                            cases.push(Case {
                                input,
                                params,
                                expected,
                            });
                        }
                    }
                }
            }
        }
        assert_eq!(fixture.len(), cases.len() * 136);
        cases
    }

    fn scalar(case: &Case) -> Vec<u32> {
        case.expected.clone()
    }

    fn vector(case: &Case) -> Vec<u32> {
        let mut luma = [0; 64];
        for y in 0..8 {
            luma[y * 8..y * 8 + 8]
                .copy_from_slice(&case.input[(y + 2) * 12 + 2..(y + 2) * 12 + 10]);
        }
        let (direction, variance) = find_direction(&luma, case.params.bit_depth).unwrap();
        let mut output = vec![u32::from(direction), variance];
        output.extend(
            filter_block(&case.input, case.params)
                .unwrap()
                .map(u32::from),
        );
        output
    }
}

#[test]
fn every_direction_strength_and_damping_matches_dav1d() {
    let report = Differential::<CdefOracle>::run();
    assert!(report.is_clean(), "{report}");
}

#[test]
fn oracle_exercises_all_search_directions_and_all_frame_edge_masks() {
    let cases = CdefOracle::cases();
    assert_eq!(cases.len(), 7680);
    let mut directions = [0; 8];
    let mut edges = [0; 16];
    for case in cases {
        directions[case.expected[0] as usize] += 1;
        edges[usize::from(case.params.edges)] += 1;
    }
    assert!(directions.into_iter().all(|count| count > 0));
    assert!(edges.into_iter().all(|count| count > 0));
}
