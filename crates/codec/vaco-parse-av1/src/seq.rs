//! `sequence_header_obu()`, AV1 spec §5.5.
//!
//! This is where almost everything `ffprobe -show_streams` prints for an AV1
//! stream comes from: profile, level, tier, bit depth, chroma subsampling,
//! colour signalling, and the coded picture size. §5.5's own reference frame
//! syntax (`frame_size_with_refs`) is the one place a *frame* header needs
//! state this header does not carry — everything here parses standalone from
//! one OBU's bytes.

use vaco_bitstream::BitReader;
use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::leb::uvlc;
use crate::profile::Tier;

/// `SELECT_SCREEN_CONTENT_TOOLS` / `SELECT_INTEGER_MV`, §3: "choose per frame"
/// rather than a fixed value — used both as the value a reduced still-picture
/// header implies and as the sentinel `2` a full header can read explicitly.
pub const SELECT_VALUE: u8 = 2;

/// `NUM_REF_FRAMES`, §3.
pub const NUM_REF_FRAMES: usize = 8;

/// The colour signalling half of `sequence_header_obu()`, §5.5.2
/// `color_config()`.
///
/// Kept as its own type because it is exactly what a pixel-format and
/// `ColorInfo` mapping needs, independent of everything else the sequence
/// header carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one independent field of color_config(), §5.5.2's own syntax table; \
              grouping them into enums would invent structure the specification does not have"
)]
pub struct ColorConfig {
    /// `BitDepth`: 8, 10 or 12.
    pub bit_depth: u8,
    pub mono_chrome: bool,
    /// Raw H.273 code points; `None` when `color_description_present_flag` was
    /// 0, meaning "unspecified" (H.273 code point 2) rather than absent.
    pub color_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
    /// `true` = full range, `false` = studio/limited.
    pub color_range: bool,
    pub subsampling_x: bool,
    pub subsampling_y: bool,
    /// H.273-style chroma sample position, 0..=3; only meaningful when both
    /// subsampling flags are set.
    pub chroma_sample_position: u8,
    pub separate_uv_delta_q: bool,
}

/// One entry of the `operating_points_cnt_minus_1 + 1` loop, §5.5.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OperatingPoint {
    pub idc: u16,
    pub seq_level_idx: u8,
    pub seq_tier: Tier,
    /// `decoder_model_present_for_this_op[i]`. Needed by the frame header's
    /// `buffer_removal_time` loop, which reads one value per operating point
    /// for which this is set.
    pub decoder_model_present: bool,
}

/// `timing_info()`, §5.5.3, when present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimingInfo {
    pub num_units_in_display_tick: u32,
    pub time_scale: u32,
    pub equal_picture_interval: bool,
    /// Only meaningful when `equal_picture_interval`.
    pub num_ticks_per_picture_minus_1: u64,
}

/// `sequence_header_obu()`, §5.5.1.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one independent field of sequence_header_obu()'s own syntax table; \
              grouping them into enums would invent structure the specification does not have"
)]
pub struct SequenceHeader {
    pub seq_profile: u8,
    pub still_picture: bool,
    pub reduced_still_picture_header: bool,
    pub timing_info: Option<TimingInfo>,
    /// One entry per operating point; at least one always exists.
    pub operating_points: Vec<OperatingPoint>,
    pub frame_width_bits: u8,
    pub frame_height_bits: u8,
    pub max_frame_width: u32,
    pub max_frame_height: u32,
    pub frame_id_numbers_present_flag: bool,
    /// `delta_frame_id_length_minus_2 + 2`, when frame ids are present.
    pub delta_frame_id_length: u8,
    /// `additional_frame_id_length_minus_1 + 1`, when frame ids are present.
    pub additional_frame_id_length: u8,
    pub use_128x128_superblock: bool,
    pub enable_filter_intra: bool,
    pub enable_intra_edge_filter: bool,
    pub enable_interintra_compound: bool,
    pub enable_masked_compound: bool,
    pub enable_warped_motion: bool,
    pub enable_dual_filter: bool,
    pub enable_order_hint: bool,
    pub enable_jnt_comp: bool,
    pub enable_ref_frame_mvs: bool,
    /// `SELECT_SCREEN_CONTENT_TOOLS` (2) means "read per frame".
    pub seq_force_screen_content_tools: u8,
    /// `SELECT_INTEGER_MV` (2) means "read per frame".
    pub seq_force_integer_mv: u8,
    /// `OrderHintBits`; 0 when `enable_order_hint` is false.
    pub order_hint_bits: u8,
    pub enable_superres: bool,
    pub enable_cdef: bool,
    pub enable_restoration: bool,
    pub color_config: ColorConfig,
    pub film_grain_params_present: bool,
    /// Whether `decoder_model_info()` was present — gates the frame header's
    /// `buffer_removal_time_present_flag` read.
    pub decoder_model_info_present_flag: bool,
    /// `buffer_removal_time_length_minus_1 + 1`, when decoder model info is
    /// present; the bit width of each `buffer_removal_time[op]` in the frame
    /// header.
    pub buffer_removal_time_length: u8,
}

impl SequenceHeader {
    /// Parse a `sequence_header_obu()` payload — the OBU's bytes with the
    /// `obu_header()` (and size field, if any) already stripped.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if any syntax element is malformed or the
    /// payload is truncated, or [`Error::LimitExceeded`] from `budget`.
    pub fn parse(payload: &[u8], budget: &mut Budget) -> Result<Self> {
        budget.check_metadata_bytes(payload.len() as u64)?;
        let mut r = BitReader::new(payload);
        let seq_profile = r.get(3) as u8;
        let still_picture = r.get_bit() != 0;
        let reduced_still_picture_header = r.get_bit() != 0;

        let mut timing_info = None;
        let mut decoder_model_info_present_flag = false;
        let mut buffer_delay_length_minus_1 = 0u32;
        let mut buffer_removal_time_length = 0u8;
        let mut operating_points = Vec::new();

        if reduced_still_picture_header {
            operating_points.push(OperatingPoint {
                idc: 0,
                seq_level_idx: r.get(5) as u8,
                seq_tier: Tier::Main,
                decoder_model_present: false,
            });
        } else {
            let timing_info_present_flag = r.get_bit() != 0;
            if timing_info_present_flag {
                let num_units_in_display_tick = r.get(32);
                let time_scale = r.get(32);
                let equal_picture_interval = r.get_bit() != 0;
                let num_ticks_per_picture_minus_1 = if equal_picture_interval {
                    uvlc(&mut r)
                } else {
                    0
                };
                timing_info = Some(TimingInfo {
                    num_units_in_display_tick,
                    time_scale,
                    equal_picture_interval,
                    num_ticks_per_picture_minus_1,
                });
                decoder_model_info_present_flag = r.get_bit() != 0;
                if decoder_model_info_present_flag {
                    // decoder_model_info(), §5.5.4. Only the field widths later
                    // reads depend on are kept; the rest is consumed and
                    // discarded.
                    buffer_delay_length_minus_1 = r.get(5);
                    let _num_units_in_decoding_tick = r.get(32);
                    buffer_removal_time_length = r.get(5) as u8 + 1;
                    let _frame_presentation_time_length_minus_1 = r.get(5);
                }
            }
            let initial_display_delay_present_flag = r.get_bit() != 0;
            let operating_points_cnt_minus_1 = r.get(5);
            budget.consume_fuel(u64::from(operating_points_cnt_minus_1) + 1)?;
            for _ in 0..=operating_points_cnt_minus_1 {
                let idc = r.get(12) as u16;
                let seq_level_idx = r.get(5) as u8;
                let seq_tier = if seq_level_idx > 7 {
                    Tier::from_flag(r.get_bit() != 0)
                } else {
                    Tier::Main
                };
                let decoder_model_present = decoder_model_info_present_flag && r.get_bit() != 0;
                if decoder_model_present {
                    // operating_parameters_info(), §5.5.5.
                    let n = buffer_delay_length_minus_1 + 1;
                    let _decoder_buffer_delay = r.get_long(n);
                    let _encoder_buffer_delay = r.get_long(n);
                    let _low_delay_mode_flag = r.get_bit();
                }
                if initial_display_delay_present_flag {
                    let initial_display_delay_present_for_this_op = r.get_bit() != 0;
                    if initial_display_delay_present_for_this_op {
                        let _initial_display_delay_minus_1 = r.get(4);
                    }
                }
                operating_points.push(OperatingPoint {
                    idc,
                    seq_level_idx,
                    seq_tier,
                    decoder_model_present,
                });
            }
        }

        let frame_width_bits_minus_1 = r.get(4) as u8;
        let frame_height_bits_minus_1 = r.get(4) as u8;
        let frame_width_bits = frame_width_bits_minus_1 + 1;
        let frame_height_bits = frame_height_bits_minus_1 + 1;
        let max_frame_width = r.get(u32::from(frame_width_bits)) + 1;
        let max_frame_height = r.get(u32::from(frame_height_bits)) + 1;

        let frame_id_numbers_present_flag = if reduced_still_picture_header {
            false
        } else {
            r.get_bit() != 0
        };
        let mut delta_frame_id_length = 0u8;
        let mut additional_frame_id_length = 0u8;
        if frame_id_numbers_present_flag {
            delta_frame_id_length = r.get(4) as u8 + 2;
            additional_frame_id_length = r.get(3) as u8 + 1;
        }

        let use_128x128_superblock = r.get_bit() != 0;
        let enable_filter_intra = r.get_bit() != 0;
        let enable_intra_edge_filter = r.get_bit() != 0;

        let (
            enable_interintra_compound,
            enable_masked_compound,
            enable_warped_motion,
            enable_dual_filter,
            enable_order_hint,
            enable_jnt_comp,
            enable_ref_frame_mvs,
            seq_force_screen_content_tools,
            seq_force_integer_mv,
            order_hint_bits,
        );
        if reduced_still_picture_header {
            enable_interintra_compound = false;
            enable_masked_compound = false;
            enable_warped_motion = false;
            enable_dual_filter = false;
            enable_order_hint = false;
            enable_jnt_comp = false;
            enable_ref_frame_mvs = false;
            seq_force_screen_content_tools = SELECT_VALUE;
            seq_force_integer_mv = SELECT_VALUE;
            order_hint_bits = 0u8;
        } else {
            enable_interintra_compound = r.get_bit() != 0;
            enable_masked_compound = r.get_bit() != 0;
            enable_warped_motion = r.get_bit() != 0;
            enable_dual_filter = r.get_bit() != 0;
            enable_order_hint = r.get_bit() != 0;
            let (jnt_comp, ref_frame_mvs) = if enable_order_hint {
                (r.get_bit() != 0, r.get_bit() != 0)
            } else {
                (false, false)
            };
            enable_jnt_comp = jnt_comp;
            enable_ref_frame_mvs = ref_frame_mvs;
            let seq_choose_screen_content_tools = r.get_bit() != 0;
            let force_screen_content_tools = if seq_choose_screen_content_tools {
                SELECT_VALUE
            } else {
                r.get_bit() as u8
            };
            seq_force_screen_content_tools = force_screen_content_tools;
            seq_force_integer_mv = if force_screen_content_tools > 0 {
                let seq_choose_integer_mv = r.get_bit() != 0;
                if seq_choose_integer_mv {
                    SELECT_VALUE
                } else {
                    r.get_bit() as u8
                }
            } else {
                SELECT_VALUE
            };
            order_hint_bits = if enable_order_hint {
                r.get(3) as u8 + 1
            } else {
                0
            };
        }

        let enable_superres = r.get_bit() != 0;
        let enable_cdef = r.get_bit() != 0;
        let enable_restoration = r.get_bit() != 0;

        let color_config = parse_color_config(&mut r, seq_profile);

        let film_grain_params_present = r.get_bit() != 0;

        r.check().map_err(|_| {
            Error::InvalidData("sequence_header_obu ran past the end of its payload")
        })?;

        if operating_points.is_empty() {
            return Err(Error::InvalidData(
                "sequence_header_obu declared zero operating points",
            ));
        }

        Ok(Self {
            seq_profile,
            still_picture,
            reduced_still_picture_header,
            timing_info,
            operating_points,
            frame_width_bits,
            frame_height_bits,
            max_frame_width,
            max_frame_height,
            frame_id_numbers_present_flag,
            delta_frame_id_length,
            additional_frame_id_length,
            use_128x128_superblock,
            enable_filter_intra,
            enable_intra_edge_filter,
            enable_interintra_compound,
            enable_masked_compound,
            enable_warped_motion,
            enable_dual_filter,
            enable_order_hint,
            enable_jnt_comp,
            enable_ref_frame_mvs,
            seq_force_screen_content_tools,
            seq_force_integer_mv,
            order_hint_bits,
            enable_superres,
            enable_cdef,
            enable_restoration,
            color_config,
            film_grain_params_present,
            decoder_model_info_present_flag,
            buffer_removal_time_length,
        })
    }

    /// Operating point 0 — the one this crate reports through
    /// [`crate::params::codec_parameters`]. §7.4 lets a decoder target any
    /// operating point the bitstream declares; the one every consumer decoder
    /// and every file this crate was tested against actually uses is 0.
    #[must_use]
    pub fn primary_operating_point(&self) -> Option<&OperatingPoint> {
        self.operating_points.first()
    }

    /// The display frame rate `timing_info()` implies, or `None` when timing
    /// info is absent or the picture interval is not constant.
    ///
    /// Most consumer encoders leave `timing_info_present_flag` at 0 and let
    /// the container carry the frame rate instead — measured across every
    /// `libsvtav1` fixture this crate was tested against — so this is a
    /// fallback a container-less raw `.obu` parse can use, not the primary
    /// source.
    #[must_use]
    pub fn frame_rate(&self) -> Option<vaco_core::Rational> {
        // `i32::MAX` as a `u32` literal, so the bound check below needs no
        // signed/unsigned cast of its own.
        const I32_MAX: u32 = 0x7FFF_FFFF;
        let t = self.timing_info?;
        if !t.equal_picture_interval || t.time_scale == 0 || t.time_scale > I32_MAX {
            return None;
        }
        let ticks = t.num_ticks_per_picture_minus_1.saturating_add(1);
        let den = u64::from(t.num_units_in_display_tick).saturating_mul(ticks);
        if den == 0 || den > u64::from(I32_MAX) {
            return None;
        }
        let den = u32::try_from(den).unwrap_or(I32_MAX);
        Some(vaco_core::Rational::new(
            t.time_scale.cast_signed(),
            den.cast_signed(),
        ))
    }
}

/// `color_config()`, §5.5.2.
fn parse_color_config(r: &mut BitReader<'_>, seq_profile: u8) -> ColorConfig {
    let high_bitdepth = r.get_bit() != 0;
    let bit_depth = if seq_profile == 2 && high_bitdepth {
        if r.get_bit() != 0 { 12 } else { 10 }
    } else if high_bitdepth {
        10
    } else {
        8
    };
    let mono_chrome = if seq_profile == 1 {
        false
    } else {
        r.get_bit() != 0
    };

    let color_description_present_flag = r.get_bit() != 0;
    let (color_primaries, transfer_characteristics, matrix_coefficients) =
        if color_description_present_flag {
            (r.get(8) as u8, r.get(8) as u8, r.get(8) as u8)
        } else {
            // CP_UNSPECIFIED / TC_UNSPECIFIED / MC_UNSPECIFIED, all H.273 code
            // point 2.
            (2, 2, 2)
        };

    if mono_chrome {
        let color_range = r.get_bit() != 0;
        return ColorConfig {
            bit_depth,
            mono_chrome: true,
            color_primaries,
            transfer_characteristics,
            matrix_coefficients,
            color_range,
            subsampling_x: true,
            subsampling_y: true,
            chroma_sample_position: 0,
            separate_uv_delta_q: false,
        };
    }

    // CP_BT_709 = 1, TC_SRGB = 13, MC_IDENTITY = 0: the sRGB/identity special
    // case forces full range 4:4:4 without reading `color_range`.
    let srgb_identity =
        color_primaries == 1 && transfer_characteristics == 13 && matrix_coefficients == 0;
    let (color_range, subsampling_x, subsampling_y) = if srgb_identity {
        (true, false, false)
    } else {
        let color_range = r.get_bit() != 0;
        match seq_profile {
            0 => (color_range, true, true),
            1 => (color_range, false, false),
            _ => {
                if bit_depth == 12 {
                    let ssx = r.get_bit() != 0;
                    let ssy = if ssx { r.get_bit() != 0 } else { false };
                    (color_range, ssx, ssy)
                } else {
                    (color_range, true, false)
                }
            }
        }
    };
    let chroma_sample_position = if subsampling_x && subsampling_y {
        r.get(2) as u8
    } else {
        0
    };
    let separate_uv_delta_q = r.get_bit() != 0;

    ColorConfig {
        bit_depth,
        mono_chrome: false,
        color_primaries,
        transfer_characteristics,
        matrix_coefficients,
        color_range,
        subsampling_x,
        subsampling_y,
        chroma_sample_position,
        separate_uv_delta_q,
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn budget() -> Budget {
        Budget::new(Limits::strict())
    }

    /// The `OBU_SEQUENCE_HEADER` payload from `sample.mp4`'s `av1C`
    /// (`ffmpeg -c:v libsvtav1`, 642x358, yuv420p): profile 0, level 2.1,
    /// 8-bit 4:2:0.
    fn real_seq_header_payload() -> [u8; 11] {
        [
            0x00, 0x00, 0x00, 0x0c, 0xc5, 0x03, 0x65, 0x00, 0xbe, 0x00, 0x10,
        ]
    }

    #[test]
    fn a_real_sequence_header_matches_the_measured_stream() {
        let payload = real_seq_header_payload();
        let mut b = budget();
        let sh = SequenceHeader::parse(&payload, &mut b).expect("parses");
        assert_eq!(sh.seq_profile, 0);
        assert_eq!(sh.max_frame_width, 642);
        assert_eq!(sh.max_frame_height, 358);
        assert_eq!(sh.color_config.bit_depth, 8);
        assert!(!sh.color_config.mono_chrome);
        assert!(sh.color_config.subsampling_x && sh.color_config.subsampling_y);
        let op = sh.primary_operating_point().expect("one operating point");
        assert_eq!(op.seq_level_idx, 1); // "2.1", matches ffprobe's level=1
        assert_eq!(op.seq_tier, Tier::Main);
    }

    #[test]
    fn every_truncation_of_a_real_header_fails_cleanly_or_parses() {
        let payload = real_seq_header_payload();
        for n in 0..payload.len() {
            let mut b = budget();
            // Must never panic; truncated input either errors or (rarely, for
            // a prefix that happens to satisfy every remaining default) still
            // parses to *something* bounded.
            let _ = SequenceHeader::parse(&payload[..n], &mut b);
        }
    }

    /// Pushes fixed-width fields MSB-first, exactly as `BitReader::get` reads
    /// them — the same shape as the syntax tables cited throughout this
    /// module, so a test built with it is a direct transcription of a
    /// `sequence_header_obu()` trace rather than a hand-packed byte guess.
    #[derive(Default)]
    struct BitPusher {
        bits: Vec<u8>,
    }

    impl BitPusher {
        fn push(&mut self, value: u64, n: u32) -> &mut Self {
            for i in (0..n).rev() {
                self.bits.push(((value >> i) & 1) as u8);
            }
            self
        }

        fn bytes(&self) -> Vec<u8> {
            let mut out = vec![0u8; self.bits.len().div_ceil(8)];
            for (i, &bit) in self.bits.iter().enumerate() {
                if bit != 0 {
                    out[i / 8] |= 0x80 >> (i % 8);
                }
            }
            out
        }
    }

    #[test]
    fn a_reduced_still_picture_header_skips_the_operating_point_loop() {
        let mut p = BitPusher::default();
        p.push(0, 3) // seq_profile = 0
            .push(1, 1) // still_picture
            .push(1, 1) // reduced_still_picture_header
            .push(5, 5) // seq_level_idx, the reduced path's only op
            .push(15, 4) // frame_width_bits_minus_1 -> frame_width_bits = 16
            .push(15, 4) // frame_height_bits_minus_1 -> frame_height_bits = 16
            .push(63, 16) // max_frame_width_minus_1 -> width 64
            .push(63, 16) // max_frame_height_minus_1 -> height 64
            // frame_id_numbers_present_flag is *not* read in the reduced
            // path (implied 0), matching `SequenceHeader::parse`.
            .push(0, 1) // use_128x128_superblock
            .push(0, 1) // enable_filter_intra
            .push(0, 1) // enable_intra_edge_filter
            // The whole `else` block (interintra/masked/warped/dual filter,
            // order hint, screen-content/integer-mv choice) is skipped too.
            .push(0, 1) // enable_superres
            .push(0, 1) // enable_cdef
            .push(0, 1) // enable_restoration
            .push(0, 1) // color_config: high_bitdepth
            .push(1, 1) // mono_chrome (profile 0 reads this bit) -> gray
            .push(0, 1) // color_description_present_flag
            .push(1, 1) // color_range (mono_chrome's only remaining field)
            .push(0, 1); // film_grain_params_present
        let bytes = p.bytes();
        let mut b = budget();
        let sh = SequenceHeader::parse(&bytes, &mut b).expect("parses");
        assert!(sh.reduced_still_picture_header);
        assert_eq!(sh.operating_points.len(), 1);
        assert_eq!(sh.operating_points[0].seq_level_idx, 5);
        assert!(sh.color_config.mono_chrome);
        assert_eq!(sh.max_frame_width, 64);
        assert_eq!(sh.max_frame_height, 64);
    }
}
